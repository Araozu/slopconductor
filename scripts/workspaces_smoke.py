"""Real Git, daemon, and CLI checks with offline provider traffic."""
import http.server
import json
from pathlib import Path
import subprocess
import threading
import time

from chat_smoke import isolated_env, request, run, start_daemon, stop_daemon, wait_turn


class WorkspaceFixture:
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


def check_workspaces(daemon_binary, cli_binary, root):
    fixture = WorkspaceFixture()
    daemon = log = None
    try:
        env = isolated_env(root, fixture)
        data = root / "data"
        repo = root / "repo with spaces"
        repo.mkdir()

        def git(*args, cwd=repo):
            return subprocess.run(["git", "-c", "user.name=Smoke", "-c", "user.email=smoke@example.invalid", *args], cwd=cwd, env=env, check=True, capture_output=True, text=True, timeout=15).stdout

        git("init")
        (repo / "file.txt").write_text("before\n", encoding="utf-8")
        (repo / ".gitignore").write_text("ignored.txt\n", encoding="utf-8")
        git("add", ".")
        git("commit", "-m", "base")
        base = git("rev-parse", "HEAD").strip()
        (repo / "file.txt").write_text("local changes\n", encoding="utf-8")
        hook = repo / ".git/hooks/post-checkout"
        hook.write_text("#!/bin/sh\nprintf unexpected > hook-ran\n", encoding="utf-8")
        hook.chmod(0o755)
        daemon, endpoint, log = start_daemon(daemon_binary, root / "first", data, env)
        token_path = data / "credentials/local-api-token"
        token = token_path.read_text(encoding="ascii").strip()

        def cli(*args):
            result = run([str(cli_binary), "--daemon", endpoint, "--token-file", str(token_path), "--json", *args], env=env)
            frames = [json.loads(line) for line in result.stdout.splitlines()]
            last = frames[-1]
            return last["receipt"] if last.get("type") == "receipt" else last

        def api(method="GET", path="/v1/workspaces", payload=None, expected=200):
            status, body = request(endpoint, token, method, payload, path)
            assert status == expected, (method, path, status, body)
            return body

        def state(w, statuses):
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                record = api(path=f"/v1/workspaces/{w['id']}")
                if record["status"] in statuses:
                    return record
                time.sleep(0.03)
            raise RuntimeError(f"workspace did not reach {statuses}: {record}")

        default = api()["items"]
        assert len(default) == 1 and default[0]["id"] == "default" and default[0]["is_default"]
        assert default[0]["path"] is None and default[0]["name"] == "No workspace"
        api("POST", "/v1/workspaces/default/remove", {"command_id": "remove-default"}, 409)
        api("POST", "/v1/workspaces/default/worktrees", {"command_id": "default-tree"}, 400)
        # The daemon requires explicit membership even for directory-less threads.
        status, _ = request(endpoint, token, "POST", {"command_id": "missing-workspace", "provider": "opencode-go", "model": "glm-5.3-flash"}, "/v1/sessions", workspace_defaults=False)
        assert status == 400
        opened = cli("workspace", "open", str(repo), "--command-id", "open-repo")
        assert Path(opened["path"]).samefile(repo) and not opened["managed_worktree"]
        assert opened["base_commit"] is None and opened["source_workspace_id"] is None
        assert cli("workspace", "open", str(repo), "--command-id", "open-repo") == opened
        assert cli("workspace", "open", str(repo), "--command-id", "open-again")["id"] == opened["id"]
        assert len(git("worktree", "list", "--porcelain").split("worktree ")) == 2
        assert not (data / "workspaces").exists()
        assert (repo / "file.txt").read_text() == "local changes\n"
        assert "+local changes" in cli("workspace", "diff", opened["id"])["patch"]
        api("POST", "/v1/workspaces", {"command_id": "open-repo", "path": str(root)}, 409)
        # Merely creating/selecting sessions, including two in the same directory,
        # must not allocate worktrees or change workspace ownership.
        for suffix in ["one", "two"]:
            receipt = api("POST", "/v1/sessions", {"command_id": f"session-{suffix}", "provider": "opencode-go", "model": "glm-5.3-flash", "workspace": {"workspace_id": opened["id"], "allowed_tools": ["read"]}}, 202)
            session = api(path=f"/v1/sessions/{receipt['session_id']}")
            assert session["workspace_id"] == opened["id"] and session["execution"]["root"] == opened["path"]
        assert len(api()["items"]) == 2 and not (data / "workspaces").exists()
        # Worktree creation is an explicit operation independent of any session.
        create = {"command_id": "tree-alpha", "base_ref": "HEAD", "name": "Alpha"}
        alpha_receipt = api("POST", f"/v1/workspaces/{opened['id']}/worktrees", create, 202)
        assert alpha_receipt["status"] == "allocating" and alpha_receipt["base_commit"] == base
        alpha = state(alpha_receipt, {"ready"})
        assert alpha["managed_worktree"] and alpha["source_workspace_id"] == opened["id"]
        assert (Path(alpha["path"]) / "file.txt").read_text() == "before\n"
        (repo / "file.txt").write_text("source moved\n", encoding="utf-8")
        git("add", "file.txt")
        git("commit", "-m", "source moved")
        assert api("POST", f"/v1/workspaces/{opened['id']}/worktrees", create, 202) == alpha_receipt
        assert api(path=f"/v1/workspaces/{alpha['id']}")["base_commit"] == base
        beta = state(cli("workspace", "worktree", opened["id"], "--base", base, "--name", "Beta", "--command-id", "tree-beta"), {"ready"})
        assert cli("workspace", "open", beta["path"], "--command-id", "open-created")["id"] == beta["id"]
        assert not (repo / "hook-ran").exists()
        api("POST", f"/v1/workspaces/{opened['id']}/worktrees", {"command_id": "invalid-base", "base_ref": "--help"}, 400)
        # Separate explicitly chosen workspaces can execute simultaneously.
        receipts = []
        for workspace, prompt in [(alpha, "alpha"), (beta, "beta")]:
            receipts.append(cli("chat", "--workspace-id", workspace["id"], "--tool", "read", "--tool", "edit", "--prompt", prompt, "--detach", "--command-id", f"job-{prompt}"))
        assert fixture.both_started.wait(10), fixture.initial
        api("POST", f"/v1/workspaces/{alpha['id']}/remove", {"command_id": "remove-active"}, 409)
        api(path=f"/v1/workspaces/{alpha['id']}/diff", expected=409)
        fixture.release.set()
        for receipt in receipts:
            wait_turn(endpoint, token, receipt["turn_id"], {"completed"})
        for workspace, prompt in [(alpha, "alpha"), (beta, "beta")]:
            assert (Path(workspace["path"]) / "file.txt").read_text() == f"{prompt}\n"
            assert f"+{prompt}" in cli("workspace", "diff", workspace["id"])["patch"]
        assert (repo / "file.txt").read_text() == "source moved\n"
        # Conservative cleanup retains dirty worktrees and recorded commits.
        cli("workspace", "remove", alpha["id"], "--command-id", "dirty-cleanup")
        dirty = state(alpha, {"ready"})
        assert dirty["error_code"] == "workspace_dirty"
        (Path(alpha["path"]) / "file.txt").write_text("before\n", encoding="utf-8")
        for filename in ["untracked.txt", "ignored.txt"]:
            file = Path(alpha["path"]) / filename
            file.write_text("retain", encoding="utf-8")
            cli("workspace", "remove", alpha["id"], "--command-id", f"cleanup-{filename}")
            assert state(alpha, {"ready"})["error_code"] == "workspace_dirty"
            file.unlink()
        git("-c", "core.hooksPath=", "commit", "--allow-empty", "-m", "retain commit", cwd=Path(alpha["path"]))
        cli("workspace", "remove", alpha["id"], "--command-id", "committed-cleanup")
        assert state(alpha, {"ready"})["error_code"] == "workspace_has_commits"
        git("reset", "--hard", base, cwd=Path(alpha["path"]))
        cli("workspace", "remove", alpha["id"], "--command-id", "clean-cleanup")
        assert state(alpha, {"removed"})["status"] == "removed"
        assert not Path(alpha["path"]).exists()
        assert cli("workspace", "rename", beta["id"], "Renamed", "--command-id", "rename-beta")["name"] == "Renamed"
        # All workspaces use the same list and record surface, with authentication/paging.
        page = cli("workspace", "list", "--limit", "1")
        assert page["items"][0]["is_default"] and page["next_after"]
        assert api(path=f"/v1/workspaces?after={page['next_after']}&limit=1")["items"][0]["id"] == opened["id"]
        for path in ["/v1/workspaces", f"/v1/workspaces/{beta['id']}", f"/v1/workspaces/{beta['id']}/events", f"/v1/workspaces/{beta['id']}/diff"]:
            status, _ = request(endpoint, "wrong", path=path)
            assert status == 401
        events = cli("workspace", "events", beta["id"])["items"]
        assert {e["kind"] for e in events} >= {"workspace_allocating", "workspace_ready", "workspace_renamed"}
        # Unregistering an opened directory always preserves its filesystem contents.
        cli("workspace", "remove", opened["id"], "--command-id", "close-repo")
        assert (repo / "file.txt").read_text() == "source moved\n"
        assert cli("workspace", "open", str(repo), "--command-id", "reopen-repo")["id"] == opened["id"]
        stop_daemon(daemon, log)
        daemon = log = None
        daemon, endpoint, log = start_daemon(daemon_binary, root / "restart", data, env)
        assert api(path=f"/v1/workspaces/{beta['id']}")["status"] == "ready"
        assert cli("workspace", "events", beta["id"])["items"] == events
        assert api(path="/v1/workspaces/default")["is_default"]
        assert api()["items"][0]["id"] == "default" or any(w["id"] == "default" for w in api()["items"])
        print("unified workspaces, explicit worktrees, isolation, cleanup, and restart smoke passed")
    finally:
        if daemon is not None:
            stop_daemon(daemon, log)
        fixture.close()
