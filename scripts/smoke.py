#!/usr/bin/env python3
"""Exercise real binaries and verify that native clients do not embed execution."""

import http.server
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import threading
import time

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
        log_path = Path(directory) / "daemon.log"
        with log_path.open("w", encoding="utf-8") as log:
            daemon = subprocess.Popen(
                [str(daemon_binary), "--listen", "127.0.0.1:0"],
                cwd=ROOT, stdout=subprocess.DEVNULL, stderr=log
            )
            try:
                deadline = time.monotonic() + 15
                endpoint = None
                while time.monotonic() < deadline:
                    text = log_path.read_text(encoding="utf-8")
                    match = re.search(r"Listening on (http://127\.0\.0\.1:\d+)", text)
                    if match:
                        endpoint = match.group(1)
                        break
                    if daemon.poll() is not None:
                        raise RuntimeError(f"daemon exited before readiness: {text}")
                    time.sleep(0.05)
                if endpoint is None:
                    raise RuntimeError("daemon did not publish a listening address")

                response = run([
                    str(cli_binary), "--daemon", endpoint, "--json", "status"
                ])
                health = json.loads(response.stdout)
                if health["service"] != "slopconductor-daemon":
                    raise RuntimeError(f"wrong daemon identity: {health}")
                if health["api_version"] != 1 or health["capabilities"] != ["health"]:
                    raise RuntimeError(f"unexpected bootstrap capabilities: {health}")

                human = run([str(cli_binary), "--daemon", endpoint, "status"])
                if "Capabilities: health" not in human.stdout:
                    raise RuntimeError("human-readable status is missing capabilities")
                if daemon.poll() is not None:
                    raise RuntimeError("CLI exit stopped the daemon")

                run([str(cli_binary), "--daemon", endpoint, "--json", "status"])
            finally:
                if daemon.poll() is None:
                    if os.name == "nt":
                        daemon.terminate()
                    else:
                        daemon.send_signal(signal.SIGINT)
                    try:
                        daemon.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        daemon.kill()
                        daemon.wait(timeout=5)

    class IncompatibleDaemon(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            payload = json.dumps({
                "service": "slopconductor-daemon",
                "version": "test",
                "api_version": 999,
                "capabilities": ["health"]
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
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)

    remote = subprocess.run(
        [str(daemon_binary), "--listen", "0.0.0.0:0"], cwd=ROOT,
        capture_output=True, text=True, timeout=15
    )
    if remote.returncode == 0 or "only supports loopback" not in remote.stderr:
        raise RuntimeError("bootstrap daemon accepted a non-loopback listener")

    print("Smoke check passed: client boundaries, JSON/human status, daemon lifetime, "
          "API mismatch, and loopback restriction.")


if __name__ == "__main__":
    main()
