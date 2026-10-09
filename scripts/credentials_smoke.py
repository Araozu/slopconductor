"""Offline credential configuration checks against the actual daemon and CLI."""

import http.server
import json
import os
from pathlib import Path
import subprocess
import threading
import time
import urllib.parse
import urllib.request

from chat_smoke import request, wait_turn


class KeyFixture:
    def __init__(self):
        self.requests = []
        self.lock = threading.Lock()
        self.first_started = threading.Event()
        self.release_first = threading.Event()
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                with fixture.lock:
                    index = len(fixture.requests)
                fixture.requests.append((self.headers.get("Authorization"), payload, self.path))
                if index == 0:
                    fixture.first_started.set()
                    fixture.release_first.wait(20)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                if self.path.endswith("/responses"):
                    events = [
                        {"type": "response.output_text.delta", "delta": "ok"},
                        {"type": "response.completed", "response": {
                            "status": "completed", "model": payload["model"],
                            "output": [{"type": "message", "content": [{"type": "output_text", "text": "ok"}]}],
                            "usage": {"input_tokens": 2, "output_tokens": 1},
                        }},
                    ]
                    body = b"".join(b"data: " + json.dumps(event).encode() + b"\n\n" for event in events)
                    terminator = b""
                else:
                    event = {
                        "id": f"credential-fixture-{index}", "object": "chat.completion.chunk",
                        "created": 1, "model": payload["model"],
                        "choices": [{"index": 0, "delta": {"content": "ok"}, "finish_reason": "stop"}],
                    }
                    body = b"data: " + json.dumps(event).encode() + b"\n\n"
                    terminator = b"data: [DONE]\n\n"
                try:
                    self.wfile.write(body + terminator)
                    self.wfile.flush()
                except OSError:
                    # The restart fixture deliberately kills a client with an
                    # in-flight request to exercise durable queued work.
                    pass

            def log_message(self, *_):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def base_url(self):
        return f"http://127.0.0.1:{self.server.server_port}/v1"

    def close(self):
        self.release_first.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(5)


