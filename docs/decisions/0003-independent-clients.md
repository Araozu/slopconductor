# 0003: API-driven independent clients

Status: accepted. Date: 2026-10-08.

## Context

The project should begin with a useful CLI. TUI, web, and Electron clients should
develop separately without redesigning execution or adding dependencies to
native builds.

## Decision

The daemon exposes one public versioned command/query/event protocol.
`slop-protocol` contains wire types; `slop-client` is a reusable native API SDK.
The CLI and future TUI use that SDK. Browser/Electron code consumes the HTTP
contract and may later use generated SDKs.

Native clients do not depend on `slop-daemon`, `slop-runtime`, or its domain
internals. Optional frontend assets are separate build/release artifacts.

## Consequences

The CLI provides early pressure to make every essential operation scriptable.
Client closure/restart does not determine task lifetime. A separately versioned
frontend needs API capability/compatibility handling.

Shared presentation code can grow when needed, while authoritative policies and
execution remain daemon-owned.
