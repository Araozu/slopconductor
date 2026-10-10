# Daemon startup and chat durability contract

Status: startup foundation and durable local orchestration implemented, 2026-10-10.
The daemon exposes health, authenticated node identity, and the
[text-chat slice](text-chat.md) with its session store and execution supervisor.
[Structured execution](structured-execution.md) adds opt-in file/shell tools,
artifacts, and interrupted-step recovery. [Durable tasks and matrices](tasks-batches.md)
and [native child tasks](child-tasks.md) share turn execution and recovery.

## Implemented scope

Startup implements ownership, configuration, local authentication, bounded
shutdown, and SQLite initialization for persistent node identity and chats.
Text turns use the transaction and recovery contract below; interrupted thinking
is not automatically resumed. Tool intents/results and artifacts now follow
the implemented protocol in [structured execution](structured-execution.md).

### Directories

On Linux, resolve these application directories according to the
[XDG Base Directory Specification](https://specifications.freedesktop.org/basedir/latest/):

| Purpose | Location | Default when the XDG variable is unset or empty |
| --- | --- | --- |
| Configuration | `$XDG_CONFIG_HOME/slopconductor/config.toml` | `~/.config/slopconductor/config.toml` |
| Authoritative database, credentials, and artifacts | `$XDG_DATA_HOME/slopconductor/` | `~/.local/share/slopconductor/` |
| Operational logs, if file logging is added | `$XDG_STATE_HOME/slopconductor/` | `~/.local/state/slopconductor/` |

Conversations are user data. Keep node identity and future session/step records
in the same database so their related mutations can be transactional. State
and cache directories must never become the only copy of conversation content.
Do not create unused log/cache/runtime directories, a top-level home directory,
or a project-local database. Runtime sockets, if introduced later, belong under
`XDG_RUNTIME_DIR`; the persistent ownership lock belongs beside the database.

Support explicit configuration/data-directory overrides through flags and
environment variables. Use flags before environment settings, file settings,
and defaults. Ignore relative XDG values as the specification requires; reject
relative explicit directory overrides. An explicitly selected missing config
file is an error; a missing default config uses documented defaults.

Use native per-user local application-data directories on Windows, with the
same explicit overrides. Windows config defaults to
`%LOCALAPPDATA%/slopconductor/config.toml`, and data defaults to
`%LOCALAPPDATA%/slopconductor`. Credential paths must remain within canonical
`LOCALAPPDATA`; this implementation relies on the user's profile ACL inheritance
rather than editing arbitrary Windows ACLs. Locations outside that root are
rejected. Test path resolution without modifying the developer's
real profile or changing process-global environment during concurrent tests.

CLI flags take precedence over their environment equivalents:

| Flag | Environment | Purpose |
| --- | --- | --- |
| `--config` | `SLOP_CONFIG` | Select an absolute config file |
| `--data-dir` | `SLOP_DATA_DIR` | Select an absolute private data directory |
| `--listen` | `SLOP_LISTEN` | Override the loopback listener |
| `--name` | `SLOP_NODE_NAME` | Override the persisted display name |
| `--default-model` | `SLOP_DEFAULT_MODEL` | Override the configured provider/model used for new chats |
| `--provider-base-url` | `SLOP_PROVIDER_BASE_URL` | Select the explicitly trusted Go destination |
| `--opencode-zen-base-url` | `SLOP_OPENCODE_ZEN_BASE_URL` | Select the explicitly trusted Zen destination |
| `--codex-base-url` | `SLOP_CODEX_BASE_URL` | Select the explicitly trusted Codex destination |

All TOML keys are optional; unknown keys are rejected and files are capped at
64 KiB. The display name is limited to 128 UTF-8 bytes without control characters.
An omitted name preserves existing identity/name on restart.

| TOML key | Default | Accepted range |
| --- | --- | --- |
| `listen` | `127.0.0.1:7331` | Loopback socket address |
| `node_name` | Hostname when valid, otherwise `Slop Conductor node` | 1–128 UTF-8 bytes |
| `database_queue_capacity` | `128` | 1–4096 requests |
| `database_busy_timeout_ms` | `5000` | 1–60000 ms |
| `shutdown_timeout_ms` | `10000` | 100–300000 ms |
| `default_model` | `opencode-go/glm-5.3-flash` | Verified compiled provider catalog model |
| `max_output_tokens` | `4096` | 1–65536 tokens, frozen in each created session |
| `execution_concurrency` | `4` | 1–64 concurrent turns globally |
| `provider_base_url` | Official Go endpoint | HTTPS or loopback HTTP; no embedded credentials, query, or fragment |
| `opencode_zen_base_url` | Official Zen endpoint | HTTPS or loopback HTTP; no embedded credentials, query, or fragment |
| `codex_base_url` | Official Codex endpoint | HTTPS or loopback HTTP; no embedded credentials, query, or fragment |

### Exclusive startup and stable identity

1. Read and validate bounded configuration before admitting requests. Initially
   configure only settings that have implemented behavior: listener, node name,
   database queue/busy limits, shutdown deadline, and bounded chat settings.
2. Create the private application data directory and resolve its canonical path.
3. Hold an exclusive OS-backed lock on `daemon.lock` for the daemon's lifetime,
   before opening/migrating the database or initializing credentials. A second
   daemon using the same directory must fail even when its port differs.
4. Use Rust's safe
   [`File::try_lock`](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock).
   Keep the lock file in place; do not unlink it on exit. Process termination
   releases the lock, so a stale file never requires deleting a PID record.
5. Initialize the schema and stable opaque node ID transactionally. Store
   identity in SQLite instead of a separately authoritative `node.json`.
6. Bind the listener and announce readiness only after initialization succeeds.
   Keep loopback-only listening for this phase.

### SQLite startup foundation

Use `rusqlite` with bundled SQLite, committed dependency versions, migrations,
and no ORM. The
[rusqlite release](https://github.com/rusqlite/rusqlite/releases)
selected in `Cargo.lock` is 0.40.2, bundling SQLite 3.53.2. Validation verifies
the linked SQLite version. The daemon rejects a SQLite build older than 3.51.3
before changing journal mode or schema. SQLite must include the
[WAL-reset corruption fix](https://sqlite.org/wal.html#walresetbug).

Use one daemon-wide database worker with a bounded request queue and short
transactions; no database thread or connection per conversation. Keep SQL in
the daemon's concrete storage module, with runtime repository interfaces added
when execution needs them. Core and protocol crates remain free of persistence
dependencies.

Set and verify `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`, and
bounded busy handling. Initialize only the startup schema needed now; add
session, command, and execution tables through subsequent migrations when their
behavior is implemented. Reject unsupported newer schemas without modifying
them. A migration failure must leave no partially applied migration or new
identity.

SQLite owns the database/WAL files. Do not rewrite or rename the database on each
step. Commit is the durability boundary; a WAL checkpoint is not required after
each message. [WAL with FULL synchronization](https://sqlite.org/pragma.html#pragma_synchronous)
provides atomic durable transactions, subject to the filesystem and device
honoring synchronization. This protects committed records, not bytes that have
not yet been committed.

### Local authentication and shutdown

Create a persistent random local API bearer token in a private credentials file
under the selected data directory, separate from the database and logs. Publish
it through a synchronized same-directory temporary file and atomic no-clobber
publication (link/rename), synchronizing the
file and directory metadata where the platform supports it. Keep Linux files
private and use protected per-user Windows storage; fail rather than claim
privacy for an unsupported insecure credential location.

Keep the existing minimal health response available without a token. Add an
authenticated node-identity query to make persistence and authentication
observable. Add native-client token-file support and a minimal CLI node query;
credentials must not appear in CLI arguments, JSON output, logs, or Debug
representations. Reject missing/invalid authorization on protected routes.
Runtime provider configuration/login is implemented separately in
[provider credentials](provider-credentials.md). Remote pairing remains planned.

For the default Linux data path:

```sh
slop --json status
slop --token-file ~/.local/share/slopconductor/credentials/local-api-token --json node
```

The CLI discovers the default token in the platform data directory for loopback
queries and accepts `SLOP_TOKEN_FILE` as an override. Other origins require
an explicit `--token-file`, so a changed endpoint does not silently select a
local credential. `slop status` never sends that token. Client debug/error output
redacts it, and the token-file client checks service/API compatibility before
sending authorization. This check is not server authentication: select a trusted
endpoint and credential file.

Handle Ctrl-C and Unix SIGTERM. Stop admitting new requests, drain accepted
database writes within the configured deadline, close the store, then release
the directory lock. Report an incomplete shutdown instead of claiming every
write drained. Crash recovery must depend on committed SQLite records, not on
this shutdown path executing.

## Transaction contract for session/runtime work

The [text-chat slice](text-chat.md) now implements session/message acceptance,
provider intent, visible checkpoints, terminal outcomes, and cancellation using
this contract. Structured execution, artifacts, and [tasks/run attempts and
matrices](tasks-batches.md) also implement it. [Native child tasks](child-tasks.md)
share turn execution, waits, and recovery.

| Boundary | Records to commit atomically before advancing |
| --- | --- |
| User message accepted | Command ID and canonical payload hash, acceptance result, message, session revision, ordered semantic event |
| Provider request dispatched | Request identity, selected model/account/settings, context/checkpoint references, dispatch intent |
| Partial provider output checkpointed | Bounded partial content and block identity, incomplete status, next checkpoint position, event |
| Provider turn completed | Canonical assistant content, supported reasoning summaries, required opaque continuation, tool-call declarations, usage, terminal outcome, next-step checkpoint, events |
| Tool dispatch authorized | Complete validated inputs/call ID, policy/workspace, side-effect classification, launch intent, event |
| Tool result recorded | Known outcome/result or durable artifact references, matching tool-result context, next-step checkpoint, event |
| Steering/cancellation applied | Command application state, resulting lifecycle/checkpoint changes, ordered event |

Commit accepted commands before replying and commit semantic events before
publishing them. Repeated command IDs return the original acceptance result;
different payloads under the same ID conflict. Publish notifications after
commit; durable replay fills a crash gap between commit and notification.

Use provisional streaming deltas for responsive display. The text supervisor
checks for changed visible content every 250 ms and commits final outcomes
immediately. Transient frames contain at most 16 KiB of visible text. Benchmark
the cadence before treating it as a latency guarantee. A crash
can lose the uncommitted streaming tail. A displayed provisional delta is not
proof of completion. Interrupted thinking is discarded; do not checkpoint it
as recoverable completed context. Persist supported completed reasoning with
its committed completed turn. Never execute partial tool arguments. Preserve provider
continuation as bounded owner-side data, separate from displayable summaries and
credentials. Current text adapters need expansion before structured tools and
continuation can use this contract.

Large artifacts need their own ordered publication: write and synchronize a
temporary file, atomically publish it, then commit its metadata/references.
Reconciliation can collect unreferenced leftovers. Never commit a completion
event referencing a file that has not been durably published.

## Recovery semantics

Database atomicity does not make external requests or filesystem/process side
effects atomic with a transaction. Record intent before dispatch and the known
outcome afterward. If persistence fails, do not proceed to another dependent
tool/model step or acknowledge success.

On restart, preserve committed completed chat steps and reconstruct their next
checkpoint. Preserve partial visible text as interrupted and discard interrupted
thinking. Retain paused/canceled
states. Queued work with no external operation can be admitted according to the
configured policy. A dispatch intent without a known result may cover both an
operation that never started and one that finished just before the crash.

An unfinished tool is recorded as failed due to daemon/process failure, with
possible unknown external effects. Do not restore or replay it. The agent uses
that failure result to decide whether to inspect effects or make a new call;
failed execution does not imply absence of changes. Interrupted thinking is
discarded rather than restored. A new inference requires an explicit bounded
policy and may consume quota again.
Startup restores facts and identifies chat interruptions; it does not silently
resume thinking. Continue an ordinary chat with a new message, explicitly
resume a paused run, or explicitly retry an interrupted job into a fresh attempt.

## Validation and documentation

For this implementation: exercise XDG defaults/overrides, invalid config,
identity persistence, concurrent startup against one directory, lock release
after forced termination, migration rollback, refusal of newer schemas, required
SQLite settings, token privacy/redaction, authorization, and bounded shutdown.
Use isolated temporary directories in smoke checks on Linux and Windows.

Run workspace formatting, Clippy with warnings denied, relevant behavioral
tests, and the real-binary smoke check. Update the existing architecture,
storage, protocol, implementation, and README descriptions to identify the
implemented scope precisely.

When chat persistence is implemented, add process-kill/failure-injection checks
before and after command commits, provider completion commits, tool dispatch,
and tool-result commits. Verify deduplication, state/event agreement, reuse of
completed results, discarded interrupted thinking, preserved interrupted visible
text, and failed unfinished tools without restoration or automatic replay.
Process-kill tests do not establish power-loss durability; that guarantee relies
on SQLite and the storage stack's synchronization contract.
