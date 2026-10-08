#!/usr/bin/env python3
"""Exercise the real daemon/client binaries with isolated temporary state."""

import http.server
import json
import os
from pathlib import Path
import re
import signal
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]


def run(command, **kwargs):
    return subprocess.run(
        command, cwd=ROOT, check=True, capture_output=True, text=True, timeout=120,
        **kwargs
    )


def check_boundaries(metadata):
    packages = {package["name"]: package for package in metadata["packages"]}

    def dependencies(name, seen=None):
        seen = set() if seen is None else seen
        for dependency in packages[name]["dependencies"]:
            child = dependency["name"]
            if child in packages and child not in seen:
                seen.add(child)
                dependencies(child, seen)
        return seen

    forbidden = {"slop-core", "slop-runtime", "slop-daemon"}
    for frontend in ("slop-cli", "slop-client"):
        violations = dependencies(frontend) & forbidden
        if violations:
            raise RuntimeError(f"{frontend} embeds execution dependencies: {violations}")

    for name in ("slop-core", "slop-protocol"):
        if dependencies(name):
            raise RuntimeError(f"{name} must be independent of other workspace crates")


def isolated_env(root, **overrides):
    env = os.environ.copy()
    env["HOME"] = str(root / "home")
    env["XDG_CONFIG_HOME"] = str(root / "xdg-config")
    env["XDG_DATA_HOME"] = str(root / "xdg-data")
    env["XDG_STATE_HOME"] = str(root / "xdg-state")
    env.pop("SLOP_CONFIG", None)
    env.pop("SLOP_DATA_DIR", None)
    env.pop("SLOP_LISTEN", None)
    env.pop("SLOP_NODE_NAME", None)
    env.pop("SLOP_TOKEN_FILE", None)
    if os.name == "nt":
        env["USERPROFILE"] = str(root / "home")
        env["LOCALAPPDATA"] = str(root / "local-app-data")
    env.update({key: str(value) for key, value in overrides.items()})
    return env


def start_daemon(binary, root, data_dir=None, extra=(), env=None):
    root.mkdir(parents=True, exist_ok=True)
    log_path = root / "daemon.log"
    command = [str(binary), "--listen", "127.0.0.1:0"]
    if data_dir is not None:
        command.extend(["--data-dir", str(data_dir)])
    command.extend(extra)
    log = log_path.open("w", encoding="utf-8")
    daemon = subprocess.Popen(
        command, cwd=ROOT, env=env or isolated_env(root),
        stdout=subprocess.DEVNULL, stderr=log
    )
    deadline = time.monotonic() + 15
    endpoint = None
    while time.monotonic() < deadline:
        text = log_path.read_text(encoding="utf-8")
        match = re.search(r"Listening on (http://127\.0\.0\.1:\d+)", text)
        if match:
            endpoint = match.group(1)
            break
        if daemon.poll() is not None:
            log.close()
            raise RuntimeError(f"daemon exited before readiness ({daemon.returncode}): {text}")
        time.sleep(0.05)
    if endpoint is None:
        daemon.kill()
        daemon.wait(timeout=5)
        log.close()
        raise RuntimeError("daemon did not publish a listening address")
    return daemon, endpoint, log


def stop_daemon(daemon, log, graceful=True):
    if daemon.poll() is None:
        if graceful:
            if os.name == "nt":
                daemon.terminate()
            else:
                daemon.send_signal(signal.SIGTERM)
        else:
            daemon.kill()
        daemon.wait(timeout=10)
    if graceful and os.name != "nt" and daemon.returncode != 0:
        log.close()
        raise RuntimeError(f"daemon did not shut down cleanly after SIGTERM: {daemon.returncode}")
    log.close()


def http_json(url, token=None):
    headers = {} if token is None else {"Authorization": f"Bearer {token}"}
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            return response.status, json.loads(response.read())
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read())


def token_for(data_dir):
    path = data_dir / "credentials" / "local-api-token"
    return path, path.read_text(encoding="ascii").strip()


