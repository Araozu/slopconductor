# Hive

A native AI agent runtime with one daemon per machine, persistent sessions, and
independent clients. Start work through a CLI or script, leave it running, and
later inspect or steer the same session from another device.

The daemon owns the agent loop and calls model providers directly. Sessions share
one runtime. Each machine owns its data; remote access initially uses Tailscale.
A VPS can optionally aggregate or mirror data. Future clients include a native
TUI, a browser interface, and an optional Electron desktop interface.

## Current state

Implemented:

- Six separate Rust workspace crates with enforced dependency boundaries.
- A loopback-only daemon exposing anonymous health and authenticated node,
  model, session, message, event, and turn APIs.
- XDG-aware configuration/data paths, exclusive daemon ownership, SQLite node
  identity with atomic migrations and FULL WAL synchronization, private local
  API credentials, and bounded shutdown.
- Durable text chats with atomic command receipts, ordered history/events,
  visible checkpoints, terminal replies/usage, and restart reconciliation.
- Automatic token-aware context compaction with durable working memory, full
  transcript retention, cached-input accounting, and bounded provider recovery.
  The default cost cap is 275,000 tokens; `slop chat --full-context` explicitly
  opts in to the model's full available window.
- Daemon-owned OpenCode Go, OpenCode Zen, and Codex inference with bounded concurrency, one active turn
  per session, and explicit cancellation. CLI exit leaves accepted work alive.
- A consumer CLI with interactive and one-shot chat, session inspection,
  stdin/file prompts, event following, and JSON output through the public API.
- Human-readable and JSON status output.
- Real-binary daemon/CLI smoke checks and
  [passing Linux/native Windows CI](docs/native-validation.md).
- OpenCode Go and Zen integrations through the shared
  `ProviderClient` interface, with model discovery and streaming/non-streaming text turns across
  Chat Completions, Responses, and Messages. Both are selectable through the
  daemon using `--model provider/model`.
- A native headless Codex connection through the same interface, with ChatGPT
  subscription login, protected credentials and serialized refresh. See the
  [Codex setup guide](docs/codex-connection.md). Platform API keys are also supported.
- Runtime provider credential configuration through authenticated API/CLI
  commands, private XDG storage, immediate key replacement for new turns, and
  daemon-owned ChatGPT login. See [provider credentials](docs/provider-credentials.md).
- Daemon-owned coding tools limited to `read`, `write`, `edit`, and `bash`, with
  workspace policies, durable invocation/results, bounded output, artifacts,
  cancellation, and recovery without replay. See [structured execution](docs/structured-execution.md).
- Unified workspaces that open existing canonical directories in place, plus
  explicit Git worktree creation with frozen bases, diffs, conservative cleanup,
  and recovery without replay. Sessions and orchestration select existing
  workspaces. See [workspaces](docs/workspaces.md).
- Persistent conversations and execution turns, with pause/resume/cancel, steering,
  fresh retries, and preserved prior workspaces/results.
- Deterministic prompt/model/settings matrices with preview, atomic acceptance,
  bounded batch admission, selective retries, and JSON Lines export. See
  [tasks and batches](docs/tasks-batches.md).
- Persistent fair admission, aggregate task/tree/batch request and tool budgets,
  and durable batch cancellation.
- Separately authorized native child creation, inspection, waits, results, and
  cancellation with explicit workspace selection and restart-safe links. See
  [child tasks](docs/child-tasks.md).
