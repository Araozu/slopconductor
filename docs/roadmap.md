# Roadmap

## Delivery strategy

Build a complete local daemon/API/CLI workflow before investing in visual
clients. Add orchestration and remote reachability to that base. TUI, web, and
Electron are independent client tracks once their required API operations exist.

Milestones are capability gates, not calendar promises. The order can change
with evidence, while accepted non-negotiables remain intact. Browser tools and
migration are deliberately later tracks.

## Recommended next delivery order

**Status, 2026-10-10.** Local sessions, project worktrees, durable tasks/run
attempts, and three-axis matrices are implemented. Steps 1 and 2 now have local
implementations and offline validation. Platform/capacity validation and trusted
remote control remain next; the complete milestone gates still apply.

1. **Scheduling and batch controls: implemented local slice.** Persistent
   admission tickets rotate across batches, independent task trees, and ordinary
   sessions. New/returning groups join at the current clock. Durable batch-wide
   cancellation closes the batch, and aggregate model-request/tool-call budgets
   reserve counts before dispatch and retain them across retries and restart.
   Descendants share these limits. See [tasks and batches](tasks-batches.md).

   Acceptance: an unrelated runnable task receives capacity within a documented
   bound on new admissions, including when a batch's cap equals global capacity.
   Canceling a batch prevents queued members from starting and uses existing
   cancellation boundaries for running members. Repeated commands return the
   original receipt; restart cannot reset budgets or lose cancellation intent.

2. **Native child tasks: implemented local slice.** Separately authorized
   create/inspect/wait/result/cancel operations use the same application services
   as public clients. Creation commits parent/run links and its native tool
   result atomically. Context/artifact previews, model/tool subsets, frozen
   project bases, finite budgets, depth/fan-out, and cancel propagation are
   explicit. Fresh child sessions/workspaces share the applicable limits.
   Waiting parents release execution slots; restart preserves dependencies and
   pauses the parent until explicit resume. See [child tasks](child-tasks.md).

   Acceptance: a parent delegates two isolated jobs, waits, and consumes their
   results. A pool filled with waiting parents still admits children. Parent
   reconnect/recovery reattaches to accepted child IDs without duplicate
   creation. Recovery preserves dependencies and unknown tool effects without
   automatically replaying inference or tools.

3. **Platform and capacity validation.** Linux/native Windows CI is configured
   for the required checks and real-binary workflows, including Git/Bash and
   cancellation of native children and their descendants. Record successful
   native Windows runs; configuration alone is not validation. Measure release
   builds with 1/10/100 sessions and a
   large mostly queued batch, reporting daemon and supervised-tool memory
   separately. Keep bounded live-provider tool checks explicitly opt-in.

   Acceptance: record native Windows results, reproducible capacity measurements,
   and the supported provider/tool combinations. M1/M2 remain partial until their
   remaining acceptance scenarios pass; do not infer completion from Linux
   offline checks alone.

4. **Trusted remote CLI access.** Start M3 with owner-targeted access over
   Tailscale, explicit pairing/revocation, read/write permissions, and protected
   transport. Preserve command identities and distinguish pending delivery from
   owner acceptance before adding aggregation or graphical clients.

   Acceptance: a work CLI inspects and steers a home-owned task; read-only or
   unpaired callers cannot mutate it, and local execution continues through
   client or network loss.

Branch publishing, richer result artifacts, and retention policy remain separate
M2 follow-ups. Complete the full milestone gates below before declaring M2 done.

## M0 — Repository and architectural foundation

**Status:** bootstrap implemented.

Deliverables:

- Git repository on `main`, Rust virtual workspace, lockfile, formatting/lint rules.
- Domain, protocol, runtime, client, daemon, and CLI crate boundaries.
- Loopback-only daemon health endpoint and compatible native CLI status command.
- Human/JSON output and real-binary smoke checks. [Linux/native Windows CI](../.github/workflows/ci.yml)
  runs formatting, Clippy, workspace tests, an independent CLI build, and the
  complete offline smoke suite. Native Windows results remain outstanding.
- Detailed product, requirements, architecture, protocol, subsystem, and roadmap docs.

Acceptance: build the CLI independently; run a daemon; query it from a separate
CLI process; verify CLI exit leaves the daemon alive; reject a wrong API version
and an unsupported non-loopback bootstrap listener.

M0 does not include model access, sessions, tools, storage, authentication, or
remote task control. Domain/runtime crates are reserved boundaries.

