# Local persistence and artifacts

## Storage ownership

Each node owns a local data directory and a SQLite database. Clients access
records through the API. Peers receive replicated application records, never
write directly into the owner's database, and never share an active database file
over a network mount.

Use SQLite as a proposed local store, with schema migrations and a bounded
database execution path. No SQLite implementation exists in M0.

SQLite WAL allows readers alongside a writer but has one writer at a time and
requires same-host database access. It therefore fits a locally owned store with
serialized writes. See [SQLite WAL documentation](https://sqlite.org/wal.html).
Choose a current supported SQLite version and review its release/security notes
when adding the binding.

## Proposed data layout

```text
<data-dir>/
  node.json                 # durable node identity and display metadata
  daemon.lock               # OS-backed directory ownership lock
  state.sqlite3             # owner records and replica projections
  state.sqlite3-wal          # SQLite-managed when WAL is enabled
  state.sqlite3-shm          # SQLite-managed when WAL is enabled
  artifacts/
    <content-hash>/...       # bounded files and metadata references
  workspaces/
    <project-id>/<run-id>/   # daemon-managed worktrees when configured
  transfer-staging/
    <transfer-id>/...        # validated future handoff packages
```

The exact platform directories should follow Linux user-directory conventions
and Windows local application-data conventions. A configured data directory is
allowed. Workspace roots may be configured separately to control disk use.

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

An external tool invocation cannot be atomic with a SQLite transaction. Record
its intent first, then launch it, then persist the known result. Recovery treats
a missing result as uncertain rather than guessing it failed harmlessly.

A model stream is also external. Persist useful partial checkpoints and the
terminal outcome. Do not turn partial tool arguments into executable operations.

Artifacts have their own write protocol: write a bounded temporary file, compute
its content hash, complete/rename it, then register metadata and references.
Clean up abandoned temporary files conservatively. Never publish a durable event
that requires an artifact before the artifact is available.

## Database execution and retention

Start with a dedicated bounded database worker and short write transactions.
A synchronous binding can run there rather than block an async executor thread.
Separate small reads can use a controlled pool if measurements justify it.

Use foreign keys, schema versions, busy handling, and deliberate WAL checkpoint
policy. Propose FULL synchronous durability for command/ownership transitions;
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
| Tool intent without confirmed launch | Reconcile launch evidence before retry |
| Started tool without known completion | Inspect supervised process/outcome; otherwise require recovery |
| Incomplete model request | Record interruption; retry only under its bounded request policy |
| Outgoing committed handoff | Source remains deactivated for that session |
| Incoming prepared handoff | Do not activate without valid handoff evidence |

A durable outbox for peer replication contains committed application records.
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
