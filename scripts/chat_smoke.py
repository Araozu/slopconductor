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


MODEL = "glm-5.3-flash"
PROVIDER = "opencode-go"
REPLY = "fixture reply survives client exit"
INTERRUPTED_TEXT = "partial visible " * 1100


class ProviderFixture:
    def __init__(self):
        self.lock = threading.Lock()
        self.requests = []
        self.gates = [threading.Event() for _ in range(4)]

        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                if self.path != "/v1/chat/completions":
                    self.send_error(404)
                    return
                size = int(self.headers.get("Content-Length", "0"))
                payload = json.loads(self.rfile.read(size))
                if self.headers.get("Authorization") != "Bearer offline-test-key":
                    self.send_error(401)
                    return
                with fixture.lock:
                    index = len(fixture.requests)
                    fixture.requests.append(payload)
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
                elif index < 2:
                    fixture.gates[index].wait(timeout=25)
                if index != 2:
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Cache-Control", "no-cache")
                    self.end_headers()
                for text, finish in (("fixture reply survives ", None), ("client exit", None), ("", "stop")):
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
    env = os.environ.copy()
    env["HOME"] = str(root / "home")
    env["XDG_CONFIG_HOME"] = str(root / "xdg-config")
    env["XDG_DATA_HOME"] = str(root / "xdg-data")
    env["XDG_STATE_HOME"] = str(root / "xdg-state")
    env["OPENCODE_GO_API_KEY"] = "offline-test-key"
    env["SLOP_PROVIDER_BASE_URL"] = fixture.base_url
    env["SLOP_DEFAULT_MODEL"] = f"{PROVIDER}/{MODEL}"
    env.pop("SLOP_TOKEN_FILE", None)
    env.pop("SLOP_CONFIG", None)
    env.pop("SLOP_DATA_DIR", None)
    if os.name == "nt":
        env["USERPROFILE"] = str(root / "home")
        env["LOCALAPPDATA"] = str(root / "local-app-data")
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
    while time.monotonic() < deadline:
        status, turn = request(endpoint, token, path=f"/v1/turns/{turn_id}")
        if status == 200 and turn["status"] in statuses:
            return turn
        time.sleep(0.05)
    raise RuntimeError(f"turn {turn_id} did not reach one of {statuses}")


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
        status, canceled = request(endpoint, token, "POST", {"command_id": "explicit-cancel-command"}, f"/v1/turns/{cancel_turn_id}/cancel")
        if status != 202:
            raise RuntimeError(f"explicit cancellation was not accepted: {status} {canceled}")
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
    finally:
        if daemon is not None and daemon.poll() is None:
            try:
                stop_daemon(daemon, log)
            except Exception:
                pass
        fixture.close()
