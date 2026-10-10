# Tasks, run attempts, and matrices

**Implemented local slice, 2026-10-10.** Tasks, explicit run attempts, matrix
preview/submission, results, selective retries, and JSON Lines export use the
authenticated public API and native CLI. They execute through the existing Rust
turn scheduler. Closing a client leaves accepted work running.

## Job and attempt ownership

A task records immutable requested inputs and effective inputs: prompt, title,
provider-qualified model, generation settings, and optional registered project.
Acceptance returns task, run, session, and turn IDs together. A run exists while
queued; admission changes its status rather than creating its identity.

Each attempt owns a fresh session with one primary turn. That turn can perform
multiple model requests and coding-tool calls under the existing execution
bounds. A project selection reserves a separate worktree, freezes its exact base
commit, and allocates the directory only after admission. A text task without a
project needs no Git checkout. Codex tasks remain text-only.

This chooses fresh attempts for reproducibility. Reusing a conversation for
successive tasks remains a future extension. Ordinary chat sessions retain their
existing conversational behavior. Attempts reject additional primary turns
through the session message API; active or paused attempts accept steering.

```sh
slop task create --model opencode-go/glm-5.3-flash \
  --project PROJECT_ID --base main --text "Fix the failing test." \
  --command-id fix-test
slop task show TASK_ID
slop task follow TASK_ID
slop task send TASK_ID --text "Keep the public interface." --command-id correction
slop run pause RUN_ID --command-id pause-fix
slop run resume RUN_ID --command-id resume-fix
slop run cancel RUN_ID --command-id cancel-fix
slop task runs TASK_ID
slop task events TASK_ID
slop workspace diff WORKSPACE_ID
```

`task create` returns immediately. `task follow` attaches to the attempt current
when the command starts; it does not switch to a subsequent retry. History,
structured tools, model requests, and artifacts remain available through that
attempt's existing session/turn/artifact operations. Project tasks enable all
four coding tools unless `--tool` restricts them. Steering accepts next-boundary
or immediate delivery, keeps the active turn's frozen model/settings, and
requires an active, paused, or awaiting-children attempt. Instruction retries return the original
receipt even if the task has since acquired another attempt.

## Status and recovery

Run status is the durable primary turn's status: `queued`, `running`, `paused`,
`awaiting_children`, `completed`, `failed`, `cancelled`, `interrupted`, or `incomplete`. Pause and cancel
acknowledgements indicate acceptance; a running operation reaches a safe boundary
before the resulting state is visible. Paused runs release execution capacity.
Resume explicitly readmits the same run and preserves its workspace/context.

Task events mirror committed session facts with a task sequence, run ID, status,
and source session event sequence. Task creation and retry add their own facts.
State, events, and command receipts commit in one SQLite transaction. Event
queries are paginated JSON; live content uses the existing session NDJSON feed.

Startup leaves undispatched queued work eligible for admission and paused work
paused. A previously running turn becomes interrupted. Unfinished tools receive
failure records, possibly with unknown effects; unfinished Git operations fail
conservatively. Recovery does not replay them or automatically create attempts.

```sh
slop run retry FAILED_RUN_ID --command-id retry-fix
# After inspecting an attempt with unknown effects:
slop run retry INTERRUPTED_RUN_ID --command-id retry-inspected \
  --acknowledge-unknown-effects
```

Only the latest unsuccessful attempt can retry. Completed, active, and paused
runs refuse retry. Each task allows at most eight attempts. A retry preserves the
prompt, effective settings, and frozen base commit, creates a fresh session and
worktree reservation, and records `retry_of`. Old sessions, outputs, workspaces,
and tool outcomes remain inspectable. Successful external changes in an old
workspace are not copied to the new attempt. Unknown external effects require
the explicit acknowledgement above; it does not undo or reconcile those effects.
No tool, provider, or whole-job retry occurs automatically.

## Fair admission, budgets, and batch cancellation

Admission rotates across eligible batches, independent task trees, and ordinary
sessions. Persistent tickets and an admission clock survive restart. Newly
queued or returning idle groups join at the current clock; a continuous stream
of arrivals cannot reuse old priority to jump ahead of waiting work. Each
admission advances its group's ticket, with creation order resolving ties.
An eligible group gets its turn after at most one admission per other group
already queued at that point. This bounds admissions, not elapsed time: active
runs are not preempted. Descendants share the root's group and batch cap.

