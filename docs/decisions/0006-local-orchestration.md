# 0006: Fair admission, cumulative budgets, and suspended child waits

Status: implemented local engineering choice. Date: 2026-10-10.

## Context

FIFO turn admission with per-batch caps leaves spare capacity usable, but a batch
whose cap fills the daemon can repeatedly claim every freed slot. Delegation
also needs bounded authority, durable child identity, and waits that do not
occupy the slots needed by children. Repeating a run must not reset its budget.

## Decision

Persist an admission clock and last-admission value for each batch, independent
task tree, or ordinary session. Choose the least recently admitted eligible
group, retaining creation order inside it. New and returning idle groups enter
at the current clock. Descendants share the root's admission group and batch
concurrency cap.

Reserve model-request/tool-call counts in the same transaction that records a
dispatch start. Check the task, its ancestors, and the applicable batch before
incrementing any ledger. Keep reservations after failure, uncertain outcomes,
retry, and restart. Budgets limit operations, not billing totals.

Require an explicit delegation policy and finite root budget. Bound depth and
total children, choose context and tools explicitly, and use fresh sessions and
separate frozen-base workspaces. Commit child creation and its native tool result
together under an invocation-derived command ID.

Persist waits on exact child attempt IDs. Awaiting parents release execution
slots; a child's terminal transaction records the known wait result before
readmission. On restart, suspend parents as paused, preserve their relationships,
and require explicit resume. Unknown external tool outcomes retain the existing
no-replay rule. Cancellation propagation is an immutable child creation choice.

## Consequences

Fairness is an admission bound, not a wall-clock deadline or preemption of
running work. Limits and canceled batch state remain frozen; a retry cannot
replenish them. Parent cancellation independence does not grant a separate
budget, and batch cancellation still covers its descendants. Known wait results
can be reused without repeating child creation or external tools.

See [tasks and batches](../tasks-batches.md) and [child tasks](../child-tasks.md)
for actual commands, bounds, recovery, and validation gaps.
