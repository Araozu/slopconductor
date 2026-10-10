# Native agent runtime

## Responsibilities

The runtime owns model interaction, context assembly, tool dispatch, task/run
state transitions, admission, and recovery checkpoints. The daemon composes it
with concrete configuration and persistence. Frontends provide commands and
presentation; they never run the agent loop.

At M0, `slop-runtime` was an empty boundary crate. The provider registry
(`slop-runtime::providers`) now implements OpenCode Go and Zen: separate static
catalogs mapping each executable model to its documented wire shape (OpenAI
Chat Completions, OpenAI Responses, or Anthropic Messages), `OPENCODE_GO_API_KEY`
and `OPENCODE_ZEN_API_KEY` authentication, live model listing, and non-streaming
plus SSE-streaming inference through a shared one-turn `ProviderClient` trait.
The same interface now includes headless Codex access through the public
Responses API, with native ChatGPT subscription login or Platform API keys;
see [Codex connection](codex-connection.md). All three clients share bounded
wire decoders. Responses preserve terminal outcomes, optional usage/provenance,
and requested/reported model IDs. The daemon composes all three providers with
a bounded turn supervisor, durable repository port, session/workspace
serialization, visible checkpoints, and cancellation. Go and Zen support the
bounded file/shell tool loop; Codex is text-only. Provider clients are selected
per turn and snapshotted at admission. See [text chat](text-chat.md) and
[structured execution](structured-execution.md). [Durable tasks and matrices](tasks-batches.md) share this turn scheduler, with
per-batch admission caps, persistent fair admission, aggregate operation budgets,
and explicit fresh retries. [Native child orchestration](child-tasks.md) is
implemented with separate authority and durable waits that release slots.

## Agent loop

The following is the target general task loop. The implemented user-turn loop
covers model intent, structured completion, tool policy/dispatch/results, budgets,
streaming, cancellation, boundary/immediate steering, explicit pause/resume, and
terminal/recovery records. Tasks map each attempt to one primary turn and use
its steering/control inbox. Child creation and awaiting-children states are
implemented; awaiting-input states remain proposed.

Steering instructions are durably accepted with command IDs and applied once at
execution boundaries. Next-boundary delivery never interrupts the current
inference or tool. Immediate delivery cancels in-flight inference and supervised
Bash, waits for actual file-operation outcomes, pairs any unstarted tool intents
with skipped results, and then replans. An interrupted inference's usage is
unknown and its incomplete reasoning is excluded from context. Paused turns
release concurrency slots, block later turns in their session, survive restart,
and resume only after an explicit command. A resumed pause is a fresh admission;
same-turn steering continuations retain the provider snapshot.

One admitted run has a serialized supervisor and bounded command inbox:

1. Load its task goal, selected policy, workspace reservation, and checkpoint.
2. Process accepted steering/control commands in owner-defined order.
3. Check cancellation, limits, and remaining provider/tool budgets.
4. Assemble bounded model context from durable messages and referenced data.
5. Record model-request intent and issue a direct provider request.
6. Stream transient deltas while periodically recording useful partial state.
7. Record the completed model response and provider continuation metadata.
8. Validate requested tool calls against the run's allowed capabilities.
9. Persist each tool invocation, supervise execution, and record its outcome.
10. Record tool results in provider-compatible context and continue as needed.
11. Finish, pause, await input/children, fail, or enter recovery-required state.

The supervisor is an async task in the daemon's shared runtime. A database record
may exist for millions of historical sessions without having a live supervisor
for each one. Resource admission creates active state only when needed.

## Domain state and durable steps

Keep a small explicit state machine. The proposed task states are queued,
active, awaiting input, paused, succeeded, failed, canceled, and recovery
required. A run is an attempt with its own lifecycle and execution steps. A task
can have several attempts; a completed model response is not a new attempt.

Persist step kinds such as assembling context, awaiting provider, tool intent
recorded, tool started, tool completed, and checkpointed. The durable checkpoint
contains serializable information about what to do next. It cannot be a Tokio
future, open socket, process handle, or in-memory continuation.

