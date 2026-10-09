# Documentation map

These documents capture the application idea, accepted constraints, proposed
design, and a staged implementation plan. They are intended to let a developer
start with a useful CLI and daemon without first building graphical clients.

**Status as of 2026-10-08:** milestone M0 is a repository/bootstrap foundation,
with runtime-only OpenCode Go, Zen, and headless Codex integrations added afterward.
The startup foundation now adds XDG-aware configuration, exclusive directory
ownership, durable SQLite node identity, local API authentication, and bounded
shutdown. Durable text chat now adds sessions, idempotent message acceptance,
paginated history/events, daemon-owned Go inference, cancellation, and a consumer
CLI. Runtime provider credential commands now persist secrets in private XDG
storage, replace Go credentials without restart, and own ChatGPT authorization
in the daemon. Structured Go execution now adds explicit workspace tools, durable
request/invocation records, artifacts, block-aware streaming, and per-turn model
settings. General task orchestration remains planned. Commands,
schemas, and workflows marked proposed describe future work.

## Reading order

| Document | Purpose |
| --- | --- |
| [Product](product.md) | Motivation, users, workflows, and product vocabulary |
| [Requirements](requirements.md) | Feature inventory, priorities, and non-negotiables |
| [Architecture](architecture.md) | System layers, crate dependencies, and ownership |
| [Clients](clients.md) | How the CLI, TUI, web, and Electron evolve independently |
| [Protocol](protocol.md) | Proposed commands, responses, events, and compatibility |
| [Text chat](text-chat.md) | Implemented local chat ownership, API, durability, and recovery |
| [Structured execution](structured-execution.md) | Implemented tool loop, structured streaming, artifacts, frozen model/settings, and recovery |
| [Provider credentials](provider-credentials.md) | Implemented runtime credential API/CLI and private XDG storage |
| [Codex connection](codex-connection.md) | Implemented headless ChatGPT subscription login and provider usage |
| [Runtime](runtime.md) | Native agent loop, provider integrations, and tools |
| [Shared provider interface](provider-interface.md) | Proposed adapter contract, common client surface, and conformance criteria |
| [Storage](storage.md) | Durable local records, events, artifacts, and recovery |
| [Daemon startup](daemon-startup-plan.md) | Startup foundation and accepted future chat durability contract |
| [Connectivity and sync](connectivity-sync.md) | Tailscale, peer access, caching, and optional aggregation |
| [Migration](migration.md) | Future portable-session and workspace handoff design |
| [Roadmap](roadmap.md) | Delivery milestones, dependencies, and acceptance criteria |
| [Implementation](implementation.md) | Concrete engineering tasks and suggested module layouts |
| [Open questions](open-questions.md) | Decisions still requiring experience or product input |
| [Sources](sources.md) | Primary technical references and changing provider assumptions |
| [Decisions](decisions/README.md) | Short records of the key architectural choices |

## Authority and terminology

User requirements in [requirements](requirements.md) are the baseline. The
architecture records chosen boundaries and engineering proposals. The roadmap
orders delivery; it does not remove features from the long-term idea.

A requirement marked **accepted** came from the product discussion. A design
marked **proposed** is an implementation recommendation and can change when
evidence warrants it. A capability marked **implemented** must exist in the code
and pass the relevant check. An example is not an implementation.

The sources establish external facts; they do not grant universal subscription
access, guarantee memory targets, or select a license for this project. Recheck
provider eligibility and supported API behavior when implementing an integration.

## Definition of an independent client

A client can be developed, built, released, stopped, and replaced without moving
agent execution into that client. It consumes the same versioned API used by
scripts. The native CLI and a future native TUI share `slop-client`; browser
clients share the wire contract and can generate their own SDK.

## Current repository entry points

- [Root manifest](../Cargo.toml): virtual Rust workspace and shared dependency versions.
- [Public protocol](../crates/slop-protocol/src/lib.rs): health/node, chat, and structured execution DTOs.
- [Client library](../crates/slop-client/src/lib.rs): bounded requests, event streams, token files, and compatibility checks.
- [Daemon](../crates/slop-daemon/src/main.rs): loopback HTTP service.
- [CLI](../crates/slop-cli/src/main.rs): independent status, session, turn, chat, capability, and artifact client.
- [Client placeholder directory](../clients/README.md): future frontend locations.
- [Smoke script](../scripts/smoke.py): checks the real daemon and CLI together.
