# Native child tasks and durable waits

**Implemented local slice, 2026-10-14.** Authorized tasks can create, inspect,
wait for, read results from, and cancel child tasks through native Rust tools.
The public API and native SDK/CLI use the same transactional application
operations. Ordinary chats and tasks have no delegation capability by default.

## Explicit authority and inputs

Set `SessionSpec.orchestration` to an `OrchestrationPolicy` and provide a finite
`budget`. The policy selects allowed provider-qualified models, coding tools,
maximum concurrent child count. `max_concurrent_children` bounds active child
execution independently of `max_children`, which bounds outstanding children.
Delegation is one level: children are always
leaves, including children migrated from older deeper trees. Legacy links and
policies remain readable, but attempts to create grandchildren are rejected at
the storage boundary. `max_children` limits concurrently outstanding children,
not cumulative history; aggregate operation budgets remain the cumulative
bound. Empty `allowed_models` grants all models in the daemon's supported
provider-qualified catalog. A nonempty list restricts models explicitly and
has no arbitrary eight-model cap.
New policies should set `max_depth` to 1. Legacy values 2–4 remain accepted for
file compatibility, but cannot grant nested spawning authority.
Allowed tools are a subset of the parent's selected workspace tools. A text-only parent
uses an empty coding-tool list. Go, Zen, and Codex can be delegating parents
when explicitly authorized. Codex subscription children omit output caps; an
explicit cap is rejected, and there is no inherited Platform billing fallback.

Children choose their prompt and model explicitly. Omitted Go/Zen output caps
use the parent's frozen cap; requested inputs remain separately recorded. No
conversation is copied implicitly. A child selects an existing workspace with a
subset of authorized coding tools. It uses the built-in `default` workspace for
text-only work. Child creation never creates a directory or worktree. If a child
needs independent writable state, create a worktree explicitly first and pass
its workspace ID. Follow-ups and retries reuse the child's selected workspace.

An omitted child budget inherits the parent's finite limits. An explicit child
budget cannot exceed them. Every child request/tool dispatch also consumes the
budgets of its ancestors and, if applicable, its root's batch. Cancellation
independence does not detach a child from those budgets. Children cannot receive
further delegation authority.

`CreateChildRequest.context` selects at most 16 completed public message IDs from
the parent attempt's session and 16 artifact IDs produced by its tools. Selected
message text and bounded previews of text/JSON artifacts enter the child prompt,
with provenance recorded in its child link. Artifact previews are at most 16 KiB
each; the complete effective prompt must fit the existing 64 KiB prompt limit.
Private provider continuation and daemon credential records are not copied.
Binary artifact consumption remains outside this slice.

## CLI workflow

The [policy example](../examples/child-policy.json) authorizes writing in the
selected workspace. Open a directory, then submit a parent:

```sh
hivectl workspace open /absolute/repository
hivectl task create --model opencode-go/glm-5.3-flash --workspace-id WORKSPACE_ID \
  --orchestration-policy examples/child-policy.json \
  --max-model-requests 32 --max-tool-calls 64 \
  --text "Delegate two independent implementations and compare their results." \
  --command-id delegated-job
hivectl run children PARENT_RUN_ID
hivectl task follow PARENT_TASK_ID
```

If a CLI policy is supplied without aggregate limit flags, it supplies a budget
of 64 model requests and 128 tool calls. API callers supply their budget explicitly.
The model-facing child tool describes the parent workspace ID and the explicit
`default` choice, so a model can select an existing workspace without inventing
an identifier. Children that share a workspace serialize writes by canonical root. Each child
can select a different existing workspace when appropriate.

Clients can also create a child directly. Replace the workspace ID in the
[child request example](../examples/child-request.json); the file contains inputs,
and the CLI supplies its command ID:

```sh
hivectl run create-child PARENT_RUN_ID examples/child-request.json --command-id child-1
hivectl run wait PARENT_RUN_ID --child-run CHILD_RUN_ID --command-id wait-child-1
hivectl run result CHILD_RUN_ID
hivectl run cancel CHILD_RUN_ID --command-id cancel-child-1
```

Creation requires the parent's latest attempt to be queued, running, paused,
or awaiting children, with no cancellation pending. Repeating an accepted
command returns its original receipt, even after state changes. Child queries
remain available for old attempts. Results expose the attempt and canonical
public output; artifacts and workspace diffs retain their existing API operations.

## Native tools and waiting