Failures are classified by whether they are retryable and whether retry can
repeat a side effect. An incomplete provider response remains incomplete even
if it emitted plausible text or a partial tool-call JSON fragment.
Interrupted thinking is discarded, rather than resumed or used as completed
context. Completed reasoning content is retained only when the adapter supports
it and the corresponding completed turn has committed.

## Provider boundary

The [shared provider interface](provider-interface.md) specifies the runtime
operations, capability validation, structured content, terminal outcomes,
and conformance criteria. It also defines the daemon's mapping to a common public
surface for every client. The registry `Provider` trait implements metadata;
`ProviderClient::infer` implements structured text/refusal/tool blocks, settings,
private continuation, and normalized incremental events for Go/Zen; Codex uses
a text-only bridge with explicit unsupported errors. The public API projects
canonical messages, provider capabilities, usage, and cancellation; Go/Zen also
expose tools and artifacts, including separately authorized
[native child tools](child-tasks.md). Private provider events stay internal.
Account-scoped discovery remains proposed.

A provider integration should implement model listing/validation, authentication
status, inference streaming, cancellation support, and usage/limit reporting.
Expose provider capabilities rather than assuming one universal request schema.

The runtime's provider-neutral representation includes role/message content,
tool calls and matching results, attachments, usage records, and an escape hatch
for opaque provider continuation data. Preserve call IDs and required reasoning
or encrypted continuation fields. Lossy conversion must be explicit.

Record the requested model, resolved model identifier, provider, account handle,
and effective settings for every run. If a setting is unsupported, reject it
during validation. Do not silently remove it from an experiment.

Share HTTP connection pools and account-level admission limits across sessions.
Distinguish a model quota from a per-session concurrency limit. Retrying transient
inference failures needs backoff and a bounded attempt policy.

### Authentication

API-key support is the first concrete provider integration, implemented for
OpenCode Go via `OPENCODE_GO_API_KEY` and Zen via `OPENCODE_ZEN_API_KEY` (Bearer
for Chat/Responses/model list, `x-api-key` plus `anthropic-version` for Messages).
The current clients accept an explicit key or read their own environment
variable; there is no fallback between gateways. The daemon now owns
[runtime API-key configuration, private XDG storage, and ChatGPT login](provider-credentials.md).
Go connection replacements affect subsequent admissions while active turns
retain their original client. Broader opaque account handles remain proposed.
Session exports, batches, logs, and event streams exclude actual secrets.

Supported subscriptions are a separate authentication capability of a provider.
They need account selection, token refresh, revocation handling, and usage-limit
visibility. Do not assume credentials accepted by an official agent client can
be used in arbitrary inference endpoints.

