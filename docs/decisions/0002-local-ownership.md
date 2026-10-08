# 0002: Local execution ownership with optional aggregation

Status: accepted. Date: 2026-10-08.

## Context

The user has home/work dual-boot PCs and a VPS, wants remote visibility, and does
not want a central application server to be a mandatory authority.

## Decision

Each OS-local daemon installation has a durable node ID, owns its local sessions
and execution, and persists its own state. Physical-machine labels can group
the Linux and Windows installations for display.

Clients can connect directly. A reachable daemon or VPS can optionally aggregate,
forward, or mirror selected owner-authored data. Mirrors remain read-only.

Session identity is independent of owner identity to allow a later deliberate
handoff. The transfer mechanism itself is future work.

## Consequences

Local use works independently of peers and an optional VPS. Remote views need
origin/freshness indicators, and mutating commands must reach the owner.

Offline replicas cannot infer authority to resume execution. Future handoffs need
explicit quiescence, ownership evidence, and failure reconciliation.
