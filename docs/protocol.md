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
Resource summaries and the larger command/error envelope below remain proposed.
The [provider credential surface](provider-credentials.md#public-api) is also
implemented, with wire structs in `slop_protocol::providers`: private API-key
replacement, safe credential status, and daemon-owned ChatGPT login. This setup
surface does not enable Codex/Zen session execution.

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
| GET /v1/models | Implemented: known Go models and local credential readiness; account entitlement is not probed |
| GET /v1/providers | Implemented: safe credential presence and daemon execution support |
| PUT /v1/providers/{provider}/api-key | Implemented: persist an API key and activate new supported requests without restart |
| POST /v1/providers/codex/login | Implemented: start/reuse the current ChatGPT authorization attempt |
| GET /v1/providers/codex/login/{id} | Implemented: inspect the latest daemon-owned login attempt |
| GET, POST /v1/projects | Register/list logical projects and local mappings |
| GET /v1/workspaces | Inspect active worktree and exclusive workspace reservations |
| GET, POST /v1/sessions | Implemented: list/create conversations owned by this node |
| GET /v1/sessions/{id} | Implemented: snapshot with revision and event watermark |
| GET /v1/sessions/{id}/messages | Implemented: paginated durable conversation |
| POST /v1/sessions/{id}/messages | Implemented: accept a user message with optional model/settings and queue one turn |
| GET /v1/sessions/{id}/events | Implemented: catch up and optionally follow NDJSON session events |
| GET /v1/turns/{id} | Implemented: turn status, message/request/invocation IDs, frozen settings, model, and usage |
| POST /v1/turns/{id}/cancel | Implemented: durable turn cancellation |
| GET /v1/messages/{id} | Implemented: canonical structured message |
| GET /v1/turns/{id}/requests | Implemented: paginated model requests/settings/usage |
| GET /v1/turns/{id}/tools | Implemented: paginated tool invocations/results |
| GET /v1/tools/{id} | Implemented: one canonical tool invocation |
| GET /v1/artifacts/{id} | Implemented: artifact metadata |
| GET /v1/artifacts/{id}/content | Implemented: authenticated artifact byte stream |
| GET, POST /v1/tasks | List/create queued work, optionally creating a session |
| GET /v1/tasks/{id} | Goal, status, attempts, and outputs |
| POST /v1/tasks/{id}/cancel | Request cancellation of pending/active work |
| POST /v1/tasks/{id}/retry | Create a new attempt after checking uncertain effects |
| POST /v1/runs/{id}/pause | Request a safe execution checkpoint |
| POST /v1/runs/{id}/resume | Resume an explicitly paused run |
| GET /v1/runs/{id} | Execution attempt, steps, usage, and workspace |
| POST /v1/batches/preview | Validate and count matrix expansion without starting work |
| GET, POST /v1/batches | Create/list batch records and member tasks |
| GET /v1/batches/{id} | Progress, parameters, members, and aggregate outputs |
| POST /v1/batches/{id}/cancel | Request cancellation according to recorded propagation policy |
| GET, POST /v1/peers | Configure and inspect trusted peer endpoints |
| POST /v1/transfers | Future handoff preparation; unavailable before M6 |

Routes marked implemented are available now; other spelling is a proposal.
Listing endpoints
are paginated and filterable. Task creation returns task/session IDs immediately;
a run ID appears when an execution attempt is admitted.

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

Text chat currently queues complete user messages in acceptance order, with one
active turn per session. It does not modify a request already in flight. Explicit
turn cancellation is supported. The richer steering modes below are proposals.

`append_only` records context without starting work. `queue_for_next_boundary`
targets the active task and applies the message before an appropriate model turn.
`interrupt_and_apply` requests interruption, waits for tool reconciliation, and
starts the next turn with the correction.

An idle session requires explicit task creation to start execution. Provider
support determines whether an in-flight inference can be steered directly or
must be canceled/reissued. The capability response and events disclose the
actual behavior. Clients show accepted and applied states separately.

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

Human diagnostics go to stderr; JSON output goes to stdout. The CLI can exit
after acceptance, follow events, or explicitly wait for completion. These modes
do not change daemon ownership.

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
