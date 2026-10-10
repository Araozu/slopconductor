# Implementation plan

## Current implementation

The workspace has six crates. `slopd` serves anonymous loopback health and an
authenticated node and text-chat APIs. `slop` calls these through `slop-client`, checks
identity/version, and renders human or JSON output. The daemon implements
XDG-aware startup config, exclusive directory ownership, SQLite schema/node
identity, private local bearer credentials, and bounded shutdown. The protocol
includes durable session/message receipts, paginated history/events, models,
and turn inspection/cancellation.

`slop-core` contains domain identities (including the static provider/model
identity used by the runtime registry) and `slop-runtime` contains the provider
registry with OpenCode Go, Zen, and headless Codex integrations (separate model
catalogs, authenticated clients, collected/streaming inference, usage reporting).
Codex adds native ChatGPT registration, signed ID-token validation, protected
credential storage and serialized renewal; see [Codex connection](codex-connection.md).
A shared object-safe `ProviderClient` covers all three adapters' text operations
and adds structured inference for Go/Zen, with terminal validation, safe diagnostics and optional
usage. A bounded daemon-owned supervisor selects Go, Zen, or Codex per turn, serializes
sessions/workspaces, checkpoints visible output, and commits outcomes. It
dispatches authorized tools for Go/Zen; Codex turns are text-only.
SQLite stores node identity, sessions, turns, messages, commands, and events.
Authenticated provider credential routes and CLI commands now support private
XDG API-key storage, startup restoration, hot provider connection replacement,
and daemon-owned ChatGPT login. The [credential guide](provider-credentials.md)
specifies the implemented surface. Structured Go/Zen turns now persist blocks/private continuation, frozen per-turn
model/settings, model requests, file/shell invocations, artifacts, and recovery
results. Its only coding tools are `read`, `write`, `edit`, and `bash`, implemented
in Rust; workspace chats enable all four unless an explicit allowlist restricts
them. Registered projects, frozen-base managed session worktrees, lazy allocation,
separate diffs, explicit cleanup, and Git recovery are now implemented; see
[projects/workspaces](projects-workspaces.md). Durable tasks/run attempts and
matrices with preview, selective retry, and result export are implemented; see
[tasks and batches](tasks-batches.md). Bounded [native children](child-tasks.md)
are implemented; peer control remains planned.

## Suggested module growth

Add modules as behavior arrives, rather than empty trees for every hypothetical
feature:

```text
crates/slop-core/src/
  identity.rs             # node/session/task/run IDs and validated values
  session.rs              # conversation/domain revisions
  task.rs                 # task/run transitions and retry classification
  budget.rs               # enforceable limits and recorded consumption
  ownership.rs            # owner and later transfer transitions

crates/slop-protocol/src/
  version.rs
  commands.rs
  queries.rs
  events.rs
  errors.rs
  capabilities.rs

crates/slop-runtime/src/
  supervisor.rs           # serial run loop and control inbox
  scheduler.rs            # admission, fairness, resource reservations
  context.rs
  providers/mod.rs        # provider boundary and concrete integrations
  tools/mod.rs            # dispatch, policy, and supervision
  persistence.rs          # runtime-facing repository interfaces
  recovery.rs

crates/slop-daemon/src/
  main.rs
  config.rs
  service.rs
  api/mod.rs              # handlers map wire DTOs to service operations
  storage/mod.rs          # initial concrete SQLite repository
  peers.rs                # M3 onward

crates/slop-client/src/
  lib.rs
  commands.rs
  events.rs
  projections.rs          # only genuinely shared client interpretation

crates/slop-cli/src/      # implemented CLI organization
  main.rs                 # process entry point and exit status
  args.rs                 # command grammar and global options
  commands/mod.rs         # dispatch and shared command context
  commands/discovery.rs   # status, node identity, and model discovery
  commands/chat.rs        # one-shot/interactive chat and session creation
  commands/session.rs     # session queries, message submission, and following
  commands/turn.rs        # turn inspection and explicit cancellation
  connection.rs           # endpoint and local API token selection
  input.rs                # bounded argument/file/stdin prompts
  history.rs              # paginated history and canonical message lookup
  follow.rs               # event cursors, reconnects, and detach behavior
  mutation.rs             # command IDs and uncertain-delivery diagnostics
  output.rs               # human/JSON selection and streaming rendering
```

