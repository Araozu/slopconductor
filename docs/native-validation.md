# Native offline validation

## Current cross-platform baseline, 2026-10-16

**Passed:** [CI run 38899848456](https://github.com/Araozu/slopconductor/actions/runs/38899848456)
validated commit [6c7facb](https://github.com/Araozu/slopconductor/commit/6c7facb02eeaca36742b00602c9565a8515e3491)
on Ubuntu 24.04 and native Windows Server 2025. Both jobs passed formatting,
the independent CLI build, Clippy with warnings denied, all applicable offline
workspace tests (252 on Linux, 242 on Windows), and the complete real-binary
daemon/CLI smoke suite. Thirteen credential-dependent tests remained opt-in
on each platform.

This supersedes the older Windows baseline for unified workspaces, Codex,
daemon peering, queued message delivery/editing/cancellation, and shared-workspace concurrency.
The evidence is an actual native Windows CI job, not a cross-compilation.
The earlier sections below retain their original local-check scope.

The [capacity harness](capacity-validation.md) adds Linux release resource
measurements. Native Windows resource measurements, credentialed live-provider
checks, and deployed Tailscale validation remain separate gates.

## Image input and first live-provider run, 2026-10-26

The uncommitted image-input work on top of `f5d6e88` passed locally on Linux:
formatting, Clippy with warnings denied, all 286 offline workspace tests, the
OpenAPI dump (`POST /v1/artifacts` and `ImageAttachment` present), and the
complete `python3 scripts/smoke.py` suite. Sixteen credentialed tests are now
opt-in, including three new image tests.

This is the first recorded credentialed run, using a short-lived OpenCode Go key:

```sh
set -a; . ./.env; set +a
cargo test -p hive-runtime --test opencode_go_live --locked -- --ignored
cargo test -p hive-runtime --test opencode_go_image_live --locked -- --include-ignored
```

All seven existing OpenCode Go checks passed (model list plus blocking and
streaming turns on Chat Completions, Anthropic Messages, and Responses). The
three image checks send a generated 64x64 solid-red PNG through `infer` and
require a completed turn that names the colour. All answered `red`:
`glm-5.3-flash` (Chat Completions), `claude-haiku-5-5` (Anthropic Messages),
and `muse-spark-1.3-contributor` (Responses).

An isolated real daemon (private data directory, `--listen 127.0.0.1:47331`,
environment-imported key) was then driven with `hivectl` and `curl`:

- `chat --image` with `opencode-go/muse-spark-1.3-contributor` answered `red`.
  The history showed `[image image/png <id>]`, where the id equals the file's
  SHA-256.
- A follow-up `session send --image` with a blue PNG answered `red, blue`. The
  earlier image is therefore replayed from the artifact store with history.
- Re-uploading identical bytes returned the same id (201). A mismatched
  declared type returned 400, a non-image body 400, `text/plain` 415, and a
  21 MiB body 413.
- Attaching an image for a ready text-only model (`opencode-go/beam-free`) was
  rejected with `unsupported_capability: image_input`. The CLI refused 17 images.
- Sending a stored artifact corrupted on disk failed the turn with
  `image_unavailable` before any provider request.

Two quirks were observed. `hivectl chat` creates the session before the send is
rejected, which leaves an empty session behind. Re-uploading bytes whose stored
file is corrupt returns `503 artifact_unavailable`, which fails safe but reports
a misleading message.

Not covered: OpenCode Zen and Codex live checks, peer-forwarded uploads (not
supported), and native Windows.

## Capacity tooling and Linux release workloads, 2026-10-16

The expanded harness passed its small debug-binary fixture checks and three
release runs at each of global concurrency four and 64. Every run covers
1/10/100 runnable sessions with 2 MiB checkpoints, a 256-member mostly queued
matrix, separate tool-process memory, bounded noisy output/artifacts, cancellation
cleanup, and 100 completed churn conversations. At the existing admission
maximum, 100 runnable sessions means 64 admitted and 36 queued.

The release binary was built from `6a3dfc8` in an isolated checkout. Formatting,
Clippy with warnings denied, all 251 offline workspace tests, and the complete
real-binary smoke suite passed in that checkout; thirteen live-provider tests
remained opt-in. Linux CI now exercises small capacity fixtures using a debug
binary, without enforcing performance thresholds. The
[capacity report](capacity-validation.md#recorded-linux-release-results-2026-10-16)
records binary identity, workload parameters, RSS/latency results, and limits.

## Shared-workspace concurrency, 2026-10-16

The workspace-root admission gate has been removed. Sessions selecting the
same canonical directory can execute model requests and tools concurrently
under the existing global, batch, and child limits. Turns within each session
remain ordered; agents coordinate conflicting filesystem changes. This
supersedes the shared-root serialization recorded for 2026-10-15 below.

Local Linux validation passed formatting, Clippy with warnings denied, all 251
offline workspace tests, and the complete `python3 scripts/smoke.py` suite.
Thirteen credential-dependent live tests remained opt-in. Native Windows was
not rerun for this change. The storage regression verifies overlapping shared-workspace turns,
ordered follow-ups, and frozen per-turn settings. The updated real-binary smoke
check holds two provider requests from one workspace concurrently, verifies
the batch cap still queues the other six members, and exercises spare global
capacity while the batch is at its cap.

## Unified workspaces, 2026-10-15

The unified workspace implementation passed the required checks locally on Linux:
formatting, Clippy with warnings denied, all 240 non-live workspace tests, and
`python3 scripts/smoke.py`. Thirteen credential-dependent live tests remained
opt-in. The generated OpenAPI document was inspected locally and contains 81
paths and 99 schemas; workspace selection is required for sessions and job/batch
specs, and the former project routes and schemas are absent. Generated JSON is
not checked in.

The real-binary suite covers the permanent directory-less default workspace,
opening Git and ordinary directories in place, workspace reuse across threads,
explicit frozen-base worktree creation, isolated edits, shared-root serialization,
conservative cleanup, persistence, forks, retries, batches, children, and provider
fixtures. Storage tests also verify directory-creation receipt replay, capacity
reservation before filesystem changes, migration from schema 21, and no replay
of interrupted filesystem operations. This local run covers Linux and uses the
same required checks as the native CI matrix.

## Earlier cross-platform baseline, 2026-10-10

**Passed, 2026-10-10:** [CI run 38089622582](https://github.com/Araozu/slopconductor/actions/runs/38089622582)
validated commit [e3ffff7](https://github.com/Araozu/slopconductor/commit/e3ffff7456f1549fb5adf9f3f6cdaf87162dfb64)
on Ubuntu 24.04 and native Windows Server 2025, using stable Rust and Python 3.12.
The Windows job used Rust 1.99.0 (MSVC) and Git 2.55.0.windows.5.

Both jobs passed the independent CLI build, formatting, Clippy with warnings
denied, workspace tests, and the complete real-binary daemon/CLI smoke suite:

```sh
cargo build -p hive-cli --locked
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
[managed workspace path constraints](workspaces.md#supervision-and-recovery).

Live-provider tests remain opt-in and were not run. Release-build capacity
measurements remain outstanding, and Windows service packaging remains planned.
M1/M2 are still partial.

## Codex provider validation, 2026-10-11

The current Codex work was checked locally on Linux with:

```sh
cargo build -p hive-cli --locked
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
cargo build -p hive-cli --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 scripts/smoke.py
cargo build --release -p hive-daemon -p hive-cli --locked
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
cargo build -p hive-cli --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 scripts/smoke.py
cargo test -p hive-client --locked
cargo test -p hive-daemon api::openapi --locked
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
