# Native child tasks and durable waits

**Implemented local slice, 2026-10-10.** Authorized tasks can create, inspect,
wait for, read results from, and cancel child tasks through native Rust tools.
The public API and native SDK/CLI use the same transactional application
operations. Ordinary chats and tasks have no delegation capability by default.

## Explicit authority and inputs

Set `TaskSpec.orchestration` to an `OrchestrationPolicy` and provide a finite
`budget`. The policy selects allowed provider-qualified models, coding tools,
maximum child count, and remaining delegation depth. The root allows at most
32 children across its whole tree, including children created by earlier
attempts; depth is 1–4. Each delegating node also enforces its direct-child limit.
Allowed models are unique, supported model IDs, with at most eight entries.
Allowed tools are a subset of the parent's project tools. A text-only parent
uses an empty coding-tool list. Codex remains text-only and cannot be a
delegating parent.

Children choose their prompt and model explicitly. Omitted Go/Zen output caps
use the parent's frozen cap; requested inputs remain separately recorded. No
conversation is copied implicitly. A child may omit its project for text-only work, or select the
same registered project with a subset of authorized coding tools. Its base must
be the parent's frozen commit; a dirty parent workspace is never copied. Each
attempt has a fresh session and its own lazily allocated worktree reservation.

An omitted child budget inherits the parent's finite limits. An explicit child
budget cannot exceed them. Every child request/tool dispatch also consumes the
budgets of its ancestors and, if applicable, its root's batch. Cancellation
independence does not detach a child from those budgets. Further delegation
requires an explicit child policy with fewer remaining levels and subsets of
the parent's models/tools/limits; no policy means a leaf.

`CreateChildRequest.context` selects at most 16 completed public message IDs from
the parent attempt's session and 16 artifact IDs produced by its tools. Selected
message text and bounded previews of text/JSON artifacts enter the child prompt,
with provenance recorded in its child link. Artifact previews are at most 16 KiB
each; the complete effective prompt must fit the existing 64 KiB prompt limit.
Private provider continuation and daemon credential records are not copied.
Binary artifact consumption remains outside this slice.

## CLI workflow

The [policy example](../examples/child-policy.json) authorizes writing in the
selected project. Register a project, then submit a parent:

```sh
slop task create --model opencode-go/glm-5.3-flash --project PROJECT_ID \
  --orchestration-policy examples/child-policy.json \
  --max-model-requests 32 --max-tool-calls 64 \
  --text "Delegate two independent implementations and compare their results." \
  --command-id delegated-job
slop run children PARENT_RUN_ID
slop task follow PARENT_TASK_ID
```

If a CLI policy is supplied without aggregate limit flags, it supplies a budget
of 64 model requests and 128 tool calls. API callers supply their budget explicitly.
Project reservations, descendants, and retained retry workspaces share the
existing 32-per-project/256-global workspace quotas, so leave capacity for children.

Clients can also create a child directly. Replace the project ID in the
[child request example](../examples/child-request.json); the file contains inputs,
and the CLI supplies its command ID:

```sh
slop run create-child PARENT_RUN_ID examples/child-request.json --command-id child-1
slop run wait PARENT_RUN_ID --child-run CHILD_RUN_ID --command-id wait-child-1
slop run result CHILD_RUN_ID
slop run cancel CHILD_RUN_ID --command-id cancel-child-1
```

Creation requires the parent's latest attempt to be queued, running, paused,
or awaiting children, with no cancellation pending. Repeating an accepted
command returns its original receipt, even after state changes. Child queries
remain available for old attempts. Results expose the attempt and canonical
public output; artifacts and worktree diffs retain their existing API operations.

## Native tools and waiting

The four coding tools remain `read`, `write`, `edit`, and `bash`. A delegation
policy separately enables `child_create`, `child_inspect`, `child_wait`,
`child_result`, and `child_cancel`; coding-tool permission alone cannot enable
them. All five have durable tool intents/results and consume tool-call budgets.
Delegating turns allow at most 64 model requests and 128 total tool proposals;
aggregate task/tree/batch budgets can stop them sooner.

Create inputs match the API child request without `command_id`. Its durable
invocation ID supplies that identity. Acceptance, parent/run links, the child
job, command receipt, and the successful creation tool result commit together.
Inspect/result/cancel target a direct child's task ID; inspect/result use its
latest attempt. Result requires a terminal attempt. Native result previews are
bounded at 16 KiB and disclose truncation with message IDs for full public reads.

Wait selects distinct, explicit direct-child **run IDs**, up to 32 at once.
Retries are different attempts and do not silently change an existing wait.
There is one pending wait per parent turn and at most 64 recorded waits per turn.
The daemon commits the dependency before the parent yields at a safe boundary.
It skips other unstarted tool proposals from that response, preserving their
failure records rather than dispatching them on wake. A waiting parent holds
no inference/tool execution slot or full in-memory context.

When every selected attempt is terminal, the daemon commits a known wait result
and readmits an awaiting parent through ordinary fair scheduling. Failed,
canceled, interrupted, or incomplete children count as terminal and remain
visible in the result. Uncertain effects are included explicitly. Paused
children require separate user action; no hidden prompt or retry advances them.
API waits on running parents take effect at an execution boundary. Queued
parents can enter `awaiting_children` immediately. An explicit pause remains
paused; resume returns to waiting if dependencies are still pending.

## Cancellation and recovery

`cancel_with_parent` defaults to true and is recorded at creation. Canceling a
parent propagates along those links; a false link lets that child branch finish
independently. Batch cancellation covers every unfinished attempt sharing the
batch, including descendants regardless of that parent cancellation choice.
Retries cannot reopen a canceled batch or a child canceled with its parent.

After daemon restart, awaiting parents become paused with their dependency
records and child IDs preserved. Queued children remain eligible for admission;
previously running children become interrupted under the existing recovery rules.
Known waits can finish while a parent is paused, but provider execution resumes
only after an explicit resume command. Native creation is not duplicated, and
unknown shell/Git operations are never replayed. A child retry with uncertain
effects requires the existing explicit acknowledgement and a fresh workspace.

## Public API and validation

Health advertises `child-tasks`; the SDK requires it before new mutations.
All routes use the existing local bearer token:

| Route | Input/result |
| --- | --- |
| POST /v1/runs/{id}/children | `CreateChildRequest` → `TaskReceipt`, HTTP 202 |
| GET /v1/runs/{id}/children | Paginated direct-child `TaskResponse` records |
| POST /v1/runs/{id}/wait | `WaitChildrenRequest` → `CommandReceipt`, HTTP 202 |
| GET /v1/runs/{id}/result | `RunResultResponse` with attempt and public output |

Child links include parent task/run IDs, root task ID, depth, cancellation
policy, and selected context IDs. Task/session events record creation, accepted
waits, awaiting state, wait completion, and interrupted waits. Schema eight adds
these links and dependencies while preserving older command receipts.

Offline checks exercise real isolated child edits, selected artifact previews,
API/CLI child creation and wait receipts, a pool of four waiting parents,
transactional creation failures, pause/wait races, schema-six upgrades,
hierarchy limits, descendant budgets and batch cancellation, and restart during
a real child shell side effect without replay.
[Recorded offline validation](native-validation.md) passes on Linux and native
Windows; live-provider validation remains outstanding.