Post-M0 additions implement runtime-only providers and the first M1 startup
foundation: XDG-aware config, locked data-directory ownership, SQLite node
identity, a local API bearer credential, authenticated node inspection, and
bounded shutdown. The next slice implements durable text sessions, idempotent
message commands, paginated history/events, daemon-owned Go inference, explicit
cancellation, and a consumer chat CLI. [Structured execution](structured-execution.md)
now implements file/shell tools, explicit workspace policy/leases, artifacts,
structured streaming, frozen per-turn model/settings, and no-replay recovery.
Durable task/run lifecycle and matrix submission are now implemented; see
[tasks and batches](tasks-batches.md). Native Windows validation remains unfinished.
The first M2 project/workspace slice is now implemented: registered repositories,
frozen bases, lazy detached worktree admission, separate diffs, explicit cleanup,
and no-replay Git recovery. See [projects/workspaces](projects-workspaces.md).

## M1 — Useful local native coding agent through the CLI

**Goal:** submit a task, disconnect, and return to inspect or steer the same
persisted session on one machine.

**Partial implementation:** local text chat covers durable acceptance, CLI detach,
event reconnect, completed history after restart, command deduplication, and
model-turn cancellation. File/shell execution and recovery now have offline
real-binary checks on Linux. Live tool validation and
native Windows scenarios remain before the full M1 exit gate.

Implement durable node/session/task/run identities, local SQLite storage and
schema migrations, a command inbox, event journal, and interrupted-step records.
Add local API credential/pairing policy before privileged tool execution.

Integrate one cloud provider through API keys with direct streaming inference.
Build a native context/tool loop with bounded file operations and supervised
shell execution. Support known outcomes, interruption, and budgets. Discard
interrupted thinking. Fail unfinished tools due to daemon/process failure without
restoring or replaying them, and let the agent decide its next action while
preserving uncertainty about external effects.

Extend the CLI with session/task creation, history, following, sending instructions,
pause/resume/cancel, and artifacts. Expose the same functions through the API.

Acceptance scenarios:

1. Create a task and receive durable IDs; terminate the CLI while execution continues.
2. Reconnect from a new CLI process and recover committed history/events.
3. Retry a task-creation command after an ambiguous connection loss and get one task.
4. Queue a correction and observe distinct acceptance/application events.
5. Cancel a long-running shell command and account for its process tree/outcome.
6. Restart the daemon and recover histories without blindly repeating unknown effects.
7. Bound tool output and context memory under a noisy-command workload.
8. Run meaningful equivalents on Linux and native Windows.

Exit gate: local daemon/API/CLI is useful for real work and the ownership/recovery
model is observable. No graphical client is required.

## M2 — Worktrees, batches, children, and more provider options

**Goal:** a script reliably starts many differentiated coding jobs.

Register projects and implement managed worktree allocation, frozen base commits,
workspace reservations, diffs, artifacts, and deliberate cleanup. Add matrix
preflight/validation, deterministic expansion, admission limits, batch metadata,
selective retries, and result export.

**Partial implementation:** sessions can select registered projects and reserve
detached workspaces. Real-binary offline checks exercise concurrent independent
edits, frozen commits, diffs, cleanup refusals, and restart durability. Durable tasks/runs and deterministic
prompt/model/settings matrices now cover
preview, atomic submission, lazy admission, selective retries, and JSON Lines
results. Native children now add selected context/artifact previews, bounded
delegation, parent/run links, durable waits, cancellation propagation, and shared
budgets/fair admission. Richer result artifacts, branch publishing, automated
retention, and the remaining platform/capacity checks still precede the full M2
exit gate.

Implement native child-task creation with explicit context/tool/budget selection,
bounded fan-out/depth, fair scheduling, and parent/child links. Waiting parents
release scarce execution slots.

Add a second provider and account-specific model catalog/capability validation.
Implement provider-supported subscriptions only after verifying the documented
flow's eligibility, quotas, and direct-runtime integration. API-key execution
remains independently usable.

Acceptance scenarios:

1. Two writable jobs target one repo but produce independent worktree diffs.
2. A 2-prompt × 2-model × 2-setting batch yields eight reproducible combinations.
3. A batch larger than admission capacity stays queued without creating every
   full context or worktree in advance.
4. A parent creates/awaits a child; parent reconnect/recovery does not duplicate it.
5. Full admission capacity of waiting parents cannot deadlock all children.
6. Unsupported model settings and exhausted account limits produce explicit errors.
7. A failed combination retries independently while successful results remain.
8. Release-build measurements cover 1/10/100 sessions and a large queued batch.