def assert_node(cli, endpoint, token_path, env=None):
    result = run([
        str(cli), "--daemon", endpoint, "--token-file", str(token_path),
        "--json", "node"
    ], env=env)
    return json.loads(result.stdout)


def check_primary_lifecycle(daemon_binary, cli_binary, root):
    env = isolated_env(root)
    if os.name == "nt":
        data_dir = Path(env["LOCALAPPDATA"]) / "slopconductor" / "data"
    else:
        data_dir = root / "data"
    daemon, endpoint, log = start_daemon(
        daemon_binary, root, data_dir, extra=["--name", "smoke-node"], env=env
    )
    cli_env = isolated_env(root / "cli")
    try:
        status = run([str(cli_binary), "--daemon", endpoint, "--json", "status"], env=cli_env)
        health = json.loads(status.stdout)
        if health["service"] != "slopconductor-daemon":
            raise RuntimeError(f"wrong daemon identity: {health}")
        if health["api_version"] != 1 or health["capabilities"] != ["health", "node"]:
            raise RuntimeError(f"unexpected bootstrap capabilities: {health}")
        human = run([str(cli_binary), "--daemon", endpoint, "status"], env=cli_env)
        if "Capabilities: health, node" not in human.stdout:
            raise RuntimeError("human-readable status is missing capabilities")

        status_code, body = http_json(endpoint + "/v1/node")
        if status_code != 401 or body.get("code") != "unauthorized":
            raise RuntimeError(f"node endpoint did not reject missing auth: {status_code} {body}")
        status_code, body = http_json(endpoint + "/v1/node", "0" * 64)
        if status_code != 401 or body.get("code") != "unauthorized":
            raise RuntimeError(f"node endpoint did not reject wrong auth: {status_code} {body}")
        missing = subprocess.run(
            [str(cli_binary), "--daemon", endpoint, "node"], cwd=ROOT,
            env=cli_env, capture_output=True, text=True, timeout=15
        )
        if missing.returncode == 0 or "requires --token-file" not in missing.stderr:
            raise RuntimeError("CLI node query did not require explicit token file")

        token_path, token = token_for(data_dir)
        node = assert_node(cli_binary, endpoint, token_path, cli_env)
        if node["name"] != "smoke-node" or not node["node_id"] or not node["os"]:
            raise RuntimeError(f"unexpected authenticated node response: {node}")
        if daemon.poll() is not None:
            raise RuntimeError("CLI exit stopped the daemon")

        db_path = data_dir / "state.sqlite3"
        with sqlite3.connect(db_path) as database:
            journal = database.execute("PRAGMA journal_mode").fetchone()[0].lower()
            schema_version = database.execute("PRAGMA user_version").fetchone()[0]
            stored_id = database.execute(
                "SELECT node_id FROM node_identity WHERE singleton = 1"
            ).fetchone()[0]
        if journal != "wal" or schema_version != 1 or stored_id != node["node_id"]:
            raise RuntimeError(
                f"unexpected SQLite state: journal={journal}, schema={schema_version}, node={stored_id}"
            )

        if os.name == "posix" and hasattr(os, "getuid"):
            for path, expected in ((data_dir, 0o700), (token_path.parent, 0o700), (token_path, 0o600)):
                actual = path.stat().st_mode & 0o777
                if actual != expected:
                    raise RuntimeError(f"{path} has mode {actual:o}, expected {expected:o}")

        duplicate = subprocess.run(
            [str(daemon_binary), "--data-dir", str(data_dir), "--listen", "127.0.0.1:0"],
            cwd=ROOT, env=env,
            capture_output=True, text=True, timeout=15
        )
        if duplicate.returncode == 0 or "already owned" not in duplicate.stderr:
            raise RuntimeError("second daemon acquired an already-owned data directory")

        # Graceful shutdown and forced process death both preserve identity/token.
        stop_daemon(daemon, log)
        daemon, endpoint, log = start_daemon(daemon_binary, root, data_dir, env=env)
        if assert_node(cli_binary, endpoint, token_path, cli_env)["node_id"] != node["node_id"]:
            raise RuntimeError("node identity changed after graceful restart")
        stop_daemon(daemon, log, graceful=False)
        daemon, endpoint, log = start_daemon(daemon_binary, root, data_dir, env=env)
        restarted = assert_node(cli_binary, endpoint, token_path, cli_env)
        if restarted["node_id"] != node["node_id"]:
            raise RuntimeError("node identity changed after forced termination")
        if token_for(data_dir)[1] != token:
            raise RuntimeError("local API token changed after daemon restart")
    finally:
        stop_daemon(daemon, log)


