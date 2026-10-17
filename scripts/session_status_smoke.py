"""Real API check: successful unreviewed results are ready, not just completed."""

from chat_smoke import (
    ProviderFixture, isolated_env, request, start_daemon, stop_daemon, wait_turn,
)


def check_session_status(daemon_binary, root):
    fixture = ProviderFixture()
    for gate in fixture.gates:
        gate.set()
    daemon = None
    try:
        env = isolated_env(root, fixture)
        data = root / "data"
        daemon, endpoint, log = start_daemon(daemon_binary, root / "run", data, env)
        token = (data / "credentials" / "local-api-token").read_text(encoding="ascii").strip()

        def api(method="GET", payload=None, path="", expected=200):
            status, body = request(endpoint, token, method, payload, path)
            if status != expected:
                raise RuntimeError(f"session status {method} {path}: {status} {body}")
            return body

        receipt = api("POST", {
            "command_id": "ready-http-create", "provider": "opencode-go",
            "model": "glm-5.3-flash", "prompt": "Finish this task",
        }, "/v1/sessions", 202)
        path = f"/v1/sessions/{receipt['session_id']}"
        wait_turn(endpoint, token, receipt["turn_id"], {"completed"})
        ready = api(path=path)
        assert "state" not in ready
        assert ready["status"] == "ready"
        assert api(path="/v1/sessions")["items"][0]["status"] == "ready"
        assert api(path=path)["status"] == "ready", "GET must not acknowledge a result"
        stop_daemon(daemon, log)
        daemon = None
        daemon, endpoint, log = start_daemon(daemon_binary, root / "restart", data, env)
        ready = api(path=path)
        assert ready["status"] == "ready"
        api("POST", {
            "command_id": "ready-http-review", "read_sequence": ready["last_event_sequence"],
        }, path + "/inbox", 202)
        reviewed = api(path=path)
        assert reviewed["status"] == "completed"
        assert reviewed["last_user_activity_at"] == ready["last_user_activity_at"]
        spec = api(path="/api-docs/openapi.json")
        assert "SessionStatus" in spec["components"]["schemas"]
        assert "SessionState" not in spec["components"]["schemas"]
        assert "/v1/sessions/{session_id}/state" not in spec["paths"]
        # Organization events after the completion do not make the result ready again.
        api("POST", {"command_id": "ready-http-star", "starred": True}, path + "/inbox", 202)
        assert api(path=path)["status"] == "completed"
    finally:
        if daemon is not None:
            stop_daemon(daemon, log)
        fixture.close()
