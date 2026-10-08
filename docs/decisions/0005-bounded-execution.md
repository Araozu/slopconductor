# 0005: Bounded admission and durable step recovery

Status: proposed. Date: 2026-10-08.

## Context

Sharing a process removes one source of duplication but does not bound context,
output buffers, tool processes, or batch fan-out. A single daemon crash also
affects several active sessions.

## Decision proposal

Admit active work through daemon/project/account resource limits. Create
contexts and worktrees lazily. Persist accepted commands, execution steps, known
outcomes, and semantic events; paginate history and use artifacts for large data.

Record external tool intent before execution and its outcome afterward. Unknown
effects require reconciliation. A reconnect or retry retains command/run
identity rather than silently starting another attempt.

## Consequences

Backpressure and recovery become first-class behavior rather than client tricks.
Waiting parent tasks release scarce execution slots. Performance targets are set
from release-build measurements.

Journaling cannot generally provide exactly-once external effects. Tests need
failure injection around command acceptance and tool launch/completion.
