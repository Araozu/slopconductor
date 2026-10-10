"""Offline lifecycle check for the real daemon and CLI chat binaries."""

import http.server
import json
import os
from pathlib import Path
import signal
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

from cli_smoke import check_cli_commands


MODEL = "glm-5.3-flash"
PROVIDER = "opencode-go"
REPLY = "fixture reply survives client exit"
INTERRUPTED_TEXT = "partial visible " * 1100


class ProviderFixture:
    def __init__(self):
        self.lock = threading.Lock()
        self.requests = []
        self.authorizations = []
        self.gates = [threading.Event() for _ in range(32)]
        self.held_indices = set()
        self.valid_keys = {"offline-test-key"}

        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                if self.path != "/v1/chat/completions":
                    self.send_error(404)
                    return
                size = int(self.headers.get("Content-Length", "0"))
                payload = json.loads(self.rfile.read(size))
                if self.headers.get("Authorization") not in {f"Bearer {key}" for key in fixture.valid_keys}:
                    self.send_error(401)
                    return
                with fixture.lock:
                    index = len(fixture.requests)
                    fixture.requests.append(payload)
                    fixture.authorizations.append(self.headers.get("Authorization"))
                if index == 2:
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Cache-Control", "no-cache")
                    self.end_headers()
                    event = {
                        "id": f"chatcmpl-fixture-{index}", "object": "chat.completion.chunk",
                        "created": 1, "model": MODEL,
                        "choices": [{"index": 0, "delta": {
                            "reasoning_content": "private synthetic reasoning",
                            "content": INTERRUPTED_TEXT,
                        }, "finish_reason": None}],
                    }
                    self.wfile.write(b"data: " + json.dumps(event).encode() + b"\n\n")
                    self.wfile.flush()
                    # Keep the provider stream open until the test kills the daemon.
                    fixture.gates[index].wait(timeout=25)
                elif index < 2 or index in fixture.held_indices:
                    fixture.gates[index].wait(timeout=25)
                if index != 2:
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Cache-Control", "no-cache")
                    self.end_headers()
                last_user = next((message.get("content", "") for message in reversed(payload.get("messages", [])) if message.get("role") == "user"), "")
                if "tool-boundary-first" in last_user or "tool-immediate-first" in last_user:
                    if "tool-boundary-first" in last_user:
                        command = "sleep 0.6; printf completed > first-tool.txt"
                        calls = [
                            {"index": 0, "id": "boundary-bash", "type": "function", "function": {"name": "bash", "arguments": json.dumps({"command": command})}},
                            {"index": 1, "id": "boundary-write", "type": "function", "function": {"name": "write", "arguments": json.dumps({"path": "skipped-tool.txt", "content": "must not be written"})}},
                        ]
                    else:
                        command = "sleep 20; printf completed > immediate-tool.txt"
                        calls = [{"index": 0, "id": "immediate-bash", "type": "function", "function": {"name": "bash", "arguments": json.dumps({"command": command})}}]
                    event = {"id": f"chatcmpl-fixture-{index}", "object": "chat.completion.chunk", "created": 1, "model": MODEL,
                             "choices": [{"index": 0, "delta": {"tool_calls": calls}, "finish_reason": "tool_calls"}]}
                    self.wfile.write(b"data: " + json.dumps(event).encode() + b"\n\n")
                    self.wfile.write(b"data: [DONE]\n\n")
                    self.wfile.flush()
                    return
                reply = REPLY if index < 4 else f"steering fixture reply {index}"
                for text, finish in ((reply[:20], None), (reply[20:], None), ("", "stop")):
                    event = {
                        "id": f"chatcmpl-fixture-{index}", "object": "chat.completion.chunk",
                        "created": 1, "model": MODEL,
                        "choices": [{"index": 0, "delta": {"content": text} if text else {}, "finish_reason": finish}],
                    }
                    self.wfile.write(b"data: " + json.dumps(event).encode() + b"\n\n")
                    self.wfile.flush()
                    if text:
                        time.sleep(0.03)
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()

            def log_message(self, *_):
                pass

        class FixtureServer(http.server.ThreadingHTTPServer):
            def handle_error(self, _request, _client_address):
                # Daemon termination intentionally closes the in-flight SSE
                # socket; the resulting BrokenPipeError is part of the test.
                pass

        self.server = FixtureServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def base_url(self):
        return f"http://127.0.0.1:{self.server.server_port}/v1"

    def count(self):
        with self.lock:
            return len(self.requests)

    def payload(self, index):
        with self.lock:
            return self.requests[index]

    def wait_for_count(self, count):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.count() >= count:
                return
            time.sleep(0.02)
        raise RuntimeError(f"provider received {self.count()} requests; expected {count}")

    def close(self):
        for gate in self.gates:
            gate.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def isolated_env(root, fixture):
    root.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["HOME"] = str(root / "home")
    env["XDG_CONFIG_HOME"] = str(root / "xdg-config")
    env["XDG_DATA_HOME"] = str(root / "xdg-data")
    env["XDG_STATE_HOME"] = str(root / "xdg-state")
    env["OPENCODE_GO_API_KEY"] = "offline-test-key"
    env["SLOP_PROVIDER_BASE_URL"] = fixture.base_url
    env["SLOP_DEFAULT_MODEL"] = f"{PROVIDER}/{MODEL}"
    config_path = root / "config.yaml"
    config_path.write_text("execution_concurrency: 1\n", encoding="utf-8")
    env["SLOP_CONFIG"] = str(config_path)
    env.pop("SLOP_TOKEN_FILE", None)
    env.pop("SLOP_CONFIG", None)
    env.pop("SLOP_DATA_DIR", None)
    if os.name == "nt":
        env["USERPROFILE"] = str(root / "home")
        # Keep root/data inside the simulated per-user credential location.
        env["LOCALAPPDATA"] = str(root)
    return env


