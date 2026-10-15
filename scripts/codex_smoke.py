"""Codex structured execution and account fixtures; no upstream requests."""

import http.server
import json
import os
import subprocess
from pathlib import Path
import threading
import time
import urllib.parse
import urllib.request

from chat_smoke import isolated_env, request, run, start_daemon, stop_daemon, wait_turn
from tools_smoke import ToolFixture


class CodexFixture:
    def __init__(self):
        self.requests = []
        self.lock = threading.Lock()
        self.started = threading.Event()
        self.release = threading.Event()
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path != "/v1/models":
                    self.send_error(404)
                    return
                if "subscription" in self.headers.get("Authorization", ""):
                    body = {"models": [{"slug": "gpt-6.1-sol", "visibility": "list"},
                                       {"slug": "unmapped-model", "visibility": "list"},
                                       {"slug": "hidden-model", "visibility": "hide"}]}
                else:
                    body = {"data": [{"id": "gpt-6.1-sol"}, {"id": "unmapped-model"}]}
                encoded = json.dumps(body).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

            def do_POST(self):
                if self.path != "/v1/responses":
                    self.send_error(404)
                    return
                payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                with fixture.lock:
                    fixture.requests.append((self.headers.get("Authorization"), payload))
                assert payload["stream"] and payload["store"] is False
                if "subscription" in self.headers.get("Authorization", ""):
                    assert "max_output_tokens" not in payload and "tools" not in payload
                    assert not any(m.get("role") == "system" for m in payload["input"])
                history = payload["input"]
                user_index = max(i for i, m in enumerate(history) if m.get("role") == "user")
                prompt = history[user_index]["content"]
                current = history[user_index + 1:]
                results = [m for m in current if m.get("type") == "function_call_output"]
                calls = []
                status = "completed"

                def call(name, args, index=0):
                    if name == "child_create":
                        args["spec"].setdefault("workspace", {"workspace_id": "default", "allowed_tools": []})
                    return {"type": "function_call", "status": "completed", "call_id": f"{prompt}-{index}",
                            "name": name, "arguments": json.dumps(args)}

                if prompt == "workflow" and not results:
                    calls = [call("read", {"path": "file.txt"}),
                             call("write", {"path": "new.txt", "content": "created"}, 1),
                             call("edit", {"path": "file.txt", "oldText": "before", "newText": "after"}, 2),
                             call("bash", {"command": "printf 'shell-ok'"}, 3)]
                elif prompt in {"malformed", "incomplete"} and not results:
                    calls = [call("write", {"path": "must-not-exist.txt", "content": "unsafe"})]
                    calls[0]["arguments"] = '{"path":'
                    if prompt == "incomplete":
                        status = "incomplete"
                elif prompt == "delegate":
                    decoded = [json.loads(m["output"]) for m in results]
                    assert all(not result["is_error"] for result in decoded), decoded
                    values = [json.loads(result["output"]) for result in decoded]
                    receipts = [value for value in values if isinstance(value, dict) and "run_id" in value]
                    waited = any(isinstance(value, list) for value in values)
                    if not receipts:
                        calls = [call("child_create", {"spec": {"prompt": "child", "model": "codex/gpt-6.1-sol",
                                                               "settings": {"reasoning_effort": "low"}}})]
                    elif not waited:
                        calls = [call("child_wait", {"child_run_ids": [receipt["run_id"] for receipt in receipts]})]
                original_prompt = next(m["content"] for m in reversed(history) if m.get("role") == "user" and isinstance(m.get("content"), str) and not m["content"].startswith("Your previous response reached its output-token limit."))
                if original_prompt == "incomplete":
                    status = "incomplete"
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()

                def emit(event):
                    self.wfile.write(b"data: " + json.dumps(event, ensure_ascii=False).encode() + b"\n\n")
                    self.wfile.flush()

                try:
                    if prompt == "cancel":
                        emit({"type": "response.output_text.delta", "delta": "partial"})
                        fixture.started.set()
                        fixture.release.wait(15)
                        return
                    text = "Inspecting." if calls or status == "incomplete" else "Done: " + prompt
                    output = [{"type": "message", "role": "assistant", "phase": "commentary" if calls else "final_answer",
                               "content": [{"type": "output_text", "text": text}]}]
                    if prompt == "refuse":
                        text = "Declined."
                        output = [{"type": "message", "role": "assistant",
                                   "content": [{"type": "refusal", "refusal": text}]}]
                    if calls:
                        output.insert(0, {"type": "reasoning", "id": "reason-1", "encrypted_content": "private-codex-fixture",
                                          "summary": [{"type": "summary_text", "text": "private-summary-fixture"}]})
                        output.extend(calls)
                    if prompt == "reported-output":
                        output.insert(0, {"type":"reasoning","id":"hidden-reasoning","encrypted_content":"private-hidden-reasoning-fixture","summary":[]})
                    index = 1 if calls or prompt == "reported-output" else 0
                    emit({"type": "response.refusal.delta" if prompt == "refuse" else "response.output_text.delta",
                          "output_index": index, "content_index": 0, "delta": text})
                    if calls:
                        for index, proposal in enumerate(calls, 2):
                            emit({"type": "response.function_call_arguments.delta", "output_index": index, "delta": proposal["arguments"]})
                    response = {"status": status, "model": payload["model"], "output": output,
                                "usage": {"input_tokens": {"reported-usage":270000, "reported-output":200000, "full-window-limit":1045000}.get(prompt, 7), "output_tokens": 70000 if prompt == "reported-output" else 3}}
                    if status == "incomplete":
                        response["incomplete_details"] = {"reason": "max_output_tokens"}
                    emit({"type": "response." + status, "response": response})
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def log_message(self, *_):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def base_url(self):
        return f"http://127.0.0.1:{self.server.server_port}/v1"

    def snapshot(self):
        with self.lock:
            return list(self.requests)

    def close(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(5)


def check_codex(daemon_binary, cli_binary, root):
    fixture = CodexFixture()
    go_fixture = ToolFixture()
    daemon = log = None
    try:
        env = isolated_env(root, fixture)
        env["SLOP_CODEX_BASE_URL"] = fixture.base_url
        env["SLOP_PROVIDER_BASE_URL"] = go_fixture.base_url
        env["OPENAI_API_KEY"] = "platform-fixture"
        data = root / "data"
        workspace = root / "workspace"
        workspace.mkdir()
        (workspace / "file.txt").write_text("before", encoding="utf-8")
        daemon, endpoint, log = start_daemon(daemon_binary, root / "first", data, env)
        token_path = data / "credentials/local-api-token"
        token = token_path.read_text().strip()
        status, node = request(endpoint, token, path="/v1/node")
        assert status == 200
        stop_daemon(daemon, log)
        daemon = log = None
        secrets = ["platform-fixture", "private-codex-fixture", "private-summary-fixture"]
        for account in ["default", "work", "old"]:
            body = {"issuer": "https://auth.openai.com", "subject": "subject-" + account,
                    "client_id": "oaiapp_" + account, "ext_agent_host_id": node["node_id"],
                    "id_token": "id-fixture-" + account, "access_token": "subscription-fixture-" + account,
                    "refresh_token": "refresh-fixture-" + account, "expires_at": 4_000_000_000,
                    "scopes": ["openid", "offline_access", "resource.invoke", "chatgpt.tokens.use.direct"]}
            secrets.extend([body["id_token"], body["access_token"], body["refresh_token"]])
            if account == "old":
                body.update(id_token="", access_token="", refresh_token="", expires_at=0, scopes=[])
            path = data / "credentials" / ("codex-chatgpt.json" if account == "default" else f"codex-chatgpt-{account}.json")
            path.write_text(json.dumps(body), encoding="utf-8")
            if os.name != "nt":
                path.chmod(0o600)
        mode_path = data / "credentials/codex-auth-mode"
        mode_path.write_text("chatgpt")
        if os.name != "nt":
            mode_path.chmod(0o600)
        daemon, endpoint, log = start_daemon(daemon_binary, root / "second", data, env)

        def cli(*args):
            try:
                result = run([str(cli_binary), "--daemon", endpoint, "--token-file", str(token_path), "--json", *args], env=env)
            except subprocess.CalledProcessError as error:
                raise AssertionError(error.stderr) from error
            assert all(secret not in result.stdout + result.stderr for secret in secrets)
            return result.stdout

        def api(path, payload=None, expected=200, method=None):
            status, body = request(endpoint, token, method or ("POST" if payload is not None else "GET"), payload, path)
            assert status == expected, (status, body)
            assert all(secret not in json.dumps(body) for secret in secrets)
            return body

        def chat(prompt, identity, *args):
            frames = [json.loads(line) for line in cli("chat", "--model", "codex/gpt-6.1-sol", "--prompt", prompt,
                                                      "--command-id", identity, *args).splitlines()]
            return next(frame["receipt"] for frame in frames if frame["type"] == "receipt")

        accounts = json.loads(cli("provider", "accounts"))
        assert [account["id"] for account in accounts] == ["default", "old", "work"]
        assert accounts[0]["active"] and not accounts[1]["authenticated"]
        # Deny local callbacks before any real registration/token exchange.
        for label, client_id in [("Work", "oaiapp_work"), ("Personal", "dynamic_agent_client")]:
            payload = {"command_id": "codex-login-" + label.lower(), "account_id": label}
            login = api("/v1/providers/codex/login", payload, 202)
            assert api("/v1/providers/codex/login", payload, 202) == login
            api("/v1/providers/codex/login", dict(payload, account_id="different"), 409)
            api("/v1/providers/codex/auth", {"auth_mode": "api_key"}, 409, "PUT")
            auth = urllib.parse.parse_qs(urllib.parse.urlparse(login["authorization_url"]).query)
            assert auth["client_id"] == [client_id] and auth["ext_agent_host_id"] == [node["node_id"]]
            denied = auth["redirect_uri"][0] + "?" + urllib.parse.urlencode({"state": auth["state"][0], "error": "access_denied"})
            with urllib.request.urlopen(denied, timeout=5) as response:
                response.read()
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                login_status = api(f"/v1/providers/codex/login/{login['login_id']}")
                if login_status["status"] == "failed":
                    break
                time.sleep(0.05)
            assert login_status["status"] == "failed"
        assert json.loads(cli("provider", "accounts")) == accounts
        assert not (data / "credentials/codex-chatgpt-personal.json").exists()
        catalog = json.loads(cli("provider", "discover", "codex"))
        assert catalog["source"] == "live" and catalog["account_id"] == "default" and catalog["observed_at_unix_ms"] > 0
        assert len(catalog["models"]) == 2 and catalog["models"][0]["ready"] and not catalog["models"][1]["ready"]
        assert catalog["models"][0]["capabilities"]["tools"]
        assert catalog["models"][0]["capabilities"]["reasoning_efforts"] == ["low", "medium", "high", "xhigh", "max"]
        assert catalog["models"][0]["capabilities"]["context_window_tokens"] == 1050000
        # A Go conversation can switch to subscription Codex's Responses wire
        # after tool execution, without inheriting Go's unsupported output cap.
        switch_workspace = root / "switch-workspace"
        switch_workspace.mkdir()
        (switch_workspace / "file.txt").write_text("header\nbefore\nfooter", encoding="utf-8")
        frames = [json.loads(line) for line in cli("chat", "--model", "opencode-go/glm-5.3-flash", "--prompt", "workflow", "--workspace", str(switch_workspace), "--command-id", "switch-from-go").splitlines()]
        go_receipt = next(frame["receipt"] for frame in frames if frame["type"] == "receipt")
        go_history = api(f"/v1/sessions/{go_receipt['session_id']}/messages")["items"]
        assert all((m["provider"], m["model"]) == ("opencode-go", "glm-5.3-flash") for m in go_history)
        api(f"/v1/sessions/{go_receipt['session_id']}/messages", {
            "command_id": "switch-explicit-cap", "text": "reject explicit cap",
            "model": "codex/gpt-6.1-sol", "settings": {"max_output_tokens": 64},
        }, 400)
        (switch_workspace / "file.txt").write_text("before", encoding="utf-8")
        switched = chat("workflow", "switch-to-codex", "--session", go_receipt["session_id"])
        assert api(f"/v1/turns/{switched['turn_id']}")["settings"]["max_output_tokens"] is None
        wire = fixture.snapshot()[-1][1]
        assert len([item for item in wire["input"] if item.get("type") == "function_call_output"]) == 9
        assert "private synthetic continuation" not in json.dumps(wire)
        mixed = api(f"/v1/sessions/{go_receipt['session_id']}/messages")["items"]
        assert mixed[:len(go_history)] == go_history
        assert all((m["provider"], m["model"]) == ("codex", "gpt-6.1-sol") for m in mixed[len(go_history):])
        cli("session", "send", go_receipt["session_id"], "--text", "return to Go", "--model", "opencode-go/glm-5.3-flash", "--command-id", "switch-codex-to-go")
        returned = api(f"/v1/sessions/{go_receipt['session_id']}/messages")["items"]
        assert returned[:len(mixed)] == mixed
        assert all((m["provider"], m["model"]) == ("opencode-go", "glm-5.3-flash") for m in returned[len(mixed):])
        assert "private-codex-fixture" not in json.dumps(go_fixture.snapshot()[-1])
        assert len([message for message in go_fixture.snapshot()[-1]["messages"] if message["role"] == "tool"]) == 9
        for marker in ["reported-usage", "reported-output"]:
            context = chat(marker, "codex-context-start-" + marker)
            continued = chat("continue context", "codex-context-continue-" + marker, "--session", context["session_id"])
            context_requests = api(f"/v1/turns/{continued['turn_id']}/requests")["items"]
            assert any(r["compaction"] and r["effective_settings"]["max_output_tokens"] is None for r in context_requests)
            assert "private-hidden-reasoning-fixture" not in json.dumps(fixture.snapshot()[-1][1])
        full = chat("reported-usage", "codex-full-start", "--full-context")
        assert api(f"/v1/sessions/{full['session_id']}")["settings"]["full_context"] is True
        continued = chat("continue full context", "codex-full-continue", "--session", full["session_id"], "--effort", "high")
        full_requests = api(f"/v1/turns/{continued['turn_id']}/requests")["items"]
        assert len(full_requests) == 1 and not full_requests[0]["compaction"]
        assert full_requests[0]["effective_settings"]["full_context"] is True
        restored = chat("reported-usage", "codex-full-reset-start", "--full-context")
        limited = chat("restore the cost cap", "codex-full-reset", "--session", restored["session_id"], "--full-context=false")
        reset_requests = api(f"/v1/turns/{limited['turn_id']}/requests")["items"]
        assert any(r["compaction"] for r in reset_requests)
        assert reset_requests[-1]["effective_settings"]["full_context"] is False
        near_limit = chat("full-window-limit", "codex-full-limit-start", "--full-context")
        recovered = chat("continue at the full window", "codex-full-limit-continue", "--session", near_limit["session_id"])
        assert any(r["compaction"] for r in api(f"/v1/turns/{recovered['turn_id']}/requests")["items"])
        receipt = chat("workflow", "codex-workflow", "--workspace", str(workspace), "--effort", "high")
        state = api(f"/v1/turns/{receipt['turn_id']}")
        assert state["status"] == "completed" and state["usage"]["total_tokens"] == 20
        assert len(state["tool_invocation_ids"]) == 4
        assert (workspace / "file.txt").read_text() == "after" and (workspace / "new.txt").read_text() == "created"
        history = api(f"/v1/sessions/{receipt['session_id']}/messages")["items"]
        assert [message["role"] for message in history] == ["user", "assistant", "tool", "tool", "tool", "tool", "assistant"]
        replayed = fixture.snapshot()[-1][1]["input"]
        assert any(item.get("encrypted_content") == "private-codex-fixture" for item in replayed)
        assert any(item.get("phase") == "commentary" for item in replayed)
        count = len(fixture.snapshot())
        chat("workflow", "codex-workflow", "--workspace", str(workspace), "--effort", "high", "--detach")
        assert len(fixture.snapshot()) == count
        refused = chat("refuse", "codex-refusal")
        refusal = api(f"/v1/sessions/{refused['session_id']}/messages")["items"][-1]
        assert refusal["blocks"][0]["type"] == "refusal" and refusal["text"] == "Declined."
        chat("after-refusal", "codex-after-refusal", "--session", refused["session_id"])
        assert any(item.get("content", [{}])[0].get("type") == "refusal"
                   for item in fixture.snapshot()[-1][1]["input"] if isinstance(item.get("content"), list) and item["content"])
        for prompt, expected in [("malformed", "failed"), ("incomplete", "incomplete")]:
            failed = chat(prompt, "codex-" + prompt, "--workspace", str(workspace), "--detach")
            turn = wait_turn(endpoint, token, failed["turn_id"], {expected})
            assert not turn["tool_invocation_ids"] and not (workspace / "must-not-exist.txt").exists()
        canceled = chat("cancel", "codex-cancel", "--detach")
        assert fixture.started.wait(5)
        cli("turn", "cancel", canceled["turn_id"], "--command-id", "codex-stop")
        wait_turn(endpoint, token, canceled["turn_id"], {"cancelled"})
        fixture.release.set()
        spec = {"prompt": "delegate", "model": "codex/gpt-6.1-sol",
                "budget": {"max_model_requests": 8, "max_tool_calls": 8},
                "orchestration": {"max_children": 1, "max_depth": 1, "allowed_models": ["codex/gpt-6.1-sol"], "allowed_tools": []}}
        parent = api("/v1/tasks", {"command_id": "codex-parent", "spec": spec}, 202)
        wait_turn(endpoint, token, parent["turn_id"], {"completed"})
        children = api(f"/v1/runs/{parent['run_id']}/children")["items"]
        assert len(children) == 1 and children[0]["latest_run"]["turn"]["status"] == "completed"
        repo = root / "repo"
        repo.mkdir()
        (repo / "file.txt").write_text("before", encoding="utf-8")
        run(["git", "-C", str(repo), "init"], env=env)
        run(["git", "-C", str(repo), "add", "file.txt"], env=env)
        run(["git", "-C", str(repo), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.test", "commit", "-m", "base"], env=env)
        project = json.loads(cli("workspace", "open", str(repo), "--command-id", "codex-project"))
        created = json.loads(cli("workspace", "worktree", project["id"], "--command-id", "codex-tree"))
        for _ in range(200):
            created = api(f"/v1/workspaces/{created['id']}")
            if created["status"] == "ready":
                break
            time.sleep(0.03)
        assert created["status"] == "ready", created
        managed = api("/v1/tasks", {"command_id": "codex-managed-task", "spec": {
            "prompt": "workflow", "model": "codex/gpt-6.1-sol",
            "workspace": {"workspace_id": created["id"], "allowed_tools": ["read", "write", "edit", "bash"]},
        }}, 202)
        wait_turn(endpoint, token, managed["turn_id"], {"completed"})
        managed_run = api(f"/v1/runs/{managed['run_id']}")
        managed_workspace = api(f"/v1/workspaces/{managed_run['workspace_id']}")
        assert (Path(managed_workspace["path"]) / "file.txt").read_text() == "after"
        assert (repo / "file.txt").read_text() == "before"
        assert "+after" in api(f"/v1/workspaces/{managed_run['workspace_id']}/diff")["patch"]
        cli("provider", "select", "chatgpt", "--account", "Work")
        chat("account-work", "codex-other-account")
        assert fixture.snapshot()[-1][0] == "Bearer subscription-fixture-work"
        stop_daemon(daemon, log)
        daemon = log = None
        count = len(fixture.snapshot())
        daemon, endpoint, log = start_daemon(daemon_binary, root / "third", data, env)
        assert next(account for account in json.loads(cli("provider", "accounts")) if account["active"])["id"] == "work"
        assert api(f"/v1/sessions/{receipt['session_id']}/messages")["items"] == history
        assert len(fixture.snapshot()) == count
        blocked = chat("blocked-account", "codex-wrong-registration", "--session", receipt["session_id"], "--detach")
        failed = wait_turn(endpoint, token, blocked["turn_id"], {"failed"})
        assert failed["error_code"] == "context_incompatible" and len(fixture.snapshot()) == count
        cli("provider", "select", "chatgpt", "--account", "default")
        chat("follow-up", "codex-after-restart", "--session", receipt["session_id"])
        cli("provider", "select", "api_key")
        chat("platform", "codex-platform", "--max-output-tokens", "64", "--effort", "low")
        assert fixture.snapshot()[-1][0] == "Bearer platform-fixture"
        assert fixture.snapshot()[-1][1]["max_output_tokens"] == 64
        (workspace / "file.txt").write_text("before", encoding="utf-8")
        platform_tools = chat("workflow", "codex-platform-tools", "--workspace", str(workspace), "--effort", "high")
        assert api(f"/v1/turns/{platform_tools['turn_id']}")["status"] == "completed"
        assert (workspace / "file.txt").read_text() == "after" and fixture.snapshot()[-1][0] == "Bearer platform-fixture"
        signed_out = json.loads(cli("provider", "logout", "old"))
        assert not signed_out["remote_revocation_confirmed"]
        cli("provider", "remove-key", "codex")
        assert not next(item for item in json.loads(cli("provider", "status")) if item["provider"] == "codex")["api_key_configured"]
        stop_daemon(daemon, log)
        daemon = log = None
        daemon, endpoint, log = start_daemon(daemon_binary, root / "fourth", data, env)
        status = next(item for item in json.loads(cli("provider", "status")) if item["provider"] == "codex")
        assert not status["api_key_configured"] and status["active_auth_mode"] is None
        for path in ["/v1/providers/codex/accounts", "/v1/providers/codex/models"]:
            assert request(endpoint, None, path=path)[0] == 401
        assert all(secret not in Path(log.name).read_text(errors="replace") for secret in secrets)
    finally:
        fixture.release.set()
        if daemon is not None:
            stop_daemon(daemon, log)
        fixture.close()
        go_fixture.close()
