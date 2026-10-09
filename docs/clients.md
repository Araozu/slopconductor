# Independent clients and CLI-first delivery

## Shared rule

Every client is a consumer of the public protocol. It can hold UI state, local
preferences, peer endpoints, and cached projections. The owner daemon holds
execution, authoritative conversations, checkpoints, tool supervision, and
provider credentials.

A client must be replaceable without changing task ownership. Closing a client
does not stop a run. Opening an additional interface does not create another
agent runtime.

## CLI as the first complete surface

The CLI is both an interactive human entry point and a scriptable control tool.
It should ship useful local execution before the TUI, web, or Electron exists.

The implemented commands are `status`, `node`, `models`, `capabilities`, `artifact`
(show/download), `provider`
(status/set-key/login/login-status), `chat`, `session`
(list/show/history/send/follow), and `turn` (show/cancel), optionally with
`--json` and `--daemon`. Status remains anonymous. For loopback endpoints the
CLI discovers the token in the platform data directory; `--token-file` and
`SLOP_TOKEN_FILE` override it. Other origins require an explicit token file.
These work after building/installing the CLI or through Cargo:

```sh
cargo run -p slop-cli -- --json status
cargo run -p slop-cli -- --token-file ~/.local/share/slopconductor/credentials/local-api-token --json node
cargo run -p slop-cli -- chat --prompt "Reply briefly."
cargo run -p slop-cli -- chat --session SESSION_ID --prompt-file prompt.md
cargo run -p slop-cli -- --json session history SESSION_ID
cargo run -p slop-cli -- turn cancel TURN_ID
cargo run -p slop-cli -- provider set-key opencode-go --key-file /path/to/key
cargo run -p slop-cli -- provider login codex
```