The Codex connection implements the documented Sign in with ChatGPT plan-usage
flow for eligible open-source/local apps and direct Responses API inference,
including protected per-account credentials and serialized token refresh.
Subscription requests require no output-token cap (`max_tokens: None`) and
preserve explicit developer instruction roles. Broader account selection,
revocation and quota discovery remain proposed. Paid/hosted products have
separate access requirements. See the [overview](https://developers.openai.com/siwc/token-sharing-open-source)
and [inference guide](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference).

Local-model support can be another provider implementation later. No model
server, GPU runtime, or local inference engine is a mandatory daemon dependency.

## Context management

Load only the context required for an active turn. Use stable message/artifact
references and retain full historical records on disk. Record what context was
selected and how it was shortened so results can be explained and experiments
can be compared.

A context policy controls recent messages, pinned instructions, workspace
summaries, relevant tool results, and attachment budgets. Summarization is a
recorded transformation with provenance, not silent history deletion.

Separate session-level context from run-level configuration. Changing a model
or machine may require capability validation and context reconstruction. A
provider-owned response ID is an optimization only when the owning account and
endpoint support it; local data must remain sufficient for supported export.

## Initial tools

The implemented tool set is exactly `read`, `write`, `edit`, and `bash`, following
[pi's basic coding tools](https://github.com/earendil-works/pi/tree/main/packages/coding-agent/src/core/tools).
`read` pages UTF-8 text by 1-based line offsets; `write` creates or overwrites
files and missing parent directories; `edit` validates unique, non-overlapping
exact text replacements before an atomic file update. `bash` runs actual Bash
from the workspace root with a filtered environment, output bounds, a policy
deadline, cancellation, and streamed stdout/stderr. Windows requires Bash on
PATH. The four tools are native Rust implementations with the existing workspace
policy, durable invocation records, and artifact storage; no pi/JS runtime is
used. See [structured execution](structured-execution.md) for schemas and limits.

The native [Git workspace service](projects-workspaces.md) implements repository
inspection, frozen-base resolution, supervised detached worktree allocation,
diff/status, and conservative cleanup. These are daemon application operations;
the coding-tool set stays at four. Dedicated model Git tools, merging, publishing,
and richer result artifacts remain proposed.

Native orchestration tools now create, inspect, wait for, read results from, and
cancel direct children through the public API's application operations. Explicit
delegation policies select context/artifact previews, model/tool subsets,
depth/fan-out limits, and inherited aggregate budgets. Sending a model-authored
instruction to a child remains future work; clients can use existing task
instruction operations. See [child tasks](child-tasks.md).

A browser tool service is later work. Its active context and displayed client
view must have matching identity. Browser processes and contexts have separate
resource limits; creating a chat cannot implicitly launch a browser.

## Tool supervision and policy

Each invocation has an identity, recorded input, workspace, start time, limits,
and outcome. Keep previews bounded and put large outputs in artifact storage.
The implemented shell uses Windows Job Objects and Unix process groups with
bounded cancellation cleanup. [Native offline validation](native-validation.md)
passes on Linux and Windows; hard daemon death on Unix does not guarantee
descendant cleanup.

A run's capabilities are chosen before execution. Child tasks inherit a bounded
subset or explicitly allowed additions. Repository content and model-generated
instructions do not expand application authorization.

File/worktree separation is not an OS sandbox. If stronger isolation is needed,
make it a distinct execution capability with documented OS support. Do not claim
arbitrary shell access is sandboxed by a working directory.

Policies should make unattended execution predictable. An operation requiring
human approval puts the task in an explicit awaiting-input state; it must not
block an invisible client-side prompt.

## Admission, child tasks, and fairness

Use daemon-level limits for admitted runs, provider requests, tools, and browser
contexts. Add per-project writer limits and per-account provider limits. A batch
cannot consume every slot indefinitely; fairness operates across batches and
interactive tasks.

The implemented scheduler rotates persistent admission tickets across batches,
independent task trees, and ordinary sessions; newly queued/returning groups
enter at the current clock. Running work is not preempted. Descendants share
their root's batch cap and consume all applicable ancestor/batch budgets before
dispatch. Account-specific admission and browser contexts remain future work.

Parents waiting for children must release scarce inference/tool execution slots.
Otherwise a full pool of waiting parents can prevent their children from ever
running. Bound child depth, count, budget, and outstanding waiting relationships.

Cancel propagation is recorded at child creation. A child may be canceled with
its parent or allowed to finish independently. Reconnection or parent restart
must reattach to the same child IDs rather than spawning duplicates.

On restart, known child waits become paused and require explicit resume. Child
completion can persist its known wait result while the parent is paused. Native
creation and its tool result commit together, so recovery never launches another
child for the accepted invocation.

## Recovery and measurement

After daemon restart, reconcile recorded steps with tool/process facts and
provider outcomes where available. Known completed results can be reused.
Interrupted thinking is discarded. An unfinished tool call is recorded as
failed due to daemon/process failure; the daemon does not restore or replay it.
The failure record preserves the invocation ID and warns when its external
effects are unknown. The agent decides whether to inspect those effects or issue
a new call. This behavior is implemented for supported user turns, local tools,
and task attempts. Retry creates a fresh attempt explicitly; handoff recovery
remains proposed.

Measure release-build baseline RSS, incremental active context and buffer memory,
idle-session metadata cost, context assembly CPU, queue fairness, and tool
subprocess consumption. Report the daemon and its supervised tools separately.
The chosen language is an architectural aid; a large unbounded message buffer
still consumes memory in Rust.