Exit gate: the user's programmatic matrix/worktree workflow is dependable through
CLI/API alone.

## M3 — Trusted remote access and multi-machine visibility

**Goal:** start at home, inspect and steer from work through Tailscale.

Add stable peer registration, pairing/trust, read/write API capabilities,
HTTPS/private-network deployment, owner-aware routing, and peer availability.
A client can target an owner directly or request an aggregate view from a
reachable daemon.

Define per-owner freshness and cursors, forward command IDs unchanged, and show
pending delivery separately from owner acceptance. Keep each node locally usable
when other nodes or an optional VPS are unreachable.

Acceptance scenarios:

1. Work CLI connects to home over the tailnet and steers a home-owned task.
2. An unavailable peer appears unavailable, with accurately labeled cached data.
3. Repeated forwarded commands create one mutation.
4. An unpaired or read-only caller cannot run privileged tools.
5. Local tasks continue through an aggregate-node outage.
6. Dual-boot nodes are separately identified and the inactive OS is not shown running.

Exit gate: remote control works with native clients and a required private network.
Phone usability arrives with the web client, not by assuming a mobile daemon.

## M4 — Optional history mirror and independent presentation clients

This milestone contains independent tracks. They share protocol prerequisites,
but none must wait for every other track.

**Mirror track:** selectively replicate events/snapshots/artifacts, resume from
cursors, handle retention gaps, label freshness, enforce storage quotas, and
keep replicas read-only.

**TUI track:** a native session/peer browser, structured tool and diff views,
steering input, reconnect, and client-side view state using `slop-client`.

**Web track:** a separately built responsive interface, phone-oriented task
inspection and steering, API compatibility, authentication, and reconnect.
An optional prebuilt bundle can be served without making frontend tooling a
native compilation dependency.

Acceptance:

- A phone on the tailnet can read progress, send a correction, and reconnect.
- Closing web/TUI interfaces leaves execution alive.
- A VPS or peer mirror shows explicitly last-known history when the owner is offline.
- Native builds work with all frontend source/build directories absent.
- The API remains usable from scripts while visual clients are changing.

## M5 — Desktop integration and visible browser tools

**Desktop track:** optional Electron shell, notifications, system integration,
and shared web presentation where appropriate. The daemon remains independently
installed/running.

**Browser track:** supervised browser/context service, bounded process/context
resources, tool-call/event identity, user/agent input arbitration, and a view
that corresponds to the controlled browser. Determine native desktop rendering
and remote web/mobile streaming separately.

Acceptance: browser navigation/tool events and the sidebar refer to the same
context; starting ordinary chats does not start browser instances; a crashed
client can reattach to an existing supported browser context.

This track may require further research before architecture is committed.

## M6 — Portable conversations and constrained task handoffs

**Status:** exploratory, not a first-release dependency.

Start with schema-versioned export/clone, then idle conversation moves preserving
identity and changing owner. Add source quiescence, target staging, durable grants,
deactivation tombstones, and failure reconciliation before paused task moves.

Add Git/workspace manifests and destination validation. Test same-OS paused
coding tasks before constrained Linux/Windows transfers. Credentials and
unaccounted-for processes do not travel implicitly.

Acceptance: inject failures at every handoff step and prove no source/target pair
is admitted simultaneously; preserve message/artifact integrity and workspace
provenance; an unknown handoff remains blocked and recoverable.

## Later or optional work

Local-model providers, automatic placement by capability/capacity, notifications
beyond desktop, scheduled jobs, richer experiment comparison, team tenancy,
public/paid hosting, and built-in P2P reachability require separate decisions.

## Cross-cutting release gates

Every useful release validates bounded queues/output, compatibility errors,
credential redaction, explicit recovery states, and client-independent lifetime.
Platform tests include Windows process semantics and path behavior.

Provider integration gates are availability/authentication dependent and should
not hold unrelated architecture/client improvements hostage. Performance claims
must refer to reproducible release-build workloads.

## First implementation slice after M0

Implement durable node identity and session CRUD, local authenticated commands,
and paginated history/events. Then add one native provider and the smallest
bounded tool loop. Build real CLI workflows around those operations before
creating visual frontends.

The user-facing demonstration at the end of M2/M4 is a script launching several
isolated coding tasks, followed by phone inspection and steering after leaving
home.
