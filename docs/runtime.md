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
and requested/reported model IDs. The agent loop, scheduler, tools, and remaining
sections below are still proposed for M1 and M2.

## Agent loop

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

## Provider boundary

The [shared provider interface](provider-interface.md) specifies the proposed
runtime operations, capability validation, structured content, terminal outcomes,
and conformance criteria. It also defines the daemon's mapping to a common public
surface for every client. The registry `Provider` trait implements metadata;
`ProviderClient` implements the text-only execution subset. Capability discovery,
structured blocks, cancellation and the broader public surface remain proposed.

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
variable; there is no fallback between gateways. Owner credential storage and
opaque account handles remain proposed. Session
exports, batches, logs, and event streams exclude actual secrets.

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

Filesystem tools: inspect/list files, read bounded ranges, write files, and apply
validated patches within the selected workspace policy.

Shell tool: executable, argument vector, explicit working directory, environment
policy, timeout, cancellation handle, and streamed stdout/stderr. Use argument
vectors for structured operations; an explicitly requested shell command can
use the configured shell. Linux and Windows shell defaults are separate.

Git tools: register a repository, inspect status/diff, create managed worktrees,
track base commits, and produce artifacts. Commits, merges, pushes, and cleanup
have explicit policies. Starting a task does not silently merge its edits.

Native orchestration tools: create a child task/session, inspect/wait for a child,
send a message, and consume an artifact. Use the same service-level command
validation as the public API.

A browser tool service is later work. Its active context and displayed client
view must have matching identity. Browser processes and contexts have separate
resource limits; creating a chat cannot implicitly launch a browser.

## Tool supervision and policy

Each invocation has an identity, recorded input, workspace, start time, limits,
and outcome. Keep previews bounded and put large outputs in artifact storage.
On Windows, process-tree supervision will need Job Object or equivalent support;
on Linux, process groups/service supervision need explicit handling.

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

Parents waiting for children must release scarce inference/tool execution slots.
Otherwise a full pool of waiting parents can prevent their children from ever
running. Bound child depth, count, budget, and outstanding waiting relationships.

Cancel propagation is recorded at child creation. A child may be canceled with
its parent or allowed to finish independently. Reconnection or parent restart
must reattach to the same child IDs rather than spawning duplicates.

## Recovery and measurement

After daemon restart, reconcile recorded steps with tool/process facts and
provider outcomes where available. Known completed results can be reused. Unknown
side effects require reconciliation before execution advances.

Measure release-build baseline RSS, incremental active context and buffer memory,
idle-session metadata cost, context assembly CPU, queue fairness, and tool
subprocess consumption. Report the daemon and its supervised tools separately.
The chosen language is an architectural aid; a large unbounded message buffer
still consumes memory in Rust.
