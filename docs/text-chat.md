# Durable local text chat

This slice connects the public API, the daemon's native execution supervisor,
SQLite, and the consumer CLI through the shared `ProviderClient` interface.
OpenCode Go, Zen, and Codex provide structured text/tool requests. Codex uses
Platform keys or saved ChatGPT authorization.
[Structured execution](structured-execution.md) now extends these
turns with opt-in workspace tools, artifacts, and per-turn model/settings.
[Workspaces](workspaces.md) and [tasks/batches](tasks-batches.md)
now build repository isolation and durable job orchestration on these primitives.

## Ownership and durability

All work uses this conversation model, including batches and child agents.
Durability applies to every conversation. Additional policy and membership are
metadata on the same record, and follow-ups use the same history and workspace.

The daemon owns accepted turns. Closing a CLI, losing its event connection, or
pressing Ctrl-C while following detaches that client. Explicit cancellation is a
separate durable command. Each session has one authoritative node and at most
one admitted turn at a time. Queued turns acquire provider resources only when
admitted; all sessions share the daemon's Tokio runtime and provider client.

The `durable-inbox` capability adds shared stars, Archive, Trash, and monotonic
read watermarks to session snapshots. `/v1/sessions/{id}/inbox` accepts a patch
with a command ID and commits state, an event, and the receipt together. Trash
is restorable retention, separate from cancellation and permanent deletion.
Organization events do not make an already-read conversation unread.
`only_if_unset` imports legacy browser state without replacing existing daemon
state. Schema 10 adds nullable inbox state and daemon-owned compose drafts while
preserving conversation history and old command receipts.

`GET /v1/drafts` pages saved drafts; `PUT /v1/drafts/{id}` stores a bounded draft
and `DELETE` removes it. `only_if_absent` on a draft save preserves a newer
daemon copy during import. An optional `draft_id` on session creation consumes
that saved draft in the same transaction that accepts the initial prompt. A
rejected creation preserves it; retrying the command returns the original
receipt without creating another conversation. Clearing browser site data
cannot restore trashed conversations or remove daemon-saved drafts.

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
assistant messages and paired tool results. Later queued messages are excluded from an earlier turn's
provider context. Completed replies and committed tool steps are reusable context; interrupted
thinking is discarded. Context is bounded rather than silently truncated.

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

