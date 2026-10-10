# Native offline validation

**Passed, 2026-10-10:** [CI run 38089622582](https://github.com/Araozu/slopconductor/actions/runs/38089622582)
validated commit [e3ffff7](https://github.com/Araozu/slopconductor/commit/e3ffff7456f1549fb5adf9f3f6cdaf87162dfb64)
on Ubuntu 24.04 and native Windows Server 2025, using stable Rust and Python 3.12.
The Windows job used Rust 1.99.0 (MSVC) and Git 2.55.0.windows.5.

Both jobs passed the independent CLI build, formatting, Clippy with warnings
denied, workspace tests, and the complete real-binary daemon/CLI smoke suite:

```sh
cargo build -p slop-cli --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python scripts/smoke.py
```

The offline suite exercises startup ownership/authentication, durable chat,
steering/pause/recovery, provider credential fixtures, structured tools and
artifacts, native process-tree cancellation, managed Git worktrees and cleanup,
matrices/retries/export, fair admission, aggregate budgets, batch cancellation,
and native child tasks/waits/recovery. The Git regression test also checks
canonical Windows workspace roots and tracked file paths beyond 260 characters.

Windows fixtures use Git for Windows' Bash, short per-user app-data temporary
roots, and the daemon's Git configuration isolation. See
[managed workspace path constraints](projects-workspaces.md#supervision-and-recovery).

Live-provider tests remain opt-in and were not run. Release-build capacity
measurements remain outstanding, and Windows service packaging remains planned.
M1/M2 are still partial.
