# Structured execution and tools

**Implemented, 2026-10-08.** The daemon can run a bounded inference/tool loop
through OpenCode Go. Sessions without an execution policy retain plain chat
behavior. CLI and SDK consumers use public structured records; neither owns
execution. Zen has the same structured runtime adapter but is not selectable
for daemon sessions. Codex retains its text-only adapter bridge and rejects
structured tools or required continuation it cannot support.

## Request and conversation model

A user turn can contain several model requests, assistant messages, tool
invocations, and paired tool-result messages. Each has a stable daemon ID.
A provider call ID remains private and is scoped to its request; it is never
used as a globally unique invocation ID. Content blocks have stable IDs and
preserve text, refusals, tool proposals, arguments, result errors, artifact
references, and uncertainty about effects. `MessageResponse.text` remains a
visible-text projection for existing clients.

`ProviderClient::infer` accepts provider-neutral messages, generation settings,
and tool definitions. Gateway adapters translate Chat Completions, Responses,
and Anthropic Messages directly. Completed arguments must be JSON objects;
partial argument fragments are provisional. Recognized upstream completion
and successfully parsed proposals precede execution. Truncated, malformed,
contradictory, or output-limited responses launch no tools. The adapter proposes
calls; the daemon validates policy and dispatches them.

Completed provider reasoning/signatures/encrypted continuation stay in a
private database column. They are excluded from public history, event frames,
CLI output, and debug formatting. Compatible continuation is replayed to the
same provider, requested model, endpoint, and credential scope. The scope is a
fingerprint, not a stored API key. Required incompatible continuation fails
with `context_incompatible` instead of being silently discarded. Incomplete
continuation is discarded. Database file permissions protect this material;
the database is not encrypted.

## Models and generation settings

Session creation records default model/settings. Message acceptance can select
a model and settings for that turn:

```json
{
  "command_id": "stable-command-id",
  "text": "Review the change.",
  "model": "opencode-go/glm-5.3",
  "settings": {"max_output_tokens": 512}
}
```

The daemon freezes the effective selection at acceptance, including every
subsequent inference request within that turn. Overrides do not mutate session
defaults, queued turns, or an in-flight request. Request records retain both
requested settings and effective settings, plus requested/reported model and
per-request usage. Turn usage aggregates model requests without double-counting
cumulative stream snapshots. Unknown counters remain unknown.

When settings are omitted, session settings apply. When a settings object is
provided, a missing token cap falls back to the session cap; absent/null
`reasoning_effort` means provider default. Legacy `max_tokens` is supported at
session creation; conflicting legacy/structured caps are rejected.

`reasoning_effort` is part of the contract and CLI (`--effort`). No daemon model
currently advertises supported effort values: explicit efforts return
`unsupported_capability` before acceptance and inference. A future adapter can
advertise model-specific values and encode them without changing turn ownership
or the wire envelope. There is no arbitrary provider-parameter passthrough.

Per-turn model selection works with compatible canonical context. A history
requiring another model's private continuation is rejected explicitly; portable
context conversion/reset is future work. New native clients check advertised
capabilities before delivering tool policies or model/settings overrides to an
older daemon, so an older server cannot silently ignore those selections.

## Workspace authority and tools

Creating a tool-enabled session requires an explicit execution policy with an
absolute workspace root on the daemon's machine and a nonempty tool allowlist.
The root is canonicalized before acceptance. Only one admitted turn can use
that same canonical root across sessions. This lease does not cover nested
roots, external editors/processes, or a second daemon with another data directory.
Managed Git worktrees and project reservations are future work.

| Tool | Behavior |
| --- | --- |
| `list_files` | Sorted, bounded directory entries within the workspace |
| `read_file` | Bounded UTF-8 reads with byte offsets and a continuation offset |
| `write_file` | Create a new UTF-8 file; existing files are never overwritten |
| `apply_patch` | Replace one unique exact text match; reject stale/ambiguous matches, preserve permissions, and rename atomically |
| `shell` | Supervised shell command with bounded output, timeout, cancellation, and process-group/Job Object cleanup |

File tools use directory handles to reject absolute paths, traversal, and
symlink escapes. Special files are rejected; Unix opens are nonblocking so a
FIFO cannot occupy a worker indefinitely. File work runs on a bounded blocking
path, with a shared four-slot tool limit. Once a file operation starts,
cancellation waits for its actual outcome; dropping an awaiter cannot free its
slot while the blocking operation continues.

Shell uses `/bin/sh -c` on Unix and PowerShell without profiles on Windows.
Its working directory must be within the workspace. Only PATH, SystemRoot,
WINDIR, TEMP, TMP, LANG, and LC_ALL are inherited. Provider credentials are not
forwarded. The command itself grants the shell the user's machine permissions;
working-directory policy is not an OS sandbox. Shell commands can invoke Git
and other actual tools; there are no dedicated Git/browser/orchestration tools.

Normal cancellation/timeout targets the process group on Unix or Job Object on
Windows and waits a bounded time for cleanup. Background descendants have no
independent lifetime. Hard daemon death on Unix can leave subprocesses running;
restart reports unknown effects and does not assume cleanup or retry execution.
Process groups do not constrain a command that deliberately escapes supervision.

## Durability and recovery

Before a provider request, the daemon commits a request record and its assistant
message ID (`pending` until visible checkpoints or completion). Completion commits canonical blocks, private continuation, usage,
and all tool intents in one transaction. A separate committed start record
precedes each side effect. Tool results and artifact metadata commit before the
next request can consume them. Persistence retries repeat only the result write,
never the tool operation. Failure to persist an outcome stops new admissions.

