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

## Codex provider validation, 2026-10-11

The current Codex work was checked locally on Linux with:

```sh
cargo build -p slop-cli --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 scripts/smoke.py
```

The independent CLI build, formatting, Clippy, 182 offline workspace tests, and
complete daemon/CLI smoke suite passed. Thirteen credentialed live tests were
ignored, including the new two-inference Codex tool/result round trip. These
checks use synthetic credentials and local HTTP fixtures.

Codex coverage includes both billing paths, model-specific effort validation,
subscription `additional_tools`, four coding tools, private reasoning/phase and
refusal replay, malformed/contradictory/incomplete output, stream cancellation,
native child creation/waits, managed worktree isolation, account-scoped discovery,
separate returning/new-account login callbacks, account selection/restart,
key removal/stale-environment suppression, and safe credential diagnostics.
Auth fixtures verify token rotation, confirmed/unconfirmed revocation, retained
registration identity, and local sign-out persistence recovery without replaying
remote revocation.

The Windows CI result above predates this Codex work; the expanded checks have
not been rerun on native Windows here. Live login, entitlement, discovery and
inference against OpenAI remain unverified without a valid credential. Release
capacity validation remains outstanding.

## Rust ownership and memory audit, 2026-10-13

The audit patch, rebased onto `bf7487f` to include the concurrent chat-history
changes, passed locally on Linux x86_64 with Rust 1.99.0:

```sh
cargo build -p slop-cli --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 scripts/smoke.py
cargo build --release -p slop-daemon -p slop-cli --locked
python3 scripts/memory_audit.py --runs 3
```

All 203 offline workspace tests and the full real-binary smoke suite passed.
Thirteen credentialed live tests remained ignored. Coverage includes context
compaction/recovery, provider switching and pinned connections, cancellation,
signed reasoning and streaming bounds, tool admission, durable history,
forks/edits/suffix deletion, projects, matrices, budgets, and child waits.

The [memory audit](memory-audit.md) records the paired release RSS comparison:
59.20 to 40.53 MiB median with four active 2 MiB histories, a 31.5% reduction.
The combined build repeated at 41.20 MiB active and 13.25 MiB baseline RSS, with
seven threads throughout. This validates the specific offline workload; broader
capacity, live-heap, supervised-tool, live-provider, and native Windows resource
measurements remain outstanding. These latest checks were not run on Windows.


## Scoped daemon peering, 2026-10-14

The local M3 peer slice passed on Linux x86_64 with Rust 1.99.0:

```sh
cargo build -p slop-cli --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 scripts/smoke.py
cargo test -p slop-client --locked
cargo test -p slop-daemon api::openapi --locked
```

The full offline workspace run passed 237 tests; thirteen credentialed live tests
were ignored. The complete real-binary smoke suite passed, including the new
[single-hop peer workflow](daemon-peering.md). Final client tests, OpenAPI coverage,
and a separate run of the complete two-daemon peer scenario also passed after
adding the destination-enforced owner precondition.

Peer coverage includes immutable grants/registrations, token separation and
redaction, duplicate authorization rejection, HTTPS-origin validation,
read-only denial, canonical history/events, active next-boundary steering,
pause/resume/cancel, subscriber bounds, idle-stream revocation/removal, gateway
shutdown while owner execution continues, restart, identity replacement, and
lost-response recovery with one accepted message. The gateway creates no local
copy of the owner's sessions. Forwarded requests carry the expected owner ID,
which the destination checks before dispatch. OpenAPI covers 81 versioned paths.

These checks use isolated localhost services and synthetic credentials. The
current peer changes have not been run on native Windows here, and a deployed
two-machine Tailscale HTTPS/event-reconnect run remains outstanding. Live model
providers and broader release capacity remain separate open validation gates.