Text chat supports interactive input, one-shot prompts, files, and piped stdin.
One-shot requests follow their accepted turn by default; `--detach` returns its
receipt immediately. JSON chat emits NDJSON receipts/events/terminal records
and canonical structured message/tool snapshots. See
[structured execution](structured-execution.md#limits-and-cli) for workspace
tools, per-turn model/settings, capability discovery, and artifact downloads.
Closing a client or pressing Ctrl-C while following detaches without canceling.
`turn cancel` is explicit. Interactive `/exit` exits; `/cancel TURN_ID` submits
the same cancellation command.

[Provider credential commands](provider-credentials.md) forward bounded secret
input through the API to daemon-owned XDG storage. The daemon owns browser login
and persistence independently of CLI lifetime. Status separates configured
credentials from daemon execution support; Go, Zen, and Codex models route
through the same public chat API, with Codex text-only.

The CLI entry point only parses arguments, starts the async client, and reports
exit status. Command handlers are separated from connection/token selection,
bounded prompt input, history lookup, event following, and output rendering.
The [implemented module layout and extension steps](implementation.md#suggested-module-growth)
describe where new command groups belong. These remain CLI concerns;
`slop-client` stays independent of argument parsing and terminal presentation.

The proposed broader command groups are:

| Group | Examples of responsibilities |
| --- | --- |
| node / peer | Inspect owner identity, register endpoints, select a target |
| project / workspace | Register repositories, inspect and allocate worktrees |
| session | Create/list/show conversations, read messages, send steering |
| task / run | Create work, inspect attempts, pause/resume/cancel/retry |
| batch | Validate a matrix, submit it, inspect/export results |
| artifact | List metadata and download outputs |
| provider / account | Broader account selection and account-scoped model capabilities |
| daemon | Inspect local service health and eventually install/manage startup |
| transfer | Future export/handoff commands after migration exists |

These execution controls are implemented by the current CLI:

```sh
slop session send SESSION_ID --text "Keep the public API unchanged" --delivery after-turn
slop session send SESSION_ID --text "Use the new endpoint" --delivery next-boundary
slop session send SESSION_ID --text "Stop the command and reconsider" --delivery immediate
slop turn pause TURN_ID
slop turn resume TURN_ID
```

The remaining examples below are **proposed syntax**:

```sh
slop project add /path/to/repo --name app
slop task create --project app --model provider/model --prompt-file task.md
slop task follow TASK_ID --events
slop batch preview --file examples/batch-request.json --json
slop batch submit --file examples/batch-request.json --json
slop task cancel TASK_ID
```

Task submission defaults to returning durable IDs after acceptance. Following
events and waiting for completion are explicit options. Piping model/tool output
does not keep execution in the CLI process.

## Script behavior

Stdout contains requested output; stderr contains diagnostics. JSON mode has a
stable schema with IDs, origin, status, and error details. Human progress bars,
color, and prompts are disabled or explicitly selected in noninteractive mode.

Define future exit codes separately for acceptance failure, unreachable daemon,
compatibility/auth failure, and a terminal task failure when waiting. The exact
future code table is open; implemented commands use success/failure.

A script generates/persists a command ID before submitting mutations. If delivery
is uncertain, it retries with the same ID. CLI timeout or Ctrl-C while following
detaches the client unless an explicit cancellation option was selected.

Support stdin/file prompt input and argument vectors that work on Linux and
Windows. Do not depend on shell expansion to create matrices or serialize JSON.

## Native client library

`slop-client` owns daemon request/response transport, API compatibility checks,
structured error interpretation, bounded NDJSON decoding, and typed chat
operations. It is UI-agnostic. Clients replay events from durable cursors and
reconcile canonical messages; they do not repeat inference on reconnect.

A future `slop-tui` depends on this library and the protocol. It need not depend
on the CLI's argument parser or terminal output renderer. Share domain-neutral
client projections only when both interfaces actually need them.

## Native TUI

A TUI can add a session sidebar, peer/status list, conversation view, structured
tool rendering, diff/artifact viewers, and focused steering input. Its view model
consumes snapshots and events. Temporary selections and scroll position remain
client state.

TUI reconnection uses the same cursor protocol as the CLI. Detach behavior,
keyboard shortcuts, and rendering do not change run semantics. Ship it whenever
the API can support it; it is not on the critical path for batch automation.

## Browser client

A responsive web client should prioritize phone workflows: see active work,
identify its machine/workspace/model, catch up on progress, send a correction,
answer a request for input, and inspect results.

Choose the frontend framework when beginning this track. JavaScript/TypeScript
is permitted here. The project has a separate package manifest and build, outside
native Cargo compilation.

The browser authenticates to one reachable daemon or optional gateway. That
node can aggregate trusted peers and forward requests. This avoids relying on
the browser to discover arbitrary machines or maintain every peer connection.
Direct cross-origin peer access would require explicit TLS/origin/auth design.

A prebuilt web bundle may optionally be served by a daemon, provided the daemon
can also compile/run without it. API compatibility is checked at runtime rather
than coupled to a single bundled frontend version.

## Optional Electron desktop client

Electron is explicitly acceptable as a frontend. It attaches to the independently
running daemon through the same API. It can add native notifications, window
management, file pickers, and a controlled browser view.

Electron must not become a required supervisor for the daemon or provider login
refresh. It may help with setup, but stopping/restarting the app leaves accepted
work running. Opening chat views creates client state and daemon records, not a
new backend process per conversation.

Reuse web presentation code where sensible, while keeping Electron-specific IPC
and privileged integrations separate. Main/renderer processes are a desktop
client concern and do not enter the agent execution architecture.

## Visible browser tools

A future browser sidebar needs a view attached to the actual controlled context.
A picture, iframe, separate unrelated tab, or terminal transcript is not enough
to establish that correspondence.

Explore two tracks: an Electron-native embedded view, and a remotely streamed
browser surface for web/mobile users. Both must keep browser-context IDs,
navigation, tool events, and user/agent input ownership coherent. Transport and
rendering are deliberately unsettled until this milestone.

## Build and release isolation

- `cargo build -p slop-cli` requires no daemon or frontend build.
- `cargo build -p slop-daemon` requires no CLI/TUI/web/Electron build.
- A later TUI is a separate Cargo package.
- Browser and desktop packages have separate build commands and dependencies.
- Generated SDKs follow the API schema; compatibility does not require matching
  all component versions exactly.
- Native release packages can bundle several binaries for convenience without
  merging their runtime responsibilities.

The existing `clients/` directory is a documented location for later clients,
not an empty frontend implementation with mandatory dependencies.
