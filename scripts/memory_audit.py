#!/usr/bin/env python3
"""Measure offline Hive capacity on Linux; use a release hived for performance data.

Fixtures use temporary storage and a local provider. JSON goes to stdout (and
--output); progress goes to stderr. No provider credentials or packages are needed.
"""

import argparse
from collections import Counter
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import http.server
import json
import math
from pathlib import Path
import platform
import shlex
import sqlite3
import sys
import tempfile
import threading
import time

from chat_smoke import request
from smoke import isolated_env, start_daemon, stop_daemon


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def wait_for(predicate, timeout, description):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.05)
    raise RuntimeError(f"Timed out waiting for {description}")


def memory(pid):
    fields = {}
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        if line.startswith(("VmRSS:", "VmHWM:", "Threads:")):
            key, value, *_ = line.split()
            fields[key.rstrip(":")] = int(value)
    return fields


def process_is_running(pid):
    try:
        return "VmRSS" in memory(pid)  # Zombies have exited and retain no RSS.
    except FileNotFoundError:
        return False


def descendants(pid):
    found, pending = set(), [pid]
    while pending:
        parent = pending.pop()
        for path in Path(f"/proc/{parent}/task").glob("*/children"):
            try:
                children = {int(value) for value in path.read_text().split()}
            except FileNotFoundError:
                continue
            pending.extend(children - found)
            found.update(children)
    return found


class Sampler:
    """Keep sampled peaks, separating daemon RSS from its supervised descendants."""

    def __init__(self, pid):
        self.pid = pid
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.peaks = Counter()
        self.error = None
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def read(self):
        daemon = memory(self.pid)
        children = []
        for pid in descendants(self.pid):
            try:
                child = memory(pid)
                if "VmRSS" in child:
                    children.append(child)
            except FileNotFoundError:
                pass
        return {
            "daemon_rss_kib": daemon["VmRSS"], "daemon_hwm_kib": daemon["VmHWM"],
            "daemon_threads": daemon["Threads"],
            "tool_rss_kib": sum(child["VmRSS"] for child in children),
            "tool_processes": len(children),
        }

    def run(self):
        try:
            while not self.stop.is_set():
                sample = self.read()
                with self.lock:
                    for key, value in sample.items():
                        self.peaks[key] = max(self.peaks[key], value)
                self.stop.wait(0.05)
        except Exception as error:
            self.error = error

    def snapshot(self, seconds):
        time.sleep(seconds)
        if self.error:
            raise RuntimeError("Resource sampler failed") from self.error
        return self.read()

    def close(self):
        self.stop.set()
        self.thread.join(timeout=5)
        if self.error:
            raise RuntimeError("Resource sampler failed") from self.error


