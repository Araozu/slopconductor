# Native memory and ownership audit

The 2026-10-13 audit covers active context ownership, provider request/stream
buffers, SQLite decoding, tool arguments, scheduler snapshots, and event output.
The daemon and execution runtime remain Rust, with one shared Tokio runtime and
one bounded database worker. The public API and durable history format do not
change.

## Implemented fixes

- Structured admission builds canonical history alone. It no longer loads and
  retains a second text-only copy, and SQLite JSON is parsed directly from the
  borrowed row while producing owned domain values for the worker boundary.
- The supervisor moves history out of its claimed work and into each normal
  inference request. After inference finishes, it moves the same buffers back
  for tools, retries, and continuations. Summary prompts also move into their
  requests; obsolete request and stream buffers are released before persistence.
- Execution borrows its repository, provider, event sender, and tool service from
  the owning scheduler task. Admission snapshots the selected provider instead
  of cloning the complete provider map. The already pinned provider future is
  used directly instead of boxing it again.
- Context budgeting, request/output validation, and history pagination count
  exact serialized JSON bytes through a counting writer. They preserve escaping,
  UTF-8, continuation accounting, and existing limits without allocating encoded
  copies just to inspect their size. Saving a context checkpoint reads the old
  cursor alone instead of deserializing the previous history.
- Provider request builders release their intermediate JSON trees before awaiting
  the network. Responses terminal snapshots move into the stream fold.
- Anthropic text, reasoning, and signature deltas append to their existing
  strings instead of copying the accumulated content on every delta. Responses
  and Anthropic streams retain text by block and track the aggregate byte bound
  with a counter. They no longer keep another concatenated text buffer.
- Visible checkpoints copy changed blocks only. They do not repeatedly concatenate
  text or retain a second full snapshot for comparison. The callback borrows the
  supervisor's local mutex; an extra `Arc` allocation is unnecessary.
- Tool-argument validation borrows strings from JSON using the execution schemas.
  File workers own their admission permits directly until actual completion,
  including when the awaiting future is dropped. NDJSON events append their
  newline to the existing encoded buffer instead of copying that buffer.

## Shared ownership retained deliberately

`Arc` remains at service boundaries where concurrent tasks or clients own handles
to the same database service, runtime, provider connection, or semaphore. The
provider registry and pinned-provider maps hold connections and small metadata,
not session histories. A selected connection remains pinned across same-turn
steering and child waits; explicit pause/resume takes a fresh snapshot.

The database admission gate and scheduler handle use short synchronous mutex
sections. Credential changes and Git mutations use async mutexes where operations
must remain serialized across awaits. Visible output uses a local synchronous
mutex because inference borrows its callback while checkpoint polling reads the
same data. Locks are released before awaiting storage.

Cloning remains where independent ownership is required for durable completions,
retryable storage outcomes, blocking tool work, or client event frames. A
compaction plan preserves the original context until it has a usable summary;
the canonical transcript remains on disk. These copies are bounded and distinct
from retaining several full histories throughout normal inference.

## Reproduce release RSS measurements

```sh
cargo build --release -p hive-daemon -p hive-cli --locked
python3 scripts/memory_audit.py --daemon target/release/hived --runs 3
```

The Linux-only script uses temporary state and synthetic local provider
credentials. It creates four completed conversations, stops the daemon, and seeds
32 alternating text messages of 64 KiB each into each conversation's inference
checkpoint. It restarts, records baseline RSS, creates 1/10/100 additional idle
sessions, then holds four concurrent inference requests with 2 MiB of historical
text each. Full context is explicitly enabled, keeping these fixtures below the
model's configured window without compaction. Every held turn must complete after
the measurement gate opens.

Samples come from `/proc/<pid>/status`; RSS and high-water values are KiB and
thread counts are recorded separately. The fixture server is a separate process
from the measured daemon and retains no request bodies. No model APIs or tool
subprocesses are involved. Allocator caches and SQLite/HTTP buffers contribute to
RSS after completion, so that sample is not a live-heap or leak measurement.

This is a controlled comparison of idle metadata and four active contexts.
Native Windows RSS, 10/100 active sessions, large queued matrices, live providers,
tool subprocess memory, sustained churn, and heap profiling remain separate
capacity-validation work.

## Recorded comparison, 2026-10-13

Linux x86_64, Rust 1.99.0, four Tokio workers, default system allocator, release
profile with thin LTO and one codegen unit. Three fresh daemon processes were
measured for each build. The baseline is `d966820`; the fixed build contains the
ownership changes above on that same base. Each cell shows median RSS in MiB,
followed by the observed range. Both builds used seven threads throughout.

| Workload | Before | Fixed |
| --- | --- | --- |
| Restarted daemon, four unloaded checkpoints | 13.26 (13.19–13.26) | 13.11 (13.09–13.12) |
| One additional idle session | 13.95 (13.88–13.96) | 13.88 (13.83–13.91) |
| Ten additional idle sessions | 14.11 (14.05–14.14) | 14.03 (14.00–14.05) |
| One hundred additional idle sessions | 14.34 (14.24–14.40) | 14.27 (14.07–14.29) |
| Four active requests, 2 MiB history each | 59.20 (59.05–61.20) | 40.53 (39.02–40.84) |
| After those requests complete | 56.50 (53.38–61.20) | 23.28 (21.23–25.31) |

Median RSS during the held requests decreased by 31.5%. Idle memory did not
materially change. The improvement is specific to this workload; it does not
establish a fixed per-session memory cost or a maximum daemon capacity.

After rebasing onto `bf7487f` to include the concurrent chat-history work, three
more release runs measured 41.20 MiB active RSS (40.96–41.25), 13.25 MiB baseline
RSS (13.01–13.33), and 23.61 MiB after completion (23.43–25.30). All used seven
threads. The paired comparison above isolates the ownership patch on its
original base; this repeat checks that the combined build retains the benefit.
