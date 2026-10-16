# Public protocol

## Scope and current implementation

The protocol is the boundary shared by every frontend and automation client.
Use HTTP/JSON for commands and queries, with a streaming transport for events.
The implemented streaming transport is NDJSON over HTTP; a WebSocket transport can be
added without changing event meaning.

**Implemented:** anonymous health, authenticated node identity, and the
[text-chat surface](text-chat.md#public-surface): models, sessions, messages,
history, event replay/follow, and turn inspection/cancellation. The wire structs
are in `hive_protocol::chat`. Mutations use explicit command IDs and return
durable acceptance receipts. Errors use `ErrorResponse { code, message }`.
[Structured execution](structured-execution.md) also adds message blocks,
model requests, tool invocations, artifacts, capability discovery, causal delta
IDs, and per-turn model/settings. Its DTOs live in `hive_protocol::execution`.
Message DTOs also expose persisted `provider` and `model` fields; these describe
each message's frozen selection or originating request's reported model, even
when subsequent turns select a different provider.
The unified [workspace API](workspaces.md) and durable
[tasks/run attempts and matrices](tasks-batches.md) are also implemented. Workspace
wire structs live in `hive_protocol::workspaces`; orchestration structs live in
`hive_protocol::orchestration`.
The orchestration contract includes cumulative budgets, batch cancellation,
one-level delegation policies, parent/child links, exact-attempt all/any/deadline waits, notification/detachment settings, and public run
results; see [child tasks](child-tasks.md#public-api-and-validation). Health
advertises `orchestration-controls` and `child-tasks` for the corresponding new
mutations. `subagent-v2` gates clients that need one-level creation, notification
defaults, detached lifecycle, or extended join semantics; older daemons can
continue serving read-only child queries. Awaiting children is a nonterminal run status.
Resource summaries and the larger command/error envelope below remain proposed.
The [provider credential surface](provider-credentials.md#public-api) is also
implemented, with wire structs in `hive_protocol::providers`: private API-key
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
from runtime-private structures. Rust clients use `hive-protocol` directly.

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

The administrator API publishes a compile-time OpenAPI document at
`GET /api-docs/openapi.json` and interactive Swagger UI at `GET /docs`. The
document covers all administrator routes, including workspace and peer routes.

| Method and route | Purpose |
| --- | --- |
| GET /v1/health | Implemented: anonymous service identity/capabilities |
| GET /v1/node | Implemented: authenticated durable node ID, name, and OS; resource summary remains proposed |
| GET /v1/capabilities | Implemented: tool schemas, per-turn selection support, and loop bounds |
| GET /v1/models | Implemented: known Go/Zen/Codex models, capabilities, and local credential readiness; account entitlement is not probed |
| GET /v1/providers | Implemented: safe credential presence and daemon execution support |
| PUT /v1/providers/{provider}/api-key | Implemented: persist an API key and activate new supported requests without restart |
| GET /v1/providers/{provider}/models | Implemented: explicit live catalog probe with auth/account scope and timestamp; unmapped models remain unavailable |
| DELETE /v1/providers/{provider}/api-key | Implemented: remove a key, suppress stale environment import, and disable new admissions on that billing path |
| GET /v1/providers/codex/accounts | Implemented: safe local registration labels and selection/token status |
| PUT /v1/providers/codex/auth | Implemented: select an existing API key or subscription account without resupplying credentials |
| POST /v1/providers/codex/accounts/{account}/logout | Implemented: clear tokens, retain registration, report confirmed/unconfirmed revocation |
| POST /v1/providers/codex/login | Implemented: start/reuse the current ChatGPT authorization attempt, optionally for a named account |
| GET /v1/providers/codex/login/{id} | Implemented: inspect the latest daemon-owned login attempt |
| GET /v1/workspaces | Implemented: paginated canonical directory records |
| POST /v1/workspaces | Implemented: open an existing directory in place |
| POST /v1/workspaces/create | Implemented: asynchronously create a directory, optionally initializing Git; returns `allocating` for polling |
| GET /v1/workspaces/{id} | Implemented: workspace owner, path, Git metadata, provenance, and state |
| POST /v1/workspaces/{id}/rename | Implemented: rename a workspace without moving its directory |
| POST /v1/workspaces/{id}/remove | Implemented: unregister a directory or conservatively remove a managed worktree |
| GET /v1/workspaces/{id}/diff | Implemented: Git patch against current HEAD or a worktree's frozen base |
| GET /v1/workspaces/{id}/events | Implemented: paginated workspace lifecycle events |
| POST /v1/workspaces/{id}/worktrees | Implemented: explicitly create a worktree as a new workspace |
| GET, POST /v1/sessions | Implemented: list/create persistent conversations; optional prompt and policy are accepted atomically |
| GET /v1/sessions/{id} | Implemented: snapshot with revision and event watermark |
| POST /v1/sessions/{id}/inbox | Implemented: idempotent durable patch of stars, Archive, Trash, and read watermark; does not cancel or purge work |
| GET /v1/drafts | Implemented: paginated daemon-owned compose drafts |
| PUT, DELETE /v1/drafts/{id} | Implemented: save or remove a compose draft; writes commit before acknowledgement |
| GET /v1/sessions/{id}/messages | Implemented: paginated durable conversation |
| POST /v1/sessions/{id}/messages | Implemented: accept a message with optional model/settings and `after-turn`, `next-boundary`, or `immediate` delivery |
| POST /v1/sessions/{id}/fork | Implemented: idempotent fork through a selected message, with independent history IDs and source lineage |
| PATCH /v1/sessions/{id}/messages/{message_id} | Implemented: edit user/assistant text in a new fork; a user edit queues a new response |
| DELETE /v1/sessions/{id}/messages/{message_id} | Implemented: idempotent suffix removal from an idle conversation; preserves execution audit records |
| GET /v1/sessions/{id}/instructions | Implemented: query durable steering status/history |
| GET /v1/sessions/{id}/events | Implemented: catch up and optionally follow NDJSON session events |
| GET /v1/turns/{id} | Implemented: turn status, message/request/invocation IDs, frozen settings, model, and usage |
| POST /v1/turns/{id}/cancel | Implemented: durable turn cancellation |
| GET /v1/sessions/{id}/queue | Implemented: inspect the durable FIFO of unsent messages |
| POST /v1/sessions/{id}/interrupt | Implemented: cancel active execution and send the leading queued batch |
| POST /v1/turns/{id}/pause, /resume | Implemented: pause at safe boundaries and explicitly readmit paused turns |
| GET /v1/messages/{id} | Implemented: canonical structured message |
| GET /v1/turns/{id}/requests | Implemented: paginated model requests/settings/usage |
| GET /v1/turns/{id}/tools | Implemented: paginated tool invocations/results |
| GET /v1/tools/{id} | Implemented: one canonical tool invocation |
| GET /v1/artifacts/{id} | Implemented: artifact metadata |
| GET /v1/artifacts/{id}/content | Implemented: authenticated artifact byte stream |
| GET, POST /v1/tasks | Implemented: conversation convenience operations; task/session and run/turn IDs are identical |
| GET /v1/tasks/{id} | Implemented: requested/effective inputs, latest attempt, and event watermark |
| GET /v1/tasks/{id}/runs, /events | Implemented: paginated attempt history and committed facts |
| POST /v1/tasks/{id}/instructions | Implemented: steer the current active or paused attempt |
| GET /v1/runs/{id} | Implemented: execution attempt, turn, usage, workspace, and effects uncertainty |
| POST /v1/runs/{id}/pause, /resume, /cancel | Implemented: existing durable turn control |
| POST /v1/runs/{id}/retry | Implemented: explicit new turn in the same conversation and workspace, preserving prior outcomes |
| POST /v1/batches/preview | Implemented: validate and expand without starting work |
| GET, POST /v1/batches | Implemented: create/list batches and member tasks |
| GET /v1/batches/{id} | Implemented: frozen parameters and latest-member status counts |
| GET /v1/batches/{id}/members, /results | Implemented: paginated inputs and outcomes |
| POST /v1/batches/{id}/retry | Implemented: atomically retry selected unsuccessful combinations |
| POST /v1/batches/{id}/cancel | Implemented: durable batch-wide cancellation; queued/paused/awaiting work stops, running work cancels at existing boundaries, batch stays closed to retries |
| POST /v1/runs/{id}/children | Implemented: create a child task under an authorized delegation policy |
| GET /v1/runs/{id}/children | Implemented: paginated direct-child task records |
| POST /v1/runs/{id}/wait | Implemented: durable wait on explicit direct-child run IDs, releasing execution slots |
| GET /v1/runs/{id}/result | Implemented: attempt result with public output |
| GET /v1/peers; PUT, DELETE /v1/peers/{id} | Implemented: scoped outbound registrations and removal |
| GET /v1/peer-grants; PUT /v1/peer-grants/{id}; POST /v1/peer-grants/{id}/revoke | Implemented: inbound grant creation/listing/revocation |
| /v1/peers/{peer_id}/... | Implemented: explicit one-hop session inspection, events, steering, and turn controls; see [daemon peering](daemon-peering.md) |
| POST /v1/transfers | Future handoff preparation; unavailable before M6 |

Routes marked implemented are available now; other spelling is a proposal.
Implemented listings are paginated; broader filtering remains proposed. Task
creation returns task/run/session/turn IDs atomically before admission. See
[tasks and batches](tasks-batches.md) for payloads, limits, and retry semantics.
Health advertises `unified-workspaces`. Every `CreateSessionRequest`,
`SessionSpec`, and `BatchSpec` carries a required `WorkspaceSelection`; use the
built-in workspace ID `default` with an empty tool list for directory-less work.
There are no project routes or project compatibility aliases.

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

The daemon owns a durable FIFO of unsent messages for each session. Closing or
replacing a client does not change delivery. `POST /v1/sessions/{id}/messages`
accepts these delivery modes:

- `after-turn` (the default) waits for the current model response and its tool
  results, then applies all consecutive after-turn messages before the next
  model request. It keeps the active run's frozen model/settings. It does not
  interrupt reasoning, response streaming, or tools.
- `after-finish` waits for the active run's final response, then immediately
  starts a fresh run with that one message and its submitted model/settings.
- `next-boundary` and `immediate` remain explicit instruction delivery modes.
  Next-boundary waits for an operation boundary; immediate cancels inference or
  supervised Bash before replanning. Started atomic file operations record their
  actual result. Neither mode requires a pause/resume interaction.

The FIFO never overtakes an after-finish message. For example, `A (after turn),
B (after turn), F (after finish), C (after turn)` applies A and B together at the
next completed model/tool boundary, starts F after the final response, and
applies C at F's first completed model/tool boundary. Idle input starts
immediately. Sending to a legacy paused run automatically readmits it.

`GET /v1/sessions/{id}/queue` lists unsent messages, including their stable IDs,
text and delivery modes. Acceptance emits `message_queued`; delivery atomically
writes the canonical user message and emits `queued_message_sent`. A finish
message reserves its future turn ID at acceptance, so existing turn inspection
and waiting clients can follow it before admission. Command retries return the
original receipt without duplicating input.

`POST /v1/sessions/{id}/interrupt` is the Ctrl-C operation. It requests immediate
cancellation of inference and active tools, waits only for cancellation cleanup,
and immediately admits the leading FIFO batch: all consecutive after-turn
messages, or exactly one after-finish message when that is first. Later messages
stay queued for their own boundary. A new run makes its first model request
before draining more messages, so interrupting a finish message cannot send the
messages behind it at the same time. With an empty queue, execution stops.
The request can pin the observed `turn_id`; a late or retried interruption never
cancels a successor. It can also carry pending `messages` with their original
command IDs, closing the race between rapid sends and interruption. Invalid
pending prompts do not prevent cancellation.

Explicit instructions retain durable accepted/applied/rejected status through
`GET /v1/sessions/{id}/instructions`. Tool proposals always have paired results;
unstarted effects are skipped before explicit immediate replanning. Interrupted
inference has unknown usage, and incomplete reasoning/private continuation is
excluded from the next request. Unknown tool outcomes are never replayed.

Concurrent messages are sequenced by the owner. An expected revision can prevent
input based on stale state from being silently applied.

## Errors and client behavior

The proposed error object contains a stable code, safe message, command ID when
known, optional structured details, and a retry hint. Initial codes include
`not_owner`, `revision_conflict`, `command_conflict`, `unsupported_capability`,
`provider_auth_required`, `provider_rate_limited`, `budget_exhausted`,
`workspace_busy`, `peer_unreachable`, and `recovery_required`.

Retryable transport failure does not imply the command was unaccepted. Recheck
using the same command identity. A non-idempotent operation gets no hidden
automatic retry beyond the command acceptance mechanism.

Human diagnostics go to stderr; JSON output goes to stdout. `hivectl session send`
and `hivectl chat --session` accept `--delivery after-turn|next-boundary|immediate`;
`hivectl turn pause|resume` controls durable pauses. The CLI can exit after
acceptance, follow events, or explicitly wait for completion. These modes do
not change daemon ownership.

## Authentication and remote forwarding

Local bearer-token authentication is implemented for node, provider credential,
and text-chat queries
and mutations. The daemon
still binds loopback only. Explicit session workspace/tool policy gates local
tool execution under the bearer credential. The local
[M3 peer slice](daemon-peering.md) implements explicit registration,
read/control grants, revocation, owner-ID checks, and one-hop forwarding. Private
HTTPS reaches a separate loopback peer listener; it exposes a restricted public
API and accepts only peer credentials. Tailnet deployment validation remains
separate. Network membership alone is not application authorization.

A forwarder identifies the target node and carries an authenticated command to
the owner. Responses preserve origin and freshness. Browser entrypoints can
proxy trusted peer requests through one reachable daemon, avoiding a requirement
that the browser contact every peer directly.