class Fixture:
    def __init__(self, timeout, tool_command=None):
        self.condition = threading.Condition()
        self.release = threading.Event()
        self.arrivals = {}
        self.timeout = timeout
        fixture = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                payload = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                last_user = max(i for i, message in enumerate(payload["messages"]) if message["role"] == "user")
                prompt = payload["messages"][last_user]["content"]
                has_result = any(message["role"] == "tool" for message in payload["messages"][last_user + 1:])
                del payload
                if prompt.startswith("hold-"):
                    with fixture.condition:
                        require(prompt not in fixture.arrivals, f"Duplicate inference: {prompt}")
                        fixture.arrivals[prompt] = time.monotonic()
                        fixture.condition.notify_all()
                    if not fixture.release.wait(timeout=timeout):
                        self.send_error(504)
                        return
                delta, finish = {"content": "done"}, "stop"
                if prompt.startswith("tool-") and not has_result:
                    delta = {"tool_calls": [{
                        "index": 0, "id": "capacity-tool", "type": "function",
                        "function": {"name": "bash", "arguments": json.dumps({"command": tool_command(prompt)})},
                    }]}
                    finish = "tool_calls"
                event = {"choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
                         "usage": {"prompt_tokens": 100, "completion_tokens": 1}}
                body = b"data: " + json.dumps(event).encode() + b"\n\ndata: [DONE]\n\n"
                try:
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionResetError):
                    pass

            def log_message(self, *_):
                pass

        class Server(http.server.ThreadingHTTPServer):
            request_queue_size = 128

        self.server = Server(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def wait(self, count):
        with self.condition:
            require(self.condition.wait_for(lambda: len(self.arrivals) >= count, self.timeout),
                    f"Expected {count} held requests, received {len(self.arrivals)}")
            require(len(self.arrivals) == count, "Admission limit exceeded")

    def close(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


class Daemon:
    def __init__(self, binary, root, fixture, args):
        self.binary, self.root, self.args = binary, root, args
        self.data = root / "data"
        self.process = self.log = self.sampler = None
        config = root / "config.toml"
        config.write_text(
            f'provider_base_url = "http://127.0.0.1:{fixture.server.server_port}/v1"\n'
            f'execution_concurrency = {args.concurrency}\n', encoding="utf-8",
        )
        self.env = isolated_env(root, OPENCODE_GO_API_KEY="offline-capacity-fixture", SLOP_CONFIG=config)
        for key in ("OPENCODE_ZEN_API_KEY", "CODEX_API_KEY", "OPENAI_API_KEY", "SLOP_PROVIDER_BASE_URL"):
            self.env.pop(key, None)
        self.start()

    def start(self):
        self.process, self.endpoint, self.log = start_daemon(self.binary, self.root / "process", self.data, env=self.env)
        self.token = (self.data / "credentials/local-api-token").read_text().strip()
        self.sampler = Sampler(self.process.pid)

    def close(self):
        try:
            if self.sampler:
                self.sampler.close()
        finally:
            if self.process:
                stop_daemon(self.process, self.log)
            self.process = self.log = self.sampler = None

    def api(self, method, path, body=None):
        status, value = request(self.endpoint, self.token, method, body, path, workspace_defaults=False)
        require(status in (200, 202), f"{path}: HTTP {status}: {value}")
        return value

    def session(self, command, workspace=None):
        return self.api("POST", "/v1/sessions", {
            "command_id": command, "provider": "opencode-go", "model": "glm-5.3-flash",
            "settings": {"full_context": True},
            "workspace": workspace or {"workspace_id": "default", "allowed_tools": []},
        })["session_id"]

    def send(self, session, command, text):
        return self.api("POST", f"/v1/sessions/{session}/messages", {"command_id": command, "text": text})["turn_id"]

    def finish(self, turns, expected="completed"):
        pending, completed = set(turns), {}
        deadline = time.monotonic() + self.args.timeout
        while pending and time.monotonic() < deadline:
            for turn in list(pending):
                state = self.api("GET", f"/v1/turns/{turn}")["status"]
                if state in {"completed", "failed", "cancelled", "interrupted", "incomplete"}:
                    require(state == expected, f"Turn {turn}: expected {expected}, got {state}")
                    completed[turn] = time.monotonic()
                    pending.remove(turn)
            if pending:
                time.sleep(0.05)
        require(not pending, f"{len(pending)} turns did not finish within {self.args.timeout}s")
        return completed

    def sample(self):
        return self.sampler.snapshot(self.args.sample_seconds)


@contextmanager
def workload(binary, args, tool_command=None):
    fixture = Fixture(args.timeout, tool_command)
    try:
        with tempfile.TemporaryDirectory(prefix="hive-capacity-") as directory:
            daemon = Daemon(binary, Path(directory), fixture, args)
            try:
                yield daemon, fixture
            finally:
                fixture.release.set()
                daemon.close()
    finally:
        fixture.close()


def distribution(values):
    ordered = sorted(values)
    return {"count": len(ordered), "p50": ordered[math.ceil(len(ordered) * 0.5) - 1],
            "p95": ordered[math.ceil(len(ordered) * 0.95) - 1], "max": ordered[-1]}


def sessions_workload(binary, args, count):
    with workload(binary, args) as (daemon, fixture):
        baseline = daemon.sample()
        sessions = [daemon.session(f"session-{i}") for i in range(count)]
        idle = daemon.sample()
        seeds = [daemon.send(session, f"seed-{i}", "seed") for i, session in enumerate(sessions)]
        daemon.finish(seeds)
        daemon.close()
        # Seed checkpoints only while the daemon is stopped; never touch user data.
        messages = [{"role": "user" if i % 2 == 0 else "assistant",
                     "blocks": [{"id": f"history-{i}", "type": "text", "text": "x" * (args.context_kib * 1024 // 32)}],
                     "continuation": None} for i in range(32)]
        history = json.dumps(messages, separators=(",", ":"))
        with sqlite3.connect(daemon.data / "state.sqlite3") as db:
            for session, turn in zip(sessions, seeds):
                ordinal = db.execute("SELECT MAX(ordinal) FROM messages WHERE session_id=?", (session,)).fetchone()[0]
                model_request = db.execute("SELECT id FROM model_requests WHERE turn_id=?", (turn,)).fetchone()[0]
                db.execute("INSERT OR REPLACE INTO session_context VALUES(?,?,?,?)", (session, ordinal, history, model_request))
        daemon.start()
        unloaded = daemon.sample()
        accepted, latencies, turns = {}, [], []
        for i, session in enumerate(sessions):
            prompt = f"hold-{i}"
            started = time.monotonic()
            turn = daemon.send(session, f"active-{i}", prompt)
            latencies.append(time.monotonic() - started)
            accepted[prompt] = started
            turns.append(turn)
        admitted = min(count, args.concurrency)
        fixture.wait(admitted)
        held = daemon.sample()
        states = Counter(daemon.api("GET", f"/v1/turns/{turn}")["status"] for turn in turns)
        require(states == Counter({"running": admitted, "queued": count - admitted}), f"Unexpected admission states: {states}")
        fixture.wait(admitted)
        release_at = time.monotonic()
        fixture.release.set()
        finished = daemon.finish(turns)
        require(len(fixture.arrivals) == count, "Not every session reached the provider exactly once")
        return {"sessions": count, "admitted": admitted, "queued": count - admitted,
                "context_text_bytes_each": sum(len(m["blocks"][0]["text"]) for m in messages),
                "baseline": baseline, "idle": idle, "unloaded_checkpoints": unloaded, "held": held,
                "after_completion": daemon.sample(), "sampled_peaks": dict(daemon.sampler.peaks),
                "accept_seconds": distribution(latencies),
                "provider_arrival_from_submission_seconds": distribution([at - accepted[prompt] for prompt, at in fixture.arrivals.items()]),
                "drain_after_release_seconds": max(finished.values()) - release_at}


def batch_workload(binary, args):
    with workload(binary, args) as (daemon, fixture):
        baseline = daemon.sample()
        cap = min(2, args.concurrency)
        spec = {"name": "capacity", "prompts": [f"hold-batch-{i}" for i in range(args.batch_size)],
                "models": ["opencode-go/glm-5.3-flash"], "settings": [{}],
                "workspace": {"workspace_id": "default", "allowed_tools": []}, "max_concurrent_runs": cap}
        preview = daemon.api("POST", "/v1/batches/preview", spec)
        started = time.monotonic()
        receipt = daemon.api("POST", "/v1/batches", {"command_id": "batch", "spec": preview["spec"]})
        accepted_seconds = time.monotonic() - started
        fixture.wait(cap)
        held = daemon.sample()
        states = daemon.api("GET", f"/v1/batches/{receipt['batch_id']}")["statuses"]
        require(states.get("running") == cap and states.get("queued", 0) == args.batch_size - cap,
                f"Unexpected batch admission: {states}")
        fixture.wait(cap)
        release_at = time.monotonic()
        fixture.release.set()
        finished = daemon.finish([member["turn_id"] for member in receipt["members"]])
        require(len(fixture.arrivals) == args.batch_size, "Batch inference count differs from member count")
        return {"members": args.batch_size, "admitted": cap, "queued": args.batch_size - cap,
                "baseline": baseline, "held": held, "after_completion": daemon.sample(),
                "sampled_peaks": dict(daemon.sampler.peaks), "accept_seconds": accepted_seconds,
                "drain_after_release_seconds": max(finished.values()) - release_at}


def tools_workload(binary, args):
    def command(prompt):
        script = (
            "from pathlib import Path\nimport time\n"
            f"data = bytearray({args.tool_memory_mib} * 1024 * 1024)\n"
            "for i in range(0, len(data), 4096): data[i] = 1\n"
            f"Path('{prompt}.ready').touch()\n"
            f"deadline = time.monotonic() + {args.timeout}\n"
            f"while not Path('{prompt}.release').exists():\n"
            "    if time.monotonic() >= deadline: raise TimeoutError('tool fixture release')\n"
            "    time.sleep(0.05)\n"
            "print('x' * (256 * 1024))\n"
        )
        return f"{shlex.quote(sys.executable)} -c {shlex.quote(script)}"

    with workload(binary, args, command) as (daemon, _):
        directory = daemon.root / "workspace"
        directory.mkdir()
        workspace = daemon.api("POST", "/v1/workspaces", {"command_id": "tools-workspace", "path": str(directory)})
        session = daemon.session("tools-session", {"workspace_id": workspace["id"], "allowed_tools": ["bash"],
                                                    "limits": {"shell_timeout_ms": min(300000, int(args.timeout * 1000))}})
        result = {"allocation_mib": args.tool_memory_mib, "baseline": daemon.sample()}
        for prompt in ("tool-complete", "tool-cancel"):
            turn = daemon.send(session, prompt, prompt)
            wait_for(lambda: (directory / f"{prompt}.ready").exists(), args.timeout, "tool allocation")
            result[f"{prompt}_held"] = daemon.sample()
            tool_pids = descendants(daemon.process.pid)
            require(result[f"{prompt}_held"]["tool_processes"] > 0, "Sampler did not observe the tool process")
            require(result[f"{prompt}_held"]["tool_rss_kib"] >= args.tool_memory_mib * 1024,
                    "Tool RSS does not include the touched allocation")
            if prompt == "tool-complete":
                (directory / f"{prompt}.release").touch()
                daemon.finish([turn])
                tool = daemon.api("GET", f"/v1/turns/{turn}/tools")["items"][0]
                require(tool["status"] == "completed" and tool["artifact_ids"], f"Tool output was not captured: {tool['status']}")
                output = json.loads(tool["output"])
                require(output["stdout_truncated"], "Noisy tool output was not previewed")
                result["output_preview_bytes"] = len(tool["output"].encode())
                result["output_stdout_bytes"] = output["stdout_bytes"]
            else:
                started = time.monotonic()
                daemon.api("POST", f"/v1/turns/{turn}/cancel", {"command_id": "cancel-tool"})
                daemon.finish([turn], "cancelled")
                result["cancel_seconds"] = time.monotonic() - started
            wait_for(lambda: not descendants(daemon.process.pid) and not any(process_is_running(pid) for pid in tool_pids),
                     args.timeout, "tool descendant cleanup (including reparented processes)")
            result[f"{prompt}_after"] = daemon.sample()
        result["sampled_peaks"] = dict(daemon.sampler.peaks)
        return result


def churn_workload(binary, args):
    with workload(binary, args) as (daemon, _):
        result = {"baseline": daemon.sample(), "rounds": []}
        for cycle in range(args.churn_rounds):
            started = time.monotonic()
            turns = []
            for i in range(args.churn_sessions):
                name = f"churn-{cycle}-{i}"
                session = daemon.session(name)
                turns.append(daemon.send(session, f"message-{name}", "seed"))
            daemon.finish(turns)
            result["rounds"].append({"completed_sessions_total": (cycle + 1) * args.churn_sessions,
                                     "elapsed_seconds": time.monotonic() - started, "after_completion": daemon.sample()})
        result["sampled_peaks"] = dict(daemon.sampler.peaks)
        return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--daemon", type=Path, default=Path("target/release/hived"))
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--sessions", type=int, nargs="+", default=[1, 10, 100])
    parser.add_argument("--concurrency", type=int, default=4)
    parser.add_argument("--context-kib", type=int, default=2048)
    parser.add_argument("--batch-size", type=int, default=256)
    parser.add_argument("--tool-memory-mib", type=int, default=32)
    parser.add_argument("--churn-rounds", type=int, default=5)
    parser.add_argument("--churn-sessions", type=int, default=20)
    parser.add_argument("--sample-seconds", type=float, default=0.3)
    parser.add_argument("--timeout", type=float, default=120)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("Resource sampling currently requires Linux /proc")
    for key in ("runs", "context_kib", "tool_memory_mib", "churn_rounds", "churn_sessions"):
        if getattr(args, key) < 1:
            parser.error(f"--{key.replace('_', '-')} must be positive")
    if not 1 <= args.concurrency <= 64:
        parser.error("--concurrency must be between 1 and 64 (the daemon limit)")
    if not all(1 <= count <= 1000 for count in args.sessions):
        parser.error("--sessions counts must be between 1 and 1000")
    if not min(2, args.concurrency) < args.batch_size <= 256:
        parser.error("--batch-size must exceed the batch concurrency and be at most 256")
    if not 0 < args.sample_seconds < args.timeout <= 300 or not math.isfinite(args.timeout):
        parser.error("Require 0 < --sample-seconds < --timeout <= 300")
    if args.context_kib > 2048:
        parser.error("--context-kib must be at most 2048 to stay within the fixture model context")
    binary = args.daemon.resolve()
    if not binary.is_file():
        parser.error(f"Daemon binary does not exist: {binary}")
    with binary.open("rb") as source:
        binary_hash = hashlib.file_digest(source, "sha256").hexdigest()
    report = {"schema_version": 2, "recorded_at": datetime.now(timezone.utc).isoformat(),
              "platform": platform.platform(), "python": platform.python_version(), "binary_sha256": binary_hash,
              "parameters": {key: value for key, value in vars(args).items() if key not in {"daemon", "output"}},
              "memory_units": "KiB; RSS may count shared pages in multiple processes",
              "latency_units": "seconds; completion is observed by API polling", "runs": []}
    for run in range(args.runs):
        entry = {"sessions": []}
        report["runs"].append(entry)
        for count in args.sessions:
            print(f"Run {run + 1}/{args.runs}: {count} sessions, concurrency {args.concurrency}", file=sys.stderr, flush=True)
            entry["sessions"].append(sessions_workload(binary, args, count))
        for name, function in (("batch", batch_workload), ("tools", tools_workload), ("churn", churn_workload)):
            print(f"Run {run + 1}/{args.runs}: {name}", file=sys.stderr, flush=True)
            entry[name] = function(binary, args)
    output = json.dumps(report, indent=2) + "\n"
    if args.output:
        args.output.write_text(output, encoding="utf-8")
    print(output, end="")


if __name__ == "__main__":
    main()