The CLI layout above is implemented; the other layouts are proposals, not lists
of existing files. Introduce a separate store/provider crate when build isolation
or reuse justifies the boundary.

To add a CLI command group, define its grammar in `args.rs`, add a handler module
under `commands/`, and register it in `commands/mod.rs`. Handlers use the shared
connection/output context and typed `slop-client` operations. Add missing API
operations to the protocol/client and their daemon implementation before exposing
them as commands. Keep prompt reading, command identity, event following, and
stream rendering in their existing modules rather than copying those concerns
into new handlers. No speculative commands or execution dependencies are needed
to extend this structure.

## Step 1: identity, configuration, and exclusive startup

**Implemented startup foundation:** see [daemon startup](daemon-startup-plan.md)
for actual paths, configuration, authentication, and the accepted recovery policy.
Project registration and runtime provider credentials now have their own public
operations. Remote pairing remains a later extension rather than a startup
configuration placeholder.

Platform config/data paths, explicit overrides, opaque node IDs, display names,
and OS-backed exclusivity are implemented. Physical-machine grouping remains
display metadata for future work.

The bounded startup config currently controls listener, name, database queue/busy
limits, and shutdown deadline. Provider/account handles and registered projects
will be added with their owning behavior; startup does not scan the home directory
or parse repositories.

Local bearer authentication and private credential publication are implemented.
Pairing and execution authorization remain necessary before privileged tools are
exposed. Endpoint and token-file selection are independent in the native client.

Acceptance: restarts preserve identity, a second daemon cannot write the same
data directory, and configuration errors are actionable without revealing secrets.

## Step 2: domain state and durable repository

**Implemented local execution subset:** schema 6 stores sessions, turns, ordered messages,
canonical command payloads/receipts, session/project events, structured requests
and tools, artifacts, steering, and managed workspaces through the bounded
database worker, with tasks/run attempts, requested/effective inputs, task events,
and frozen batches. Fresh attempt/session ownership is documented in
[tasks and batches](tasks-batches.md).

Implement session/task/run IDs and transition functions in `slop-core`. Keep
errors and invariants domain-specific. Decide run creation/admission semantics
and implement one-primary-task-per-session initially.

Extend the existing SQLite schema/migrations and bounded database worker with
session repository operations. The selected bundled `rusqlite` binding preserves
native Windows packaging and has backup support; add backup behavior when needed.
Do not add a full ORM merely to create a few tables.

Implement command deduplication by identity/payload hash and transactional
state/event commits. Add paginated snapshots/history, artifact registration, and
per-session event sequences.

Acceptance: command retry after a dropped response returns the same created
entities; a restart reconstructs committed state; a conflicting payload cannot
reuse a command ID.

## Step 3: public API and native client expansion

**Implemented text subset:** authenticated sessions/messages/history/events,
turn inspection/cancellation, model metadata, the native client, and consumer
chat CLI. [Text chat](text-chat.md) specifies the actual routes and recovery
behavior. [Tasks and batches](tasks-batches.md) now supply task creation/state,
run controls, matrix acceptance, and results. A general command-status query
remains proposed.

Translate domain objects into protocol DTOs in the service/API layer. Keep API
handlers short: authenticate, validate, invoke an operation, render its result.

Implement session creation/list/show, task creation/state, command status, and
event replay. Add NDJSON follow with bounded client buffering and snapshot
watermarks. Expose structured errors and version/capability negotiation.

Extend `slop-client` with typed operations and replay/dedup helpers. Do not retry
mutations under a new command ID. Update schema fixtures once the real contract
exists.

Acceptance: a second client can reconstruct a session from snapshot/events, and
a lagging/disconnected client cannot stop unrelated execution.

## Step 4: direct provider integrations (implemented)

