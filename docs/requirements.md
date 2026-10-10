# Features and non-negotiables

## Status vocabulary

**Accepted** requirements reflect the product discussion. **Proposed** details
are engineering choices recorded here for review. **M0** is the current scaffold;
M1 and later are planned milestones defined in the [roadmap](roadmap.md).
Priority orders work; it does not imply a future capability is already available.

## Non-negotiables

| ID | Accepted constraint | Practical consequence |
| --- | --- | --- |
| N01 | Native Rust backend | Daemon, scheduler, providers, and tool orchestration run in Rust. |
| N02 | No JS execution backend | No Node, Bun, Deno, JS, or TS agent runtime or native build prerequisite. |
| N03 | One daemon per execution environment | Clients attach to a service that owns all local sessions. |
| N04 | Shared execution infrastructure | A conversation must not require an application process, isolated language VM, or dedicated OS thread. |
| N05 | Own the agent runtime | Integrate with supported model APIs directly; do not execute external agent CLIs as the core implementation. |
| N06 | Structured product experience | Messages, tools, diffs, status, and artifacts are protocol objects. |
| N07 | Client-independent lifetime | Disconnecting, exiting, or crashing a client must not cancel accepted work. |
| N08 | Local ownership | Each daemon owns its local execution and durable data. |
| N09 | Optional aggregation/sync | Normal local use must not require a central application server. |
| N10 | Programmatic control | Scripts and agents can launch, inspect, steer, and cancel through a public API. |
| N11 | Multiple independent clients | CLI first; native TUI, web, and Electron can grow separately. |
| N12 | Native CLI/TUI builds remain independent | Browser assets and Electron builds cannot be prerequisites. |
| N13 | Linux and Windows are primary targets | OS behavior is tested and explicit, including dual boot. |
| N14 | Tailscale required initially for remote access | Connectivity builds on an existing private network. |
| N15 | API keys and supported subscriptions | Authentication must fit the provider's documented access path. |
| N16 | Efficient many-session operation | Measure baseline and incremental session memory; bound retained data. |
| N17 | Git worktree orchestration | Multiple writable coding tasks can use separate workspaces in one project. |
| N18 | Native child task creation | Agents can create sessions/tasks using the same controlled operations as clients. |

N02 permits JS in a browser interface or optional Electron client. N03 permits
supervised subprocesses for actual tools, such as Git, shell, tests, and browsers.
It is a single long-lived coordinating application service per node, not a
promise that an entire machine contains only one operating-system process.

## Functional feature inventory

| ID | Feature | Priority / milestone | Acceptance outline |
| --- | --- | --- | --- |
| F01 | Local daemon health and CLI status | M0, implemented | CLI checks identity/version and emits human or JSON output. |
| F02 | Durable session creation/listing/history | M1 | Session survives client exit and daemon restart. |
| F03 | Direct provider inference | M1 | Own native loop streams one provider's real response. |
| F04 | File and shell tools | M1 | Invocation, completion, limits, and output are recorded. |
| F05 | Reconnectable events | M1 | A client catches up after disconnect without rerunning work. |
| F06 | Steering and cancellation | M1 | Queued, accepted, applied, and interrupted states are distinct. |
| F07 | Run/task lifecycle and recovery | M1 | Restarted attempts and uncertain operations are visible. |
| F08 | Project registration and worktrees | M2, session slice implemented | Registered projects reserve frozen-base worktrees; independent sessions allocate lazily and expose diffs/cleanup. |
| F09 | Prompt/model/parameter matrices | M2 | Expansion is deterministic, previewable, and bounded. |
| F10 | Batch results and selective retries | M2 | Outputs preserve parameters; successful tasks are retained. |
| F11 | Agent-created child tasks | M2 | Parent links, context selection, budgets, and limits are explicit. |
| F12 | Multiple providers and model catalogs | M2 | Unsupported settings fail validation rather than being dropped. |
| F13 | Supported subscription authentication | M2, eligibility dependent | Direct documented flow; credentials remain outside task data. |
| F14 | Peer registration and remote CLI access | M3 | Trusted clients target an owner over Tailscale. |
| F15 | Aggregate multi-machine views | M3 | Peer availability and origin are shown accurately. |
| F16 | Optional history/artifact mirroring | M4 | Replicas can show last-known data with freshness indicators. |
| F17 | Native TUI | M4, independent track | Uses the public client SDK; no execution ownership. |
| F18 | Responsive web client | M4, independent track | Phone reconnect, event viewing, and steering work. |
| F19 | Optional Electron client | M5, independent track | Attaches to the same API and can close while work continues. |
| F20 | Browser automation and visible view | M5 | Commands and the displayed browser refer to the same controlled context. |
| F21 | Idle conversation transfer | M6 | Identity, history, and required attachments survive transfer. |
| F22 | Paused task/workspace migration | M6, exploratory | Destination restores state and ownership transfers safely. |
| F23 | Optional local-model provider | Later | Uses the same provider boundary without affecting cloud-only installs. |

