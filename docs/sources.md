# Primary references and external assumptions

Reviewed during the initial design on **2026-10-08**. External services, supported
flows, dependencies, and platform requirements can change. Recheck the relevant
source when implementing that subsystem.

These references support specific external facts. The product requirements,
crate boundaries, roadmap, and handoff protocol are this project's own design.

## Rust and dependencies

- [Cargo workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html):
  shared manifests, lockfile, package selection, and separate package builds.
- [Rust ownership](https://doc.rust-lang.org/nomicon/ownership.html):
  memory/resource ownership without a garbage collector.
- [Tokio task scheduling](https://tokio.rs/tokio/tutorial/spawning):
  lightweight asynchronous execution inside a shared runtime.
- [Axum API documentation](https://docs.rs/axum/latest/axum/):
  bootstrap routing and service composition.
- [Reqwest API documentation](https://docs.rs/reqwest/latest/reqwest/):
  native HTTP client behavior and feature selection.
- [Clap documentation](https://docs.rs/clap/latest/clap/):
  native argument parsing.

Actual resolved dependency versions are recorded in [Cargo.lock](../Cargo.lock).
The workspace manifest contains version requirements; no minimum supported Rust
version has been claimed yet.

## Persistence and Git

- [SQLite WAL](https://sqlite.org/wal.html): local concurrency, durability choices,
  and same-host constraints.
- [SQLite backup API](https://sqlite.org/backup.html): consistent backup mechanism.
- [Git bundle](https://git-scm.com/docs/git-bundle): repository-object/ref transfer
  as one part of a future workspace package.

## Private networking and browser reachability

- [Tailscale connection types](https://tailscale.com/docs/reference/connection-types):
  direct and relayed private connectivity.
- [Tailscale control/data planes](https://tailscale.com/docs/concepts/control-data-planes):
  coordination and device data paths.
- [Tailscale Serve](https://tailscale.com/docs/features/tailscale-serve):
  private HTTPS access to local services.
- [WebRTC peer connections](https://webrtc.org/getting-started/peer-connections):
  signaling and ICE/relay pieces relevant to a possible later P2P transport.

## OpenAI access

- [Codex authentication](https://learn.chatgpt.com/docs/auth):
  official Codex login modes; this is not by itself a generic runtime integration.
- [ChatGPT plan-usage overview](https://developers.openai.com/siwc/token-sharing-open-source):
  documented third-party plan-usage flow and its intended app categories.
- [Direct model inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference):
  supported Responses API route for the documented plan-usage flow.
- [Codex app-server](https://learn.chatgpt.com/docs/app-server):
  context for official agent integration; this project does not select that
  runtime as its own execution backend.

A documented flow is not a guarantee that every provider, account, app category,
or deployment is eligible. Store the integration's checked assumptions and
availability with implementation. The repository contains no OpenAI login or
direct OpenAI integration yet; OpenCode gateway inference is implemented.

## OpenCode gateways

- [OpenCode Go endpoints](https://docs.opencode.ai/docs/go/#endpoints):
  subscription gateway and model-specific wire formats.
- [OpenCode Zen endpoints](https://docs.opencode.ai/docs/zen/#endpoints):
  pay-as-you-go gateway, advertised model listing, and model-specific wire formats.

Zen's execution mappings were checked on **2026-10-08**. Its Gemini/Google and
Jev/System One formats are unsupported by the current three-wire text adapter.
Live model listing does not grant execution support or establish account access.
Go and Zen have separate keys and catalogs; some identical model IDs use different
wire formats. No live Zen inference was performed for this addition.

## CI

[GitHub's checkout action](https://github.com/actions/checkout) is used with the
verified v7 commit pinned in the workflow. The workflow targets Linux and Windows;
a local Linux check does not establish that a hosted Windows job has run.
