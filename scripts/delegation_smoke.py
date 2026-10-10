"""Real daemon/CLI governance and native delegation using an offline provider."""
import http.server
import json
from pathlib import Path
import subprocess
import threading
import time

from chat_smoke import isolated_env, request, run, start_daemon, stop_daemon, wait_turn


class DelegationFixture:
    def __init__(self):
        self.project_id = None
        self.calls = []
        self.lock = threading.Lock()
        self.children_release = threading.Event()
        self.held_release = threading.Event()
        self.manual_release = threading.Event()
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                if self.path != "/v1/chat/completions" or self.headers.get("Authorization") != "Bearer offline-test-key":
                    self.send_error(401)
                    return
                payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                prompt = next(m["content"] for m in payload["messages"] if m["role"] == "user")
                results = [m for m in payload["messages"] if m["role"] == "tool"]
                with fixture.lock:
                    fixture.calls.append(payload)

                def call(name, args, index=0):
                    return {"index": index, "id": f"fixture-{len(results)}-{index}", "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}

                calls = []
                content = "result:" + prompt
                if prompt == "delegate" or prompt.startswith("pool-") and not prompt.startswith("pool-child-") or prompt == "recovery-parent":
                    receipts = []
                    waited = False
                    artifact_ids = []
                    for message in results:
                        try:
                            value = json.loads(message["content"])
                            assert not value["is_error"], value
                            artifact_ids.extend(value["artifact_ids"])
                            try:
                                value = json.loads(value["output"])
                            except json.JSONDecodeError:
                                continue
                        except (TypeError, json.JSONDecodeError):
                            raise AssertionError(message)
                        if isinstance(value, dict) and "task_id" in value:
                            receipts.append(value)
                        if isinstance(value, list):
                            waited = True
                            assert all(v["status"] in {"completed", "interrupted", "failed", "cancelled", "incomplete"} for v in value), value
                    if not receipts:
                        prompts = ["child-one", "child-two"] if prompt == "delegate" else ["recovery-child" if prompt == "recovery-parent" else "pool-child-" + prompt.removeprefix("pool-")]
                        for index, child_prompt in enumerate(prompts):
                            spec = {"prompt": child_prompt, "model": "opencode-go/glm-5.3-flash", "settings": {"max_output_tokens": 64}}
                            if prompt in {"delegate", "recovery-parent"}:
                                spec["project"] = {"project_id": fixture.project_id, "allowed_tools": ["bash"] if prompt == "recovery-parent" else ["write"]}
                            args = {"spec": spec}
                            if prompt == "delegate" and index == 0:
                                args["context"] = {"artifact_ids": artifact_ids}
                            calls.append(call("child_create", args, index))
                        if prompt == "delegate" and not results:
                            calls = [call("bash", {"command": "printf '%020000d' 0"})]
                    elif not waited:
                        calls = [call("child_wait", {"child_run_ids": [r["run_id"] for r in receipts]})]
                    else:
                        content = "collected:" + prompt
                elif prompt.startswith("pool-child-") or prompt.startswith(("child-one", "child-two")):
                    if not results:
                        fixture.children_release.wait(timeout=25)
                        if prompt.startswith(("child-one", "child-two")):
                            calls = [call("write", {"path": "file.txt", "content": prompt + "\n"})]
                elif prompt == "recovery-child" and not results:
                    calls = [call("bash", {"command": "printf '1\\n' >> marker.txt; sleep 4"})]
                elif prompt == "budget-tool" and not results:
                    calls = [call("write", {"path": "denied.txt", "content": "must not appear"})]
                elif prompt.startswith("held-"):
                    fixture.held_release.wait(timeout=25)
                elif prompt == "manual-parent" and not any(m["role"] == "assistant" for m in payload["messages"]):
                    fixture.manual_release.wait(timeout=25)

                delta = {"tool_calls": calls} if calls else {"content": content}
                event = {"id": "fixture", "model": payload["model"], "choices": [{"index": 0, "delta": delta, "finish_reason": "tool_calls" if calls else "stop"}], "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}}
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                self.wfile.write(b"data: " + json.dumps(event).encode() + b"\n\ndata: [DONE]\n\n")
                self.wfile.flush()

            def log_message(self, *_):
                pass

        class Server(http.server.ThreadingHTTPServer):
            def handle_error(self, *_):
                pass

        self.server = Server(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def base_url(self):
        return f"http://127.0.0.1:{self.server.server_port}/v1"

    def close(self):
        self.children_release.set()
        self.held_release.set()
        self.manual_release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def check_delegation(daemon_binary, cli_binary, root):
    fixture = DelegationFixture()
    daemon = log = None
    try:
        env = isolated_env(root, fixture)
        config = root / "concurrency.toml"
        root.mkdir(parents=True, exist_ok=True)
        config.write_text("execution_concurrency = 4\n", encoding="utf-8")
        env["SLOP_CONFIG"] = str(config)
        repo = root / "repository"
        repo.mkdir()
        (repo / "file.txt").write_text("before\n", encoding="utf-8")
        for args in [("init",), ("add", "."), ("commit", "-m", "base")]:
            subprocess.run(["git", "-c", "user.name=Smoke", "-c", "user.email=smoke@example.invalid", *args], cwd=repo, env=env, check=True, capture_output=True, timeout=15)
        data = root / "data"
        daemon, endpoint, log = start_daemon(daemon_binary, root / "first", data, env)
        token_path = data / "credentials/local-api-token"
        token = token_path.read_text(encoding="utf-8").strip()

        def api(path, payload=None, status=200):
            actual, body = request(endpoint, token, method="GET" if payload is None else "POST", payload=payload, path=path)
            assert actual == status, (path, actual, body)
            return body

        def cli(*args):
            return json.loads(run([str(cli_binary), "--daemon", endpoint, "--token-file", str(token_path), "--json", *args], env=env).stdout)

        def wait_for(predicate, description):
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                value = predicate()
                if value:
                    return value
                time.sleep(0.03)
            raise RuntimeError(f"did not observe {description}")

        project = cli("project", "register", str(repo), "--command-id", "delegation-project")
        fixture.project_id = project["id"]
        policy = {"max_children": 4, "max_depth": 2, "allowed_models": ["opencode-go/glm-5.3-flash"], "allowed_tools": ["write", "bash"]}
        policy_file = root / "policy.json"
        policy_file.write_text(json.dumps(policy), encoding="utf-8")
        parent = cli("task", "create", "--model", "opencode-go/glm-5.3-flash", "--project", project["id"], "--orchestration-policy", str(policy_file), "--text", "delegate", "--command-id", "delegate")
        wait_for(lambda: cli("run", "show", parent["run_id"])["turn"]["status"] == "awaiting_children", "parent releasing its slot")
        children = cli("run", "children", parent["run_id"])["items"]
        assert len(children) == 2
        assert all(c["child"]["parent_run_id"] == parent["run_id"] for c in children)
        assert len({c["latest_run"]["workspace_id"] for c in children}) == 2
        assert children[0]["child"]["context"]["artifact_ids"]
        assert "Selected parent artifact" in children[0]["spec"]["prompt"]
        assert len(children[0]["spec"]["prompt"]) < 64 * 1024
        fixture.children_release.set()
        wait_turn(endpoint, token, parent["turn_id"], {"completed"})
        assert cli("run", "result", parent["run_id"])["output"]["text"] == "collected:delegate"
        for child in children:
            workspace = api(f"/v1/workspaces/{child['latest_run']['workspace_id']}")
            assert (Path(workspace["path"]) / "file.txt").read_text() == child["spec"]["prompt"] + "\n"
        assert (repo / "file.txt").read_text() == "before\n"
        assert cli("task", "show", parent["task_id"])["budget_usage"] == {"model_requests": 8, "tool_calls": 6}

        # All four execution slots can wait on children without deadlocking them.
        fixture.children_release.clear()
        pool_policy = dict(policy, max_children=1, max_depth=1, allowed_tools=[])
        parents = []
        for index in range(4):
            spec = {"prompt": f"pool-{index}", "model": "opencode-go/glm-5.3-flash", "settings": {"max_output_tokens": 64}, "budget": {"max_model_requests": 16, "max_tool_calls": 16}, "orchestration": pool_policy}
            parents.append(api("/v1/tasks", {"command_id": f"pool-{index}", "spec": spec}, 202))
        wait_for(lambda: all(api(f"/v1/runs/{p['run_id']}")["turn"]["status"] == "awaiting_children" for p in parents), "four waiting parents")
        wait_for(lambda: sum(api(f"/v1/runs/{p['run_id']}/children")["items"][0]["latest_run"]["turn"]["status"] == "running" for p in parents) == 4, "four admitted children")
        fixture.children_release.set()
        for parent in parents:
            wait_turn(endpoint, token, parent["turn_id"], {"completed"})

        # Client-created children use the same policy and durable wait operation.
        manual_policy = root / "manual-policy.json"
        manual_policy.write_text(json.dumps(dict(pool_policy, max_depth=2)), encoding="utf-8")
        manual = cli("task", "create", "--model", "opencode-go/glm-5.3-flash", "--orchestration-policy", str(manual_policy), "--text", "manual-parent", "--command-id", "manual-parent")
        cli("run", "pause", manual["run_id"], "--command-id", "pause-manual")
        manual_spec = api(f"/v1/runs/{manual['run_id']}")
        child_file = root / "manual-child.json"
        child_file.write_text(json.dumps({"spec": {"prompt": "manual-child", "model": "opencode-go/glm-5.3-flash", "orchestration": pool_policy}, "context": {"message_ids": [manual_spec["turn"]["user_message_id"]]}}), encoding="utf-8")
        manual_child = cli("run", "create-child", manual["run_id"], str(child_file), "--command-id", "manual-child")
        assert cli("run", "create-child", manual["run_id"], str(child_file), "--command-id", "manual-child") == manual_child
        parent_snapshot = cli("task", "show", manual["task_id"])
        child_snapshot = cli("task", "show", manual_child["task_id"])
        assert child_snapshot["spec"]["budget"] == parent_snapshot["spec"]["budget"]
        assert child_snapshot["spec"]["settings"] == parent_snapshot["spec"]["settings"]
        assert "budget" not in child_snapshot["requested_spec"]
        wait_turn(endpoint, token, manual_child["turn_id"], {"completed"})
        waited = cli("run", "wait", manual["run_id"], "--child-run", manual_child["run_id"], "--command-id", "manual-wait")
        fixture.manual_release.set()
        wait_turn(endpoint, token, manual["turn_id"], {"paused"})
        cli("run", "resume", manual["run_id"], "--command-id", "resume-manual")
        wait_turn(endpoint, token, manual["turn_id"], {"completed"})
        assert cli("run", "wait", manual["run_id"], "--child-run", manual_child["run_id"], "--command-id", "manual-wait") == waited

        # Tool budget denial happens before filesystem dispatch.
        limited = cli("task", "create", "--model", "opencode-go/glm-5.3-flash", "--project", project["id"], "--tool", "write", "--text", "budget-tool", "--max-model-requests", "1", "--max-tool-calls", "0", "--command-id", "budget-tool")
        failed = wait_turn(endpoint, token, limited["turn_id"], {"failed"})
        assert failed["error_code"] == "operation_budget_exhausted", failed
        task = cli("task", "show", limited["task_id"])
        workspace = api(f"/v1/workspaces/{task['latest_run']['workspace_id']}")
        assert not (Path(workspace["path"]) / "denied.txt").exists()
        assert task["budget_usage"] == {"model_requests": 1, "tool_calls": 0}

        # One aggregate request reservation wins across an entire batch.
        batch_spec = {"name": "one request", "prompts": [f"budget-batch-{i}" for i in range(8)], "models": ["opencode-go/glm-5.3-flash"], "settings": [{"max_output_tokens": 64}], "max_concurrent_runs": 4, "budget": {"max_model_requests": 1, "max_tool_calls": 0}}
        batch = api("/v1/batches", {"command_id": "budget-batch", "spec": batch_spec}, 202)
        for member in batch["members"]:
            wait_turn(endpoint, token, member["turn_id"], {"completed", "failed"})
        summary = cli("batch", "show", batch["batch_id"])
        assert summary["statuses"] == {"completed": 1, "failed": 7}, summary
        assert summary["budget_usage"] == {"model_requests": 1, "tool_calls": 0}

        held = dict(batch_spec, name="cancel held", prompts=[f"held-{i}" for i in range(8)], budget={"max_model_requests": 16, "max_tool_calls": 0})
        batch = api("/v1/batches", {"command_id": "held-batch", "spec": held}, 202)
        wait_for(lambda: cli("batch", "show", batch["batch_id"])["statuses"].get("running") == 4, "full batch admission")
        cancelled = cli("batch", "cancel", batch["batch_id"], "--command-id", "cancel-held")
        assert len(cancelled["run_ids"]) == 8
        for member in batch["members"]:
            wait_turn(endpoint, token, member["turn_id"], {"cancelled"})
        assert cli("batch", "cancel", batch["batch_id"], "--command-id", "cancel-held") == cancelled
        fixture.held_release.set()

        # Kill a real child shell after its side effect; preserve parent links and
        # the known wait, disclose uncertainty, and never repeat the old tool.
        recovery = cli("task", "create", "--model", "opencode-go/glm-5.3-flash", "--project", project["id"], "--orchestration-policy", str(policy_file), "--text", "recovery-parent", "--command-id", "recovery-parent")
        child = wait_for(lambda: (lambda items: items[0] if items else None)(cli("run", "children", recovery["run_id"])["items"]), "recovery child acceptance")
        workspace = api(f"/v1/workspaces/{child['latest_run']['workspace_id']}")
        marker = Path(workspace["path"]) / "marker.txt"
        wait_for(lambda: marker.exists(), "child shell side effect")
        wait_for(lambda: api(f"/v1/runs/{recovery['run_id']}")["turn"]["status"] == "awaiting_children", "durable parent wait")
        daemon.kill()
        daemon.wait(timeout=10)
        log.close()
        daemon = log = None
        daemon, endpoint, log = start_daemon(daemon_binary, root / "restart", data, env)
        assert cli("run", "show", recovery["run_id"])["turn"]["status"] == "paused"
        retained = cli("run", "children", recovery["run_id"])["items"]
        assert len(retained) == 1 and retained[0]["id"] == child["id"]
        assert retained[0]["latest_run"]["turn"]["status"] == "interrupted"
        assert retained[0]["latest_run"]["effects_unknown"]
        api(f"/v1/runs/{child['latest_run']['id']}/retry", {"command_id": "unacknowledged-child-retry"}, 400)
        cli("run", "resume", recovery["run_id"], "--command-id", "resume-parent")
        wait_turn(endpoint, token, recovery["turn_id"], {"completed"})
        assert len(cli("run", "children", recovery["run_id"])["items"]) == 1
        assert marker.read_text() == "1\n"
        retried = cli("run", "retry", child["latest_run"]["id"], "--command-id", "inspected-child-retry", "--acknowledge-unknown-effects")
        wait_turn(endpoint, token, retried["turn_id"], {"completed"})
        fresh = cli("task", "show", child["id"])["latest_run"]
        assert fresh["workspace_id"] != child["latest_run"]["workspace_id"]
        assert marker.read_text() == "1\n"
        assert cli("batch", "cancel", batch["batch_id"], "--command-id", "cancel-held") == cancelled
    finally:
        if daemon is not None:
            stop_daemon(daemon, log)
        fixture.close()