def run(command, env=None, timeout=30):
    return subprocess.run(command, cwd=Path(__file__).resolve().parents[1], env=env,
                          check=True, capture_output=True, text=True, timeout=timeout)


def start_daemon(binary, root, data_dir, env):
    root.mkdir(parents=True, exist_ok=True)
    log_path = root / "daemon.log"
    log = log_path.open("w", encoding="utf-8")
    daemon = subprocess.Popen(
        [str(binary), "--listen", "127.0.0.1:0", "--data-dir", str(data_dir)],
        cwd=Path(__file__).resolve().parents[1], env=env,
        stdout=subprocess.DEVNULL, stderr=log,
    )
    deadline = time.monotonic() + 15
    endpoint = None
    while time.monotonic() < deadline:
        text = log_path.read_text(encoding="utf-8")
        marker = "Listening on http://127.0.0.1:"
        if marker in text:
            endpoint = text.split(marker, 1)[1].splitlines()[0].strip()
            endpoint = "http://127.0.0.1:" + endpoint
            break
        if daemon.poll() is not None:
            log.close()
            raise RuntimeError(f"daemon exited during startup: {text}")
        time.sleep(0.03)
    if endpoint is None:
        daemon.kill()
        daemon.wait(timeout=5)
        log.close()
        raise RuntimeError("chat daemon did not publish its address")
    return daemon, endpoint, log


def stop_daemon(daemon, log):
    if daemon.poll() is None:
        if os.name == "nt":
            daemon.terminate()
        else:
            daemon.send_signal(signal.SIGTERM)
        daemon.wait(timeout=15)
    if os.name != "nt" and daemon.returncode != 0:
        log.close()
        raise RuntimeError(f"chat daemon shutdown failed with {daemon.returncode}")
    log.close()


def request(endpoint, token, method="GET", payload=None, path=""):
    url = endpoint + path
    data = None if payload is None else json.dumps(payload).encode()
    headers = {} if token is None else {"Authorization": f"Bearer {token}"}
    if data is not None:
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=8) as response:
            body = response.read()
            return response.status, json.loads(body) if body else None
    except urllib.error.HTTPError as error:
        body = error.read()
        if not body:
            return error.code, None
        try:
            return error.code, json.loads(body)
        except json.JSONDecodeError:
            return error.code, {"message": body.decode("utf-8", errors="replace")}


