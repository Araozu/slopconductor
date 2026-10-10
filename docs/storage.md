# Local persistence and artifacts

## Storage ownership

Each node owns a local data directory and a SQLite database. Clients access
records through the API. Peers receive replicated application records, never
write directly into the owner's database, and never share an active database file
over a network mount.

The daemon implements a SQLite startup store for node identity, a schema
migration, an exclusive data-directory lock, and one bounded database worker.
Schema version 2 added sessions, text turns, ordered messages, command receipts,
and semantic events. Schema 3 adds structured blocks, private continuation,
frozen settings, model requests, tool intents/results, workspace roots, and
artifact metadata. Schema 4 adds durable steering instructions and
pause/resume/usage-uncertainty state. Schema 5 adds projects, managed session
workspaces, project events, and a session's managed-workspace reference.
See [structured execution](structured-execution.md) and
[projects/workspaces](projects-workspaces.md). The worker
uses bundled SQLite through `rusqlite`, WAL, FULL synchronization, foreign keys,
and a bounded busy timeout. Node identity and display-name changes commit in a
transaction before startup is announced.

SQLite WAL allows readers alongside a writer but has one writer at a time and
requires same-host database access. It therefore fits a locally owned store with
serialized writes. See [SQLite WAL documentation](https://sqlite.org/wal.html).
The bundled SQLite version includes the WAL-reset corruption fix. Verify the
linked version and relevant fixes when updating the binding.

## Data layout

```text
<data-dir>/
  daemon.lock               # OS-backed directory ownership lock
  state.sqlite3             # node identity, sessions, turns, messages, commands, events
  state.sqlite3-wal          # SQLite-managed when WAL is enabled
  state.sqlite3-shm          # SQLite-managed when WAL is enabled
  credentials/
    local-api-token         # persistent private local API credential
    opencode-go-api-key     # optional saved provider API keys
    opencode-zen-api-key
    codex-api-key
    codex-chatgpt.json       # optional validated ChatGPT registration/tokens
  artifacts/
    <content-hash>/...       # bounded files and metadata references
  workspaces/
    <project-id>/<workspace-id>/ # managed worktrees allocated on admission
  transfer-staging/
    <transfer-id>/...        # validated future handoff packages
```

The lock, database, SQLite sidecars when needed, local API credential, and
[runtime provider credentials](provider-credentials.md) are implemented.
Provider files are created only when configured. Artifacts and lazily allocated
managed workspaces are implemented; transfer staging remains a future layout.

Linux config is `$XDG_CONFIG_HOME/slopconductor/config.toml` (default
`~/.config/slopconductor/config.toml`), and authoritative data is
`$XDG_DATA_HOME/slopconductor` (default `~/.local/share/slopconductor`). Relative
XDG values are ignored. Logs, when file logging is added, belong in
`$XDG_STATE_HOME/slopconductor` (default `~/.local/state/slopconductor`). No
separate application directory is created directly under the user's home.
Windows uses `%LOCALAPPDATA%/slopconductor`. Explicit absolute config/data paths
are supported. Workspace roots may be configured separately in future work.

Credential references belong in the database; actual secrets belong in an
OS credential store or explicitly chosen secure alternative. Logs are separate
operational output and must redact credentials.

## Proposed record families

| Records | Important fields |
| --- | --- |
| Nodes | Stable ID, display name, OS, optional physical-machine group |
| Projects | Logical ID, repository identity, local path mappings |
| Workspaces | Project, base commit, branch, path, owner run, reservation mode |
| Sessions | Stable ID, owner ID/epoch, title, lifecycle, context policy, revision |
| Messages | Session, author/role, content/artifact refs, completion status |
| Tasks | Session, goal, target, policy, budget, state, parent task |
| Runs | Task, attempt number, effective config, state, checkpoint, timestamps |
| Model requests | Run/turn, provider/account/model, request identity, outcome, usage |
| Tool invocations | Run/turn, invocation ID, inputs, side-effect class, known outcome |
| Batches and members | Matrix definition, frozen inputs, combination index, task links |
| Commands | ID, payload hash, target, acceptance result, applied status |
| Events | Session sequence, causal IDs, ownership epoch, versioned payload |
| Artifacts | Content hash, size, media type, provenance, retention/reference count |
| Peers and replica cursors | Endpoint, trust, source ID, received sequence, freshness |
| Transfers | Source/target, checkpoint digest, durable handoff state and evidence |

Core domain types and public DTOs are mapped to these records; SQL schemas are
not the wire contract. Index by owner/session/sequence and by commonly filtered
task/batch states. Paginate histories and listings.

## Transactions and semantic durability

Within one transaction, validate a mutating command, record its idempotency key,
change local state, and append the resulting durable events. Commit before
returning an accepted acknowledgement. Publishing committed events can follow
the transaction; reconnect replay fills any notification gap.

The [text-chat slice](text-chat.md) implements atomic session/message acceptance,
deduplication, provider intent, visible checkpoints, and terminal outcomes.
The implemented [structured tool slice](structured-execution.md) follows the
write protocol below. An external tool invocation cannot be atomic with a
SQLite transaction. Record its intent first, then launch it, then persist the
known result. On daemon/process failure, an unfinished call is marked failed
with that cause and any uncertainty about effects. It is not restored or
automatically replayed. The agent receives that failure and chooses its next
action; a failed record is not proof that nothing changed externally.

A model stream is also external. Persist bounded visible-text checkpoints and
the completed turn/terminal outcome. Interrupted thinking is discarded; only
completed supported reasoning and continuation are retained with the completed
turn. Do not turn partial tool arguments into executable operations.

Artifacts have their own write protocol: write a bounded temporary file, compute
its content hash, complete/rename it, then register metadata and references.
Clean up abandoned temporary files conservatively. Never publish a durable event
that requires an artifact before the artifact is available.

## Database execution and retention

The startup store uses a dedicated bounded database worker and short write
transactions. Synchronous SQLite calls run there rather than on the async
executor.
Separate small reads can use a controlled pool if measurements justify it.

Use foreign keys, schema versions, busy handling, and deliberate WAL checkpoint
policy. FULL synchronous durability is enabled for the startup store;
any weaker durability setting must disclose the power-loss consequences.

Retention is configurable for tool logs, transient checkpoints, completed
sessions, and artifacts. A retained event may reference an artifact, so
garbage collection must preserve referenced content or mark expired data
explicitly. Archiving a session is separate from deleting it.

Snapshots accelerate replay and projections. A snapshot records its schema
version and durable event watermark. It does not authorize a replica to execute.

## Restart reconciliation

At startup, classify work using recorded state:

| Recorded condition | Recovery behavior |
| --- | --- |
| Queued task, no external operation | Eligible for normal admission |
| Completed step with persisted result | Reuse recorded result |
| Paused run with valid checkpoint | Remain paused until explicitly resumed |
| Unfinished tool intent/call after daemon/process failure | Record failure and possible unknown effects; no restoration/replay; agent decides next action |
| Interrupted thinking | Discard incomplete thinking; never resume it as completed context |
| Incomplete model request | Record interruption and discard interrupted thinking; a new request follows explicit policy |
| Outgoing committed handoff | Source remains deactivated for that session |
| Incoming prepared handoff | Do not activate without valid handoff evidence |

Current startup restores node identity and chat history, marks running text turns
interrupted without reissuing inference, and leaves undispatched queued turns
eligible for admission. Paused turns remain paused; accepted steering attached
to a crashed running turn is rejected with a durable event. Unfinished tools receive paired failure records with
uncertain effects when started; completed steps are preserved and no operation
is replayed. Handoff recovery remains planned.
A future durable outbox for peer replication contains committed
application records.
Its backpressure does not prevent ordinary local execution indefinitely.

## Backup, export, and replication

An active SQLite database needs a consistent backup mechanism. The
[SQLite backup API](https://sqlite.org/backup.html) is a reference for producing
a consistent copy. Copying only the main database file while WAL is active is
not a general backup strategy.

A node backup may include its identity, database, and referenced artifacts,
while keeping credentials under a separate explicit backup policy. Restoring an
old backup must not accidentally resurrect ownership that was transferred away.

Session export is an application-level package with schema version, messages,
checkpoint, artifact manifest, and provenance. History replication is a
source-authored event exchange with cursors. Neither operation is raw
multi-writer database synchronization.

## Future migration compatibility

Keep absolute local paths and credential handles outside portable session
content. A checkpoint refers to logical projects/workspaces and tool capabilities;
the target supplies its mappings. Record provider-owned opaque context
separately so a destination can validate whether it is still usable.

See [migration](migration.md) for handoff and workspace packaging.