`TaskSpec.budget` and `BatchSpec.budget` optionally set `max_model_requests` and
`max_tool_calls`, each 0–1,000,000. A missing budget retains the existing per-turn
limits without an additional aggregate cap. Zero forbids that operation.
Task budgets cover all attempts and descendants; batch budgets cover every
member attempt and descendant. Task/batch queries expose `budget_usage`.

Before dispatch, one transaction checks every applicable scope and reserves
the count along with its request/tool start record. Denial increments no scope
and fails the turn as `operation_budget_exhausted` without dispatching the
operation. Reservations are never refunded after failure, cancellation,
uncertain outcomes, or restart, and retries cannot replenish them. Counts limit
operations rather than tokens or billing totals. A delegation policy requires
an explicit finite budget; see [child tasks](child-tasks.md).

```sh
slop task create --model opencode-go/glm-5.3-flash --project PROJECT_ID \
  --text "Fix the failing test." --max-model-requests 16 --max-tool-calls 32 \
  --command-id bounded-job
slop batch cancel BATCH_ID --command-id cancel-sweep
```

Supplying only one task limit flag defaults the other to 64 requests or 128 tool
calls. Batch limits are part of its JSON spec and are preserved by preview.
Batch cancellation atomically closes the batch and records cancellation for all
unfinished members and descendants. Queued, paused, and awaiting attempts stop
immediately; running attempts finish cancellation at existing execution
boundaries. Completed results remain intact. The batch stays closed to retries,
and repeating the same command returns its original `BatchCancelReceipt`.

## Matrix preview and submission

The stable input is `BatchSpec`, illustrated by
[examples/batch-request.json](../examples/batch-request.json). It has three
ordered arrays: `prompts`, provider-qualified `models`, and generation `settings`.
Expansion uses prompt as the outer dimension and settings as the fastest
changing dimension. Duplicate input values retain distinct indexes.

```json
{
  "name": "prompt-model-sweep",
  "prompts": ["Improve error handling.", "Inspect and fix failure paths."],
  "models": ["opencode-go/glm-5.3-flash", "opencode-go/glm-5.3"],
  "settings": [{"max_output_tokens": 1024}, {"max_output_tokens": 2048}],
  "project": {
    "project_id": "PROJECT_ID",
    "base_ref": "HEAD",
    "allowed_tools": ["read", "write", "edit", "bash"]
  },
  "max_concurrent_runs": 2
}
```

```sh
slop --json batch preview batch.json --output frozen-batch.json
slop --json batch submit frozen-batch.json --command-id sweep-1
slop batch show BATCH_ID
slop batch members BATCH_ID
slop batch results BATCH_ID
slop batch export BATCH_ID --output results.jsonl
slop batch retry BATCH_ID --index 3 --index 6 --command-id retry-selected
```

Preview validates every model/settings/tool combination, checks cardinality and
current workspace capacity, and resolves the base to an exact commit without
creating jobs, worktrees, or model requests. The optional output file saves the
returned spec, including its exact commit and `default_max_output_tokens` for
uncapped API-key cells. Missing output caps on subscription cells remain uncapped.
Effective cell settings are also shown in the preview. Saved preview/export
files require a new destination and never overwrite an existing file.

Submit can accept an original spec directly, resolving inputs at acceptance, or
the saved preview spec to preserve its base/defaults. Capabilities and capacity
are checked again: provider authorization and available space can change after
preview. Invalid or oversized submissions create no partial batch. Member tasks,
their initial runs/sessions/prompts, workspace metadata, and the batch receipt
commit together. Queued members hold metadata on disk; admission loads context
and allocates worktrees lazily.

Batch status counts describe each member's latest attempt. A selective retry
requires distinct explicit zero-based indexes and validates the entire selection
before committing any new attempts. Selecting a successful or active member
rejects the whole command. Successful members retain their IDs, outputs, and
workspaces. Per-task attempt history includes previous failures.