Tool-enabled sessions now reconcile unfinished calls into failed paired results
with possible unknown external effects. Completed tool steps survive interruption.
No operation is replayed automatically; see [structured execution](structured-execution.md#durability-and-recovery).

## Public surface

All chat endpoints require the existing local bearer token. Health stays
anonymous. The daemon accepts loopback listeners only.

| Method and route | Behavior |
| --- | --- |
| GET /v1/models | Known executable Go, Zen, and Codex models with per-provider readiness and capabilities |
| GET, POST /v1/sessions | Paginated listing or durable creation |
| GET /v1/sessions/{id} | Session configuration, revision, and event watermark |
| PATCH /v1/sessions/{id} | Rename one conversation thread with `{"command_id":"rename-1","title":"Readable title"}` |
| GET, POST /v1/sessions/{id}/messages | Paginated history or durable message/turn acceptance |
| POST /v1/sessions/{id}/fork | Copy history through a selected message into a new conversation |
| PATCH /v1/sessions/{id}/messages/{message_id} | Edit user/assistant text in a new fork, discarding the original suffix |
| DELETE /v1/sessions/{id}/messages/{message_id} | Remove that message and all later messages from an idle conversation |
| GET /v1/sessions/{id}/instructions | Paginated durable steering instruction status/history |
| GET /v1/sessions/{id}/events | Replay from `after`; optional NDJSON following |
| GET /v1/sessions/{id}/queue | Unsent prompts in FIFO order |
| PATCH /v1/sessions/{id}/queue/{message_id} | Edit unsent text and/or after-turn/after-finish delivery in place |
| DELETE /v1/sessions/{id}/queue/{message_id} | Cancel only this unsent prompt |
| GET /v1/turns/{id} | Outcome, message IDs, requested/resolved model, and usage |
| POST /v1/turns/{id}/cancel | Idempotent explicit cancellation |
| POST /v1/turns/{id}/pause, /resume | Durable pause at safe boundaries and explicit fresh admission |

## Image input

Health advertises `image-input`. Clients upload raw image bytes first, then
attach the returned artifact ids to a user message:

```sh
# Upload returns {"id": "<sha256>", "sha256": "<sha256>", "size_bytes": N, "media_type": "image/png"}
curl -H "Authorization: Bearer $TOKEN" -H "Content-Type: image/png" \
  --data-binary @photo.png http://127.0.0.1:7331/v1/artifacts
hivectl session send SESSION --text "What is in this image?" --image photo.png
hivectl chat --prompt "Describe this" --image photo.png
```

`POST /v1/artifacts` accepts `image/png`, `image/jpeg`, `image/gif`, and
`image/webp` up to 20 MiB. Magic bytes must match the declared `Content-Type`
(else `400 invalid_image` or `415 unsupported_media_type`); oversize bodies
return `413 image_too_large`. Storage is content-addressed (`id == sha256`),
so re-uploading the same bytes is idempotent and needs no command id. Missing
artifact storage returns `503 artifact_unavailable`.

`SendMessageRequest` and `CreateSessionRequest` (initial prompt) carry
`images: [{ "artifact_id": "<sha256>" }]`. At most 16 images per message. Text
may be empty when images are present. Each id must exist in the artifacts table
with an allowed image media type (else `404 not_found` / `400 invalid_request`),
and the turn's effective model (including a per-message `model` override) must
advertise `image_input` in `/v1/models`, otherwise the send is rejected with
`unsupported_capability`. Command-id replay with different images conflicts.
Uploads target the daemon that owns the session; the peer gateway is not
extended for uploads.

The stored user message holds one text block first plus one `image` block per
attachment (`BlockContent::Image { artifact_id, media_type }`), exposed through
history, queued-message inspection, forks, and edits (queue text edits and
message edits keep the images). `next-boundary`/`immediate` steering is
text-only and rejects image attachments. History prints image blocks as
`[image <media_type> <short id>]`; bytes stream from
`GET /v1/artifacts/{id}/content`. At dispatch the daemon verifies each file's
sha256 against its id and encodes data URLs for the provider (Chat Completions
`image_url`, Responses `input_image`, Anthropic base64 `image`); missing or
corrupt images fail the turn as `image_unavailable` instead of being dropped.
Histories replayed to a model without `image_input` substitute
`[image omitted: <model> does not accept image input]`. Context budgeting counts
a fixed 2,000-token estimate per image rather than the wire base64 length.

## Editing queued prompts

Health advertises `queue-editing`. Queue PATCH accepts `command_id` plus optional
`text` and `delivery`; at least one change is required. Text must be nonempty,
and delivery must be `after-turn` or `after-finish`. The message keeps its FIFO
position, frozen model and settings. DELETE accepts `command_id`. Both return a
command receipt and commit `queued_message_updated` or `queued_message_cancelled`
with the mutation. Requests serialize with dispatch: a message already sent or
cancelled returns 409, without changing history or cancelling an active run.
Retried command IDs return their original receipts even after dispatch; changed
payloads under an existing command ID conflict. IDs belonging to another session
return 404. These routes also have typed native client methods.

## Forking, editing, and deleting history

Health advertises `chat-history-editing`. All three mutations require a
`command_id`, accept an optional `expected_revision` of the source session, and
commit the receipt, state, and semantic event together. A repeat returns the
original receipt even after the source message is deleted or the daemon restarts.
Reusing a command ID with a different operation, target, or payload conflicts.
Messages must belong to the specified session. Internal compaction messages are
not valid targets.

`POST /v1/sessions/{id}/fork` accepts `message_id` and an optional `title`. The
new conversation contains history **through and including** that message and
inherits the session's model, settings, execution policy, and budget limits.
Its receipt identifies the new session and the copied selected message. It does
not start inference. Copied messages preserve their original provider/model
identity. Messages and turns have independent IDs, while
`session.fork` records the source session/message and whether this was an edit.
The original conversation remains unchanged. A fork of running work is a
committed snapshot; provisional deltas are not copied, and inherited unfinished
turns become interrupted historical records rather than queued execution.

`PATCH` accepts replacement `text`. Editing a user message copies only its
preceding history, accepts the replacement as a new user turn, and generates a
new response using the inherited session defaults. Editing assistant text copies
through that message and stores the replacement without starting inference.
Assistant edits discard that message's original provider continuation and tool
proposals. Tool-result messages cannot be edited. Neither edit includes later
messages, and neither changes the source session.

`DELETE` removes the selected message and its entire suffix from history,
canonical message lookups, public run output, child context selection, and future
inference. Outstanding queued, running, paused, or awaiting-child turns return
`409 history_busy`; cancel them and wait for terminal state before deletion.
Deletion clears compacted context because a saved summary may contain removed
facts. History ordinals and event cursors remain monotonic; a later message
continues after the retained execution records. `messages_deleted` identifies
the original cutoff, and clients refresh history when receiving that event.
The CLI follower also skips missing canonical records when replaying events
committed before deletion, while continuing to advance its durable cursor.

A cut inside an assistant/tool exchange preserves the retained public messages
but excludes that entire incomplete exchange from inference. Completed paired
exchanges preserve structured blocks and private provider continuation. Forks
reference the original immutable request records; inherited turns cannot be
retried as execution. Deletion retains internal message tombstones, receipts,
requests, tools, artifacts, lineage, and spent budgets for audit and recovery;
it is a transcript operation, not a purge of execution records or filesystem
effects. Existing forks retain their copied history after source deletion.

Each fork inherits the required selected workspace ID and creates no directory
or worktree. If independent filesystem state is needed, create a worktree
explicitly before creating the fork. Sessions using one workspace share its canonical
directory and can execute concurrently. Transcript operations do not undo or replay
tools.
The new fork starts its own operation ledger; inherited history is not charged
as new inference or tool execution.

The CLI prints message IDs in `hivectl session history`. For example:

```sh
hivectl session fork SESSION --message MESSAGE --command-id fork-1
hivectl session rename SESSION "Readable title" --command-id rename-1
hivectl session edit SESSION --message MESSAGE --text "Revised prompt" --command-id edit-1
hivectl session delete-message SESSION --message MESSAGE --command-id delete-1
```

`session edit` follows a new user turn unless `--detach` is supplied. Its input
also supports `--prompt-file` or stdin. Each operation supports
`--expected-revision`; fork accepts `--title`. JSON output includes the receipt
and new session ID, allowing scripts to continue the branch explicitly.

Model readiness reports local configuration. It does not prove account
entitlement, provider availability, or a successful paid inference. Unsupported
models fail validation; an explicitly selected model is never silently replaced.
Provider credentials stay in private files under the daemon's XDG data directory
and out of session records, public responses, and diagnostics. The
[credential API/CLI](provider-credentials.md) sets keys while the daemon runs;
new admissions use the saved connection and active turns retain their original
connection. Legacy environment keys are imported only when no saved key exists.

The steering inbox is bounded to 32 pending instructions per logical turn.
Plain-text turns allow an initial request, two output-limit continuations, and
eight compaction requests, plus one request per steering instruction or resume,
capped at 64 total requests. Workspace execution keeps its configured `max_model_requests`
as a hard cumulative limit; new interactive workspaces default to `u32::MAX`
instead of stopping after 16 requests. `next-boundary` applies between completed inference
and tool steps, never in the middle of a token stream.

The default model is `opencode-go/glm-5.3-flash`, with a 4,096-token output cap
for providers that accept a cap and four concurrent requests across sessions.
Codex's ChatGPT subscription mode has no output cap; explicitly requested caps
are rejected before acceptance. Platform Codex and Go/Zen defaults retain the
configured cap. `default_model`,
`max_output_tokens`, `context_token_budget`, and `execution_concurrency` can be
set in the daemon TOML configuration; session defaults and accepted per-turn overrides are frozen
for execution. The execution
queue allows 32 queued turns per session and 1,024 globally. Provider context is
limited to 8,192 messages and 16 MiB. The default cost cap is 275,000 tokens;
`settings.full_context: true` / `--full-context` opts in to the model's verified
full window. Automatic compaction keeps the inference view
within these bounds while retaining the full transcript. Historical sessions
already beyond the cap are summarized in bounded pages instead of rejected.
An individual input that cannot be reduced still records `context_limit`.
Visible text is limited to 1 MiB. Lists have at most 200 items per page, and history pages also have a 16 MiB encoded JSON bound.

`--default-model`/`SLOP_DEFAULT_MODEL` overrides the configured model.
`--provider-base-url`/`SLOP_PROVIDER_BASE_URL` selects an explicitly trusted Go
endpoint. `--opencode-zen-base-url`/`SLOP_OPENCODE_ZEN_BASE_URL` and
`--codex-base-url`/`SLOP_CODEX_BASE_URL` select the corresponding endpoints.
These overrides support controlled gateways and local transport fixtures. HTTP
is limited to loopback hosts; other endpoints require HTTPS. Embedded credentials,
query, and fragment components are rejected. Each setting controls only the
destination that receives that provider's credential.

Event replay reads committed facts after the cursor. Streaming subscribers have
bounded buffering and can reconnect using the last durable sequence. Losing
provisional deltas does not lose committed messages or stop execution. Clients
reconcile display against canonical message records when a turn ends.
CLI turn following uses a finite replay when the turn is already terminal,
including when the reconnect cursor has passed its completion event. Canonical
replies are fetched by message ID without paging through the full conversation;
deleted replies remain absent.

Every message stores and exposes `provider` and `model`. User messages, including
applied steering, record the turn's selected provider/model. Assistant messages
record their own request's reported model when supplied, otherwise its selected
model; tool results inherit that originating request's identity. These fields
survive restart and later provider/model changes. Schema 12 backfills existing
messages from their request or turn records, rather than current session defaults.

A new turn may change provider in the same session by specifying a qualified
`model`, such as `opencode-zen/glm-5.3`. The daemon carries forward canonical
text and paired tool exchanges, and uses private continuation only with its
original provider/model/wire. The stored transcript and private records are
preserved. Model/settings remain frozen within an active turn; switching applies
to a new turn and does not change the session's defaults.
Switching from another provider to Codex subscription mode omits an inherited
output-token cap; explicitly requesting one still returns `unsupported_capability`.

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
credentials or paid calls. The real-binary credential smoke now includes offline
Zen and Codex text turns and verifies key activation after restart. A dedicated
Codex fixture also covers structured tools, private replay, account selection,
managed worktrees, and child tasks. Live provider tests remain explicit opt-ins with
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

### Last user interaction

`SessionResponse.last_user_activity_at` is a nullable Unix-millisecond timestamp
from the authoritative daemon clock, persisted in schema 25 and returned by both
session detail and session-list APIs (including peer inspection). It records newly
accepted prompts (including initial prompts and explicit task/batch submissions
and retries), immediate/next-boundary steering, and queued-input edits. User
message edits that submit a replacement prompt record activity on the new branch.
Agent-authored child prompts and child-send operations do not count.

The update is atomic with command acceptance. Idempotent retries, rejected
commands, assistant output, deferred queue dispatch, recovery, and organization
changes (rename/read/star/archive/trash) do not refresh it. Values do not decrease
if the daemon clock moves backward. Empty sessions and migrated sessions without
known input times return `null` until new input is accepted; migration does not
invent historical times. This is input activity, not a generic last-modified time.
Session-list pagination remains creation-ordered; clients can use this field for
recency presentation without fetching message history.

### Execution and review status (ready)

`SessionResponse.status` is a daemon-derived status returned by session detail,
list, and peer inspection. It is independent of inbox organization flags and is
advertised by `session-status`. Clients do not set this status directly.

| Status | Meaning |
| --- | --- |
| `idle` | No visible non-history-only turn |
| `queued` | Work is waiting for admission |
| `running` | A turn is executing |
| `paused` | A turn is paused |
| `awaiting_children` | A turn is waiting for child work |
| `ready` | Latest work finished successfully and its completion has not been reviewed |
| `completed` | Latest work finished successfully and its completion is acknowledged (or no historical completion event is known) |
| `failed` | Latest turn failed |
| `stopped` | Latest turn was cancelled, interrupted, or incomplete |

Live work takes precedence over terminal outcomes (running, awaiting children,
paused, then queued); otherwise the newest visible non-history-only turn wins.
A completed older turn cannot make a thread ready while newer work is pending.
Forked history does not invent a new completion awaiting review.

Review uses the existing shared inbox cursor. After displaying a result, send
`POST /v1/sessions/{session_id}/inbox` with a fresh `command_id` and
`read_sequence` set to the displayed session's `last_event_sequence`.
`ready` compares that cursor to the latest turn's durable `turn_completed` event,
not to subsequent rename/star/archive events. GET/list/event subscriptions
never mark a thread read. A read made before completion, or a retry of an older
read command, cannot acknowledge a later completion. Ready survives daemon
restart and clears to completed when the completion is acknowledged. A subsequent
successful unreviewed result becomes ready again. Failed/stopped results do not
become ready merely because they are unread. The cursor is shared across clients,
not per-user. Archive/trash/star flags remain independent, and acknowledging a
result does not update `last_user_activity_at`.
