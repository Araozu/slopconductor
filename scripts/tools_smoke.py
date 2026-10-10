"""Offline structured tool loop, recovery, and client checks using real binaries."""

import hashlib
import http.server
import json
import os
from pathlib import Path
import shlex
import sys
import threading
import time

from chat_smoke import isolated_env, request, run, start_daemon, stop_daemon, wait_turn


class ToolFixture:
    def __init__(self):
        self.lock = threading.Lock()
        self.requests = []
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                if self.path != "/v1/chat/completions" or self.headers.get("Authorization") != "Bearer offline-test-key":
                    self.send_error(401)
                    return
                payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                with fixture.lock:
                    fixture.requests.append(payload)
                prompt = next(m["content"] for m in reversed(payload["messages"]) if m["role"] == "user")
                current = payload["messages"][next(i for i in reversed(range(len(payload["messages"]))) if payload["messages"][i]["role"] == "user") + 1:]
                completed_tools = any(m["role"] == "tool" for m in current)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()

                def emit(delta, finish=None):
                    event = {"model": payload["model"], "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
                    self.wfile.write(b"data: " + json.dumps(event).encode() + b"\n\n")
                    self.wfile.flush()

                if prompt in {"workflow", "workflow incomplete", "cancel shell", "crash shell", "malformed", "limited", "restricted"} and not completed_tools:
                    if prompt in {"workflow", "workflow incomplete"}:
                        command = "if [ -n \"${OPENCODE_GO_API_KEY+x}\" ]; then exit 9; fi; printf '%024000d' 0"
                        calls = [
                            ("read", {"path": "file.txt", "offset": 2, "limit": 1}),
                            ("write", {"path": "nested/new.txt", "content": "created"}),
                            ("write", {"path": "nested/new.txt", "content": "overwritten"}),
                            ("edit", {"path": "file.txt", "edits": [{"oldText": "before", "newText": "after"}]}),
                            ("bash", {"command": command, "timeout": 5}),
                        ]
                    elif prompt in {"cancel shell", "crash shell"}:
                        seconds = 25 if prompt == "cancel shell" else 2
                        # Native Python PIDs work on Windows too, where Bash's $$
                        # is an MSYS PID. Check a child and its descendant.
                        script = (
                            "import os, subprocess, sys; from pathlib import Path; "
                            f"child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep({seconds})']); "
                            f"marker = Path('{prompt.split()[0]}.pid'); temporary = marker.with_suffix('.tmp'); "
                            "temporary.write_text(str(os.getpid()) + ' ' + str(child.pid), encoding='ascii'); "
                            "temporary.replace(marker); "
                            "child.wait()"
                        )
                        command = f"{shlex.quote(Path(sys.executable).as_posix())} -c {shlex.quote(script)} & wait"
                        calls = [("bash", {"command": command})]
                    else:
                        calls = [("write", {"path": "must-not-exist.txt", "content": "unsafe"})]
                    emit({"content": "Inspecting workspace. ", "reasoning_content": "private synthetic continuation"})
                    for i, (name, args) in enumerate(calls):
                        encoded = json.dumps(args) if prompt not in {"malformed", "limited"} else '{"path":'
                        cut = len(encoded) // 2
                        emit({"tool_calls": [{"index": i, "id": f"call-{i}", "type": "function", "function": {"name": name, "arguments": encoded[:cut]}}]})
                        emit({"tool_calls": [{"index": i, "function": {"arguments": encoded[cut:]}}]})
                    emit({}, "length" if prompt == "limited" else "tool_calls")
                else:
                    emit({"content": "Tool workflow "})
                    time.sleep(0.02)
                    emit({"content": "complete."})
                    emit({}, "length" if prompt == "workflow incomplete" else "stop")
                self.wfile.write(b'data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3,"total_tokens":10}}\n\n')
                self.wfile.write(b"data: [DONE]\n\n")
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

    def snapshot(self):
        with self.lock:
            return list(self.requests)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def check_tools(daemon_binary, cli_binary, root):
    fixture = ToolFixture()
    daemon = log = None
    try:
        env = isolated_env(root, fixture)
        data = root / "data"
        workspace = root / "workspace"
        workspace.mkdir(parents=True)
        (workspace / "file.txt").write_text("header\nbefore\nfooter", encoding="utf-8", newline="\n")
        daemon, endpoint, log = start_daemon(daemon_binary, root / "first", data, env)
        token_path = data / "credentials" / "local-api-token"
        token = token_path.read_text(encoding="ascii").strip()

        def cli(*args):
            return run([str(cli_binary), "--daemon", endpoint, "--token-file", str(token_path), "--json", *args], env=env)

        def get(path):
            status, body = request(endpoint, token, path=path)
            assert status == 200, (path, status, body)
            return body

        def send(session, prompt, command, **fields):
            payload = {"command_id": command.replace(" ", "-"), "text": prompt, **fields}
            status, receipt = request(endpoint, token, "POST", payload, f"/v1/sessions/{session}/messages")
            assert status == 202, (status, receipt)
            return receipt, payload

        def create(command):
            status, receipt = request(endpoint, token, "POST", {
                "command_id": command.replace(" ", "-"), "provider": "opencode-go", "model": "glm-5.3-flash",
                "execution": {"root": str(workspace), "allowed_tools": ["read", "edit", "bash", "write"]},
            }, "/v1/sessions")
            assert status == 202, (status, receipt)
            return receipt["session_id"]

        capabilities = get("/v1/capabilities")
        assert capabilities["per_turn_model_selection"]
        descriptors = {tool["name"]: tool for tool in capabilities["tools"]}
        assert set(descriptors) == {"read", "write", "edit", "bash"}, descriptors
        assert descriptors["read"]["side_effects"] == "read"
        assert all(descriptors[name]["side_effects"] == "write" for name in ["write", "edit", "bash"])
        result = cli("chat", "--workspace", str(workspace), "--prompt", "workflow", "--command-id", "tool-cli")
        frames = [json.loads(line) for line in result.stdout.splitlines()]
        receipt = next(f["receipt"] for f in frames if f["type"] == "receipt")
        session, turn = receipt["session_id"], receipt["turn_id"]
        state = get(f"/v1/turns/{turn}")
        assert state["status"] == "completed"
        assert state["usage"]["total_tokens"] == 20
        assert (workspace / "file.txt").read_text() == "header\nafter\nfooter"
        assert (workspace / "nested/new.txt").read_text() == "overwritten"
        assert len(state["model_request_ids"]) == 2 and len(state["tool_invocation_ids"]) == 5
        tools = get(f"/v1/turns/{turn}/tools")["items"]
        assert all(t["status"] == "completed" for t in tools), tools
        assert len([f for f in frames if f["type"] == "tool_snapshot"]) == 5
        assert any(f["type"] == "delta" and f["kind"] == "tool_arguments" and f["block_id"] and f["request_id"] for f in frames)
        history = get(f"/v1/sessions/{session}/messages")["items"]
        assert [m["role"] for m in history] == ["user", "assistant", *(["tool"] * 5), "assistant"]
        public = json.dumps(history) + result.stdout + json.dumps(tools)
        assert "private synthetic continuation" not in public
        wire = fixture.snapshot()[1]
        assert any(m.get("reasoning_content") == "private synthetic continuation" for m in wire["messages"])
        wire_tools = [m for m in wire["messages"] if m["role"] == "tool"]
        assert len(wire_tools) == 5
        read_result = json.loads(json.loads(wire_tools[0]["content"])["output"])
        assert read_result["text"] == "before" and read_result["next_offset"] == 3, read_result
        assert {tool["function"]["name"] for tool in wire["tools"]} == {"read", "write", "edit", "bash"}
        artifact = tools[4]["artifact_ids"][0]
        metadata = get(f"/v1/artifacts/{artifact}")
        downloaded = root / "download.json"
        cli("artifact", "download", artifact, "--output", str(downloaded))
        content = downloaded.read_bytes()
        assert len(content) == metadata["size_bytes"] and hashlib.sha256(content).hexdigest() == artifact
        assert len(json.loads(content)["stdout"]) >= 24000
        for path in ["/v1/capabilities", f"/v1/messages/{history[-1]['id']}", f"/v1/turns/{turn}/requests", f"/v1/turns/{turn}/tools", f"/v1/tools/{tools[0]['id']}", f"/v1/artifacts/{artifact}", f"/v1/artifacts/{artifact}/content"]:
            status, _ = request(endpoint, None, path=path)
            assert status == 401, (path, status)
        count = len(fixture.snapshot())
        cli("chat", "--workspace", str(workspace), "--prompt", "workflow", "--command-id", "tool-cli", "--detach")
        assert len(fixture.snapshot()) == count
        result = cli("chat", "--workspace", str(workspace), "--tool", "read", "--prompt", "restricted", "--command-id", "read-only-cli")
        frames_restricted = [json.loads(line) for line in result.stdout.splitlines()]
        restricted_receipt = next(f["receipt"] for f in frames_restricted if f["type"] == "receipt")
        restricted_tools = get(f"/v1/turns/{restricted_receipt['turn_id']}/tools")["items"]
        assert len(restricted_tools) == 1 and restricted_tools[0]["error_code"] == "tool_not_allowed", restricted_tools
        assert not restricted_tools[0]["effects_unknown"]
        assert not (workspace / "must-not-exist.txt").exists()
        assert {tool["function"]["name"] for tool in fixture.snapshot()[-1]["tools"]} == {"read"}
        status, error = request(endpoint, token, "POST", {
            "command_id": "unsupported-tool", "provider": "opencode-go", "model": "glm-5.3-flash",
            "execution": {"root": str(workspace), "allowed_tools": ["list_files"]},
        }, "/v1/sessions")
        assert status == 400, (status, error)
        count = len(fixture.snapshot())
        status, error = request(endpoint, token, "POST", {"command_id": "effort-rejected", "text": "hello", "settings": {"reasoning_effort": "high"}}, f"/v1/sessions/{session}/messages")
        assert status == 400 and error["code"] == "unsupported_capability"
        assert len(fixture.snapshot()) == count
        for prompt, terminal in [("malformed", "failed"), ("limited", "incomplete")]:
            fresh = create(f"create-{prompt}")
            receipt, _ = send(fresh, prompt, f"send-{prompt}")
            wait_turn(endpoint, token, receipt["turn_id"], {terminal})
            assert get(f"/v1/turns/{receipt['turn_id']}/tools")["items"] == []
            assert not (workspace / "must-not-exist.txt").exists()

        incomplete_session = create("create-incomplete-workflow")
        (workspace / "file.txt").write_text("header\nbefore\nfooter", encoding="utf-8")
        receipt, _ = send(incomplete_session, "workflow incomplete", "incomplete-workflow")
        wait_turn(endpoint, token, receipt["turn_id"], {"incomplete"})
        assert (workspace / "file.txt").read_text() == "header\nafter\nfooter"
        assert all(t["status"] == "completed" for t in get(f"/v1/turns/{receipt['turn_id']}/tools")["items"])
        receipt, _ = send(incomplete_session, "inspect incomplete effects", "inspect-incomplete")
        wait_turn(endpoint, token, receipt["turn_id"], {"completed"})
        assert len([m for m in fixture.snapshot()[-1]["messages"] if m["role"] == "tool"]) == 5

        # Switching is safe in plain text context, with settings frozen for each turn.
        status, plain = request(endpoint, token, "POST", {"command_id": "plain", "provider": "opencode-go", "model": "glm-5.3-flash"}, "/v1/sessions")
        assert status == 202
        receipt, _ = send(plain["session_id"], "plain", "plain-first")
        wait_turn(endpoint, token, receipt["turn_id"], {"completed"})
        receipt, _ = send(plain["session_id"], "switch", "plain-switch", model="opencode-go/glm-5.3", settings={"max_output_tokens": 512})
        selected = wait_turn(endpoint, token, receipt["turn_id"], {"completed"})
        assert selected["requested_model"] == "opencode-go/glm-5.3" and selected["settings"]["max_output_tokens"] == 512
        records = get(f"/v1/turns/{receipt['turn_id']}/requests")["items"]
        assert records[0]["effective_settings"]["max_output_tokens"] == 512
        assert fixture.snapshot()[-1]["max_tokens"] == 512

        def running_shell(prompt):
            session = create(f"create-{prompt}")
            receipt, _ = send(session, prompt, f"send-{prompt}")
            marker = workspace / f"{prompt.split()[0]}.pid"
            deadline = time.monotonic() + 10
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            assert marker.exists(), "shell never launched"
            pids = [int(value) for value in marker.read_text(encoding="ascii").split()]
            assert len(pids) == 2 and all(process_is_running(pid) for pid in pids), "shell descendants never launched"
            return session, receipt["turn_id"], pids

        _, canceled, pids = running_shell("cancel shell")
        status, _ = request(endpoint, token, "POST", {"command_id": "cancel-tool"}, f"/v1/turns/{canceled}/cancel")
        assert status == 202
        wait_turn(endpoint, token, canceled, {"cancelled"})
        tool = get(f"/v1/turns/{canceled}/tools")["items"][0]
        assert tool["effects_unknown"] and tool["error_code"] == "cancelled", tool
        deadline = time.monotonic() + 5
        while any(process_is_running(pid) for pid in pids) and time.monotonic() < deadline:
            time.sleep(0.02)
        assert not any(process_is_running(pid) for pid in pids), "canceled shell descendants survived"
        crashed_session, crashed, _ = running_shell("crash shell")
        count = len(fixture.snapshot())
        daemon.kill()
        daemon.wait(timeout=5)
        log.close()
        daemon = log = None
        time.sleep(2.1)  # hard process death cannot promise descendant cleanup on Unix
        daemon, endpoint, log = start_daemon(daemon_binary, root / "second", data, env)
        recovered = get(f"/v1/turns/{crashed}")
        assert recovered["status"] == "interrupted"
        tool = get(f"/v1/turns/{crashed}/tools")["items"][0]
        assert tool["error_code"] == "daemon_restarted" and tool["effects_unknown"]
        assert len(fixture.snapshot()) == count
        assert get(f"/v1/artifacts/{artifact}") == metadata
        assert len(get(f"/v1/sessions/{session}/messages")["items"]) == 8
        receipt, _ = send(crashed_session, "inspect recovery", "inspect-recovery")
        wait_turn(endpoint, token, receipt["turn_id"], {"completed"})
        assert any(m["role"] == "tool" and json.loads(m["content"])["effects_unknown"] for m in fixture.snapshot()[-1]["messages"])
    finally:
        if daemon is not None:
            stop_daemon(daemon, log)
        fixture.close()


def process_is_running(pid):
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes

        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        kernel.OpenProcess.restype = wintypes.HANDLE
        kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        kernel.WaitForSingleObject.restype = wintypes.DWORD
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel.CloseHandle.restype = wintypes.BOOL
        handle = kernel.OpenProcess(0x00100000, False, pid)  # SYNCHRONIZE
        if not handle:
            if ctypes.get_last_error() == 87:  # ERROR_INVALID_PARAMETER: PID no longer exists
                return False
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            status = kernel.WaitForSingleObject(handle, 0)
            if status == 0xFFFFFFFF:  # WAIT_FAILED
                raise ctypes.WinError(ctypes.get_last_error())
            return status == 258  # WAIT_TIMEOUT: process has not exited
        finally:
            kernel.CloseHandle(handle)
    try:
        os.kill(pid, 0)
        if sys.platform == "linux":
            # A killed grandchild can remain a zombie until its adopter reaps it.
            return not Path(f"/proc/{pid}/stat").read_text().rpartition(") ")[2].startswith("Z")
        return True
    except (ProcessLookupError, FileNotFoundError):
        return False
