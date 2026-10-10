# Public protocol

## Scope and current implementation

The protocol is the boundary shared by every frontend and automation client.
Use HTTP/JSON for commands and queries, with a streaming transport for events.
The implemented streaming transport is NDJSON over HTTP; a WebSocket transport can be
added without changing event meaning.

**Implemented:** anonymous health, authenticated node identity, and the
[text-chat surface](text-chat.md#public-surface): models, sessions, messages,
history, event replay/follow, and turn inspection/cancellation. The wire structs
are in `slop_protocol::chat`. Mutations use explicit command IDs and return
durable acceptance receipts. Errors use `ErrorResponse { code, message }`.
[Structured execution](structured-execution.md) also adds message blocks,
model requests, tool invocations, artifacts, capability discovery, causal delta
IDs, and per-turn model/settings. Its DTOs live in `slop_protocol::execution`.
Registered [projects/workspaces](projects-workspaces.md) and durable
[tasks/run attempts and matrices](tasks-batches.md) are also implemented. Their
wire structs live in `slop_protocol::projects` and `slop_protocol::orchestration`.
Resource summaries and the larger command/error envelope below remain proposed.
The [provider credential surface](provider-credentials.md#public-api) is also
implemented, with wire structs in `slop_protocol::providers`: private API-key
replacement, safe credential status, and daemon-owned ChatGPT login. Saved Go,
Zen, and Codex credentials enable execution through the common session surface.

The native client currently accepts an HTTP(S) origin without path, query,
fragment, or embedded credentials. It applies connect/request timeouts, avoids
redirects, retries, and proxy routing, and checks service identity and API
version. A token-file client checks health before sending its credential to the
node route. Health identifies compatibility; it is not authenticated proof of a
server's identity. The caller must select a trusted origin and token file.

## Versioning and capability discovery

The URL has a major API version. Schema generation later publishes that
version's request, response, and event definitions. Additive response fields are
allowed; unknown event types are displayed generically or ignored without
breaking cursor advancement. A changed field meaning, removed operation, or
incompatible enum interpretation requires explicit version handling.

The future health/capability response describes node identity, supported API
versions, auth mode, runtime/tool features, and provider capabilities. A healthy
daemon does not necessarily have an available model credential or admission
capacity. Clients must distinguish connectivity from readiness to execute.

The [shared provider interface](provider-interface.md) defines the proposed
provider/account/model-scoped capability descriptors and normalized content,
usage, errors, and lifecycle facts exposed through these DTOs. Provider SDK
objects and raw transport events stay inside runtime adapters; every client uses
the same canonical public surface.

Generated schema files and SDKs are release artifacts. Do not generate an SDK
from runtime-private structures. Rust clients use `slop-protocol` directly.

## Command identity and proposed broader envelope

The implemented text requests carry `command_id` directly, with
`expected_revision` available on message submission. `CommandReceipt` contains
the original command/session IDs, optional turn/message IDs, revision, and event
sequence. SQLite stores a canonical serialized request together with its scope
and receipt. The envelopes below describe the broader future task API.

Every mutating operation carries an opaque command ID, generated once by the
caller, and an optional expected entity revision. The owner persists the ID and
a hash of the canonical request payload.

```json
{
  "command_id": "opaque-command-id",
  "expected_revision": 12,
  "payload": {
    "session_id": "opaque-session-id",
    "text": "Keep the public API unchanged.",
    "behavior": "queue_for_next_boundary"
  }
}
```

Identifiers in examples are schematic; the implementation should use globally
unique opaque identifiers. Retransmit the same command ID and payload after a
connection failure. Reuse with different content returns a conflict. A repeated
valid command returns the original acknowledgement, including any created IDs.

An acknowledgement proves acceptance and persistence, not completion:

```json
{
  "command_id": "opaque-command-id",
  "status": "accepted",
  "owner_node_id": "opaque-node-id",
  "entity_revision": 13,
  "created": {},
  "event_cursor": {
    "session_id": "opaque-session-id",
    "sequence": 81
  }
}
```

Execution reports a later applied, completed, rejected-at-execution, or failed
event. Remote forwarders preserve the command ID. A forwarder may return
`pending_delivery`, but cannot manufacture the owner's `accepted` result.

## Proposed API surface

| Method and route | Purpose |
| --- | --- |
| GET /v1/health | Implemented: anonymous service identity/capabilities |
| GET /v1/node | Implemented: authenticated durable node ID, name, and OS; resource summary remains proposed |
| GET /v1/capabilities | Implemented: tool schemas, per-turn selection support, and loop bounds |
| GET /v1/models | Implemented: known Go/Zen/Codex models, capabilities, and local credential readiness; account entitlement is not probed |
| GET /v1/providers | Implemented: safe credential presence and daemon execution support |
| PUT /v1/providers/{provider}/api-key | Implemented: persist an API key and activate new supported requests without restart |
| POST /v1/providers/codex/login | Implemented: start/reuse the current ChatGPT authorization attempt |
| GET /v1/providers/codex/login/{id} | Implemented: inspect the latest daemon-owned login attempt |
| GET, POST /v1/projects | Implemented: register/list repositories and local mappings |
| GET /v1/projects/{id}/workspaces | Implemented: paginated managed workspaces |
| GET /v1/workspaces/{id}, /diff | Implemented: workspace state and tracked patch |
| POST /v1/workspaces/{id}/remove | Implemented: explicit conservative cleanup |
| GET /v1/workspaces | Inspect active worktree and exclusive workspace reservations |
| GET, POST /v1/sessions | Implemented: list/create conversations owned by this node |
| GET /v1/sessions/{id} | Implemented: snapshot with revision and event watermark |
| GET /v1/sessions/{id}/messages | Implemented: paginated durable conversation |
| POST /v1/sessions/{id}/messages | Implemented: accept a message with optional model/settings and `after-turn`, `next-boundary`, or `immediate` delivery |
| GET /v1/sessions/{id}/instructions | Implemented: query durable steering status/history |
| GET /v1/sessions/{id}/events | Implemented: catch up and optionally follow NDJSON session events |
| GET /v1/turns/{id} | Implemented: turn status, message/request/invocation IDs, frozen settings, model, and usage |
| POST /v1/turns/{id}/cancel | Implemented: durable turn cancellation |
| POST /v1/turns/{id}/pause, /resume | Implemented: pause at safe boundaries and explicitly readmit paused turns |
| GET /v1/messages/{id} | Implemented: canonical structured message |
| GET /v1/turns/{id}/requests | Implemented: paginated model requests/settings/usage |
| GET /v1/turns/{id}/tools | Implemented: paginated tool invocations/results |
| GET /v1/tools/{id} | Implemented: one canonical tool invocation |
| GET /v1/artifacts/{id} | Implemented: artifact metadata |
| GET /v1/artifacts/{id}/content | Implemented: authenticated artifact byte stream |
| GET, POST /v1/tasks | Implemented: list/create durable jobs with initial run/session/turn IDs |
| GET /v1/tasks/{id} | Implemented: requested/effective inputs, latest attempt, and event watermark |
| GET /v1/tasks/{id}/runs, /events | Implemented: paginated attempt history and committed facts |
| POST /v1/tasks/{id}/instructions | Implemented: steer the current active or paused attempt |
| GET /v1/runs/{id} | Implemented: execution attempt, turn, usage, workspace, and effects uncertainty |
| POST /v1/runs/{id}/pause, /resume, /cancel | Implemented: existing durable turn control |
| POST /v1/runs/{id}/retry | Implemented: explicit fresh attempt, preserving prior outcomes |
| POST /v1/batches/preview | Implemented: validate and expand without starting work |
| GET, POST /v1/batches | Implemented: create/list batches and member tasks |
| GET /v1/batches/{id} | Implemented: frozen parameters and latest-member status counts |
| GET /v1/batches/{id}/members, /results | Implemented: paginated inputs and outcomes |
| POST /v1/batches/{id}/retry | Implemented: atomically retry selected unsuccessful combinations |
| POST /v1/batches/{id}/cancel | Proposed: batch-wide cancellation with propagation policy |
| GET, POST /v1/peers | Configure and inspect trusted peer endpoints |
| POST /v1/transfers | Future handoff preparation; unavailable before M6 |

Routes marked implemented are available now; other spelling is a proposal.
Implemented listings are paginated; broader filtering remains proposed. Task
creation returns task/run/session/turn IDs atomically before admission. See
[tasks and batches](tasks-batches.md) for payloads, limits, and retry semantics.

## Event contract

Implemented `EventResponse` contains session ID, sequence, kind, optional
turn/message IDs, and revision. `EventFrame` is tagged by `type` as `durable`,
`delta`, or `heartbeat`. Durable events refer to canonical records; deltas do not
advance the cursor. The richer envelope below remains proposed.

Durable events have an envelope containing event ID, session ID, owner ID,
ownership epoch, monotonically increasing session sequence, entity revision,
type, payload schema version, timestamp, and causal command/run IDs where
applicable. Timestamps help display; sequence numbers establish order.

A cursor is scoped to one session. Aggregate views track multiple cursors; a
single scalar cannot establish order across independent daemons.

Initial semantic event categories:

- Session created/archived and configuration changed.
- Task queued/admitted and run started/state changed.
- User message accepted and steering applied.
- Assistant message checkpointed/completed.
- Tool invocation recorded, started, completed, interrupted, or outcome unknown.
- Workspace allocated and artifact registered.
- Child task created, awaited, and result received.
- Usage updated, budget reached, and provider availability changed.
- Ownership handoff prepared, committed, and reconciled.

Streaming token deltas are transient frames with a stream ID and chunk index.
They do not advance durable cursors. Periodic partial-message checkpoints and the
final message establish recoverable text; clients reconcile transient display
with those canonical records.
Interrupted thinking is discarded. An unfinished tool after daemon/process
failure is represented as a failed tool result with its invocation ID, failure
cause, and any uncertainty about external effects. This is implemented for
opt-in local tools in [structured execution](structured-execution.md). The daemon never restores or replays
that call; the agent chooses its next action.

## Replay, gaps, and backpressure

A snapshot includes the event watermark it reflects. A client then subscribes
after that watermark. The server must avoid a snapshot/subscription race by
reading committed events after the cursor before moving to live delivery.

Delivery can repeat events after reconnect. Clients deduplicate by session
sequence/event ID. If retention removed requested events, return a gap indicator
and a route to a new snapshot. Never silently skip missing history.

Each client connection has a bounded buffer. If it cannot keep up, disconnect it
with a recoverable cursor. Durable history and agent execution continue. Large
tool outputs are streamed artifacts or chunk references, with bounded previews.

## Steering semantics

Text chat supports three delivery modes on `POST /v1/sessions/{id}/messages`:

- `after-turn` (omitted by default) queues a regular turn after current work.
- `next-boundary` targets the active logical turn and applies the instruction
  after the current inference or tool has reached a completed boundary and
  before its next model request or tool launch. It does not act at token-level
  streaming boundaries. A yielded continuation and paused turn remain valid
  targets.
- `immediate` interrupts in-flight inference or signals cancellation to
  supervised Bash, then waits for the operation's actual outcome. File
  operations already started finish and record their result. Interrupted
  inference usage is unknown; incomplete reasoning and private continuation are
  discarded.

Idle delivery falls back to ordinary turn admission. Instructions have durable
IDs, accepted/applied/rejected status, and semantic events. `GET
/v1/sessions/{id}/instructions` lists their bounded history. `POST
/v1/turns/{id}/pause` and `/resume` persist pause state; a paused turn releases
the execution slot, prevents later same-session turns from overtaking it, and
never resumes automatically after restart. Resume reuses committed steps and
cumulative budgets without replaying tools. Model/settings overrides are
rejected for instructions attached to an active logical turn.

After a model response proposes multiple tools, steering is applied only after
all proposals have explicit paired results. Pending, unstarted side effects are
recorded as skipped before a fresh model decision. Unknown tool outcomes are
never replayed.

Concurrent messages are sequenced by the owner. An expected revision can prevent
an instruction based on stale state from being silently applied.

## Errors and client behavior

The proposed error object contains a stable code, safe message, command ID when
known, optional structured details, and a retry hint. Initial codes include
`not_owner`, `revision_conflict`, `command_conflict`, `unsupported_capability`,
`provider_auth_required`, `provider_rate_limited`, `budget_exhausted`,
`workspace_busy`, `peer_unreachable`, and `recovery_required`.

Retryable transport failure does not imply the command was unaccepted. Recheck
using the same command identity. A non-idempotent operation gets no hidden
automatic retry beyond the command acceptance mechanism.

Human diagnostics go to stderr; JSON output goes to stdout. `slop session send`
and `slop chat --session` accept `--delivery after-turn|next-boundary|immediate`;
`slop turn pause|resume` controls durable pauses. The CLI can exit after
acceptance, follow events, or explicitly wait for completion. These modes do
not change daemon ownership.

## Authentication and remote forwarding

Local bearer-token authentication is implemented for node, provider credential,
and text-chat queries
and mutations. The daemon
still binds loopback only. Explicit session workspace/tool policy gates local
tool execution under the bearer credential. Remote pairing and separate
read/write authorization remain future work. M3 adds authenticated access over Tailscale, with separate
read/write capabilities. Network membership alone is not the entire application
authorization policy.

A forwarder identifies the target node and carries an authenticated command to
the owner. Responses preserve origin and freshness. Browser entrypoints can
proxy trusted peer requests through one reachable daemon, avoiding a requirement
that the browser contact every peer directly.