def check_distinct_data_dirs(daemon_binary, cli_binary, root):
    processes = []
    data_dirs = []
    try:
        for index in range(2):
            instance_root = root / f"instance-{index}"
            instance_env = isolated_env(instance_root)
            if os.name == "nt":
                data = Path(instance_env["LOCALAPPDATA"]) / "slopconductor" / f"data-{index}"
            else:
                data = root / f"data-{index}"
            data_dirs.append(data)
            instance = start_daemon(
                daemon_binary, instance_root, data, env=instance_env
            )
            processes.append(instance)
        first = assert_node(cli_binary, processes[0][1], token_for(data_dirs[0])[0])
        second = assert_node(cli_binary, processes[1][1], token_for(data_dirs[1])[0])
        if first["node_id"] == second["node_id"]:
            raise RuntimeError("independent data directories share a node identity")
    finally:
        for daemon, _, log in processes:
            stop_daemon(daemon, log)


def check_xdg_and_config(daemon_binary, root):
    config_env = isolated_env(root / "config-validation")
    if os.name == "nt":
        default_env = isolated_env(root / "windows-default")
        default_data = Path(default_env["LOCALAPPDATA"]) / "slopconductor"
        daemon, _, log = start_daemon(
            daemon_binary, root / "windows-default-run", env=default_env
        )
        stop_daemon(daemon, log)
        if not (default_data / "state.sqlite3").exists():
            raise RuntimeError("Windows default data directory did not use LOCALAPPDATA")
    else:
        home = root / "profile"
        default_env = isolated_env(root / "default")
        default_env["HOME"] = str(home)
        default_env.pop("XDG_CONFIG_HOME", None)
        default_env.pop("XDG_DATA_HOME", None)
        default_data = home / ".local/share/slopconductor"
        daemon, _, log = start_daemon(daemon_binary, root / "default-run", env=default_env)
        stop_daemon(daemon, log)
        if not (default_data / "state.sqlite3").exists():
            raise RuntimeError("unset XDG variables did not use the documented home data directory")

        relative_env = isolated_env(root / "relative")
        relative_env["HOME"] = str(home)
        relative_env["XDG_CONFIG_HOME"] = "relative-config"
        relative_env["XDG_DATA_HOME"] = "relative-data"
        relative_data = home / ".local/share/slopconductor"
        daemon, _, log = start_daemon(daemon_binary, root / "relative-run", env=relative_env)
        stop_daemon(daemon, log)
        if not (relative_data / "state.sqlite3").exists():
            raise RuntimeError("relative XDG values did not fall back to the home directories")
        default_config = home / ".config/slopconductor/config.toml"
        default_config.parent.mkdir(parents=True, exist_ok=True)
        default_config.write_text("unknown_default_key = true\n", encoding="utf-8")
        relative_config = subprocess.run(
            [str(daemon_binary), "--data-dir", str(root / "relative-config-data")],
            cwd=ROOT, env=relative_env, capture_output=True, text=True, timeout=15
        )
        if relative_config.returncode == 0 or "invalid TOML" not in relative_config.stderr:
            raise RuntimeError("relative XDG_CONFIG_HOME did not fall back to the default config path")

    missing_config = root / "missing.toml"
    missing = subprocess.run(
        [str(daemon_binary), "--config", str(missing_config), "--data-dir", str(root / "missing-data")],
        cwd=ROOT, env=config_env, capture_output=True, text=True, timeout=15
    )
    if missing.returncode == 0 or "does not exist" not in missing.stderr:
        raise RuntimeError("daemon accepted an explicitly selected missing config")
    invalid_config = root / "invalid.toml"
    invalid_config.write_text("unknown_key = true\n", encoding="utf-8")
    invalid = subprocess.run(
        [str(daemon_binary), "--config", str(invalid_config), "--data-dir", str(root / "invalid-data")],
        cwd=ROOT, env=config_env, capture_output=True, text=True, timeout=15
    )
    if invalid.returncode == 0 or "invalid TOML" not in invalid.stderr:
        raise RuntimeError("daemon accepted an invalid config")


