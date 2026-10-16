# Working on Hive

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

`hive-core` is pure domain logic. `hive-protocol` is the public wire contract.
`hive-runtime` implements native execution. `hive-daemon` composes the service.
`hive-client` implements the public API client. `hive-cli` is a frontend.

Additional presentation clients (TUI, web, Electron/desktop, mobile, or other
interfaces) belong in separate repositories outside this repository. Keep their
source, package manifests, assets, tests, and build/release tooling there. This
repository owns the daemon, runtime, public protocol, native client library, and
existing CLI. External clients consume the public API and may reuse `hive-client`.

Clients must not depend on `hive-runtime` or `hive-daemon`. Domain and protocol
crates must not acquire presentation, networking, or persistence dependencies.
Do not make generated web assets a prerequisite for compiling the daemon or CLI.

## OpenAPI documentation (utoipa)

The daemon publishes a compile-time OpenAPI document via `utoipa`:
`GET /api-docs/openapi.json` and interactive Swagger UI at `GET /docs`.
Source of truth is `crates/hive-daemon/src/api/openapi.rs` (`ApiDoc`),
which covers every administrator route (including the peer gateway). A coverage test fails if a
route is added without registering it.

Generate the document locally; never check the generated JSON into source
control (it is git-ignored):

- `cargo run -p hive-daemon -- --dump-openapi > /tmp/openapi.json` — no
  storage, socket, or runtime needed; prints the document and exits.
- Or, with a daemon running: `curl http://127.0.0.1:7331/api-docs/openapi.json`.

When you touch the public HTTP API, keep the annotations in sync:

- Derive `utoipa::ToSchema` on every new or changed `hive-protocol`
  request/response type. `utoipa` is the only presentation-adjacent
  dependency allowed in `hive-protocol`, solely for schema generation.
- Annotate every new or changed handler with `#[utoipa::path(...)]`
  (method, `path = "..."`, `tag`, `params` for path/query args,
  `request_body` for JSON bodies, `security` for authenticated routes,
  and all `responses` with `body = ...`). Any nearby handler shows the
  pattern.
- Register the handler in `paths(...)` and every new schema in
  `components(schemas(...))` in `ApiDoc`, and extend `EXPECTED_API_PATHS`
  in the coverage test. Unregistered handlers/types silently disappear
  from the published document.
- Run `cargo test -p hive-daemon api::openapi` and regenerate the JSON
  to eyeball the diff when routes change.

## Validation

Run `cargo fmt --all -- --check` and Clippy with warnings denied. Run relevant
behavioral checks for changed behavior. The bootstrap integration check is
`python3 scripts/smoke.py` on Linux or `python scripts/smoke.py` on Windows.

Keep `Cargo.lock` in version control. Do not commit credentials, provider
responses containing secrets, local databases, worktrees, or build outputs.
Do not add placeholder arithmetic tests or claim proposed endpoints work.

Persistent sessions, project workspaces, tasks/run attempts, matrices, fair
admission, aggregate operation budgets, batch cancellation, and bounded child
tasks are implemented. Scoped daemon peering now adds a separate optional
loopback peer listener, explicit owner targeting, and session inspection/control.
Deployed tailnet validation remains open. TUI, browser client, and Electron
client remain planned in external repositories. Linux and native Windows offline
CI pass; live-provider validation remains outstanding. See
`docs/native-validation.md` for the recorded run and scope and
`docs/capacity-validation.md` for Linux release workloads and measurement limits.
