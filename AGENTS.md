# Working on Slop Conductor

## Read the design relevant to the change

Start with `docs/README.md`, `docs/requirements.md`, and
`docs/architecture.md`. Read the relevant implementation and subsystem document
before changing a boundary. Distinguish accepted requirements from proposals and
implemented behavior. Update documentation when implementation changes a plan.

## Preserve the product constraints

- The daemon and agent execution runtime are Rust. Do not introduce Node, Bun,
  Deno, JavaScript, or TypeScript into backend execution or native client builds.
- Agent sessions run inside the shared native daemon. Do not launch an external
  agent CLI or a process/VM/thread per chat as the execution architecture.
- Model-provider integrations call supported APIs directly.
- CLI/TUI/web/Electron clients use the public API and never own execution.
- Keep local execution independent of client lifetime and optional aggregation.
- Each session has one authoritative daemon. Replicas are read-only copies.
- A browser or Electron client may use JavaScript and its own toolchain. Keep
  those dependencies inside that client and out of native builds.
- Shell, Git, browsers, and other actual tools may use supervised subprocesses.
  Worktrees provide filesystem separation, not an OS security sandbox.
- Do not automatically replay a tool operation whose outcome is unknown.

## Workspace boundaries

`slop-core` is pure domain logic. `slop-protocol` is the public wire contract.
`slop-runtime` implements native execution. `slop-daemon` composes the service.
`slop-client` implements the public API client. `slop-cli` is a frontend.

Clients must not depend on `slop-runtime` or `slop-daemon`. Domain and protocol
crates must not acquire presentation, networking, or persistence dependencies.
Do not make generated web assets a prerequisite for compiling the daemon or CLI.

## Validation

Run `cargo fmt --all -- --check` and Clippy with warnings denied. Run relevant
behavioral checks for changed behavior. The bootstrap integration check is
`python3 scripts/smoke.py` on Linux or `python scripts/smoke.py` on Windows.

Keep `Cargo.lock` in version control. Do not commit credentials, provider
responses containing secrets, local databases, worktrees, or build outputs.
Do not add placeholder arithmetic tests or claim proposed endpoints work.

There is currently no provider integration, persistent session store, remote
authentication, TUI, browser client, or Electron client.
