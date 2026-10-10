# Architecture decisions

Use short decision records for consequential choices. Each record states its
status, context, decision, and consequences. Update a record's status when it is
superseded; a proposal is not an implemented capability.

| Record | Status |
| --- | --- |
| [0001: Native shared runtime](0001-native-runtime.md) | Accepted product constraint |
| [0002: Local execution ownership](0002-local-ownership.md) | Accepted product constraint |
| [0003: API-driven independent clients](0003-independent-clients.md) | Accepted product constraint |
| [0004: Tailscale first](0004-tailscale-first.md) | Accepted initial deployment requirement |
| [0005: Resource admission and durable recovery](0005-bounded-execution.md) | Proposed engineering design |
| [0006: Fair admission, budgets, and child waits](0006-local-orchestration.md) | Implemented local engineering choice |
