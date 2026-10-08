# Public protocol

## Scope and current implementation

The protocol is the boundary shared by every frontend and automation client.
Use HTTP/JSON for commands and queries, with a streaming transport for events.
The first streaming proposal is NDJSON over HTTP; a WebSocket transport can be
added without changing event meaning.

**Implemented at M0:** only `GET /v1/health`, returning the
[HealthResponse](../crates/slop-protocol/src/lib.rs) DTO. All other routes and
envelopes below are proposed. Bootstrap errors are ordinary HTTP/client errors;
the proposed structured error format is not yet implemented.

The native client currently accepts an HTTP(S) origin without path, query,
fragment, or embedded credentials. It applies connect/request timeouts, avoids
redirects and proxy routing, and checks service identity and API version.

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

Generated schema files and SDKs are release artifacts. Do not generate an SDK
from runtime-private structures. Rust clients use `slop-protocol` directly.

## Proposed command envelope

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
| GET /v1/health | Current bootstrap identity/capabilities |
| GET /v1/node | Durable node identity, OS, availability, and resource summary |
| GET /v1/capabilities | Available tools, providers, settings, and policy capabilities |
| GET /v1/models | Account-specific available model identifiers |
| GET, POST /v1/projects | Register/list logical projects and local mappings |
| GET /v1/workspaces | Inspect active worktree and exclusive workspace reservations |
| GET, POST /v1/sessions | List/create conversations owned by this node |
| GET /v1/sessions/{id} | Snapshot with revision and event watermark |
| GET /v1/sessions/{id}/messages | Paginated durable conversation |
| POST /v1/sessions/{id}/messages | Append or steer an existing active task |
| GET /v1/sessions/{id}/events | Catch up and optionally follow session events |
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
| GET /v1/artifacts/{id} | Metadata and controlled artifact download |
| GET, POST /v1/peers | Configure and inspect trusted peer endpoints |
| POST /v1/transfers | Future handoff preparation; unavailable before M6 |

Route spelling is a proposal and should stabilize with M1. Listing endpoints
are paginated and filterable. Task creation returns task/session IDs immediately;
a run ID appears when an execution attempt is admitted.

## Event contract

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

M1 should implement local credential/pairing semantics before adding privileged
tool execution. M3 adds authenticated access over Tailscale, with separate
read/write capabilities. Network membership alone is not the entire application
authorization policy.

A forwarder identifies the target node and carries an authenticated command to
the owner. Responses preserve origin and freshness. Browser entrypoints can
proxy trusted peer requests through one reachable daemon, avoiding a requirement
that the browser contact every peer directly.