def check_credentials(daemon_binary, cli_binary, root, start_daemon, stop_daemon, isolated_env):
    fixture = KeyFixture()
    daemon = log = None
    first_key = "fixture-first-provider-key"
    second_key = "fixture-replacement-provider-key"
    keys = [first_key, second_key, "fixture-zen-key", "fixture-codex-key"]
    try:
        env = isolated_env(
            root,
            SLOP_PROVIDER_BASE_URL=fixture.base_url,
            SLOP_OPENCODE_ZEN_BASE_URL=fixture.base_url,
            SLOP_CODEX_BASE_URL=fixture.base_url,
        )
        for variable in ("OPENCODE_GO_API_KEY", "OPENCODE_ZEN_API_KEY", "OPENAI_API_KEY", "SLOP_DEFAULT_MODEL"):
            env.pop(variable, None)
        data_dir = Path(env["LOCALAPPDATA"] if os.name == "nt" else env["XDG_DATA_HOME"]) / "slopconductor"
        daemon, endpoint, log = start_daemon(daemon_binary, root, env=env)
        credential_dir = data_dir / "credentials"
        token_file = credential_dir / "local-api-token"
        token = token_file.read_text().strip()
        cli = [str(cli_binary), "--daemon", endpoint, "--token-file", str(token_file), "--json"]

        def command(*args, input=None):
            result = subprocess.run(cli + list(args), input=input, env=env, text=True,
                                    capture_output=True, check=True, timeout=20)
            assert all(key not in result.stdout + result.stderr for key in keys)
            return result.stdout

        statuses = json.loads(command("provider", "status"))
        assert len(statuses) == 3 and not any(item["api_key_configured"] for item in statuses)
        assert not any(item["ready"] for item in json.loads(command("models")))
        status, _ = request(endpoint, None, "PUT", {"api_key": first_key}, "/v1/providers/opencode-go/api-key")
        assert status == 401 and not (credential_dir / "opencode-go-api-key").exists()

        configured = json.loads(command("provider", "set-key", "opencode-go", input=first_key + "\n"))
        assert configured["api_key_configured"] and configured["execution_supported"]
        assert any(item["ready"] and item["provider"] == "opencode-go" for item in json.loads(command("models")))
        for provider, key in (("opencode-zen", keys[2]), ("codex", keys[3])):
            key_file = root / f"{provider}-fixture-input"
            key_file.write_text(key + "\n")
            configured = json.loads(command("provider", "set-key", provider, "--key-file", str(key_file)))
            assert configured["api_key_configured"] and configured["execution_supported"]
        for provider, key in (("opencode-go", first_key), ("opencode-zen", keys[2]), ("codex", keys[3])):
            path = credential_dir / f"{provider}-api-key"
            assert path.read_text() == key
            if os.name != "nt":
                assert path.stat().st_mode & 0o777 == 0o600
                assert credential_dir.stat().st_mode & 0o777 == 0o700

        for provider, key, expected in (("missing", first_key, 400), ("opencode-go", "", 400),
                                         ("opencode-go", "bad\nheader", 400), ("opencode-go", "x" * 17000, 400),
                                         ("opencode-go", "x" * 40000, 413)):
            status, body = request(endpoint, token, "PUT", {"api_key": key}, f"/v1/providers/{provider}/api-key")
            assert status == expected, (status, body)
            assert all(secret not in json.dumps(body) for secret in keys)
        assert (credential_dir / "opencode-go-api-key").read_text() == first_key

        def detached_chat(identity):
            frames = [json.loads(line) for line in command("chat", "--prompt", "reply ok", "--detach", "--command-id", identity).splitlines()]
            return next(frame["receipt"]["turn_id"] for frame in frames if frame["type"] == "receipt")

        first_turn = detached_chat("credentials-first-turn")
        assert fixture.first_started.wait(10)
        for _ in range(2):
            status, body = request(endpoint, token, "PUT", {"api_key": second_key}, "/v1/providers/opencode-go/api-key")
            assert status == 200 and body["api_key_configured"]
        second_turn = detached_chat("credentials-second-turn")
        assert wait_turn(endpoint, token, second_turn, {"completed"})["status"] == "completed"
        fixture.release_first.set()
        assert wait_turn(endpoint, token, first_turn, {"completed"})["status"] == "completed"
        assert [item[0] for item in fixture.requests] == [f"Bearer {first_key}", f"Bearer {second_key}"]

        if os.name != "nt":
            path = credential_dir / "opencode-go-api-key"
            path.chmod(0o644)
            status, body = request(endpoint, token, "PUT", {"api_key": first_key}, "/v1/providers/opencode-go/api-key")
            assert status == 503 and body["code"] == "credential_storage_unavailable"
            assert path.read_text() == second_key
            path.chmod(0o600)

        # Denial finishes the real callback flow before any external token request.
        login_payload = {"command_id": "credential-login-1"}
        status, login = request(endpoint, token, "POST", login_payload, "/v1/providers/codex/login")
        assert status == 202
        assert request(endpoint, token, "POST", login_payload, "/v1/providers/codex/login") == (202, login)
        assert request(endpoint, token, "POST", {"command_id": "other-login"}, "/v1/providers/codex/login")[0] == 409
        authorization = urllib.parse.parse_qs(urllib.parse.urlparse(login["authorization_url"]).query)
        host_id = authorization["ext_agent_host_id"][0]
        assert host_id == request(endpoint, token, path="/v1/node")[1]["node_id"]
        assert json.loads(command("provider", "login-status", login["login_id"]))["status"] == "pending"
        denied = authorization["redirect_uri"][0] + "?" + urllib.parse.urlencode({
            "error": "access_denied", "state": authorization["state"][0],
        })
        with urllib.request.urlopen(denied, timeout=5) as response:
            response.read()
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            status = request(endpoint, token, path=f"/v1/providers/codex/login/{login['login_id']}")[1]
            if status["status"] == "failed":
                break
            time.sleep(0.05)
        assert status["status"] == "failed"
        assert not (credential_dir / "codex-chatgpt.json").exists()
        assert request(endpoint, token, "POST", {"command_id": "credential-login-2"}, "/v1/providers/codex/login")[0] == 202

        stop_daemon(daemon, log)
        daemon = log = None
        env["OPENCODE_GO_API_KEY"] = "stale-environment-key"
        daemon, endpoint, log = start_daemon(daemon_binary, root, env=env)
        cli[2] = endpoint
        assert all(item["api_key_configured"] for item in json.loads(command("provider", "status")))
        assert request(endpoint, token, path="/v1/providers/codex/login/credential-login-2")[0] == 404
        third_turn = detached_chat("credentials-restarted-turn")
        assert wait_turn(endpoint, token, third_turn, {"completed"})["status"] == "completed"
        assert fixture.requests[-1][0] == f"Bearer {second_key}" and len(fixture.requests) == 3
        accepted_receipts = {}
        for provider, model in (("opencode-zen", "glm-5.3"), ("codex", "gpt-6.1-sol")):
            identity = f"credentials-{provider}-turn"
            output = command(
                "chat", "--model", f"{provider}/{model}", "--prompt", "reply ok", "--detach",
                "--command-id", identity,
            )
            frames = [json.loads(line) for line in output.splitlines()]
            accepted_receipts[identity] = next(
                frame["receipt"] for frame in frames if frame["type"] == "receipt"
            )
            turn = next(frame["receipt"]["turn_id"] for frame in frames if frame["type"] == "receipt")
            assert wait_turn(endpoint, token, turn, {"completed"})["status"] == "completed"
        assert fixture.requests[-2][1]["model"] == "glm-5.3"
        assert fixture.requests[-1][1]["model"] == "gpt-6.1-sol"
        assert fixture.requests[-1][1]["max_output_tokens"] == 4096
        login = request(endpoint, token, "POST", {"command_id": "credential-login-3"}, "/v1/providers/codex/login")[1]
        authorization = urllib.parse.parse_qs(urllib.parse.urlparse(login["authorization_url"]).query)
        assert authorization["ext_agent_host_id"][0] == host_id

        # A synthetic saved registration exercises subscription mode without
        # contacting the authorization or token endpoints.
        stop_daemon(daemon, log)
        daemon = log = None
        (credential_dir / "codex-chatgpt.json").write_text(json.dumps({
            "issuer": "https://auth.openai.com",
            "subject": "offline-fixture-subject",
            "client_id": "oaiapp_offline_fixture",
            "ext_agent_host_id": host_id,
            "id_token": "offline-fixture-id-token",
            "access_token": "offline-fixture-access-token",
            "refresh_token": "offline-fixture-refresh-token",
            "scopes": ["openid", "offline_access", "resource.invoke", "chatgpt.tokens.use.direct"],
            "expires_at": 4_000_000_000,
        }))
        (credential_dir / "codex-auth-mode").write_text("chatgpt")
        if os.name != "nt":
            (credential_dir / "codex-chatgpt.json").chmod(0o600)
            (credential_dir / "codex-auth-mode").chmod(0o600)
        daemon, endpoint, log = start_daemon(daemon_binary, root, env=env)
        cli[2] = endpoint
        codex_status = next(item for item in json.loads(command("provider", "status")) if item["provider"] == "codex")
        assert codex_status["api_key_configured"] and codex_status["chatgpt_configured"]
        assert codex_status["active_auth_mode"] == "chatgpt"

        before = len(fixture.requests)
        for extra in (("--max-output-tokens", "64"), ("--workspace", str(root))):
            rejected = subprocess.run(
                cli + ["chat", "--model", "codex/gpt-6.1-sol", "--prompt", "reply", *extra,
                       "--command-id", f"subscription-rejected-{before}"],
                env=env, text=True, capture_output=True, timeout=20,
            )
            assert rejected.returncode != 0 and "unsupported_capability" in rejected.stderr
            assert len(fixture.requests) == before

        subscription_identity = "subscription-uncapped-turn"
        output = command(
            "chat", "--model", "codex/gpt-6.1-sol", "--prompt", "reply without an explicit cap",
            "--detach", "--command-id", subscription_identity,
        )
        frames = [json.loads(line) for line in output.splitlines()]
        subscription_receipt = next(
            frame["receipt"] for frame in frames if frame["type"] == "receipt"
        )
        turn = next(frame["receipt"]["turn_id"] for frame in frames if frame["type"] == "receipt")
        assert wait_turn(endpoint, token, turn, {"completed"})["status"] == "completed"
        assert fixture.requests[-1][2].endswith("/responses")
        assert fixture.requests[-1][1].get("max_output_tokens") is None

        # Switching modes must affect only new admissions. Both saved auth
        # records remain present, and accepted command IDs still replay their
        # original receipts after the active mode changes.
        platform_key = "fixture-codex-platform-after-subscription"
        key_status = json.loads(command("provider", "set-key", "codex", input=platform_key + "\n"))
        assert key_status["api_key_configured"] and key_status["active_auth_mode"] == "api_key"
        for identity, model, prompt, expected in (
            ("credentials-codex-turn", "codex/gpt-6.1-sol", "reply ok",
             accepted_receipts["credentials-codex-turn"]),
            (subscription_identity, "codex/gpt-6.1-sol", "reply without an explicit cap",
             subscription_receipt),
        ):
            replay = command(
                "chat", "--model", model, "--prompt", prompt, "--detach", "--command-id", identity
            )
            replay_frames = [json.loads(line) for line in replay.splitlines()]
            replay_receipt = next(
                frame["receipt"] for frame in replay_frames if frame["type"] == "receipt"
            )
            assert replay_receipt == expected

        stop_daemon(daemon, log)
        daemon = log = None
        daemon, endpoint, log = start_daemon(daemon_binary, root, env=env)
        cli[2] = endpoint
        codex_status = next(
            item for item in json.loads(command("provider", "status"))
            if item["provider"] == "codex"
        )
        assert codex_status["api_key_configured"] and codex_status["chatgpt_configured"]
        assert codex_status["active_auth_mode"] == "api_key"
        platform_turn_output = command(
            "chat", "--model", "codex/gpt-6.1-sol", "--prompt", "reply with platform key",
            "--max-output-tokens", "64", "--detach", "--command-id", "platform-key-after-restart",
        )
        platform_turn_frames = [json.loads(line) for line in platform_turn_output.splitlines()]
        platform_turn = next(
            frame["receipt"]["turn_id"] for frame in platform_turn_frames
            if frame["type"] == "receipt"
        )
        assert wait_turn(endpoint, token, platform_turn, {"completed"})["status"] == "completed"
        assert fixture.requests[-1][0] == f"Bearer {platform_key}"
        assert fixture.requests[-1][1]["max_output_tokens"] == 64

        # Leave a Zen turn durably queued behind an in-flight Go request, then
        # restart. The scheduler must not claim it until saved Zen credentials
        # have been assembled into the runtime provider registry.
        queue_fixture = KeyFixture()
        queue_root = root / "queued-provider-restart"
        queue_config = queue_root / "config.toml"
        queue_config.parent.mkdir(parents=True)
        queue_config.write_text("execution_concurrency = 1\n")
        queue_env = isolated_env(
            queue_root,
            SLOP_CONFIG=queue_config,
            SLOP_PROVIDER_BASE_URL=queue_fixture.base_url,
            SLOP_OPENCODE_ZEN_BASE_URL=queue_fixture.base_url,
        )
        for variable in ("OPENCODE_GO_API_KEY", "OPENCODE_ZEN_API_KEY", "OPENAI_API_KEY"):
            queue_env.pop(variable, None)
        queue_daemon = queue_log = None
        try:
            queue_daemon, queue_endpoint, queue_log = start_daemon(
                daemon_binary, queue_root, env=queue_env
            )
            queue_data = Path(
                queue_env["LOCALAPPDATA"] if os.name == "nt" else queue_env["XDG_DATA_HOME"]
            ) / "slopconductor"
            queue_token_file = queue_data / "credentials" / "local-api-token"
            queue_cli = [str(cli_binary), "--daemon", queue_endpoint,
                         "--token-file", str(queue_token_file), "--json"]
            for provider, key in (("opencode-go", "queue-go-key"),
                                  ("opencode-zen", "queue-zen-key")):
                subprocess.run(
                    queue_cli + ["provider", "set-key", provider], input=key + "\n",
                    env=queue_env, text=True, capture_output=True, check=True, timeout=20,
                )

            def queued_chat(model, identity):
                output = subprocess.run(
                    queue_cli + ["chat", "--model", model, "--prompt", "reply ok",
                                 "--detach", "--command-id", identity],
                    env=queue_env, text=True, capture_output=True, check=True, timeout=20,
                ).stdout
                frames = [json.loads(line) for line in output.splitlines()]
                return next(frame["receipt"]["turn_id"] for frame in frames
                            if frame["type"] == "receipt")

            active = queued_chat("opencode-go/glm-5.3-flash", "queue-active-go")
            assert queue_fixture.first_started.wait(10)
            queued = queued_chat("opencode-zen/glm-5.3", "queue-pending-zen")
            queued_record = request(queue_endpoint, queue_token_file.read_text().strip(),
                                    path=f"/v1/turns/{queued}")[1]
            assert queued_record["status"] == "queued"
            queue_daemon.kill()
            queue_daemon.wait(timeout=5)
            queue_log.close()
            queue_daemon = queue_log = None
            queue_fixture.release_first.set()
            queue_daemon, queue_endpoint, queue_log = start_daemon(
                daemon_binary, queue_root, env=queue_env
            )
            queue_cli[2] = queue_endpoint
            restarted_queued = wait_turn(
                queue_endpoint, queue_token_file.read_text().strip(), queued, {"completed"}
            )
            assert restarted_queued["status"] == "completed"
            assert queue_fixture.requests[-1][1]["model"] == "glm-5.3"
            assert queue_fixture.requests[-1][0] == "Bearer queue-zen-key"
            assert active != queued
        finally:
            queue_fixture.release_first.set()
            if queue_daemon is not None:
                stop_daemon(queue_daemon, queue_log)
            queue_fixture.close()

        for path in data_dir.glob("state.sqlite3*"):
            assert all(key.encode() not in path.read_bytes() for key in keys)
        assert all(key not in (root / "daemon.log").read_text() for key in keys)
        assert not (root / "home" / ".slop").exists()
    finally:
        fixture.release_first.set()
        if daemon is not None:
            stop_daemon(daemon, log)
        fixture.close()
