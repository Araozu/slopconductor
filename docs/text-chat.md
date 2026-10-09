# Durable local text chat

This slice connects the public API, the daemon's native execution supervisor,
SQLite, and the consumer CLI. It uses the existing OpenCode Go adapter directly.
Zen and Codex remain available as runtime adapters; their daemon integration is
separate work. Tools, projects, worktrees, and general task/run orchestration are
not part of text chat.

## Ownership and durability

The daemon owns accepted turns. Closing a CLI, losing its event connection, or
pressing Ctrl-C while following detaches that client. Explicit cancellation is a
separate durable command. Each session has one authoritative node and at most
one admitted turn at a time. Queued turns acquire provider resources only when
admitted; all sessions share the daemon's Tokio runtime and provider client.

SQLite remains in the existing XDG data directory on Linux, or local application
data on Windows. The same private directory, exclusive lock, bounded database
worker, WAL mode, FULL synchronization, and startup schema checks apply. There
is no additional directory under the user's home.

Session creation, user-message acceptance, and cancellation carry a caller's
command ID. Store the ID once before delivery. The daemon records the canonical
payload, original receipt, resulting state, revision, and semantic events in one
transaction. Repeating the same ID and payload returns that receipt; changing
its payload or operation conflicts. A lost HTTP response therefore does not
require another message or provider call.

Acceptance means the command was committed. Admission records provider intent
before dispatch. Visible output receives bounded partial checkpoints; canonical
terminal text, resolved model, usage, outcome, and completion events commit
together. A provisional text delta is not proof of completion. Durable event
cursors are per-session sequence numbers and never advance for transient deltas.

The supervisor checks changed visible content every 250 ms. Safe terminal writes
have bounded retries without repeating the provider call. If a terminal outcome
cannot be persisted, execution stops admitting further work and the API reports
`runtime_unavailable`; startup recovery reconciles unfinished records.

History is ordered by conversational turn, with its user message before its
assistant message. Later queued messages are excluded from an earlier turn's
provider context. Completed replies are reusable context; interrupted thinking
is discarded. Context is bounded rather than silently truncated.

## Restart and cancellation

Startup marks in-flight turns interrupted and preserves committed history.
It does not reconstruct provider sockets, resume thinking, or retry an inference
whose outcome was unknown. Queued work which had not been dispatched remains
eligible for admission. Continue a recovered conversation by sending a new
message; doing so creates a new turn.

Cancellation is persisted before execution reacts. Queued work can become
terminal immediately; active execution drops its provider future. Cancellation
and completion are serialized by the database worker. Cancellation of a local
HTTP request cannot guarantee that the provider stopped processing or billing.

No tool calls are executed by this slice. When tools arrive, an unfinished call
after daemon/process failure must become a failed result with possible unknown
external effects. It must not be restored or replayed automatically; the agent
decides its next action from that record.

## Public surface

All chat endpoints require the existing local bearer token. Health stays
anonymous. The daemon accepts loopback listeners only.

| Method and route | Behavior |
| --- | --- |
| GET /v1/models | Known executable Go models and local credential readiness |
| GET, POST /v1/sessions | Paginated listing or durable creation |
| GET /v1/sessions/{id} | Session configuration, revision, and event watermark |
| GET, POST /v1/sessions/{id}/messages | Paginated history or durable message/turn acceptance |
| GET /v1/sessions/{id}/events | Replay from `after`; optional NDJSON following |
| GET /v1/turns/{id} | Outcome, message IDs, requested/resolved model, and usage |
| POST /v1/turns/{id}/cancel | Idempotent explicit cancellation |

Model readiness reports local configuration. It does not prove account
entitlement, provider availability, or a successful paid inference. Unsupported
models fail validation; an explicitly selected model is never silently replaced.
Provider credentials stay in private files under the daemon's XDG data directory
and out of session records, public responses, and diagnostics. The
[credential API/CLI](provider-credentials.md) sets keys while the daemon runs;
new admissions use the saved connection and active turns retain their original
connection. Legacy environment keys are imported only when no saved key exists.

The default model is `opencode-go/glm-5.3-flash`, with a 4,096-token output cap
and four concurrent requests across sessions. `default_model`,
`max_output_tokens`, and `execution_concurrency` can be set in the daemon TOML
configuration; output caps are frozen in each created session. The execution
queue allows 32 queued turns per session and 1,024 globally. Provider context is
limited to 256 messages and 1 MiB; exceeding it records `context_limit` without
dispatching inference. Visible text is limited to 1 MiB. Lists have at most 200
items per page, and history pages also have an 8 MiB encoded JSON bound.

`--default-model`/`SLOP_DEFAULT_MODEL` overrides the configured model.
`--provider-base-url`/`SLOP_PROVIDER_BASE_URL` selects an explicitly trusted
provider endpoint, principally for local transport fixtures. HTTP is limited to
loopback hosts; other endpoints require HTTPS. Embedded credentials, query, and
fragment components are rejected. This setting selects the destination to
which the daemon sends its provider credential.

Event replay reads committed facts after the cursor. Streaming subscribers have
bounded buffering and can reconnect using the last durable sequence. Losing
provisional deltas does not lose committed messages or stop execution. Clients
reconcile display against canonical message records when a turn ends.

There are at most 128 event subscribers. Transient frames carry at most 16 KiB
of visible text; heartbeats occur every five seconds. History cursors describe
conversation order, while event cursors describe committed changes. A running
turn's visible message can be inserted or updated before later queued messages;
use events and a fresh history read to reconcile those updates.

## Verification

Workspace checks exercise transactional command deduplication, state/event
agreement, ordered context, session serialization, cancellation, and recovery.
The real-binary smoke check uses an isolated local HTTP fixture which speaks the
existing provider wire protocol. It exercises the daemon and CLI without cloud
credentials or paid calls. Live provider tests remain explicit opt-ins with
request/token budgets.

The crash fixture emits visible text and synthetic reasoning, waits for a
committed checkpoint, kills the actual daemon process, and restarts the same
store. It verifies interrupted status, preserved completed history/visible text,
no repeated provider request, and a fresh follow-up context without the discarded
thinking or incomplete assistant output.

Live verification for this slice on 2026-10-08 passed the existing Go streaming
adapter check and one daemon/CLI turn using `glm-5.3-flash`. Each request had a
512-token output cap. The consumer turn completed with reported usage; retrying
the same CLI command retained one turn and two messages, and its canonical
reply survived daemon restart. Credentials and live session data were held in
temporary test state, outside version control.

Process-kill checks establish application recovery, not power-loss behavior.
SQLite's synchronization contract and the filesystem/storage stack determine
power-loss durability. An active WAL database should be backed up with a
consistent SQLite backup mechanism, rather than copying only its main file.