Results preserve combination index, requested/effective inputs, attempt and
session IDs, workspace ID, status/errors, usage, request/tool IDs, and the latest
canonical assistant message. Tools expose artifact IDs through the existing API;
workspaces expose live diffs separately. JSON Lines export reads all result pages.
Each page is a consistent database read, but an export during execution/retry is
not one global snapshot. Export after terminal outcomes for stable comparisons.
Outputs do not include provider credentials or private continuation.

## Public API

All routes below require the existing local bearer token. Health advertises
`tasks-runs` and `batch-matrices`; the SDK checks them before new mutations.

| Method and route | Payload / result |
| --- | --- |
| POST /v1/tasks | `{command_id, spec: TaskSpec}` → `TaskReceipt`, HTTP 202 |
| GET /v1/tasks | Paginated tasks |
| GET /v1/tasks/{id} | Task inputs, owner, batch/index, event watermark, latest run |
| GET /v1/tasks/{id}/runs | Paginated attempt history |
| GET /v1/tasks/{id}/events | Paginated committed task events |
| POST /v1/tasks/{id}/instructions | `SendMessageRequest` with next-boundary/immediate delivery → session receipt |
| GET /v1/runs/{id} | Attempt, session/workspace, primary turn, and effects uncertainty |
| POST /v1/runs/{id}/pause, /resume, /cancel | `{command_id}` → existing turn receipt, HTTP 202 |
| POST /v1/runs/{id}/retry | `{command_id, acknowledge_unknown_effects?: false}` → `TaskReceipt`, HTTP 202 |
| POST /v1/batches/preview | `BatchSpec` → frozen spec and ordered combinations, HTTP 200 |
| POST /v1/batches | `{command_id, spec: BatchSpec}` → batch/member receipt, HTTP 202 |
| GET /v1/batches | Paginated batches |
| GET /v1/batches/{id} | Frozen spec, owner, size, latest status counts |
| GET /v1/batches/{id}/members | Paginated member tasks in combination order |
| GET /v1/batches/{id}/results | Paginated member tasks and canonical outputs |
| POST /v1/batches/{id}/retry | `{command_id, indices, acknowledge_unknown_effects?: false}` → selected new-run receipts, HTTP 202 |
| POST /v1/batches/{id}/cancel | `{command_id}` → `BatchCancelReceipt` with affected run IDs, HTTP 202 |

Queries use `after` and `limit` (1–200; default 50). Follow each returned
`next_after`; member/result cursors are opaque creation-order values rather than
combination indexes. Run history uses attempt-number cursors and task events use
task-sequence cursors. Mutations reuse the original command ID and payload after
uncertain delivery; different input or scope conflicts. Idempotent retries do not
resolve Git refs or preflight mutable provider settings again.

## Limits and validation

Matrices have 1–256 members, checked with overflow-safe cardinality before
expansion. Batch concurrency is 1–16 and also obeys daemon-wide admission limits.
The database claim transaction counts active members, so concurrent admissions
cannot exceed the batch cap. When that cap is below the global cap, a batch at
its limit leaves spare global slots available for other work. Paused members
release their batch slots.

Fair admission also applies when a batch cap equals the global cap. Paused and
awaiting-children runs release both global and batch execution slots. Health
advertises `orchestration-controls`; the SDK checks it before budgeted submissions
and batch cancellation.

There are at most 1,024 active job attempts, with the existing 1,024 queued-turn
limit also enforced. Managed workspaces retain their 256-global/32-per-project
limits; old attempt reservations count until explicitly removed. A project
matrix can therefore have fewer than 256 members. Request bodies are bounded at
2 MiB, preview JSON at 8 MiB, and query pages target 4 MiB with bounded individual
records. Prompt, context, tool, Git, and subprocess limits remain unchanged.

Offline validation covers an eight-cell matrix with two models and two settings,
isolated diffs, lazy allocation, spare-capacity admission, selective retry/export,
command deduplication, pause/resume/steering, transactional fault injection,
schema-five upgrade, and interruption of a real shell side effect without replay.
Additional checks cover persistent admission rotation, returning groups,
aggregate reservation across retries/restart, budget denial before filesystem
dispatch, atomic batch cancellation, and [native delegation](child-tasks.md).
Native Windows and live-provider validation remain outstanding. Awaiting-input
states, branch publishing, automatic retention, and conversation reuse remain
future work.
