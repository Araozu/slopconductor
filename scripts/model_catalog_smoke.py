"""Dynamic Go/Zen catalog and reasoning controls through real native binaries."""

import http.server
import json
import threading

from chat_smoke import request, wait_turn


def check_model_catalog(daemon_binary, root, start_daemon, stop_daemon, isolated_env):
    root.mkdir(parents=True, exist_ok=True)
    calls = []
    metadata_requests = []
    catalog = {}
    for provider in ("opencode-go", "opencode"):
        catalog[provider] = {"npm": "@ai-sdk/openai-compatible", "models": {}}
        for name, npm in (("fresh-chat", "@ai-sdk/openai-compatible"),
                          ("fresh-response", "@ai-sdk/openai"),
                          ("fresh-messages", "@ai-sdk/anthropic")):
            catalog[provider]["models"][name] = {
                "id": name, "name": name, "provider": {"npm": npm},
                "reasoning": True, "tool_call": True,
                "modalities": {"output": ["text"]},
                "reasoning_options": [{"type": "effort", "values": ["low", "max"]}],
                "limit": {"context": 123456, "input": 120000},
            }
        catalog[provider]["models"]["unsupported-wire"] = {
            "id": "unsupported-wire", "name": "Unsupported", "provider": {"npm": "@ai-sdk/google"},
        }

    class Handler(http.server.BaseHTTPRequestHandler):
        def reply(self, value):
            raw = json.dumps(value).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def do_GET(self):
            if self.path == "/metadata.json":
                metadata_requests.append(dict(self.headers))
                self.reply(catalog)
            elif self.path == "/v1/models":
                self.reply({"data": [{"id": model} for model in catalog["opencode-go"]["models"]]})
            else:
                self.send_error(404)

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            calls.append((self.path, body))
            if self.path.endswith("/responses"):
                self.reply({"model": body["model"], "status": "completed", "output": [
                    {"type": "message", "content": [{"type": "output_text", "text": "ok"}]}]})
            elif self.path.endswith("/messages"):
                self.reply({"model": body["model"], "stop_reason": "end_turn",
                            "content": [{"type": "text", "text": "ok"}]})
            else:
                self.reply({"model": body["model"], "choices": [
                    {"finish_reason": "stop", "message": {"content": "ok"}}]})

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    origin = f"http://127.0.0.1:{server.server_port}"
    config = root / "config.toml"
    config.write_text(f'provider_base_url = "{origin}/v1"\nopencode_zen_base_url = "{origin}/v1"\n'
                      f'models_dev_url = "{origin}/metadata.json"\n'
                      'default_model = "opencode-go/fresh-chat"\n', encoding="utf-8")
    env = isolated_env(root, OPENCODE_GO_API_KEY="synthetic-go-key", OPENCODE_ZEN_API_KEY="synthetic-zen-key",
                       SLOP_CONFIG=str(config))
    env.pop("SLOP_DEFAULT_MODEL", None)
    data = root / "data"
    daemon = log = None
    try:
        daemon, endpoint, log = start_daemon(daemon_binary, root / "first", data_dir=data, env=env)
        token = (data / "credentials" / "local-api-token").read_text().strip()

        def api(method, path, payload=None, expected=200):
            status, body = request(endpoint, token, method, payload, path)
            assert status == expected, (path, status, body)
            return body

        models = {model["id"]: model for model in api("GET", "/v1/models")}
        for provider in ("opencode-go", "opencode-zen"):
            for model, path, field in (("fresh-chat", "/v1/chat/completions", "reasoning_effort"),
                                       ("fresh-response", "/v1/responses", "reasoning"),
                                       ("fresh-messages", "/v1/messages", "output_config")):
                info = models[f"{provider}/{model}"]
                assert info["ready"] and info["capabilities"]["reasoning_efforts"] == ["low", "max"], info
                assert info["capabilities"]["context_window_tokens"] == 123456, info
                receipt = api("POST", "/v1/sessions", {"command_id": f"create-{provider}-{model}",
                              "provider": provider, "model": model, "settings": {"reasoning_effort": "max"}}, 202)
                turn = api("POST", f'/v1/sessions/{receipt["session_id"]}/messages',
                           {"command_id": f"send-{provider}-{model}", "text": "hello"}, 202)
                completed = wait_turn(endpoint, token, turn["turn_id"], {"completed"})
                assert completed["settings"]["reasoning_effort"] == "max", completed
                actual_path, payload = calls[-1]
                assert actual_path == path, (actual_path, payload)
                assert payload[field] == ("max" if field == "reasoning_effort" else {"effort": "max"}), payload
                count = len(calls)
                rejected = api("POST", f'/v1/sessions/{receipt["session_id"]}/messages',
                               {"command_id": f"bad-{provider}-{model}", "text": "no dispatch",
                                "settings": {"reasoning_effort": "high"}}, 400)
                assert rejected["code"] == "unsupported_capability", rejected
                assert len(calls) == count
            live = api("GET", f"/v1/providers/{provider}/models")
            discovered = {model["model"]: model for model in live["models"]}
            assert discovered["fresh-chat"]["ready"]
            assert discovered["fresh-chat"]["capabilities"]["reasoning_efforts"] == ["low", "max"]
            assert not discovered["unsupported-wire"]["ready"]
        assert len(metadata_requests) == 1, metadata_requests
        assert not any(key.lower() in {"authorization", "x-api-key"} for key in metadata_requests[0])
    finally:
        if daemon is not None:
            stop_daemon(daemon, log)
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