def wait_turn(endpoint, token, turn_id, statuses):
    deadline = time.monotonic() + 15
    last = None
    while time.monotonic() < deadline:
        status, turn = request(endpoint, token, path=f"/v1/turns/{turn_id}")
        last = (status, turn)
        if status == 200 and turn["status"] in statuses:
            return turn
        time.sleep(0.05)
    raise RuntimeError(f"turn {turn_id} did not reach one of {statuses}; last response: {last}")


def wait_instructions(endpoint, token, session_id, expected_status):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        status, page = request(endpoint, token, path=f"/v1/sessions/{session_id}/instructions?limit=50")
        if status == 200 and any(item["status"] == expected_status for item in page["items"]):
            return page["items"]
        time.sleep(0.03)
    raise RuntimeError(f"steering instruction did not reach {expected_status}: {status} {page}")


def wait_tool_status(endpoint, token, turn_id, status_name):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        status, page = request(endpoint, token, path=f"/v1/turns/{turn_id}/tools?limit=50")
        if status == 200 and any(item["status"] == status_name for item in page["items"]):
            return page["items"]
        time.sleep(0.03)
    raise RuntimeError(f"tool invocation did not reach {status_name}: {status} {page}")


def wait_checkpoint(endpoint, token, session_id, turn_id):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        status, page = request(
            endpoint, token,
            path=f"/v1/sessions/{session_id}/messages?limit=200",
        )
        if status == 200:
            checkpoint = next((
                message for message in page["items"]
                if message.get("turn_id") == turn_id
                and message["role"] == "assistant"
                and message["status"] == "checkpoint"
            ), None)
            if checkpoint is not None:
                return checkpoint
        time.sleep(0.05)
    raise RuntimeError(f"turn {turn_id} did not durably checkpoint visible text")


def read_replay(endpoint, token, session_id, after):
    query = urllib.parse.urlencode({"after": after, "follow": "true"})
    req = urllib.request.Request(
        f"{endpoint}/v1/sessions/{session_id}/events?{query}",
        headers={"Authorization": f"Bearer {token}"},
    )
    frames = []
    with urllib.request.urlopen(req, timeout=12) as response:
        while True:
            line = response.readline()
            if not line:
                break
            frame = json.loads(line)
            frames.append(frame)
            event = frame.get("event", {})
            if event.get("kind") in {"turn_completed", "turn_failed", "turn_cancelled", "turn_interrupted"}:
                return frames
    return frames


