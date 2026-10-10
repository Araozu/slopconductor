"""Real Git, daemon, and CLI checks with offline provider traffic."""
import http.server
import json
from pathlib import Path
import subprocess
import threading
import time

from chat_smoke import isolated_env, request, run, start_daemon, stop_daemon, wait_turn


class ProjectFixture:
    def __init__(self):
        self.lock = threading.Lock()
        self.initial = []
        self.both_started = threading.Event()
        self.release = threading.Event()
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                if self.path != "/v1/chat/completions" or self.headers.get("Authorization") != "Bearer offline-test-key":
                    self.send_error(401)
                    return
                payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                prompt_index = max(i for i, message in enumerate(payload["messages"]) if message["role"] == "user")
                prompt = payload["messages"][prompt_index]["content"]
                completed = any(message["role"] == "tool" for message in payload["messages"][prompt_index + 1:])
                if not completed and prompt in {"alpha", "beta"}:
                    with fixture.lock:
                        fixture.initial.append(prompt)
                        if len(fixture.initial) == 2:
                            fixture.both_started.set()
                    fixture.release.wait(timeout=15)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                if not completed and prompt in {"alpha", "beta"}:
                    delta = {"tool_calls": [{"index": 0, "id": f"edit-{prompt}", "type": "function", "function": {
                        "name": "edit", "arguments": json.dumps({"path": "file.txt", "oldText": "before", "newText": prompt}),
                    }}]}
                    finish = "tool_calls"
                else:
                    delta, finish = {"content": "Workspace complete."}, "stop"
                event = {"model": payload["model"], "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
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
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def check_projects(daemon_binary, cli_binary, root):
    fixture = ProjectFixture()
    daemon = log = None
    try:
        env = isolated_env(root, fixture)
        data = root / "data"
        repo = root / "repo with spaces"
        repo.mkdir()

        def git(*args, cwd=repo):
            return subprocess.run(
                ["git", "-c", "user.name=Smoke", "-c", "user.email=smoke@example.invalid", *args], cwd=cwd, env=env, check=True, capture_output=True, text=True, timeout=15,
            ).stdout

        git("init")
        (repo / "file.txt").write_text("before\n", encoding="utf-8")
        (repo / ".gitignore").write_text("ignored.txt\n", encoding="utf-8")
        git("add", ".")
        git("commit", "-m", "base")
        base = git("rev-parse", "HEAD").strip()
        hook = repo / ".git/hooks/post-checkout"
        hook.write_text("#!/bin/sh\nprintf unexpected > hook-ran\n", encoding="utf-8")
        hook.chmod(0o755)
        daemon, endpoint, log = start_daemon(daemon_binary, root / "first", data, env)
        token_path = data / "credentials/local-api-token"
        token = token_path.read_text(encoding="ascii").strip()

        def cli(*args):
            return run([str(cli_binary), "--daemon", endpoint, "--token-file", str(token_path), "--json", *args], env=env)

        def get(path):
            status, body = request(endpoint, token, path=path)
            assert status == 200, (path, status, body)
            return body

        def workspace_state(workspace_id, statuses):
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                record = get(f"/v1/workspaces/{workspace_id}")
                if record["status"] in statuses:
                    return record
                time.sleep(0.03)
            raise RuntimeError(f"workspace did not reach {statuses}: {record}")

        registration = cli("project", "register", str(repo), "--command-id", "register-repo")
        project = json.loads(registration.stdout)
        project_id = project["id"]
        assert json.loads(cli("project", "register", str(repo), "--command-id", "register-repo").stdout) == project
        assert json.loads(cli("project", "register", str(repo), "--command-id", "register-again").stdout)["id"] == project_id
        assert json.loads(cli("project", "list").stdout)["items"] == [project]
        status, error = request(endpoint, token, "POST", {"command_id": "register-repo", "path": str(root)}, "/v1/projects")
        assert status == 409 and error["code"] == "command_conflict", (status, error)

        def reserve(command, base_ref=None):
            payload = {"command_id": command, "provider": "opencode-go", "model": "glm-5.3-flash",
                       "project": {"project_id": project_id, "base_ref": base_ref, "allowed_tools": ["read", "edit"]}}
            status, receipt = request(endpoint, token, "POST", payload, "/v1/sessions")
            assert status == 202, (status, receipt)
            session = get(f"/v1/sessions/{receipt['session_id']}")
            return payload, receipt, get(f"/v1/workspaces/{session['managed_workspace_id']}")

        payload, reserved_receipt, alpha = reserve("reserve-alpha")
        assert alpha["status"] == "reserved" and alpha["base_commit"] == base
        assert not Path(alpha["path"]).exists()
        # Move HEAD after acceptance; allocation must still use the frozen base.
        (repo / "file.txt").write_text("new main head\n", encoding="utf-8")
        git("commit", "-am", "advance")
        status, retried = request(endpoint, token, "POST", payload, "/v1/sessions")
        assert status == 202 and retried == reserved_receipt
        assert get(f"/v1/workspaces/{alpha['id']}")["base_commit"] == base

        alpha_frames = [json.loads(line) for line in cli("chat", "--session", alpha["session_id"], "--prompt", "alpha", "--command-id", "job-alpha", "--detach").stdout.splitlines()]
        alpha_turn = next(frame["receipt"]["turn_id"] for frame in alpha_frames if frame["type"] == "receipt")
        beta_args = ("chat", "--project", project_id, "--base", base, "--tool", "read", "--tool", "edit", "--prompt", "beta", "--command-id", "job-beta", "--detach")
        beta_frames = [json.loads(line) for line in cli(*beta_args).stdout.splitlines()]
        beta_receipt = next(frame["receipt"] for frame in beta_frames if frame["type"] == "receipt")
        beta_session = get(f"/v1/sessions/{beta_receipt['session_id']}")
        beta = get(f"/v1/workspaces/{beta_session['managed_workspace_id']}")
        assert fixture.both_started.wait(timeout=10), "two isolated turns did not execute concurrently"
        assert alpha["path"] != beta["path"]
        assert get(f"/v1/workspaces/{alpha['id']}")["status"] == "ready"
        status, _ = request(endpoint, token, "POST", {"command_id": "remove-active"}, f"/v1/workspaces/{alpha['id']}/remove")
        assert status == 409
        status, _ = request(endpoint, token, path=f"/v1/workspaces/{alpha['id']}/diff")
        assert status == 409
        fixture.release.set()
        wait_turn(endpoint, token, alpha_turn, {"completed"})
        wait_turn(endpoint, token, beta_receipt["turn_id"], {"completed"})
        for workspace, label in [(alpha, "alpha"), (beta, "beta")]:
            assert Path(workspace["path"], "file.txt").read_text() == label + "\n"
            assert not Path(workspace["path"], "hook-ran").exists()
            diff = json.loads(cli("workspace", "diff", workspace["id"]).stdout)
            assert diff["base_commit"] == base and f"+{label}" in diff["patch"] and "-before" in diff["patch"], diff
            assert diff["head_commit"] == base
        assert (repo / "file.txt").read_text() == "new main head\n"
        assert json.loads(cli(*beta_args).stdout.splitlines()[-1])["receipt"]["turn_id"] == beta_receipt["turn_id"]
        page = json.loads(cli("project", "workspaces", project_id, "--limit", "1").stdout)
        assert len(page["items"]) == 1 and page["next_after"]
        tail = get(f"/v1/projects/{project_id}/workspaces?after={page['next_after']}&limit=1")
        assert len(tail["items"]) == 1 and not tail["next_after"]

        for path in ["/v1/projects", f"/v1/projects/{project_id}", f"/v1/projects/{project_id}/workspaces", f"/v1/projects/{project_id}/events", f"/v1/workspaces/{alpha['id']}", f"/v1/workspaces/{alpha['id']}/diff"]:
            status, _ = request(endpoint, None, path=path)
            assert status == 401, (path, status)

        # Cleanup cannot remove tracked changes, untracked/ignored files, or new commits.
        removed_receipt = json.loads(cli("workspace", "remove", alpha["id"], "--command-id", "remove-dirty").stdout)
        dirty = workspace_state(alpha["id"], {"ready"})
        assert dirty["error_code"] == "workspace_dirty" and Path(alpha["path"]).exists()
        events_before = get(f"/v1/projects/{project_id}/events")["items"]
        assert json.loads(cli("workspace", "remove", alpha["id"], "--command-id", "remove-dirty").stdout) == removed_receipt
        assert get(f"/v1/projects/{project_id}/events")["items"] == events_before
        git("-c", f"core.hooksPath={data / 'git-hooks'}", "restore", ".", cwd=alpha["path"])
        for filename in ["untracked.txt", "ignored.txt"]:
            path = Path(alpha["path"], filename)
            path.write_text("retain", encoding="utf-8")
            cli("workspace", "remove", alpha["id"], "--command-id", "remove-" + filename)
            assert workspace_state(alpha["id"], {"ready"})["error_code"] == "workspace_dirty"
            assert path.exists()
            path.unlink()
        cli("workspace", "remove", alpha["id"], "--command-id", "remove-clean")
        assert workspace_state(alpha["id"], {"removed"})["error_code"] is None
        assert not Path(alpha["path"]).exists()
        status, error = request(endpoint, token, "POST", {"command_id": "send-removed", "text": "continue"}, f"/v1/sessions/{alpha['session_id']}/messages")
        assert status == 400 and error["code"] == "workspace_unavailable"
        git("commit", "-am", "retain detached commit", cwd=beta["path"])
        cli("workspace", "remove", beta["id"], "--command-id", "remove-committed")
        assert workspace_state(beta["id"], {"ready"})["error_code"] == "workspace_has_commits"

        # Reserved workspaces consume no filesystem resources and can be removed.
        _, _, pending = reserve("reserve-unused")
        cli("workspace", "remove", pending["id"], "--command-id", "remove-unused")
        assert get(f"/v1/workspaces/{pending['id']}")["status"] == "removed"
        assert not Path(pending["path"]).exists()
        status, _ = request(endpoint, token, "POST", {"command_id": "invalid-base", "provider": "opencode-go", "model": "glm-5.3-flash", "project": {"project_id": project_id, "base_ref": "--help", "allowed_tools": ["edit"]}}, "/v1/sessions")
        assert status == 400
        status, _ = request(endpoint, token, "POST", {"command_id": "codex-worktree", "provider": "codex", "model": "gpt-5.4", "project": {"project_id": project_id, "base_ref": base, "allowed_tools": ["edit"]}}, "/v1/sessions")
        assert status == 400

        # A preexisting destination fails without deleting it or replaying Git.
        _, _, blocked = reserve("reserve-blocked")
        Path(blocked["path"]).mkdir(parents=True)
        sentinel = Path(blocked["path"], "keep.txt")
        sentinel.write_text("unowned", encoding="utf-8")
        frames = [json.loads(line) for line in cli("chat", "--session", blocked["session_id"], "--prompt", "blocked", "--detach", "--command-id", "send-blocked").stdout.splitlines()]
        blocked_turn = next(frame["receipt"]["turn_id"] for frame in frames if frame["type"] == "receipt")
        wait_turn(endpoint, token, blocked_turn, {"failed"})
        failed = get(f"/v1/workspaces/{blocked['id']}")
        assert failed["status"] == "failed" and failed["error_code"] == "workspace_path_denied"
        assert not failed["effects_unknown"] and sentinel.read_text() == "unowned"

        stop_daemon(daemon, log)
        daemon = log = None
        daemon, endpoint, log = start_daemon(daemon_binary, root / "restart", data, env)
        assert get(f"/v1/projects/{project_id}") == project
        assert get(f"/v1/workspaces/{beta['id']}")["status"] == "ready"
        assert get(f"/v1/workspaces/{blocked['id']}") == failed
        assert sentinel.read_text() == "unowned"
        diff = json.loads(cli("workspace", "diff", beta["id"]).stdout)
        assert "+beta" in diff["patch"] and diff["head_commit"] != base
        events = json.loads(cli("project", "events", project_id).stdout)["items"]
        assert {event["kind"] for event in events} >= {"project_registered", "workspace_reserved", "workspace_allocating", "workspace_ready", "workspace_failed", "workspace_cleanup_failed", "workspace_removed"}

        # Replacing a ready workspace cannot silently redirect future file tools.
        moved = Path(beta["path"]).with_name("moved-worktree")
        Path(beta["path"]).rename(moved)
        Path(beta["path"]).mkdir()
        frames = [json.loads(line) for line in cli("chat", "--session", beta["session_id"], "--prompt", "inspect replacement", "--detach", "--command-id", "replaced-root").stdout.splitlines()]
        replaced_turn = next(frame["receipt"]["turn_id"] for frame in frames if frame["type"] == "receipt")
        wait_turn(endpoint, token, replaced_turn, {"failed"})
        assert get(f"/v1/workspaces/{beta['id']}")["status"] == "failed"
        assert (moved / "file.txt").read_text() == "beta\n"
    finally:
        fixture.release.set()
        if daemon is not None:
            stop_daemon(daemon, log)
        fixture.close()