The four coding tools remain `read`, `write`, `edit`, and `bash`. A delegation
policy separately enables `child_create`, `child_discover`, `child_send`,
`child_inspect`, `child_wait`, `child_result`, and `child_cancel`; coding-tool
permission alone cannot enable them. They have durable tool intents/results and
consume tool-call budgets. Discovery is a bounded, paginated catalog of runtime
adapter capabilities; it does not reveal credentials or claim account entitlement.
Delegating turns obey explicit workspace caps when present and the aggregate
task/tree/batch budgets. There is no additional small per-turn step ceiling;
the CLI's default 64-request/128-tool budget applies across the whole task and
its children.

Create inputs match the API child request without `command_id`. Its durable
invocation ID supplies that identity. Acceptance, parent/run links, the child
job, command receipt, and the successful creation tool result commit together.
Inspect/result/cancel/send target an owned direct child's task ID; ownership is
the parent conversation, so a later parent turn can control children from prior
turns. Inspection follows the earliest unfinished turn and exposes bounded
status/activity/usage, with the latest run ID reported separately. Result returns
the latest terminal attempt only after every child turn has finished.
Native cancellation stops every unfinished turn, including queued follow-ups,
so queued steering cannot restart a canceled child. Native result previews
disclose truncation and canonical message references. `child_send` schedules durable steering for the next safe
boundary or after the current turn; retries reuse its invocation receipt.

New children default to completion notifications; `manual` disables automatic
delivery but does not detach lifecycle. Completion notices are durable and
deduplicated by child run, delivered in bounded batches at safe inference or
finalization boundaries, and include terminal status, effects uncertainty, a
bounded final preview, and canonical references. They never interrupt an active
provider request or tool. Paused/canceled parents are not automatically resumed;
detached completions remain available to a later explicit parent turn. Legacy
child links migrate to manual notification mode. An attached child still delays
parent finalization in manual mode; its terminal transition readmits the parent
so it can inspect the child or finish its turn.

Wait selects distinct direct-child **run IDs**, up to 32 at once. Omitting IDs
snapshots each direct child's earliest unfinished turn, or its latest terminal
turn if idle (and rejects snapshots over 32); retries
are different attempts and do not silently change an existing wait. `all` is
the default mode; `any` completes when one selected child is terminal. A bounded
timeout returns completed and pending IDs without canceling children. Paused
children are nonterminal. Attached children independently hold parent
finalization open; manual notification does not detach them. Set `detached=true`
to allow a child to outlive normal parent completion, while
`cancel_with_parent` independently controls cancellation propagation.
There is one pending wait per parent turn and at most 64 recorded waits per turn.
The daemon commits the dependency before the parent yields at a safe boundary.
It skips other unstarted tool proposals from that response, preserving their
failure records rather than dispatching them on wake. A waiting parent holds
no inference/tool execution slot or full in-memory context.

When the selected attempts meet the chosen mode or timeout expires, the daemon commits a known wait result
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
effects requires the existing explicit acknowledgement and retains the child's
workspace. Create a separate workspace explicitly when isolation from earlier
filesystem effects is desired.

## Public API and validation

Health advertises `child-tasks`; the SDK requires it before new mutations.
All routes use the existing local bearer token:

| Route | Input/result |
| --- | --- |
| POST /v1/runs/{id}/children | `CreateChildRequest` → `TaskReceipt`, HTTP 202 |
| GET /v1/runs/{id}/children | Paginated direct-child `SessionResponse` records |
| POST /v1/runs/{id}/wait | `WaitChildrenRequest` → `CommandReceipt`, HTTP 202 |
| GET /v1/runs/{id}/result | `RunResultResponse` with attempt and public output |

Progress reuses `GET /v1/runs/{id}` plus the existing task event stream/pages;
steering reuses the public task/session message operation (also exposed through
the CLI's existing run/task send command). These operations remain available
after the parent turn finishes and are scoped to the owned child conversation.

Child links include parent task/run IDs, root task ID, depth, cancellation and
notification policy, detached state, and selected context IDs. Task/session
events record creation, accepted waits, awaiting state, wait completion, and
interrupted waits. Schema migrations preserve older command receipts and treat
legacy links conservatively as manual notifications. Deeper legacy trees remain
readable, but their children have no spawning authority.

Offline checks exercise real isolated child edits, selected artifact previews,
API/CLI child creation and wait receipts, a pool of four waiting parents,
transactional creation failures, pause/wait races, schema-six upgrades,
hierarchy limits, descendant budgets and batch cancellation, and restart during
a real child shell side effect without replay.
The 2026-10-14 extension adds checks for completion delivery into provider context,
any/timeout joins with active siblings, receipt migration/replay, cross-provider
settings, concurrency limits, and progress/discovery pagination. These checks
passed locally on Linux. [Recorded native Windows validation](native-validation.md)
covers the earlier slice; a Windows rerun and live-provider validation of the new
controls remain outstanding.
