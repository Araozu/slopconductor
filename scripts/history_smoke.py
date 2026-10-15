"""Exercise transcript forks, edits and deletion through the real API and CLI."""

import json
import os
import signal
import subprocess
import time

from chat_smoke import ProviderFixture, isolated_env, request, start_daemon, stop_daemon, wait_turn


def check_history(daemon_binary, cli_binary, root):
    fixture = ProviderFixture()
    for gate in fixture.gates:
        gate.set()
    daemon = None
    try:
        env = isolated_env(root, fixture)
        # Keep Git paths short for Windows path limits. This remains below the
        # private temp root.
        data = root / "data"
        daemon, endpoint, log = start_daemon(daemon_binary, root / "run", data, env)
        token_path = data / "credentials" / "local-api-token"
        token = token_path.read_text(encoding="ascii").strip()

        def api(method="GET", payload=None, path="", expected=200):
            status, body = request(endpoint, token, method, payload, path)
            if status != expected:
                raise RuntimeError(f"history {method} {path}: {status} {body}")
            return body

        def cli(*args):
            result = subprocess.run([
                str(cli_binary), "--daemon", endpoint, "--token-file", str(token_path),
                "--json", *args,
            ], env=env, capture_output=True, text=True, timeout=20, check=True)
            return [json.loads(line) for line in result.stdout.splitlines()]

        def wait_workspace_turn(receipt):
            turn = wait_turn(endpoint, token, receipt["turn_id"],
                             {"completed", "failed", "cancelled", "interrupted", "incomplete"})
            if turn["status"] != "completed":
                session = api(path=f"/v1/sessions/{receipt['session_id']}")
                workspace = api(path=f"/v1/workspaces/{session['workspace_id']}")
                raise RuntimeError(f"history turn failed: {turn}; workspace: {workspace}")

        source = api("POST", {"command_id": "history-source", "provider": "opencode-go",
                              "model": "glm-5.3-flash"}, "/v1/sessions", 202)["session_id"]
        for index, text in enumerate(("original first prompt", "original later prompt")):
            receipt = api("POST", {"command_id": f"history-send-{index}", "text": text},
                          f"/v1/sessions/{source}/messages", 202)
            wait_turn(endpoint, token, receipt["turn_id"], {"completed"})
        original = api(path=f"/v1/sessions/{source}/messages")["items"]
        fork_args = ["session", "fork", source, "--message", original[1]["id"],
                     "--command-id", "history-fork"]
        fork, = cli(*fork_args)
        if cli(*fork_args) != [fork]:
            raise RuntimeError("CLI fork retry created another conversation")
        copied = api(path=f"/v1/sessions/{fork['session_id']}/messages")["items"]
        if [m["text"] for m in copied] != [m["text"] for m in original[:2]]:
            raise RuntimeError("fork included history after the selected message")
        if fixture.count() != 2:
            raise RuntimeError("fork dispatched an inherited provider request")
        edit_args = ["session", "edit", source, "--message", original[2]["id"],
                     "--text", "edited later prompt", "--command-id", "history-edit", "--detach"]
        edited, = cli(*edit_args)
        edit = edited["receipt"]
        wait_turn(endpoint, token, edit["turn_id"], {"completed"})
        if cli(*edit_args) != [edited] or fixture.count() != 3:
            raise RuntimeError("editing a user message was not idempotent")
        prompt = fixture.payload(2)["messages"]
        if prompt[-1]["content"] != "edited later prompt" or "original later prompt" in json.dumps(prompt):
            raise RuntimeError("edit regenerated with the original suffix")
        if api(path=f"/v1/sessions/{source}/messages")["items"] != original:
            raise RuntimeError("edit mutated the original conversation")

        # Authentication, session ownership, revisions and validation are checked
        # before creating branches or changing existing history.
        status, _ = request(endpoint, None, "POST", {"command_id": "unauthorized"},
                            f"/v1/sessions/{source}/fork")
        if status != 401:
            raise RuntimeError("fork route accepted an unauthenticated mutation")
        api("PATCH", {"command_id": "wrong-source", "text": "no"},
            f"/v1/sessions/{source}/messages/{copied[0]['id']}", 404)
        api("DELETE", {"command_id": "stale-delete", "expected_revision": 0},
            f"/v1/sessions/{source}/messages/{original[2]['id']}", 409)
        api("PATCH", {"command_id": "empty-edit", "text": " "},
            f"/v1/sessions/{source}/messages/{original[2]['id']}", 400)

        delete_args = ["session", "delete-message", source, "--message", original[2]["id"],
                       "--command-id", "history-delete"]
        deleted, = cli(*delete_args)
        if cli(*delete_args) != [deleted]:
            raise RuntimeError("CLI delete retry did not return the original receipt")
        if api(path=f"/v1/sessions/{source}/messages")["items"] != original[:2]:
            raise RuntimeError("delete did not remove exactly the selected suffix")
        api(path=f"/v1/messages/{original[2]['id']}", expected=404)
        if api(path=f"/v1/sessions/{fork['session_id']}/messages")["items"] != copied:
            raise RuntimeError("deleting source history changed its fork")
        follow = api("POST", {"command_id": "history-follow", "text": "continue after deletion"},
                     f"/v1/sessions/{source}/messages", 202)
        wait_turn(endpoint, token, follow["turn_id"], {"completed"})
        if "original later prompt" in json.dumps(fixture.payload(3)["messages"]):
            raise RuntimeError("deleted suffix returned in provider context")
        if os.name == "posix":
            # Reconnect from before deletion: old events must not trap the CLI
            # on canonical message lookups that now correctly return 404.
            output_path = root / "history-follow.ndjson"
            with output_path.open("w", encoding="utf-8") as output:
                follower = subprocess.Popen([
                    str(cli_binary), "--daemon", endpoint, "--token-file", str(token_path),
                    "--json", "session", "follow", source, "--after", "0",
                ], env=env, stdout=output, stderr=subprocess.PIPE, text=True)
                try:
                    deadline = time.monotonic() + 15
                    while time.monotonic() < deadline:
                        lines = output_path.read_text(encoding="utf-8").splitlines(keepends=True)
                        frames = [json.loads(line) for line in lines if line.endswith("\n")]
                        canonical = [f for f in frames if f.get("type") == "canonical"]
                        if (any(f["turn"]["id"] == follow["turn_id"] for f in canonical)
                                and frames[-1].get("type") == "heartbeat"):
                            break
                        if follower.poll() is not None:
                            raise RuntimeError("CLI follower exited while replaying deleted history")
                        time.sleep(0.05)
                    else:
                        raise RuntimeError("CLI follower could not advance past deleted message events")
                    deleted_ids = {m["id"] for m in original[2:]}
                    if any(f.get("type") == "message_snapshot" and f["message"]["id"] in deleted_ids for f in frames):
                        raise RuntimeError("CLI follower displayed a deleted message snapshot")
                    follower.send_signal(signal.SIGINT)
                    _, diagnostics = follower.communicate(timeout=8)
                    if follower.returncode != 0:
                        raise RuntimeError(f"CLI follower could not detach: {diagnostics}")
                finally:
                    if follower.poll() is None:
                        follower.kill()
                        follower.communicate(timeout=5)

        repository = root / "history-repository"
        repository.mkdir()
        subprocess.run(["git", "init", str(repository)], env=env, check=True,
                       capture_output=True, text=True, timeout=15)
        subprocess.run(["git", "-C", str(repository), "-c", "user.name=Smoke",
                        "-c", "user.email=smoke@example.invalid", "commit", "--allow-empty",
                        "-m", "base"], env=env, check=True, capture_output=True,
                       text=True, timeout=15)
        workspace = cli("workspace", "open", str(repository), "--command-id", "history-open")[0]
        workspace_id = workspace["id"]
        selected = {"workspace_id": workspace_id, "allowed_tools": ["read"]}
        with_workspace = api("POST", {"command_id": "workspace-source", "provider": "opencode-go",
                                       "model": "glm-5.3-flash", "prompt": "workspace initial prompt",
                                       "workspace": selected}, "/v1/sessions", 202)
        wait_workspace_turn(with_workspace)
        before_fork = fixture.count()
        source_snapshot = api(path=f"/v1/sessions/{with_workspace['session_id']}")
        source_workspace = api(path=f"/v1/workspaces/{source_snapshot['workspace_id']}")
        workspace_history = api(path=f"/v1/sessions/{with_workspace['session_id']}/messages")["items"]
        workspace_fork = api("POST", {"command_id": "workspace-fork", "message_id": workspace_history[-1]["id"]},
                             f"/v1/sessions/{with_workspace['session_id']}/fork", 202)
        if fixture.count() != before_fork:
            raise RuntimeError("fork dispatched an inherited provider request")
        fork_snapshot = api(path=f"/v1/sessions/{workspace_fork['session_id']}")
        fork_workspace = api(path=f"/v1/workspaces/{fork_snapshot['workspace_id']}")
        if (source_snapshot["workspace_id"] != fork_snapshot["workspace_id"]
                or source_workspace["path"] != fork_workspace["path"]
                or fork_workspace["id"] != workspace_id):
            raise RuntimeError("history fork did not retain the selected workspace")
        continued = api("POST", {"command_id": "workspace-fork-follow", "text": "continue workspace fork"},
                        f"/v1/sessions/{workspace_fork['session_id']}/messages", 202)
        wait_workspace_turn(continued)
        before_restart = fixture.count()
        stop_daemon(daemon, log)
        daemon, endpoint, log = start_daemon(daemon_binary, root / "restart", data, env)
        if cli(*fork_args) != [fork] or cli(*edit_args) != [edited] or cli(*delete_args) != [deleted]:
            raise RuntimeError("history command receipts did not survive restart")
        if fixture.count() != before_restart:
            raise RuntimeError("restart replayed inherited or edited work")
    finally:
        if daemon is not None and daemon.poll() is None:
            stop_daemon(daemon, log)
        fixture.close()
