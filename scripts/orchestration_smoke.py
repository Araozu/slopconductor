"""Real daemon/CLI jobs and matrices against an offline model fixture."""
import http.server
import json
from pathlib import Path
import subprocess
import threading
import time

from chat_smoke import isolated_env, request, run, start_daemon, stop_daemon, wait_turn


class OrchestrationFixture:
    def __init__(self):
        self.lock = threading.Lock()
        self.calls = []
        self.attempts = {}
        self.release = threading.Event()
        self.control_release = threading.Event()
        self.cancel_release = threading.Event()
        self.fail_once = True
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                if self.path != "/v1/chat/completions" or self.headers.get("Authorization") != "Bearer offline-test-key":
                    self.send_error(401)
                    return
                payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                prompt = next(m["content"] for m in payload["messages"] if m["role"] == "user")
                done = any(m["role"] == "tool" for m in payload["messages"])
                cap = payload.get("max_tokens", payload.get("max_completion_tokens"))
                key = (prompt, payload["model"], cap)
                with fixture.lock:
                    fixture.calls.append(payload)
                    if not done:
                        fixture.attempts[key] = fixture.attempts.get(key, 0) + 1
                    attempt = fixture.attempts.get(key, 0)
                if prompt in {"alpha", "beta"} and not done:
                    fixture.release.wait(timeout=15)
                if prompt == "control-held" and attempt == 1:
                    fixture.control_release.wait(timeout=15)
                if prompt == "cancel-held" and attempt == 1:
                    fixture.cancel_release.wait(timeout=15)
                if key == ("alpha", "glm-5.3-flash", 17) and not done and fixture.fail_once:
                    fixture.fail_once = False
                    self.send_error(400)
                    return
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                if not done and prompt in {"alpha", "beta", "restart-held"}:
                    if prompt == "restart-held":
                        name = "bash"
                        arguments = {"command": "printf 'started\\n' >> attempt-log.txt" + ("; sleep 3" if attempt == 1 else "")}
                    else:
                        name = "edit"
                        arguments = {"path": "file.txt", "oldText": "before", "newText": f"{prompt}/{payload['model']}/{cap}"}
                    delta = {"tool_calls": [{"index": 0, "id": f"fixture-call-{len(fixture.calls)}", "type": "function", "function": {"name": name, "arguments": json.dumps(arguments)}}]}
                    finish = "tool_calls"
                else:
                    delta, finish = {"content": f"result for {prompt}/{payload['model']}/{cap}"}, "stop"
                event = {"model": payload["model"], "choices": [{"index": 0, "delta": delta, "finish_reason": finish}], "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}}
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
        self.release.set()
        self.control_release.set()
        self.cancel_release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def check_orchestration(daemon_binary, cli_binary, root):
    fixture = OrchestrationFixture()
    daemon = log = None
    try:
        env = isolated_env(root, fixture)
        config = root / "concurrency.toml"
        config.write_text("execution_concurrency = 4\n", encoding="utf-8")
        env["SLOP_CONFIG"] = str(config)
        repo = root / "repository"
        repo.mkdir()

        def git(*args):
            return subprocess.run(["git", "-c", "user.name=Smoke", "-c", "user.email=smoke@example.invalid", *args], cwd=repo, env=env, check=True, capture_output=True, text=True, timeout=15).stdout.strip()

        git("init")
        (repo / "file.txt").write_text("before\n", encoding="utf-8")
        git("add", ".")
        git("commit", "-m", "base")
        base = git("rev-parse", "HEAD")
        data = root / "data"
        daemon, endpoint, log = start_daemon(daemon_binary, root / "first", data, env)
        token_path = data / "credentials/local-api-token"
        token = token_path.read_text(encoding="utf-8").strip()

        def api(path, payload=None, status=200, method=None):
            actual, body = request(endpoint, token, method=method or ("GET" if payload is None else "POST"), payload=payload, path=path)
            assert actual == status, (path, actual, body)
            return body

        def cli(*args):
            return json.loads(run([str(cli_binary), "--daemon", endpoint, "--token-file", str(token_path), "--json", *args], env=env).stdout)

        def wait_for(predicate, description):
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                value = predicate()
                if value:
                    return value
                time.sleep(0.03)
            raise RuntimeError(f"did not observe {description}")

        assert request(endpoint, None, path="/v1/tasks")[0] == 401
        assert request(endpoint, None, "POST", {"name": "bad"}, "/v1/batches/preview")[0] == 401
        project = cli("project", "register", str(repo), "--command-id", "matrix-project")
        spec = {"name": "eight jobs", "prompts": ["alpha", "beta"], "models": ["opencode-go/glm-5.3-flash", "opencode-go/glm-5.3"], "settings": [{"max_output_tokens": 17}, {"max_output_tokens": 18}], "project": {"project_id": project["id"], "base_ref": "HEAD", "allowed_tools": ["edit"]}, "max_concurrent_runs": 2}
        source = root / "batch.json"
        source.write_text(json.dumps(spec), encoding="utf-8")
        frozen_path = root / "frozen.json"
        preview = cli("batch", "preview", str(source), "--output", str(frozen_path))
        assert [(c["prompt_index"], c["model_index"], c["settings_index"]) for c in preview["combinations"]] == [(p, m, s) for p in range(2) for m in range(2) for s in range(2)]
        assert preview["spec"]["project"]["base_ref"] == base
        assert cli("task", "list")["items"] == []
        assert api(f"/v1/projects/{project['id']}/workspaces")["items"] == []
        oversized = dict(spec, prompts=["too many"] * 65)
        api("/v1/batches/preview", oversized, status=429)
        api("/v1/batches/preview", dict(spec, settings=[{"reasoning_effort": "invented"}]), status=400)
        api("/v1/batches/preview", dict(spec, project=dict(spec["project"], allowed_tools=["unsupported"])), status=400)
        api("/v1/batches/preview", dict(spec, prompts=["within matrix cap"] * 9), status=429)
        default_preview = api("/v1/batches/preview", dict(spec, settings=[{}]))
        assert default_preview["spec"]["default_max_output_tokens"] == 4096
        assert all(c["spec"]["settings"]["max_output_tokens"] == 4096 for c in default_preview["combinations"])
        # A preview's exact commit survives a later source HEAD change.
        (repo / "later.txt").write_text("later", encoding="utf-8")
        git("add", ".")
        git("commit", "-m", "advance")
        accepted = cli("batch", "submit", str(frozen_path), "--command-id", "eight-jobs")
        assert len(accepted["members"]) == 8
        assert cli("batch", "submit", str(frozen_path), "--command-id", "eight-jobs") == accepted
        api("/v1/batches", {"command_id": "eight-jobs", "spec": dict(preview["spec"], name="changed")}, status=409)
        batch_id = accepted["batch_id"]
        members = wait_for(lambda: (lambda items: items if sum(t["latest_run"]["turn"]["status"] == "running" for t in items) == 2 else None)(cli("batch", "members", batch_id)["items"]), "two admitted jobs")
        workspaces = api(f"/v1/projects/{project['id']}/workspaces")["items"]
        wait_for(lambda: sum(Path(w["path"]).is_dir() for w in workspaces) == 2, "lazy allocation of only two worktrees")
        assert all(w["base_commit"] == base for w in workspaces)
        assert len({w["path"] for w in workspaces}) == 8
        assert sum(w["status"] == "reserved" for w in api(f"/v1/projects/{project['id']}/workspaces")["items"]) == 6
        # Spare daemon capacity remains usable while this batch reaches its cap.
        independent = cli("task", "create", "--model", "opencode-go/glm-5.3-flash", "--text", "independent", "--command-id", "independent")
        wait_turn(endpoint, token, independent["turn_id"], {"completed"})
        assert cli("task", "create", "--model", "opencode-go/glm-5.3-flash", "--text", "independent", "--command-id", "independent") == independent
        assert cli("run", "show", independent["run_id"])["turn"]["status"] == "completed"
        api(f"/v1/runs/{independent['run_id']}/retry", {"command_id": "retry-success"}, status=409)
        fixture.release.set()
        for member in accepted["members"]:
            wait_turn(endpoint, token, member["turn_id"], {"completed", "failed"})
        members = cli("batch", "members", batch_id)["items"]
        assert [t["combination_index"] for t in members] == list(range(8))
        assert cli("batch", "show", batch_id)["statuses"] == {"completed": 7, "failed": 1}
        successes = {t["id"]: t["latest_run"]["id"] for t in members if t["latest_run"]["turn"]["status"] == "completed"}
        assert (repo / "file.txt").read_text(encoding="utf-8") == "before\n"
        for task in members[1:]:
            workspace = api(f"/v1/workspaces/{task['latest_run']['workspace_id']}")
            expected = f"{task['spec']['prompt']}/{task['spec']['model'].split('/', 1)[1]}/{task['spec']['settings']['max_output_tokens']}"
            assert (Path(workspace["path"]) / "file.txt").read_text(encoding="utf-8").strip() == expected
            assert expected in cli("workspace", "diff", workspace["id"])["patch"]
        retried = cli("batch", "retry", batch_id, "--index", "0", "--command-id", "retry-zero")
        assert len(retried["members"]) == 1
        assert cli("batch", "retry", batch_id, "--index", "0", "--command-id", "retry-zero") == retried
        wait_turn(endpoint, token, retried["members"][0]["turn_id"], {"completed"})
        current = cli("task", "show", members[0]["id"])
        assert current["latest_run"]["attempt"] == 2
        assert current["latest_run"]["workspace_id"] != members[0]["latest_run"]["workspace_id"]
        assert current["latest_run"]["retry_of"] == members[0]["latest_run"]["id"]
        assert len(cli("task", "runs", current["id"])["items"]) == 2
        assert cli("batch", "show", batch_id)["statuses"] == {"completed": 8}
        assert {t["id"]: t["latest_run"]["id"] for t in cli("batch", "members", batch_id)["items"] if t["id"] in successes} == successes
        api(f"/v1/batches/{batch_id}/retry", {"command_id": "retry-duplicate", "indices": [0, 0]}, status=400)
        api(f"/v1/batches/{batch_id}/retry", {"command_id": "retry-complete", "indices": [1]}, status=409)
        first_page = cli("batch", "results", batch_id, "--limit", "2")
        assert len(first_page["items"]) == 2 and first_page["next_after"] is not None
        next_page = cli("batch", "results", batch_id, "--after", str(first_page["next_after"]))
        assert len(next_page["items"]) == 6
        export_path = root / "results.jsonl"
        assert cli("batch", "export", batch_id, "--output", str(export_path))["members"] == 8
        exported = [json.loads(line) for line in export_path.read_text(encoding="utf-8").splitlines()]
        assert len(exported) == 8 and all(r["output"]["status"] == "completed" for r in exported)
        assert all(r["task"]["latest_run"]["turn"]["usage"]["total_tokens"] == 14 for r in exported)
        assert all(r["task"]["spec"]["project"]["base_ref"] == base for r in exported)
        # Real API/CLI controls and durable steering use the primary run turn.
        controlled = cli("task", "create", "--model", "opencode-go/glm-5.3-flash", "--text", "control-held", "--command-id", "controlled-job")
        wait_turn(endpoint, token, controlled["turn_id"], {"running"})
        snapshot = cli("task", "show", controlled["task_id"])
        assert snapshot["requested_spec"]["settings"]["max_output_tokens"] is None
        assert snapshot["spec"]["settings"]["max_output_tokens"] == 4096
        pause = cli("run", "pause", controlled["run_id"], "--command-id", "pause-control")
        assert cli("run", "pause", controlled["run_id"], "--command-id", "pause-control") == pause
        fixture.control_release.set()
        wait_turn(endpoint, token, controlled["turn_id"], {"paused"})
        instruction = cli("task", "send", controlled["task_id"], "--text", "Keep the interface", "--delivery", "immediate", "--command-id", "control-instruction")
        cli("run", "resume", controlled["run_id"], "--command-id", "resume-control")
        wait_turn(endpoint, token, controlled["turn_id"], {"completed"})
        assert cli("task", "send", controlled["task_id"], "--text", "Keep the interface", "--delivery", "immediate", "--command-id", "control-instruction") == instruction
        followed = run([str(cli_binary), "--daemon", endpoint, "--token-file", str(token_path), "task", "follow", controlled["task_id"]], env=env)
        assert "result for control-held" in followed.stdout
        cancelled = cli("task", "create", "--model", "opencode-go/glm-5.3-flash", "--text", "cancel-held", "--command-id", "cancel-job")
        wait_turn(endpoint, token, cancelled["turn_id"], {"running"})
        cli("run", "cancel", cancelled["run_id"], "--command-id", "cancel-control")
        wait_turn(endpoint, token, cancelled["turn_id"], {"cancelled"})
        fixture.cancel_release.set()
        # Interrupt an actual shell tool after its first external write.
        held = cli("task", "create", "--model", "opencode-go/glm-5.3-flash", "--project", project["id"], "--base", base, "--tool", "bash", "--text", "restart-held", "--command-id", "restart-job")
        held_task = cli("task", "show", held["task_id"])
        held_workspace = api(f"/v1/workspaces/{held_task['latest_run']['workspace_id']}")
        old_path = Path(held_workspace["path"])
        wait_for(lambda: (old_path / "attempt-log.txt").exists(), "the shell side effect")
        wait_for(lambda: any(t["status"] == "running" for t in api(f"/v1/turns/{held['turn_id']}/tools")["items"]), "journaled running tool")
        daemon.kill()
        daemon.wait(timeout=5)
        log.close()
        daemon, endpoint, log = start_daemon(daemon_binary, root / "second", data, env)
        recovered = cli("run", "show", held["run_id"])
        assert recovered["turn"]["status"] == "interrupted" and recovered["effects_unknown"]
        assert recovered["turn"]["error_code"] == "daemon_restarted"
        before = len(fixture.calls)
        time.sleep(0.25)
        assert len(fixture.calls) == before
        refused = subprocess.run([str(cli_binary), "--daemon", endpoint, "--token-file", str(token_path), "run", "retry", held["run_id"], "--command-id", "retry-held"], env=env, capture_output=True, text=True, timeout=15)
        assert refused.returncode != 0 and "unknown" in refused.stderr
        retried_held = cli("run", "retry", held["run_id"], "--command-id", "retry-held", "--acknowledge-unknown-effects")
        wait_turn(endpoint, token, retried_held["turn_id"], {"completed"})
        new_task = cli("task", "show", held["task_id"])
        new_workspace = api(f"/v1/workspaces/{new_task['latest_run']['workspace_id']}")
        assert Path(new_workspace["path"]) != old_path
        assert (old_path / "attempt-log.txt").read_text(encoding="utf-8") == "started\n"
        assert (Path(new_workspace["path"]) / "attempt-log.txt").read_text(encoding="utf-8") == "started\n"
        events = cli("task", "events", held["task_id"])["items"]
        assert any(e["kind"] == "turn_interrupted" for e in events)
        assert any(e["kind"] == "run_retry_accepted" for e in events)
        assert cli("run", "show", held["run_id"])["turn"]["status"] == "interrupted"
        assert cli("task", "list", "--limit", "1")["next_after"] is not None
        assert cli("batch", "list")["items"][0]["id"] == batch_id
        assert cli("batch", "show", batch_id)["statuses"] == {"completed": 8}
    finally:
        fixture.release.set()
        fixture.control_release.set()
        fixture.cancel_release.set()
        if daemon is not None and log is not None and not log.closed:
            stop_daemon(daemon, log)
        fixture.close()