The [shared provider interface](provider-interface.md) is the adapter and
public-surface contract. Go and Zen implement structured `ProviderClient::infer`
and supply the daemon's durable chat/tool vertical slice, with blocks, private
continuation, per-turn model/settings, and artifacts. Codex supports text-only
turns. See [structured execution](structured-execution.md). Go/Zen also support
the separately authorized [native child tools](child-tasks.md). Broader account
discovery remains planned.

Implement provider capability validation and API-key account references. Add a
direct streaming request adapter and preserve response/tool-call metadata.

Build a one-turn task without tools first. Persist request intent, terminal
outcome, usage, and partial-message checkpoints. Validate selected models against
the account's available catalog or explicit configured model support.

Then add the multi-turn context loop and structured tool calls. Provider retries
are bounded and observable. A stream must produce a recognized terminal outcome
before the run treats it as complete.

Acceptance: a real supported model runs through the native daemon; CLI detach and
reconnect do not depend on an external agent program.

Provider-backed integration checks require a configured credential and explicit
test budget. Pure persistence/scheduler/protocol checks do not require one.

## Step 5: bounded tools, steering, and recovery

The basic coding-tool slice is implemented: line-based `read`, creating and
overwriting `write`, validated exact-text `edit`, and supervised `bash`, with
workspace authority, environment/output limits, deadlines, and process-tree
cancellation. See [structured execution](structured-execution.md). Additional
tool families remain separate work.

Persist invocation intent and known outcome. Discard interrupted thinking after a
crash. Mark unfinished tools failed due to daemon/process failure without
restoring or replaying them, preserving possible unknown effects. The agent
chooses its next action. Text-chat next-boundary/immediate steering, durable
pause/resume, cancellation, accepted/applied events, and restart behavior are
implemented. General run awaiting-input events remain future work.

Acceptance: use failure injection around persistence/launch/completion to verify
known results are reused and unknown edits/commands are not blindly replayed.
Check Linux and Windows process behavior with real children.

## Step 6: projects, worktrees, and matrix orchestration

