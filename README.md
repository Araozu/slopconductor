# Slop Conductor

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
- Daemon-owned OpenCode Go, OpenCode Zen, and Codex inference with bounded concurrency, one active turn
  per session, and explicit cancellation. CLI exit leaves accepted work alive.
- A consumer CLI with interactive and one-shot chat, session inspection,
  stdin/file prompts, event following, and JSON output through the public API.
- Human-readable and JSON status output.
- Real-binary daemon/CLI smoke checks; native Windows validation remains outstanding.
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
- Registered projects and managed session worktrees with frozen base commits,
  lazy allocation, independent diffs, explicit cleanup, and Git recovery without
  replay. See [projects and workspaces](docs/projects-workspaces.md).
- Durable tasks and explicit run attempts, with pause/resume/cancel, steering,
  fresh retries, and preserved prior workspaces/results.
- Deterministic prompt/model/settings matrices with preview, atomic acceptance,
  bounded batch admission, selective retries, and JSON Lines export. See
  [tasks and batches](docs/tasks-batches.md).

Planned: more daemon provider options, child tasks, remote
control, additional clients, browser tools, and session migration. See the
[roadmap](docs/roadmap.md).

The [recommended next steps](docs/roadmap.md#recommended-next-delivery-order) are
fair scheduling, batch cancellation and aggregate budgets; native child tasks;
Windows and capacity validation; then trusted remote CLI access. These remain
planned, and the current daemon accepts loopback connections only.

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
cargo run -- chat --workspace /absolute/project --prompt "Fix the failing test."
cargo run -- chat --workspace /absolute/project --tool read --prompt "Review the code."
cargo run -- chat --session SESSION_ID
cargo run -- --json session history SESSION_ID
cargo run -- turn cancel TURN_ID
```

The CLI is the default workspace member, so plain `cargo build` and `cargo run`
target it. The binaries are named `slopd` and `slop`. The default endpoint is
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
Windows); native builds do not require it.

Use `provider login codex` to authorize a ChatGPT subscription through the
daemon, or configure a Codex Platform API key with `provider set-key codex`.
Zen and Codex support text turns; Go and Zen support structured tool turns.
Select a model with `chat --model provider/model`. See
[runtime credentials](docs/provider-credentials.md).

Use `--command-id ID` for a script's stable mutation identity. After uncertain
delivery, retry the same operation with the same ID and input. Completed history
survives daemon restart. Interrupted thinking is discarded and in-flight turns
are marked interrupted without automatically repeating the provider request.
See [text chat](docs/text-chat.md) for API and recovery details.

For isolated jobs in one repository, register it and use its returned project ID:

```sh
cargo run -- project register /absolute/project
cargo run -- chat --project PROJECT_ID --base main --prompt "Fix the failing test." --detach
cargo run -- chat --project PROJECT_ID --base main --prompt "Improve error handling." --detach
cargo run -- project workspaces PROJECT_ID
cargo run -- workspace diff WORKSPACE_ID
cargo run -- workspace remove WORKSPACE_ID
```

Each session freezes its base at acceptance and allocates a detached worktree
when its first turn is admitted. Git must be on the daemon's PATH. Cleanup is
explicit and refuses active workspaces, dirty files, and new detached commits;
inspect its asynchronous outcome with `workspace show`. See the
[workspace guide](docs/projects-workspaces.md) for API, limits, and recovery.

Durable jobs and prompt/model/settings matrices use the same runtime:

```sh
cargo run -- task create --model opencode-go/glm-5.3-flash --project PROJECT_ID --text "Fix the failing test." --command-id fix-test
cargo run -- task follow TASK_ID
cargo run -- run retry FAILED_RUN_ID --command-id retry-fix
cargo run -- --json batch preview examples/batch-request.json --output frozen-batch.json
cargo run -- batch submit frozen-batch.json --command-id sweep-1
cargo run -- batch results BATCH_ID
cargo run -- batch export BATCH_ID --output results.jsonl
cargo run -- batch retry BATCH_ID --index 3 --command-id retry-selected
```

Replace the example's project ID before previewing. Each attempt retains its
own session/workspace; retries preserve successful batch members. See
[tasks and batches](docs/tasks-batches.md) for API, controls, and recovery.

`SLOP_LISTEN` and `SLOP_DAEMON_URL` provide the equivalent endpoint settings.
Use `--config`/`SLOP_CONFIG`, `--data-dir`/`SLOP_DATA_DIR`, and
`--name`/`SLOP_NODE_NAME` for startup overrides. See the
[startup guide](docs/daemon-startup-plan.md) for config settings, Windows path
restrictions, and the chat durability/recovery rules.
The bootstrap accepts loopback listeners only. Authenticated remote control is a
planned milestone; requiring Tailscale does not mean that feature is implemented.

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

The [OpenCode Go live checks](crates/slop-runtime/tests/opencode_go_live.rs)
require `OPENCODE_GO_API_KEY` and an explicit opt-in; normal workspace tests skip
them. They use the shared provider interface and document their request/token
bounds. See the [provider conformance review](docs/provider-interface.md#conformance-review-of-the-current-slice)
for verified behavior and the remaining daemon/API work.

For a walkthrough of daemon concepts and the implemented request, persistence,
streaming, cancellation, and restart flows, see the [slop-daemon guide](crates/slop-daemon/slop-daemon-guide.md).

Building the CLI does not build the daemon, its execution layer, or a frontend.
Browser and Electron projects will have their own builds when introduced.

## Design documents

Start at the [documentation index](docs/README.md). The main references are:

- [Product idea and workflows](docs/product.md)
- [Features and non-negotiables](docs/requirements.md)
- [Architecture and crate boundaries](docs/architecture.md)
- [CLI, TUI, web, and desktop separation](docs/clients.md)
- [Roadmap and acceptance criteria](docs/roadmap.md)
- [Implementation plan](docs/implementation.md)

The name is a working name taken from this repository. No distribution license
has been selected; workspace packages are currently marked `publish = false`.
