"""Run a CI check and annotate its failure without changing its exit status."""

from collections import deque
import subprocess
import sys


def main():
    if len(sys.argv) < 2:
        raise SystemExit("usage: ci_check.py COMMAND [ARG ...]")
    tail = deque(maxlen=60)
    with subprocess.Popen(
        sys.argv[1:], stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, encoding="utf-8", errors="replace",
    ) as process:
        for line in process.stdout:
            print(line, end="", flush=True)
            tail.append(line)
        status = process.wait()
    if status:
        message = "".join(tail)[-12000:]
        message = message.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")
        print(f"::error title=Check failed::{message}", flush=True)
    raise SystemExit(status)


if __name__ == "__main__":
    main()