def check_api_mismatch(cli_binary, root):
    class IncompatibleDaemon(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            payload = json.dumps({
                "service": "slopconductor-daemon", "version": "test",
                "api_version": 999, "capabilities": ["health", "node"]
            }).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *_):
            pass

    server = http.server.HTTPServer(("127.0.0.1", 0), IncompatibleDaemon)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        endpoint = f"http://127.0.0.1:{server.server_port}"
        rejected = subprocess.run(
            [str(cli_binary), "--daemon", endpoint, "status"], cwd=ROOT,
            capture_output=True, text=True, timeout=15
        )
        if rejected.returncode == 0 or "incompatible daemon API" not in rejected.stderr:
            raise RuntimeError("CLI accepted an incompatible daemon API")
        token_path = root / "token"
        token_path.parent.mkdir(parents=True, exist_ok=True)
        token_path.write_text("a" * 64, encoding="ascii")
        if os.name == "posix":
            token_path.chmod(0o600)
        env = os.environ.copy()
        env["SLOP_TOKEN_FILE"] = str(token_path)
        no_secret = subprocess.run(
            [str(cli_binary), "--daemon", endpoint, "node"],
            cwd=ROOT, env=env, capture_output=True, text=True, timeout=15
        )
        if no_secret.returncode == 0 or "incompatible daemon API" not in no_secret.stderr:
            raise RuntimeError("client sent node credentials before API compatibility check")
        remote_credentials = subprocess.run(
            [str(cli_binary), "--daemon", "https://example.com", "node"],
            cwd=ROOT, env=env, capture_output=True, text=True, timeout=15
        )
        if remote_credentials.returncode == 0 or "remote node queries require an explicit --token-file" not in remote_credentials.stderr:
            raise RuntimeError("CLI silently selected local token-file credentials for a remote endpoint")
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def main():
    subprocess.run(["cargo", "build", "--workspace", "--locked"], cwd=ROOT, check=True)
    metadata = json.loads(run([
        "cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"
    ]).stdout)
    check_boundaries(metadata)
    suffix = ".exe" if os.name == "nt" else ""
    binaries = Path(metadata["target_directory"]) / "debug"
    daemon_binary = binaries / ("slopd" + suffix)
    cli_binary = binaries / ("slop" + suffix)

    with tempfile.TemporaryDirectory(prefix="slop-smoke-") as directory:
        root = Path(directory)
        check_primary_lifecycle(daemon_binary, cli_binary, root / "lifecycle")
        check_distinct_data_dirs(daemon_binary, cli_binary, root / "distinct")
        check_xdg_and_config(daemon_binary, root / "paths")
        check_api_mismatch(cli_binary, root / "incompatible")

        remote = subprocess.run(
            [str(daemon_binary), "--listen", "0.0.0.0:0", "--data-dir", str(root / "remote-data")],
            cwd=ROOT, env=isolated_env(root / "remote-env"),
            capture_output=True, text=True, timeout=15
        )
        if remote.returncode == 0 or "only supports loopback" not in remote.stderr:
            raise RuntimeError("bootstrap daemon accepted a non-loopback listener")

    print("Smoke check passed: client boundaries, auth, compatibility, stable identity, "
          "startup ownership, isolated paths, SQLite settings, and loopback restriction.")


if __name__ == "__main__":
    main()
