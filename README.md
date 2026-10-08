# Slop Conductor

A native AI agent runtime with one daemon per machine, persistent sessions, and
independent clients. Start work through a CLI or script, leave it running, and
later inspect or steer the same session from another device.

The daemon owns the agent loop and calls model providers directly. Sessions share
one runtime. Each machine owns its data; remote access initially uses Tailscale.
A VPS can optionally aggregate or mirror data. Future clients include a native
TUI, a browser interface, and an optional Electron desktop interface.

## Current state

This repository is an **initial scaffold**, not a working coding agent.

Implemented:

- Six separate Rust workspace crates with enforced dependency boundaries.
- A loopback-only daemon exposing `GET /v1/health`.
- A native CLI that checks daemon identity, API version, and capabilities.
- Human-readable and JSON status output.
- A daemon/CLI smoke check and a Linux/Windows CI workflow.
- Runtime-only OpenCode Go and Zen integrations through the shared
  `ProviderClient` interface, with model discovery and streaming/non-streaming text turns across
  Chat Completions, Responses, and Messages. The daemon does not expose inference.
- A native headless Codex connection through the same interface, with ChatGPT
  subscription login, protected credentials and serialized refresh. See the
  [Codex setup guide](docs/codex-connection.md). Platform API keys are also supported.

Planned: session persistence, daemon-owned model execution, more providers,
coding tools, worktrees, batch execution, child tasks, event replay, remote
control, additional clients, browser tools, and session migration. See the
[roadmap](docs/roadmap.md).

## Run the bootstrap

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
```

The CLI is the default workspace member, so plain `cargo build` and `cargo run`
target it. The binaries are named `slopd` and `slop`. The default endpoint is
`http://127.0.0.1:7331`. Override the loopback address when necessary:

```sh
cargo run -p slop-daemon -- --listen 127.0.0.1:7441
cargo run -p slop-cli -- --daemon http://127.0.0.1:7441 status
```

`SLOP_LISTEN` and `SLOP_DAEMON_URL` provide the equivalent environment settings.
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
