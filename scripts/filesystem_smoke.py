"""Authenticated real-daemon directory workspaces and filesystem navigation."""
import tempfile
import time
from pathlib import Path
from urllib.parse import urlencode
from chat_smoke import isolated_env, request, start_daemon, stop_daemon, wait_turn
from workspaces_smoke import WorkspaceFixture


def check_filesystem(binary, root):
    fixture = WorkspaceFixture()
    daemon = log = None
    try:
        env = isolated_env(root, fixture)
        daemon, endpoint, log = start_daemon(binary, root / "daemon", root / "data", env)
        token = (root / "data/credentials/local-api-token").read_text().strip()
        def api(method, path, payload=None, expected=200):
            status, body = request(endpoint, token, method, payload, path)
            assert status == expected, (status, body)
            return body
        status, _ = request(endpoint, "bad-token", path="/v1/filesystem")
        assert status == 401
        parent = root / "folders"
        parent.mkdir()
        (parent / "file.txt").write_text("contents")
        listing = api("GET", "/v1/filesystem?" + urlencode({"path": str(parent), "limit": 1}))
        assert listing["entries"][0]["kind"] == "file"
        api("POST", "/v1/filesystem/directories", {"parent": str(parent), "name": "nested"})
        api("POST", "/v1/filesystem/directories", {"parent": str(parent), "name": "../escape"}, 400)
        payload = {"command_id": "create-folder", "parent": str(parent), "name": "plain project"}
        created = api("POST", "/v1/workspaces/create", payload, 202)
        assert api("POST", "/v1/workspaces/create", payload, 202) == created
        def ready(record):
            for _ in range(300):
                record = api("GET", f"/v1/workspaces/{record['id']}")
                if record["status"] != "allocating":
                    assert record["status"] == "ready", record
                    return record
                time.sleep(0.03)
            raise RuntimeError("workspace creation did not finish")
        project = ready(created)
        assert project["git_common_dir"] is None
        assert not (Path(project["path"]) / ".git").exists()
        again = api("POST", "/v1/workspaces", {"command_id": "register-folder", "path": project["path"]}, 202)
        assert again == project
        other = api("POST", "/v1/workspaces", {"command_id": "register-other", "path": str(parent / "nested")}, 202)
        assert other["id"] != project["id"]
        selection = {"workspace_id": project["id"], "allowed_tools": ["read", "write"]}
        session_payload = {"command_id": "folder-session", "provider": "opencode-go", "model": "glm-5.3-flash", "workspace": selection}
        receipt = api("POST", "/v1/sessions", session_payload, 202)
        session = api("GET", f"/v1/sessions/{receipt['session_id']}")
        assert session["execution"]["root"] == project["path"]
        assert session["workspace_id"] == project["id"]
        assert api("POST", "/v1/sessions", session_payload, 202) == receipt
        task = api("POST", "/v1/tasks", {"command_id": "folder-task", "spec": {"title": "Folder task", "prompt": "hello", "model": "opencode-go/glm-5.3-flash", "settings": {}, "workspace": selection}}, 202)
        task_session = api("GET", f"/v1/sessions/{task['session_id']}")
        assert task_session["execution"]["root"] == project["path"]
        assert task_session["workspace_id"] == project["id"]
        assert wait_turn(endpoint, token, task["turn_id"], {"completed", "failed"})["status"] == "completed"
        git = ready(api("POST", "/v1/workspaces/create", {"command_id": "create-git", "parent": str(parent), "name": "git project", "git": True}, 202))
        assert git["git_common_dir"]
        stop_daemon(daemon, log)
        daemon = log = None
        daemon, endpoint, log = start_daemon(binary, root / "restart", root / "data", env)
        assert api("GET", f"/v1/workspaces/{project['id']}") == project
        print("filesystem and directory workspace smoke passed")
    finally:
        if daemon is not None:
            stop_daemon(daemon, log)
        fixture.close()


if __name__ == "__main__":
    with tempfile.TemporaryDirectory(prefix="hive-filesystem-") as directory:
        check_filesystem(Path(__file__).resolve().parents[1] / "target/debug/hived", Path(directory))