On restart, running model requests and turns become interrupted. Every tool
without a committed outcome gets one paired failure result under its original
IDs. An unstarted intent reports no launch; a started invocation reports
`effects_unknown: true`. Completed results remain facts in subsequent context,
including completed steps from interrupted/cancelled/failed/incomplete tool turns. Queued,
undispatched turns remain eligible. Sending a new message is the explicit way
to continue; there is no automatic replay of unknown operations.

Artifacts are content-addressed SHA-256 files in the private data directory.
A bounded temporary file is synchronized and published before its metadata
and references commit. Public downloads are authenticated and streamed. The
native SDK verifies committed length and checksum at EOF; the CLI publishes a
new destination only after verification and refuses to overwrite an existing
file. Retention, artifact garbage collection, exports, and quotas on accumulated
history/disk usage remain future work.

SQLite schema 3 migrates schema 1/2 transactionally, preserving node identity,
command payloads/receipts, text history, and events. Old history page cursors
should be discarded after migration because conversation ordinals now reserve
space for intermediate steps. Event sequence cursors remain valid. Older daemon
binaries reject the newer schema; migration is not a downgrade mechanism.

## Public API and streaming

Existing session/message/event/turn routes are extended additively in API v1.
Plain mutation payloads retain their original serialization. All routes below
require the existing local bearer token, including artifact bytes.

| Route | Record |
| --- | --- |
| `GET /v1/capabilities` | Tool schemas, selection support, and loop bounds |
| `GET /v1/models` | Executable Go models, local readiness, tool/effort/streaming capabilities |
| `GET /v1/messages/{id}` | Canonical structured message |
| `GET /v1/turns/{id}/requests` | Paginated model requests and usage/settings |
| `GET /v1/turns/{id}/tools` | Paginated invocation states/results |
| `GET /v1/tools/{id}` | One canonical invocation |
| `GET /v1/artifacts/{id}` | Hash, committed length, and media type |
| `GET /v1/artifacts/{id}/content` | Streamed artifact bytes |

NDJSON uses the existing durable/delta/heartbeat envelope. Delta fields now
include message/block/request/invocation IDs, stream ID, chunk index, and `kind`
(`text`, `tool_arguments`, `stdout`, `stderr`). These frames are transient and
can be missed. Durable events include request start/finish, canonical assistant
completion/checkpoints, tool intent/start/result, and turn outcomes with causal
IDs. Consumers deduplicate transient chunks and fetch canonical messages/tools
after durable events; only durable events advance the reconnect cursor.
A slow or disconnected subscriber cannot hold up the agent loop.

A client can render tool calls and outcomes entirely from these public records.
Capability metadata describes local implementation support, not account
entitlement or a successful live provider request. New block kinds have a
public unknown variant so consumers can evolve without interpreting private
upstream payloads.

## Limits and CLI

Context is at most 256 messages/1 MiB; individual files at most 1 MiB; a read
returns at most 256 KiB; a directory returns at most 512 entries. Each model
response proposes at most 16 tools, with at most 64 KiB of arguments per call.
Visible assistant output is at most 1 MiB and encoded history pages at most
16 MiB (text and blocks coexist for compatibility).

Default workspace policy allows 32 calls and 16 model requests per turn, a
30-second shell timeout, and 1 MiB of raw combined tool output. Policy maxima
are 128 calls, 64 requests, 300 seconds, and 8 MiB. Tool previews are 16 KiB;
larger output is an artifact. Shell JSON encoding can expand capped raw output;
the client artifact bound is 64 MiB. There are four tool slots and the existing
configurable turn admission limit (default four). These are bounds, not a
memory/performance benchmark.

```sh
slop chat --workspace /absolute/project --prompt "Inspect this project."
slop chat --workspace /absolute/project --tool read_file --tool apply_patch --tool shell --prompt "Fix the failing test."
slop chat --session SESSION_ID --model opencode-go/glm-5.3 --max-output-tokens 512 --prompt "Review this."
slop capabilities
slop turn requests TURN_ID
slop turn tools TURN_ID
slop artifact download ARTIFACT_ID --output output.json
```

`--workspace` alone enables `read_file` and `list_files`. Specifying `--tool`
sets the exact allowlist; writes and shell need explicit selection. Workspace
policy is chosen at session creation. `--model`/settings on a new chat set its
defaults; on a resumed chat they override the next turn. JSON following includes
canonical message/tool snapshots, deltas, receipts, and terminal records.
Closing the CLI detaches; `slop turn cancel TURN_ID` explicitly cancels execution.

## Verification and remaining scope

Rust fixtures cover all three structured wire shapes, byte-split SSE/Unicode,
private continuation, incomplete/malformed proposals, exact patch preconditions,
symlink escapes, output bounds, process cancellation, workspace leases,
frozen settings, migration, recovery, and artifact integrity. The real-binary
smoke check adds a complete file/patch/shell workflow, canonical client rendering,
authentication on new routes, restart without tool replay, and per-turn selection.
All provider traffic in the new checks is offline. Linux is exercised; Windows
supervision is implemented but needs a native Windows run. No new live tool call
or paid model test is claimed.

General task/run controls, pause/resume, child agents, worktree orchestration,
account quotas, hosted tools, media attachments, reasoning summaries, remote
pairing, and graphical clients remain outside this slice. Transport assumptions
follow the [provider references](provider-interface.md#decoder-references), plus
[OpenAI function calling](https://developers.openai.com/api/docs/guides/function-calling)
and [Anthropic tool definitions](https://platform.claude.com/docs/en/agents-and-tools/tool-use/define-tools).
