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
- Daemon-owned OpenCode Go inference with bounded concurrency, one active turn
  per session, and explicit cancellation. CLI exit leaves accepted work alive.
- A consumer CLI with interactive and one-shot chat, session inspection,
  stdin/file prompts, event following, and JSON output through the public API.
- Human-readable and JSON status output.
- A daemon/CLI smoke check and a Linux/Windows CI workflow.
- OpenCode Go and Zen integrations through the shared
  `ProviderClient` interface, with model discovery and streaming/non-streaming text turns across
  Chat Completions, Responses, and Messages. Go is wired through the daemon;
  Zen remains a runtime adapter.
- A native headless Codex connection through the same interface, with ChatGPT
  subscription login, protected credentials and serialized refresh. See the
  [Codex setup guide](docs/codex-connection.md). Platform API keys are also supported.

Planned: more daemon provider options, coding tools, worktrees, batch execution, child tasks, remote
control, additional clients, browser tools, and session migration. See the
[roadmap](docs/roadmap.md).

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

Set `OPENCODE_GO_API_KEY` in the daemon's environment to enable inference. The
CLI does not read provider credentials or call the provider. The default model
is `opencode-go/glm-5.3-flash`; select another verified Go model with
`chat --model opencode-go/MODEL`. `--prompt-file PATH` and piped stdin are also
supported. One-shot chat waits for completion unless `--detach` is supplied.
Ctrl-C while following detaches; cancellation is an explicit command.

Use `--command-id ID` for a script's stable mutation identity. After uncertain
delivery, retry the same operation with the same ID and input. Completed history
survives daemon restart. Interrupted thinking is discarded and in-flight turns
are marked interrupted without automatically repeating the provider request.
See [text chat](docs/text-chat.md) for API and recovery details.

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
