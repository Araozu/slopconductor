"""Offline long conversations, provider usage, compaction recovery and limits."""
import http.server
import json
import sqlite3
import threading

from chat_smoke import request, wait_turn


def check_context(daemon_binary, root, start_daemon, stop_daemon, isolated_env):
    root.mkdir(parents=True, exist_ok=True)
    calls, rejected = [], set()
    lock = threading.Lock()

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            messages = payload["messages"]
            users = [m["content"] for m in messages if m["role"] == "user"]
            def text(value):
                return value if isinstance(value, str) else "".join(b.get("text", "") for b in value)
            last = text(users[-1])
            summary = last.startswith("Summarize this historical agent conversation")
            with lock:
                calls.append(payload)
                key = (self.headers.get("x-opencode-session"), last)
                first = key not in rejected
                rejected.add(key)
            status, error = 200, None
            if last == "reject-once" and first:
                status, error = 400, {"code": "context_length_exceeded", "message": "private fixture secret"}
            if last == "rate-once" and first:
                status, error = 429, {"code": "rate_limit_exceeded"}
            if last == "quota":
                status, error = 429, {"code": "insufficient_quota", "message": "private fixture secret"}
            if summary:
                reply = "Earlier work: preserve the user's project goal and constraints; recorded steps completed; continue the latest instruction."
            else:
                reply = "Recorded result. " + "r" * 1800
            if self.path.endswith("/messages"):
                usage = {"input_tokens": 42, "output_tokens": 10}
                if last == "cached-usage":
                    usage.update(cache_read_input_tokens=300000, cache_creation_input_tokens=1000)
                body = {"model": payload["model"], "stop_reason": "end_turn", "content": [{"type": "text", "text": reply}], "usage": usage}
                if last == "context-stop" and first:
                    body["stop_reason"] = "model_context_window_exceeded"
                    body["content"].append({"type": "tool_use", "id": "truncated-call", "name": "write", "input": "unfinished"})
            else:
                usage = {"prompt_tokens": 270000 if last == "reported-usage" else 100, "completion_tokens": 10}
                body = {"model": payload["model"], "choices": [{"finish_reason": "stop", "message": {"content": reply}}], "usage": usage}
                if last == "bad-summary":
                    body["usage"]["prompt_tokens"] = 270000
                if last.startswith("long-turn-"):
                    body.pop("usage")  # Exercise the unreported-usage fallback.
                if last == "output-once" and first:
                    body["choices"][0]["finish_reason"] = "length"
                original = next(text(m["content"]) for m in reversed(messages) if m["role"] == "user" and not text(m["content"]).startswith("Your previous response reached its output-token limit."))
                if original == "output-forever":
                    body["choices"][0]["finish_reason"] = "length"
                if summary and "bad-summary" in last:
                    body["choices"][0]["finish_reason"] = "length"
            if error:
                body = {"error": error}
            raw = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            if status == 429:
                self.send_header("Retry-After", "0")
            self.end_headers()
            self.wfile.write(raw)

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    base = f"http://127.0.0.1:{server.server_port}/v1"
    config = root / "config.toml"
    config.write_text(f'provider_base_url = "{base}"\n', encoding="utf-8")
    env = isolated_env(root, OPENCODE_GO_API_KEY="context-fixture-key", SLOP_CONFIG=config)
    data = root / "data"
    daemon = log = None
    try:
        daemon, endpoint, log = start_daemon(daemon_binary, root / "first", data_dir=data, env=env)
        token = (data / "credentials" / "local-api-token").read_text().strip()
        serial = 0

        def api(method, path, payload=None):
            status, body = request(endpoint, token, method, payload, path)
            assert status in {200, 202}, (path, status, body)
            return body

        def create(model="glm-5.3-flash"):
            nonlocal serial
            serial += 1
            return api("POST", "/v1/sessions", {"command_id": f"context-create-{serial}", "provider": "opencode-go", "model": model})["session_id"]

        def send(session, prompt, terminal="completed"):
            nonlocal serial
            serial += 1
            receipt = api("POST", f"/v1/sessions/{session}/messages", {"command_id": f"context-send-{serial}", "text": prompt})
            return wait_turn(endpoint, token, receipt["turn_id"], {terminal})

        def records(turn):
            return api("GET", f"/v1/turns/{turn['id']}/requests")["items"]

        for model, prompt, tokens in [("glm-5.3-flash", "reported-usage", 270000), ("claude-haiku-5-5", "cached-usage", 301042)]:
            session = create(model)
            first = send(session, prompt)
            assert first["usage"]["input_tokens"] == tokens, (model, first)
            second = send(session, "continue the goal")
            assert any(r["compaction"] for r in records(second)), records(second)
            assert second["usage"]["input_tokens"] == (200 if model == "glm-5.3-flash" else 84)

        session = create()
        for i in range(300):
            turn = send(session, f"long-turn-{i} Preserve project goal. " + "q" * 1200)
            if i < 200:
                assert not any(r["compaction"] for r in records(turn))
        # History remains complete and paginated; internal summaries stay out.
        page = api("GET", f"/v1/sessions/{session}/messages?limit=200")
        history = page["items"]
        while page["next_after"] is not None:
            page = api("GET", f"/v1/sessions/{session}/messages?limit=200&after={page['next_after']}")
            history.extend(page["items"])
        assert len(history) == 600
        assert not any("Working memory" in m["text"] or "Earlier work:" in m["text"] for m in history)
        with lock:
            before_restart = len(calls)
            assert any("Summarize this historical" in json.dumps(c) for c in calls)
        stop_daemon(daemon, log)
        daemon = log = None
        daemon, endpoint, log = start_daemon(daemon_binary, root / "second", data_dir=data, env=env)
        send(session, "after-restart")
        with lock:
            latest = calls[-1]
            assert len(calls) >= before_restart + 1
        assert "Working memory" in json.dumps(latest)
        assert "long-turn-0" not in json.dumps(latest)
        # Simulate an old conversation with a full transcript and no checkpoint.
        stop_daemon(daemon, log)
        daemon = log = None
        with sqlite3.connect(data / "state.sqlite3") as db:
            db.execute("DELETE FROM session_context WHERE session_id=?", (session,))
            # Pre-compaction releases had neither a checkpoint nor byte anchors.
            db.execute("UPDATE model_requests SET context='{}' WHERE turn_id IN(SELECT id FROM turns WHERE session_id=?)", (session,))
        daemon, endpoint, log = start_daemon(daemon_binary, root / "historical", data_dir=data, env=env)
        repaired = send(session, "after-historical-overflow")
        assert any(r["compaction"] for r in records(repaired))

        recovery = create()
        send(recovery, "Initial goal " + "g" * 1500)
        recovered = send(recovery, "reject-once")
        recs = records(recovered)
        assert recs[0]["error_code"] == "context_limit" and any(r["compaction"] for r in recs)
        context_stop = send(create("claude-haiku-5-5"), "context-stop")
        assert any(r["finish_reason"] == "context_limit" for r in records(context_stop))
        assert not context_stop["tool_invocation_ids"]
        bad = create()
        send(bad, "bad-summary")
        failed = send(bad, "continue after summary failure", "failed")
        assert failed["error_code"] == "context_compaction_failed"
        assert any(m["text"] == "bad-summary" for m in api("GET", f"/v1/sessions/{bad}/messages")["items"])
        assert send(create(), "rate-once")["status"] == "completed"
        output_once = send(create(), "output-once")
        assert len(records(output_once)) == 2 and not output_once["tool_invocation_ids"]
        output = create()
        limited = send(output, "output-forever", "incomplete")
        assert len(records(limited)) <= 3
        send(output, "finish the previous task")
        with lock:
            previous = calls[-1]["messages"]
            assert any(m["role"] == "user" and m["content"] == "output-forever" for m in previous)
            assert any(m["role"] == "assistant" for m in previous)
        quota = send(create(), "quota", "failed")
        assert quota["error_code"] == "provider_quota_exhausted"
        assert len(records(quota)) == 1
        assert "private fixture secret" not in json.dumps(quota)
    finally:
        if daemon is not None:
            stop_daemon(daemon, log)
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
