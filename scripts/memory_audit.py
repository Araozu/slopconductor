#!/usr/bin/env python3
"""Measure Linux daemon RSS with offline fixtures; pass a release slopd binary.

Temporary SQLite checkpoints supply identical 2 MiB contexts without hundreds
of provider calls. The fixture runs outside the measured daemon, retains no
request bodies, and holds four inferences open while RSS is sampled.
"""

import argparse
import http.server
import json
from pathlib import Path
import sqlite3
import sys
import tempfile
import threading
import time

from chat_smoke import request, wait_turn
from smoke import isolated_env, start_daemon, stop_daemon


def memory(pid):
    fields = {}
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith(("VmRSS:", "VmHWM:", "Threads:")):
            key, value, *_ = line.split()
            fields[key.rstrip(":")] = int(value)
    return fields


def measure(binary):
    condition = threading.Condition()
    release = threading.Event()
    arrivals = 0

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            nonlocal arrivals
            payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            last = payload["messages"][-1]["content"]
            held = last == "hold-for-memory-sample"
            del payload
            if held:
                with condition:
                    arrivals += 1
                    condition.notify_all()
                if not release.wait(timeout=30):
                    self.send_error(504)
                    return
            body = json.dumps({
                "choices": [{"finish_reason": "stop", "message": {"content": "done"}}],
                "usage": {"prompt_tokens": 100, "completion_tokens": 1},
            }).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    daemon = log = None
    try:
        with tempfile.TemporaryDirectory(prefix="slop-memory-") as directory:
            root = Path(directory)
            data = root / "data"
            config = root / "config.toml"
            config.write_text(
                f'provider_base_url = "http://127.0.0.1:{server.server_port}/v1"\n'
                'execution_concurrency = 4\n', encoding="utf-8",
            )
            env = isolated_env(root, OPENCODE_GO_API_KEY="offline-memory-fixture", SLOP_CONFIG=config)
            # Never use real credentials or provider endpoints inherited by fixtures.
            for key in ("OPENCODE_ZEN_API_KEY", "CODEX_API_KEY", "OPENAI_API_KEY", "SLOP_PROVIDER_BASE_URL"):
                env.pop(key, None)
            daemon, endpoint, log = start_daemon(binary, root / "seed", data_dir=data, env=env)
            token = (data / "credentials/local-api-token").read_text().strip()

            def api(method, path, body=None):
                status, value = request(endpoint, token, method, body, path)
                if status not in (200, 202):
                    raise RuntimeError((path, status, value))
                return value

            sessions = []
            for i in range(4):
                session = api("POST", "/v1/sessions", {
                    "command_id": f"seed-session-{i}", "provider": "opencode-go",
                    "model": "glm-5.3-flash", "settings": {"full_context": True},
                })["session_id"]
                receipt = api("POST", f"/v1/sessions/{session}/messages", {
                    "command_id": f"seed-message-{i}", "text": "seed",
                })
                turn = wait_turn(endpoint, token, receipt["turn_id"], {"completed"})
                sessions.append((session, turn["id"]))
            stop_daemon(daemon, log)
            daemon = log = None
            with sqlite3.connect(data / "state.sqlite3") as db:
                for session, turn in sessions:
                    messages = [{
                        "role": "user" if i % 2 == 0 else "assistant",
                        "blocks": [{"id": f"history-{i}", "type": "text", "text": "x" * 65536}],
                        "continuation": None,
                    } for i in range(32)]
                    ordinal = db.execute("SELECT MAX(ordinal) FROM messages WHERE session_id=?", (session,)).fetchone()[0]
                    model_request = db.execute("SELECT id FROM model_requests WHERE turn_id=?", (turn,)).fetchone()[0]
                    db.execute("INSERT INTO session_context VALUES(?,?,?,?)", (
                        session, ordinal, json.dumps(messages, separators=(",", ":")), model_request,
                    ))
            daemon, endpoint, log = start_daemon(binary, root / "measure", data_dir=data, env=env)
            samples = {"baseline": memory(daemon.pid)}
            for i in range(100):
                api("POST", "/v1/sessions", {
                    "command_id": f"idle-{i}", "provider": "opencode-go", "model": "glm-5.3-flash",
                })
                if i + 1 in (1, 10, 100):
                    samples[f"idle_{i + 1}"] = memory(daemon.pid)
            turns = []
            for i, (session, _) in enumerate(sessions):
                turns.append(api("POST", f"/v1/sessions/{session}/messages", {
                    "command_id": f"active-{i}", "text": "hold-for-memory-sample",
                })["turn_id"])
            with condition:
                if not condition.wait_for(lambda: arrivals == 4, timeout=15):
                    raise RuntimeError(f"Expected four held inferences, got {arrivals}")
            time.sleep(0.3)
            samples["active_4_with_2_mib_context_each"] = memory(daemon.pid)
            release.set()
            for turn in turns:
                wait_turn(endpoint, token, turn, {"completed"})
            samples["after_completion"] = memory(daemon.pid)
            return samples
    finally:
        release.set()
        if daemon is not None:
            stop_daemon(daemon, log)
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon", type=Path, default=Path("target/release/slopd"))
    parser.add_argument("--runs", type=int, default=3)
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("RSS sampling currently requires Linux /proc")
    if args.runs < 1:
        parser.error("--runs must be positive")
    print(json.dumps({"units": "KiB (RSS/HWM)", "runs": [measure(args.daemon.resolve()) for _ in range(args.runs)]}, indent=2))


if __name__ == "__main__":
    main()