**Implemented project/workspace subset:** repository registration, frozen base
commits, metadata-only reservations, detached allocation on turn admission,
bounded Git supervision, independent diffs, explicit conservative cleanup, and
no-replay Git recovery. Each workspace belongs to a session; a task attempt owns its session. The CLI/API and
offline acceptance workflow are described in
[projects/workspaces](projects-workspaces.md). Task/run controls and matrices are implemented
as described in
[tasks and batches](tasks-batches.md). The [recommended delivery order](roadmap.md#recommended-next-delivery-order)
now marks scheduling/batch controls and native children implemented locally,
with platform and capacity validation next before remote control.

Record logical repository identity, current exact base commit, local mappings,
and workspace policy. Use managed worktrees for parallel writers; default shared
directory execution needs an exclusive reservation.

Implement batch preview before submission. Validate product cardinality with
overflow protection and a configured cap, freeze inputs, and persist member
metadata. Allocate contexts/worktrees lazily on admission. Keep deterministic
combination indexes and selective retries.

The [example request](../examples/batch-request.json) now matches the implemented
`BatchSpec` accepted by `batch preview` and `batch submit`. Broader arbitrary
parameter axes remain future extensions. Aggregate budgets and native child
creation are implemented.

**Implemented orchestration foundation:** persistent admission tickets rotate
across batches, independent task trees, and ordinary sessions. Newly queued and
returning groups join at the current clock. Durable batch cancellation prevents
queued admission, uses existing active cancellation boundaries, and closes retry
admission. Aggregate operation ledgers reserve before dispatch and keep counts
across attempts, descendants, and restart. Tests cover a batch whose cap fills
global capacity and a returning group attempting to reuse old priority.

[Native child-task tools](child-tasks.md) use the public API's application
operations under a separate policy from the four coding tools. Creation and its
native tool result commit together with parent/run links and command receipts.
Explicit context/artifact previews, model/tool subsets, finite ancestor budgets,
depth/fan-out limits, cancellation policy, and wait dependencies are implemented.
Children have fresh sessions and isolated project workspaces. Waiting parents
release slots; recovery preserves known IDs/dependencies and pauses the parent
until explicit resume, without replaying unknown effects.

Acceptance: batch results are attributable to exact inputs/workspaces, bounded
concurrency remains responsive, and waiting parents cannot block all children.
Exercise restart after child acceptance, parent cancellation policy, budget
exhaustion across retries, and a full admission pool of waiting parents. Follow
with live-provider workflows and release-build capacity measurements before
declaring the M1/M2 gates complete. [Native offline validation](native-validation.md)
now passes on Linux and Windows.

## Step 7: providers, accounts, and supported subscriptions

Add a second provider so the neutral context/capability boundary is exercised.
Separate requested settings from effective settings in recorded runs.

Evaluate supported subscription auth per provider. For OpenAI, consult current
Sign in with ChatGPT plan-usage docs, define the eligible app/host identities,
and implement the documented login/token lifecycle and inference route. This is
a direct integration investigation, not a plan to invoke Codex's agent runtime.

Credential handling must be independent of the UI used to complete browser
login. A short-lived login helper can assist, but a desktop client cannot own
refresh required for unattended daemon execution.

Runtime credential setup is implemented through `provider set-key`, `provider
login codex`, and the authenticated provider API. Secrets live under the selected
XDG data directory; runtime replacements affect new admissions without restart.

Acceptance: revoked/expired credentials and shared usage limits produce
recoverable account states. Ineligible flows remain unavailable rather than
being presented as supported.

## Step 8: peer API and optional aggregate view

Use Tailscale for reachability. Implement trust/pairing, owner-aware targeting,
read/write capabilities, and per-source freshness. Add a reachable node's
aggregate API and optional command forwarding.

Only afterward add selected read-only event/artifact mirroring and an optional
VPS entrypoint. Keep raw database files local and independent.

Acceptance: direct owner execution survives gateway loss, offline delivery is
accurately pending, and cached views cannot silently execute replicated tasks.

## Independent client work

A native TUI can start after session/event operations stabilize; it shares
`slop-client`, not CLI rendering. A web client can start from generated wire
schemas and a stable authenticated API. Electron can wrap/extend the web
presentation later.

Each client track has its own tests, assets, and release process. Avoid conditional
native builds that download frontend tooling or require generated web directories
to exist.

## Verification strategy

Required local and CI checks:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 scripts/smoke.py
```

[GitHub Actions](../.github/workflows/ci.yml) runs these checks and an independent
CLI build on Ubuntu 24.04 and native Windows Server 2025, using stable Rust and
Python 3.12. Pushes, pull requests, and manual dispatches run the same offline
suite without provider credentials. Windows uses Git for Windows' Bash and
per-user app-data temporary storage, preserving the daemon's credential-location
requirements. Both platforms verify cancellation of a native process and its
descendant. [Recorded native validation](native-validation.md) passes both jobs
and the full offline suite. Live-provider validation and release-build capacity
measurements remain outstanding.

The smoke check runs real binaries, verifies native client dependency boundaries,
checks human/JSON output, daemon lifetime after client exit, API mismatch
rejection, and the loopback-only bootstrap boundary.

As behavior arrives, prioritize transition/invariant checks, SQLite restart
integration, command dedup after ambiguous delivery, event replay/gap handling,
real subprocess cancellation, deterministic matrix expansion, scheduling
fairness, and transfer fault injection. Avoid tests that only restate trivial
boilerplate.

## Performance investigation

Benchmark release binaries. Record idle daemon RSS, 1/10/100 active and idle
sessions, retained context bytes, queued batch metadata, and supervised process
memory. Compare results under bounded output and long histories.

Set numerical goals after the first realistic provider/tool loop. Control
allocation, caches, background indexes, and fan-out before assuming the language
alone meets the user's memory expectations.

## Deployment and packaging

Keep local developer execution simple. Add Linux user-service/system-service and
native Windows startup/service packaging after graceful lifecycle and data
locking exist. Release the daemon and CLI independently, with an optional combined
installer for convenience.

A public license, release signing, upgrade strategy, and broader tenancy model
remain [open questions](open-questions.md). Do not publish packages or deploy a
hosted product as part of this initial scaffold.