## Reliability requirements

R01: Accepted commands have stable identifiers. Repeated delivery returns the
previous acknowledgement/result or rejects a conflicting payload.

R02: Durable state changes and their semantic events are committed together.
Clients observe committed facts, with separate transient deltas where useful.

R03: Persist tool intent before launch and its known outcome afterward. Recovery
must distinguish never started, running, completed, interrupted, and unknown.
An external side effect cannot generally be made exactly-once by journaling it.
Interrupted thinking is discarded. An unfinished tool call after daemon/process
failure is recorded as failed with that cause, without restoring or replaying
the call. The agent decides its next action from the durable failure record;
failure does not imply that the tool made no external changes.

R04: Each session has one execution owner. Peers and mirrors cannot independently
resume an owned session. Future migration chooses a blocked state when ownership
is uncertain.

R05: Client backpressure does not stall agent execution indefinitely. Clients
that fall behind can reconnect from durable cursors.

R06: A network partition or unreachable peer is visible as an availability state,
not a fabricated failure or a claim that an instruction was applied.

R07: Sleep, shutdown, and dual-boot transitions interrupt execution on that OS.
Persisted history remains recoverable; resumption is explicit and capability
dependent.

R08: Linux configuration and durable data use XDG directories, with explicit
overrides. Do not create a separate application directory directly under the
user's home. Completed steps and their semantic events are committed atomically
before acknowledgment or dependent execution.

## Resource and concurrency requirements

P01: Admission limits cover active runs, model requests, subprocesses, browser
contexts, queued child tasks, and per-project writers.

P02: Inactive session histories are stored on disk. Listing sessions uses
pagination rather than loading complete conversations.

P03: Tool output streams into capped buffers and durable artifact storage.
Large attachments and binary outputs are referenced, not copied into each event.

P04: Context assembly has a token/byte budget and preserves relevant tool-call
relationships and provider-specific continuation data.

P05: Batch submission cannot allocate full agent contexts or worktrees for every
queued combination. Validate size before creation; admit resources lazily.

P06: Track daemon RSS, live heap where measurable, active context size, buffer
bytes, queue depth, and tool/browser subprocess memory separately.

Proposed evaluation workloads are 1, 10, and 100 idle/active sessions plus a larger
mostly queued batch. Numerical memory targets will be selected after measuring a
release build; no unmeasured MiB-per-chat guarantee is part of the current plan.

## Interface and portability requirements

I01: Every core operation is available without a graphical client.

I02: Noninteractive use has stable JSON output, meaningful exit status, and no
prompts that unexpectedly block unattended jobs.

I03: Wire types are independent of runtime internals. Browser SDKs may be
generated from versioned schemas in a future milestone.

I04: Commands identify their owner, target, workspace, and policy where relevant.
Paths from one machine are not silently interpreted on another.

I05: Session IDs do not encode the owner's identity. Machine-specific paths,
credential handles, and tool availability are separate mappings.

I06: API key and subscription integrations expose their capabilities and
limits. A model's supported settings are provider-specific.

I07: Browser/Electron client code may use JS, while executable agent policy,
scheduling, and tool decisions remain daemon-owned.

I08: Provider credentials can be configured while the daemon is running through
the public API and CLI. The daemon persists secrets in its private application
data directory (XDG on Linux), outside conversations and logs. New supported
requests use updates without a daemon restart.

## Exclusions from the first useful release

The first useful local CLI release does not include browser tools, graphical
clients, history replication, automatic scheduling across machines, task
migration, team tenancy, automatic Git merging, or built-in internet NAT
traversal. These remain separate roadmap tracks where applicable.

A general OS sandbox, live process/checkpoint migration, transparent switching
between operating systems, and unrestricted reuse of subscription credentials
are not assumed capabilities. Implementation must name what it actually supports.