def check_chat_lifecycle(daemon_binary, cli_binary, root):
    fixture = ProviderFixture()
    daemon = None
    log = None
    try:
        env = isolated_env(root, fixture)
        if os.name == "nt":
            data_dir = Path(env["LOCALAPPDATA"]) / "slopconductor" / "chat-data"
        else:
            data_dir = root / "data"
        daemon, endpoint, log = start_daemon(daemon_binary, root / "run-1", data_dir, env)
        token_path = data_dir / "credentials" / "local-api-token"
        token = token_path.read_text(encoding="ascii").strip()

        status, unauthenticated = request(endpoint, None, "POST", {}, "/v1/sessions")
        if status != 401 or unauthenticated.get("code") != "unauthorized":
            raise RuntimeError(f"unauthenticated chat mutation was not rejected before parsing: {status} {unauthenticated}")
        status, invalid_model = request(endpoint, token, "POST", {
            "command_id": "invalid-model-command", "title": None, "provider": PROVIDER,
            "model": "unknown-model", "max_tokens": None,
        }, "/v1/sessions")
        if status != 400:
            raise RuntimeError(f"invalid model was not rejected: {status} {invalid_model}")
        status, invalid_cursor = request(endpoint, token, path="/v1/sessions/missing/events?after=-1&follow=false")
        if status != 400:
            raise RuntimeError(f"negative event cursor was not rejected: {status} {invalid_cursor}")

        accepted = run([
            str(cli_binary), "--daemon", endpoint, "--token-file", str(token_path), "--json",
            "chat", "--prompt", "write a short reply", "--detach", "--command-id", "cli-chat-command",
        ], env=env)
        objects = [json.loads(line) for line in accepted.stdout.splitlines() if line.strip()]
        session_receipt = next(value["receipt"] for value in objects if value.get("type") == "session_receipt")
        message_receipt = next(value["receipt"] for value in objects if value.get("type") == "receipt")
        session_id = session_receipt["session_id"]
        turn_id = message_receipt["turn_id"]
        if not turn_id or accepted.returncode != 0:
            raise RuntimeError(f"CLI did not return durable acceptance: {accepted.stdout}")
        fixture.wait_for_count(1)

        # Reconnect after the client has exited. The durable terminal event and
        # canonical message must be recoverable from the receipt cursor.
        fixture.gates[0].set()
        frames = read_replay(endpoint, token, session_id, message_receipt["event_sequence"])
        if not any(frame.get("event", {}).get("kind") == "turn_completed" for frame in frames):
            raise RuntimeError(f"replay missed terminal turn event: {frames}")
        history_status, page = request(endpoint, token, path=f"/v1/sessions/{session_id}/messages?limit=200")
        canonical = next((message for message in page["items"] if message["role"] == "assistant"), None)
        if history_status != 200 or canonical is None or canonical["text"] != REPLY:
            raise RuntimeError(f"canonical assistant history missing after reconnect: {page}")

        check_cli_commands(cli_binary, endpoint, token_path, env, root,
                           session_receipt, message_receipt, REPLY)

        create_payload = {
            "command_id": "cli-chat-command:session", "title": None,
            "provider": PROVIDER, "model": MODEL, "max_tokens": None,
        }
        status, repeated_create = request(endpoint, token, "POST", create_payload, "/v1/sessions")
        if status != 202 or repeated_create != session_receipt:
            raise RuntimeError(f"same create command did not return its receipt: {status} {repeated_create}")
        send_payload = {"command_id": "cli-chat-command", "text": "write a short reply", "expected_revision": None}
        status, repeated_send = request(endpoint, token, "POST", send_payload, f"/v1/sessions/{session_id}/messages")
        if status != 202 or repeated_send != message_receipt:
            raise RuntimeError(f"same send command did not return its receipt: {status} {repeated_send}")
        conflict = {**send_payload, "text": "different payload"}
        status, error = request(endpoint, token, "POST", conflict, f"/v1/sessions/{session_id}/messages")
        if status != 409 or error.get("code") != "command_conflict":
            raise RuntimeError(f"changed command payload was not rejected: {status} {error}")
        if fixture.count() != 1:
            raise RuntimeError(f"idempotent retry produced {fixture.count()} provider requests")

        stop_daemon(daemon, log)
        daemon, endpoint, log = start_daemon(daemon_binary, root / "run-2", data_dir, env)
        token = token_path.read_text(encoding="ascii").strip()
        status, page = request(endpoint, token, path=f"/v1/sessions/{session_id}/messages?limit=200")
        if status != 200 or not any(message["text"] == REPLY for message in page["items"]):
            raise RuntimeError("durable conversation history did not survive daemon restart")

        status, created = request(endpoint, token, "POST", {
            "command_id": "cancel-session-command", "title": None,
            "provider": PROVIDER, "model": MODEL, "max_tokens": None,
        }, "/v1/sessions")
        if status != 202:
            raise RuntimeError(f"could not create cancellation fixture session: {status} {created}")
        cancel_session = created["session_id"]
        status, cancel_receipt = request(endpoint, token, "POST", {
            "command_id": "cancel-message-command", "text": "cancel this turn", "expected_revision": None,
        }, f"/v1/sessions/{cancel_session}/messages")
        if status != 202:
            raise RuntimeError(f"could not accept cancellation fixture turn: {status} {cancel_receipt}")
        fixture.wait_for_count(2)
        cancel_turn_id = cancel_receipt["turn_id"]
        canceled = run([
            str(cli_binary), "--daemon", endpoint, "--token-file", str(token_path), "--json",
            "turn", "cancel", cancel_turn_id, "--command-id", "explicit-cancel-command",
        ], env=env)
        cancel_command = json.loads(canceled.stdout)
        if cancel_command["turn_id"] != cancel_turn_id:
            raise RuntimeError(f"explicit CLI cancellation returned the wrong receipt: {cancel_command}")
        fixture.gates[1].set()
        turn = wait_turn(endpoint, token, cancel_turn_id, {"cancelled"})
        if turn["status"] != "cancelled" or fixture.count() != 2:
            raise RuntimeError(f"explicit cancellation outcome is wrong: {turn}")

        # A process crash while the provider is still streaming must retain a
        # visible checkpoint, discard reasoning, and recover the turn as
        # interrupted without issuing another provider request.
        status, crash_receipt = request(endpoint, token, "POST", {
            "command_id": "crash-message-command", "text": "interrupt this turn", "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 202:
            raise RuntimeError(f"could not accept crash-recovery fixture turn: {status} {crash_receipt}")
        crash_turn_id = crash_receipt["turn_id"]
        fixture.wait_for_count(3)
        checkpoint = wait_checkpoint(endpoint, token, session_id, crash_turn_id)
        if checkpoint["text"] != INTERRUPTED_TEXT or "private synthetic reasoning" in checkpoint["text"]:
            raise RuntimeError(f"visible checkpoint contains unexpected provider content: {checkpoint}")

        daemon.kill()
        daemon.wait(timeout=10)
        log.close()
        daemon = None
        log = None
        daemon, endpoint, log = start_daemon(daemon_binary, root / "run-3", data_dir, env)
        token = token_path.read_text(encoding="ascii").strip()
        recovered = wait_turn(endpoint, token, crash_turn_id, {"interrupted"})
        if recovered["error_code"] != "daemon_restarted":
            raise RuntimeError(f"crashed turn did not recover as interrupted: {recovered}")
        status, recovered_page = request(
            endpoint, token, path=f"/v1/sessions/{session_id}/messages?limit=200",
        )
        interrupted = next((
            message for message in recovered_page["items"]
            if message.get("turn_id") == crash_turn_id and message["role"] == "assistant"
        ), None)
        if (status != 200 or interrupted is None or interrupted["status"] != "interrupted"
                or interrupted["text"] != INTERRUPTED_TEXT
                or not any(message["text"] == REPLY for message in recovered_page["items"])):
            raise RuntimeError(f"crash recovery lost completed history or visible checkpoint: {recovered_page}")
        time.sleep(0.3)
        if fixture.count() != 3:
            raise RuntimeError(f"daemon restart automatically retried interrupted provider work: {fixture.count()} requests")

        status, followup = request(endpoint, token, "POST", {
            "command_id": "post-crash-followup-command", "text": "continue after restart", "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 202:
            raise RuntimeError(f"could not accept post-crash followup: {status} {followup}")
        fixture.wait_for_count(4)
        sent_messages = fixture.payload(3).get("messages", [])
        serialized_context = json.dumps(sent_messages)
        if "partial visible" in serialized_context or "private synthetic reasoning" in serialized_context:
            raise RuntimeError(f"interrupted output leaked into resumed provider context: {serialized_context}")
        if not any(message.get("content") == "continue after restart" for message in sent_messages):
            raise RuntimeError(f"post-crash user input missing from provider context: {sent_messages}")
        post_crash = wait_turn(endpoint, token, followup["turn_id"], {"completed"})
        if post_crash["status"] != "completed" or fixture.count() != 4:
            raise RuntimeError(f"post-crash followup did not complete exactly once: {post_crash}")

        fixture.held_indices.update({4, 5})
        status, boundary_turn = request(endpoint, token, "POST", {
            "command_id": "boundary-holder", "text": "boundary holder", "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 202:
            raise RuntimeError(f"could not start boundary holder: {status} {boundary_turn}")
        fixture.wait_for_count(5)
        status, boundary_instruction = request(endpoint, token, "POST", {
            "command_id": "boundary-instruction", "text": "boundary instruction", "delivery": "next-boundary",
            "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 202 or boundary_instruction["turn_id"] != boundary_turn["turn_id"]:
            raise RuntimeError(f"boundary delivery did not target active turn: {status} {boundary_instruction}")
        status, repeated_instruction = request(endpoint, token, "POST", {
            "command_id": "boundary-instruction", "text": "boundary instruction", "delivery": "next-boundary",
            "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 202 or repeated_instruction != boundary_instruction:
            raise RuntimeError(f"steering command replay changed its accepted receipt: {status} {repeated_instruction}")
        status, conflicted_instruction = request(endpoint, token, "POST", {
            "command_id": "boundary-instruction", "text": "different instruction", "delivery": "next-boundary",
            "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 409 or conflicted_instruction.get("code") != "command_conflict":
            raise RuntimeError(f"reused steering ID with a different payload was accepted: {status} {conflicted_instruction}")
        fixture.gates[4].set()
        fixture.wait_for_count(6)
        boundary_context = json.dumps(fixture.payload(5).get("messages", []))
        if "boundary instruction" not in boundary_context:
            raise RuntimeError(f"boundary instruction was not applied before the next request: {boundary_context}")
        fixture.gates[5].set()
        wait_turn(endpoint, token, boundary_turn["turn_id"], {"completed"})
        wait_instructions(endpoint, token, session_id, "applied")

        fixture.held_indices.update({6, 7})
        status, after_holder = request(endpoint, token, "POST", {
            "command_id": "after-holder", "text": "after holder", "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 202:
            raise RuntimeError(f"could not start after-turn holder: {status} {after_holder}")
        fixture.wait_for_count(7)
        status, after_turn = request(endpoint, token, "POST", {
            "command_id": "after-turn-message", "text": "after turn only", "delivery": "after-turn",
            "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 202 or after_turn["turn_id"] == after_holder["turn_id"]:
            raise RuntimeError(f"after-turn delivery did not queue a new turn: {status} {after_turn}")
        fixture.gates[6].set()
        fixture.wait_for_count(8)
        if "after turn only" in json.dumps(fixture.payload(6).get("messages", [])):
            raise RuntimeError("after-turn message leaked into the active turn context")
        if "after turn only" not in json.dumps(fixture.payload(7).get("messages", [])):
            raise RuntimeError("after-turn message was missing from its queued turn")
        fixture.gates[7].set()
        wait_turn(endpoint, token, after_holder["turn_id"], {"completed"})
        wait_turn(endpoint, token, after_turn["turn_id"], {"completed"})

        fixture.held_indices.update({8, 9})
        status, immediate_turn = request(endpoint, token, "POST", {
            "command_id": "immediate-holder", "text": "immediate holder", "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 202:
            raise RuntimeError(f"could not start immediate holder: {status} {immediate_turn}")
        fixture.wait_for_count(9)
        status, immediate_instruction = request(endpoint, token, "POST", {
            "command_id": "immediate-instruction", "text": "immediate correction", "delivery": "immediate",
            "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 202 or immediate_instruction["turn_id"] != immediate_turn["turn_id"]:
            raise RuntimeError(f"immediate delivery did not target active turn: {status} {immediate_instruction}")
        time.sleep(0.3)
        fixture.gates[8].set()
        fixture.wait_for_count(10)
        immediate_context = json.dumps(fixture.payload(9).get("messages", []))
        if "immediate correction" not in immediate_context:
            raise RuntimeError(f"immediate correction was not applied before replanning: {immediate_context}")
        fixture.gates[9].set()
        wait_turn(endpoint, token, immediate_turn["turn_id"], {"completed"})
        requests_status, requests_page = request(endpoint, token, path=f"/v1/turns/{immediate_turn['turn_id']}/requests?limit=20")
        if requests_status != 200 or len(requests_page["items"]) != 2 or requests_page["items"][0]["status"] != "interrupted":
            raise RuntimeError(f"immediate inference was not interrupted exactly once: {requests_status} {requests_page}")

        tool_root = root / "steering-workspace"
        tool_root.mkdir(parents=True, exist_ok=True)
        policy = {"root": str(tool_root), "allowed_tools": ["bash", "write"], "shell_timeout_ms": 10000,
                  "max_output_bytes": 4096, "max_tool_calls": 4, "max_model_requests": 4}
        status, tool_session = request(endpoint, token, "POST", {
            "command_id": "tool-steering-session", "title": "tool steering", "provider": PROVIDER,
            "model": MODEL, "max_tokens": None, "execution": policy,
        }, "/v1/sessions")
        if status != 202:
            raise RuntimeError(f"could not create steering tool session: {status} {tool_session}")
        status, tool_turn = request(endpoint, token, "POST", {
            "command_id": "tool-boundary-turn", "text": "tool-boundary-first", "expected_revision": None,
        }, f"/v1/sessions/{tool_session['session_id']}/messages")
        if status != 202:
            raise RuntimeError(f"could not start tool boundary turn: {status} {tool_turn}")
        fixture.wait_for_count(11)
        wait_tool_status(endpoint, token, tool_turn["turn_id"], "running")
        status, tool_steer = request(endpoint, token, "POST", {
            "command_id": "tool-boundary-steer", "text": "skip the pending write", "delivery": "next-boundary",
            "expected_revision": None,
        }, f"/v1/sessions/{tool_session['session_id']}/messages")
        if status != 202:
            raise RuntimeError(f"could not steer a running tool: {status} {tool_steer}")
        fixture.wait_for_count(12)
        fixture.payload(11)
        if "skip the pending write" not in json.dumps(fixture.payload(11).get("messages", [])):
            raise RuntimeError("tool-boundary instruction was not applied before the next model request")
        wait_turn(endpoint, token, tool_turn["turn_id"], {"completed"})
        tool_root.joinpath("first-tool.txt").read_text(encoding="utf-8")
        if (tool_root / "skipped-tool.txt").exists():
            raise RuntimeError("unstarted pending tool side effect ran after boundary steering")
        invocations = wait_tool_status(endpoint, token, tool_turn["turn_id"], "failed")
        if len(invocations) != 2 or not any(item.get("error_code") == "steering_applied_before_dispatch" for item in invocations):
            raise RuntimeError(f"pending tool intents were not paired with explicit skipped results: {invocations}")

        immediate_root = root / "immediate-workspace"
        immediate_root.mkdir(parents=True, exist_ok=True)
        immediate_policy = {**policy, "root": str(immediate_root)}
        status, immediate_session = request(endpoint, token, "POST", {
            "command_id": "immediate-tool-session", "title": "immediate tool", "provider": PROVIDER,
            "model": MODEL, "max_tokens": None, "execution": immediate_policy,
        }, "/v1/sessions")
        if status != 202:
            raise RuntimeError(f"could not create immediate tool session: {status} {immediate_session}")
        status, immediate_tool_turn = request(endpoint, token, "POST", {
            "command_id": "immediate-tool-turn", "text": "tool-immediate-first", "expected_revision": None,
        }, f"/v1/sessions/{immediate_session['session_id']}/messages")
        if status != 202:
            raise RuntimeError(f"could not start immediate tool turn: {status} {immediate_tool_turn}")
        fixture.wait_for_count(13)
        wait_tool_status(endpoint, token, immediate_tool_turn["turn_id"], "running")
        status, immediate_tool = request(endpoint, token, "POST", {
            "command_id": "immediate-tool-steer", "text": "stop that command", "delivery": "immediate",
            "expected_revision": None,
        }, f"/v1/sessions/{immediate_session['session_id']}/messages")
        if status != 202:
            raise RuntimeError(f"could not immediately steer running Bash: {status} {immediate_tool}")
        fixture.wait_for_count(14)
        wait_turn(endpoint, token, immediate_tool_turn["turn_id"], {"completed"})
        if (immediate_root / "immediate-tool.txt").exists():
            raise RuntimeError("immediate delivery did not stop the running Bash side effect")

        replacement_key = "steering-snapshot-replacement-key"
        fixture.valid_keys.add(replacement_key)
        fixture.held_indices.update({14, 15})
        status, pinned_turn = request(endpoint, token, "POST", {
            "command_id": "pinned-provider-turn", "text": "provider snapshot holder", "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 202:
            raise RuntimeError(f"could not start provider snapshot turn: {status} {pinned_turn}")
        fixture.wait_for_count(15)
        status, key_updated = request(endpoint, token, "PUT", {"api_key": replacement_key}, "/v1/providers/opencode-go/api-key")
        if status != 200 or not key_updated.get("api_key_configured"):
            raise RuntimeError(f"could not replace provider key during active turn: {status} {key_updated}")
        status, pinned_instruction = request(endpoint, token, "POST", {
            "command_id": "pinned-provider-steer", "text": "continue with existing provider", "delivery": "next-boundary",
            "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 202:
            raise RuntimeError(f"could not steer pinned provider turn: {status} {pinned_instruction}")
        fixture.gates[14].set()
        fixture.wait_for_count(16)
        fixture.gates[15].set()
        wait_turn(endpoint, token, pinned_turn["turn_id"], {"completed"})
        status, fresh_turn = request(endpoint, token, "POST", {
            "command_id": "fresh-provider-turn", "text": "use replacement provider", "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 202:
            raise RuntimeError(f"could not start fresh provider turn: {status} {fresh_turn}")
        fixture.wait_for_count(17)
        if fixture.authorizations[14:17] != ["Bearer offline-test-key", "Bearer offline-test-key", f"Bearer {replacement_key}"]:
            raise RuntimeError(f"provider hot replacement crossed the logical turn boundary: {fixture.authorizations[14:17]}")
        wait_turn(endpoint, token, fresh_turn["turn_id"], {"completed"})

        fixture.held_indices.add(17)
        status, paused_turn = request(endpoint, token, "POST", {
            "command_id": "pause-holder", "text": "pause holder", "expected_revision": None,
        }, f"/v1/sessions/{session_id}/messages")
        if status != 202:
            raise RuntimeError(f"could not start pause holder: {status} {paused_turn}")
        fixture.wait_for_count(18)
        status, pause_receipt = request(endpoint, token, "POST", {
            "command_id": "pause-active-command",
        }, f"/v1/turns/{paused_turn['turn_id']}/pause")
        if status != 200:
            raise RuntimeError(f"could not pause active turn: {status} {pause_receipt}")
        fixture.gates[17].set()
        wait_turn(endpoint, token, paused_turn["turn_id"], {"paused"})
        status, parallel_session = request(endpoint, token, "POST", {
            "command_id": "paused-slot-session", "title": "slot release", "provider": PROVIDER,
            "model": MODEL, "max_tokens": None,
        }, "/v1/sessions")
        if status != 202:
            raise RuntimeError(f"could not create slot-release session: {status} {parallel_session}")
        fixture.held_indices.add(18)
        status, parallel_turn = request(endpoint, token, "POST", {
            "command_id": "paused-slot-turn", "text": "use released slot", "expected_revision": None,
        }, f"/v1/sessions/{parallel_session['session_id']}/messages")
        if status != 202:
            raise RuntimeError(f"could not use released execution slot: {status} {parallel_turn}")
        fixture.wait_for_count(19)
        fixture.gates[18].set()
        wait_turn(endpoint, token, parallel_turn["turn_id"], {"completed"})
        stop_daemon(daemon, log)
        daemon, endpoint, log = start_daemon(daemon_binary, root / "run-4", data_dir, env)
        token = token_path.read_text(encoding="ascii").strip()
        if wait_turn(endpoint, token, paused_turn["turn_id"], {"paused"})["status"] != "paused":
            raise RuntimeError("restart did not preserve paused state")
        time.sleep(0.3)
        if fixture.count() != 19:
            raise RuntimeError("restart automatically resumed a paused turn")
        status, resumed_receipt = request(endpoint, token, "POST", {
            "command_id": "explicit-resume",
        }, f"/v1/turns/{paused_turn['turn_id']}/resume")
        if status != 200:
            raise RuntimeError(f"could not explicitly resume paused turn: {status} {resumed_receipt}")
        fixture.wait_for_count(20)
        wait_turn(endpoint, token, paused_turn["turn_id"], {"completed"})
    finally:
        if daemon is not None and daemon.poll() is None:
            try:
                stop_daemon(daemon, log)
            except Exception:
                pass
        fixture.close()