- Scoped daemon peering for inspecting and controlling existing sessions over
  Tailscale HTTPS, with explicit owner registration, read/control grants, and
  revocation. See [connection setup](#connect-over-tailscale).

Planned: more daemon provider options, aggregate views, history mirroring,
additional clients, browser tools, and session migration. See the
[roadmap](docs/roadmap.md).

The [recommended next steps](docs/roadmap.md#recommended-next-delivery-order) are
Windows and capacity validation, plus deployed Tailscale validation.
Scheduling/batch controls, child tasks, and scoped daemon peering have
implemented local slices.
Both daemon listeners bind to loopback; Tailscale Serve exposes the peer listener
over private HTTPS.

## Run local text chat

Install a current stable Rust toolchain. The repository requests rustfmt and
Clippy through `rust-toolchain.toml`.

In one terminal:

```sh
cargo run -p slop-daemon
```

In another:

```sh
cargo run -- status
cargo run -- --json status
cargo run -- --json node
cargo run -- models
cargo run -- chat --prompt "Reply with a short greeting."
cargo run -- chat --workspace /absolute/repository --prompt "Fix the failing test."
cargo run -- chat --workspace /absolute/repository --tool read --prompt "Review the code."
cargo run -- chat --session SESSION_ID
cargo run -- --json session history SESSION_ID
cargo run -- turn cancel TURN_ID
```

The CLI is the default workspace member, so plain `cargo build` and `cargo run`
target it. The binaries are named `hived` and `slop`. The default endpoint is
`http://127.0.0.1:7331`. Override the loopback address when necessary:

```sh
cargo run -p slop-daemon -- --listen 127.0.0.1:7441
cargo run -p slop-cli -- --daemon http://127.0.0.1:7441 status
```

On Linux, configuration defaults to
`$XDG_CONFIG_HOME/slopconductor/config.toml` (`~/.config/...`) and durable data
to `$XDG_DATA_HOME/slopconductor` (`~/.local/share/...`). No separate directory
is added directly under the user's home. Windows uses
`%LOCALAPPDATA%/slopconductor`. The daemon creates a private local API token at
`<data-dir>/credentials/local-api-token`. For loopback endpoints the CLI discovers
that token using the platform data directory; `--token-file` or `SLOP_TOKEN_FILE`
can override it. Explicit daemon data-directory overrides require the matching
CLI token-file override. Other origins require an explicit `--token-file`.

Configure Go while the daemon is running:

```sh
cargo run -- provider set-key opencode-go --key-file /path/to/key
cargo run -- provider status
```

The key can also be piped on stdin. It is persisted in the daemon's private
XDG data directory and enables new turns immediately. The CLI forwards input
through the public API; it never calls the provider or owns credential storage.
`OPENCODE_GO_API_KEY` remains a startup import when no saved Go key exists;
saved credentials take precedence on restart. The default model
is `opencode-go/glm-5.3-flash`; select another verified Go model with
`chat --model opencode-go/MODEL`. `--prompt-file PATH` and piped stdin are also
supported. One-shot chat waits for completion unless `--detach` is supplied.
Ctrl-C while following detaches; cancellation is an explicit command.

`chat --workspace PATH` enables the four coding tools. Repeated `--tool` options
restrict that set; `--tool read` permits only file reads. `read` uses 1-based line
offsets, `write` creates or overwrites files, and `edit` makes exact text
replacements. `bash` runs from the workspace root under the daemon's output and
time limits. Windows tool use requires `bash.exe` on PATH (for example, Git for
Windows); native builds do not require it. Chats without a workspace selection
use the built-in `default` workspace, named “No workspace,” with no coding tools;
pass `--no-workspace` to state that choice explicitly.

Use `provider login codex` to authorize a ChatGPT subscription through the
daemon, or configure a Codex Platform API key with `provider set-key codex`.
Go, Zen, and Codex support structured text and tool turns. Codex also supports
model-specific `--effort`, account selection, and explicit `provider discover`.
Select a model with `chat --model provider/model`. See
[runtime credentials](docs/provider-credentials.md).

Use `--command-id ID` for a script's stable mutation identity. After uncertain
delivery, retry the same operation with the same ID and input. Completed history
survives daemon restart. Interrupted thinking is discarded and in-flight turns
are marked interrupted without automatically repeating the provider request.
See [text chat](docs/text-chat.md) for API and recovery details.

Open a directory once, then select its returned workspace ID for sessions and jobs:

```sh
cargo run -- workspace open /absolute/repository
cargo run -- chat --workspace-id WORKSPACE_ID --prompt "Fix the failing test." --detach
cargo run -- chat --workspace-id WORKSPACE_ID --prompt "Improve error handling." --detach
cargo run -- workspace worktree WORKSPACE_ID --base main --name fix-test
cargo run -- workspace diff WORKTREE_ID
cargo run -- workspace remove WORKTREE_ID
```

Opening a directory registers it in place. Worktrees are created only by the
explicit `workspace worktree` operation; its frozen base and lifecycle are
available through `workspace show`. Sessions that share one canonical path
serialize their writes. Ordinary workspace removal only unregisters the path;
worktree removal refuses active users, dirty files, and commits beyond the frozen
base. See the [workspace guide](docs/workspaces.md) for API and recovery.

Durable jobs and prompt/model/settings matrices use the same runtime:

```sh
cargo run -- task create --model opencode-go/glm-5.3-flash --workspace-id WORKSPACE_ID --text "Fix the failing test." --command-id fix-test
cargo run -- task follow TASK_ID
cargo run -- run retry FAILED_RUN_ID --command-id retry-fix
cargo run -- --json batch preview examples/batch-request.json --output frozen-batch.json
cargo run -- batch submit frozen-batch.json --command-id sweep-1
cargo run -- batch results BATCH_ID
cargo run -- batch export BATCH_ID --output results.jsonl
cargo run -- batch retry BATCH_ID --index 3 --command-id retry-selected
```

Replace the example's workspace ID before previewing. Matrix preview and
submission never create worktrees; each attempt retains its selected workspace,
and retries preserve successful batch members. See
[tasks and batches](docs/tasks-batches.md) for API, controls, and recovery.

`SLOP_LISTEN` and `SLOP_DAEMON_URL` provide the equivalent endpoint settings.
Use `--config`/`SLOP_CONFIG`, `--data-dir`/`SLOP_DATA_DIR`, and
`--name`/`SLOP_NODE_NAME` for startup overrides. See the
[startup guide](docs/daemon-startup-plan.md) for config settings, Windows path
restrictions, and the chat durability/recovery rules.
The bootstrap accepts loopback listeners only. The optional
[peer listener and gateway](docs/daemon-peering.md) add scoped inspection/control
of existing owner sessions over explicitly configured private HTTPS forwarding.
Deployed Tailscale validation remains open.

## Connect over Tailscale

Assume both machines are already on your tailnet. The **owner** runs the sessions;
the **connecting machine** runs its own local daemon as a gateway. Connections
are directional: configure the reverse direction separately if you need it.
The following Linux shell examples run from the repository on each machine.
If the binaries are on your PATH, replace `cargo run -p slop-daemon --` with
`hived` and `cargo run --` with `slop`. On Windows, use credential paths beneath
`%LOCALAPPDATA%` and your shell's equivalent commands.

### On the owner

Start the daemon with its separate peer listener, or restart your existing daemon
with this flag:

```sh
cargo run -p slop-daemon -- --peer-listen 127.0.0.1:7332
```

In another terminal, expose port **7332** through
[Tailscale Serve](https://tailscale.com/docs/reference/examples/serve#expose-a-development-server-to-your-tailnet).
Keep the administrator API on port 7331 local:

```sh
tailscale serve --bg http://127.0.0.1:7332
```

Save the HTTPS origin printed by Serve, such as `https://home.example.ts.net`.
Remote peer registration requires HTTPS. Create a grant for the connecting
machine:

```sh
mkdir -p ~/.config/slopconductor
cargo run -- peer grant --label work --scope control \
  --token-output ~/.config/slopconductor/work-peer-token \
  --command-id work-grant-1
```

Save the owner node ID printed by the grant command. Transfer `work-peer-token`
privately to the connecting machine; the file contains the peer credential.
Use `--scope read` for inspection only. A control grant permits sending messages
and controlling turns across all public conversations on this owner, including
tool execution allowed by each session's existing policy.

### On the connecting machine

Start your local daemon if it is not already running. It needs no peer listener
or provider credentials to inspect and control the owner:

```sh
cargo run -p slop-daemon
```

In another terminal, put the transferred token at
`~/.config/slopconductor/work-peer-token` (create the directory if needed), then
register the owner. Replace the example HTTPS origin and `OWNER_NODE_ID` with
the values saved above:

```sh
chmod 600 ~/.config/slopconductor/work-peer-token
cargo run -- peer add home --origin https://home.example.ts.net \
  --owner-node-id OWNER_NODE_ID \
  --credential-file ~/.config/slopconductor/work-peer-token
cargo run -- peer probe home
cargo run -- --peer home session list
```

Use session IDs from the owner to inspect or steer existing work:

```sh
cargo run -- --peer home session history SESSION_ID
cargo run -- --peer home session follow SESSION_ID --after 0
cargo run -- --peer home session send SESSION_ID --text "Focus on the failing test" \
  --delivery next-boundary --detach --command-id correction-1
cargo run -- --peer home turn pause TURN_ID --command-id pause-1
cargo run -- --peer home turn resume TURN_ID --command-id resume-1
cargo run -- --peer home turn cancel TURN_ID --command-id cancel-1
```

Keep `--daemon` and `--token-file` pointed at your local gateway and its local
administrator credential if you override their defaults; `--peer home` selects
the remote owner. Ctrl-C while following detaches, and accepted work continues
on the owner if the client or gateway disconnects.

To revoke access, run `cargo run -- peer revoke work-grant-1` on the owner.
To remove the local registration, run `cargo run -- peer remove home` on the
connecting machine. Peering currently operates on existing conversations;
remote task creation, history mirroring, and aggregate views remain planned.
See [daemon peering](docs/daemon-peering.md) for configuration, limits, and
recovery. Offline two-daemon checks pass; deployed two-machine Tailscale
validation remains open.

## Build and check

```sh
cargo build -p slop-cli --locked
cargo build -p slop-daemon --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 scripts/smoke.py
```

On Windows, use `python` or `py -3` for the smoke script. Python is only a
development verification dependency.

[GitHub Actions](.github/workflows/ci.yml) runs the formatting, Clippy, workspace
tests, independent CLI build, and complete offline smoke suite on Ubuntu 24.04
and native Windows Server 2025 for pushes, pull requests, and manual dispatches.
It uses stable Rust and Python 3.12. Windows selects Git for Windows' Bash and
places temporary credential fixtures beneath the runner's per-user app-data
directory. The smoke suite verifies cancellation of native child processes and
their descendants on both platforms. CI needs no provider secrets; live-provider
checks remain explicitly opt-in. The
[recorded native validation](docs/native-validation.md) passed both jobs and the
complete offline suite.
Rust compiler and Clippy errors appear as annotations with source locations.
Failed Clippy, test, and smoke checks also annotate a bounded tail of their output
while retaining their original exit status.

The [OpenCode Go live checks](crates/slop-runtime/tests/opencode_go_live.rs)
require `OPENCODE_GO_API_KEY` and an explicit opt-in; normal workspace tests skip
them. They use the shared provider interface and document their request/token
bounds. See the [provider conformance review](docs/provider-interface.md#conformance-review-of-the-current-slice)
for verified behavior and the remaining daemon/API work.

For a walkthrough of daemon concepts and the implemented request, persistence,
streaming, cancellation, and restart flows, see the [slop-daemon guide](crates/slop-daemon/slop-daemon-guide.md).

## API reference (OpenAPI)

A running daemon serves its full OpenAPI document at
`GET /api-docs/openapi.json` with interactive Swagger UI at `GET /docs`.
To regenerate the document without starting the daemon:

```sh
cargo run -p slop-daemon -- --dump-openapi > /tmp/openapi.json
```

The generated JSON is a local artifact and is git-ignored; the source of
truth is the `utoipa` annotations in `crates/slop-daemon/src/api/`.

Building the CLI does not build the daemon, its execution layer, or a frontend.
Browser and Electron projects will have their own builds when introduced.

## Design documents

Start at the [documentation index](docs/README.md) and the
[master per-crate overview](docs/overview.md). The main references are:

- [Product idea and workflows](docs/product.md)
- [Features and non-negotiables](docs/requirements.md)
- [Architecture and crate boundaries](docs/architecture.md)
- [CLI, TUI, web, and desktop separation](docs/clients.md)
- [Roadmap and acceptance criteria](docs/roadmap.md)
- [Implementation plan](docs/implementation.md)

The name is a working name taken from this repository. No distribution license
has been selected; workspace packages are currently marked `publish = false`.
