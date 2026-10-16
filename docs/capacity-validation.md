# Offline capacity validation

`scripts/memory_audit.py` measures the native daemon against local provider
fixtures. Use a release binary for capacity evidence. The smaller debug-build
run in Linux CI verifies the fixture assertions; its numbers are not release
performance measurements. Python is development tooling, outside execution.

## Reproduce

```sh
cargo build --release -p hive-daemon -p hive-cli --locked
python3 scripts/memory_audit.py --runs 3 --output /tmp/hive-capacity-4.json
python3 scripts/memory_audit.py --runs 3 --concurrency 64 --output /tmp/hive-capacity-64.json
```

The default binary is `target/release/hived`; `--daemon PATH` selects another
build. The harness currently requires Linux `/proc` and Python 3.11 or newer.
It needs no provider credentials, external Python packages, or external network services.
Each workload uses a fresh daemon and temporary data/configuration directory,
synthetic credentials, and a loopback provider fixture. User databases and
workspaces are never used. The fixture runs outside the measured process tree
and discards request bodies after parsing them.

Progress goes to stderr. The complete JSON report goes to stdout and, when
specified, `--output`. Reports contain the binary SHA-256, observation time,
platform, Python version, workload parameters, memory snapshots and sampled
peaks, command acceptance latencies, provider arrival latencies, and drain times.
Keep generated reports outside source control; record reviewed results here.

## Workloads and assertions

- **Sessions:** `--sessions 1 10 100` creates empty idle conversations, completes
  seed turns, stops the daemon, and seeds identical inference checkpoints with
  `--context-kib 2048` of text each. The restarted daemon is sampled before and
  during inference, then after every accepted turn completes. Full context is
  explicitly enabled. The fixture holds requests until the configured admission
  limit is occupied and verifies both running and queued counts. Every session
  must reach the provider exactly once. Acceptance and provider-arrival latency
  distributions report nearest-rank p50/p95/max; arrival includes deliberate
  fixture hold time for queued work. Completion is observed by API polling.
- **Queued matrix:** a previewed, atomically submitted `--batch-size 256` matrix
  has a concurrency cap of two, or one when global concurrency is one. With
  default settings, exactly two members run and 254 remain queued while memory
  is sampled. Every member must subsequently complete exactly one inference.
  Members select the directory-less workspace and do not create worktrees.
- **Tools:** a supervised Python tool touches a `--tool-memory-mib 32` allocation
  before sampling. One invocation completes with 256 KiB of stdout; the harness
  verifies a truncated public preview and a durable output artifact. Another
  invocation is cancelled through the public API. Descendants must disappear
  after both completion and cancellation. The allocation is in an actual tool
  process supervised by the native runtime.
- **Churn:** `--churn-rounds 5 --churn-sessions 20` creates and completes 100
  conversations, recording memory after each round. Completed history remains
  durable, so this is a retained-history workload rather than a deletion/leak
  test.

`--concurrency` configures the daemon's existing 1–64 admission range. A workload
of 100 runnable sessions at concurrency 64 means 64 admitted and 36 queued;
it is not evidence of 100 simultaneously active contexts. Default concurrency
four gives 4 admitted and 96 queued. The high-concurrency profile also admits
all ten sessions in the ten-session workload. Numerical memory targets remain
unset until the measured workloads match intended use.

## Measurement limits

RSS, process high-water RSS, and thread counts come from `/proc/PID/status`.
All task-thread child lists are traversed to find supervised descendants;
their RSS and process count are reported separately from the daemon. RSS can
count shared pages in multiple processes. The sampler polls every 50 ms;
sampled peaks can miss shorter spikes. The reported high-water RSS is the
kernel's lifetime value for that process. Snapshots wait `--sample-seconds 0.3`
before reading. Post-completion RSS includes allocator caches, SQLite, and HTTP
buffers and is not a live-heap measurement.

The sessions workload's sampled peaks start after checkpoint seeding/restart.
Batch, tool, and churn peaks cover their whole daemon lifetime. Each workload
has a finite `--timeout 120` second wait/drain bound, configurable up to 300.
No timing or RSS threshold is enforced in CI; behavioral assertions fail the
command on excess admission, incomplete work, failed output capture, or
surviving tool descendants.

Native Windows resource sampling, live providers, heap profiling, browser
processes, and longer production workloads remain separate validation work.
Native Windows behavioral CI is recorded in [native validation](native-validation.md).
The earlier four-context ownership comparison is preserved in the
[memory audit](memory-audit.md); its fixture differs from the current harness.

## Recorded Linux release results, 2026-10-16

Measured native source: `6a3dfc89bf9c89b46eff16b3dc9fddef03bf171b`, built in an
isolated checkout so concurrent queued-prompt work could not change the binary.
Rust 1.99.0, Linux x86_64, four logical CPUs, 7.57 GiB host RAM, the repository's
release profile (thin LTO, one codegen unit), and the default system allocator.
Binary SHA-256:
`3219983767b5f50ea5652fbde27fefbb0bcace2072b3b3b91ca654d4218869ca`.
Both profiles below completed three independent runs with the default 2 MiB
context, 256-member batch, 32 MiB tool allocation, and five 20-session churn
rounds. These are offline fixture results on a shared host.

Each RSS cell is median MiB followed by the observed range. Idle conversations
are empty; held conversations each have a 2 MiB inference checkpoint.

| Global concurrency | Sessions | Admitted / queued | Idle daemon RSS | Held daemon RSS |
| --- | --- | --- | --- | --- |
| 4 | 1 | 1 / 0 | 29.91 (29.86–30.05) | 37.86 (37.85–37.88) |
| 4 | 10 | 4 / 6 | 30.07 (29.98–30.14) | 57.87 (57.79–59.19) |
| 4 | 100 | 4 / 96 | 30.19 (30.13–30.35) | 57.82 (57.82–57.99) |
| 64 | 1 | 1 / 0 | 29.77 (29.58–29.93) | 37.52 (37.29–37.73) |
| 64 | 10 | 10 / 0 | 30.01 (29.98–30.04) | 80.36 (80.18–80.51) |
| 64 | 100 | 64 / 36 | 30.20 (30.02–30.21) | 302.86 (302.71–305.45) |

All held session samples used seven daemon threads. At concurrency four, the
100-session workload's median sampled peak was 61.02 MiB (60.64–63.38), and
median RSS after completion was 54.38 MiB (50.06–58.30). At concurrency 64,
the corresponding peak was 302.93 MiB (302.77–305.51), with 228.47 MiB
(202.70–246.93) after completion. Higher retained RSS after a large active
workload requires allocator/heap investigation before drawing conclusions
about live retained context; these measurements alone do not establish a leak.

The 100-session workload's median per-run p95 acceptance latency was 15.82 ms
at concurrency four and 53.42 ms at concurrency 64. Median drain time after
releasing held provider requests was 3.21 s and 1.33 s respectively. Deliberate
fixture gating and polling make these workload observations, not model response
latency or production throughput guarantees.

For the 256-member matrix, every run held exactly two admitted and 254 queued
members, then completed all members. Median held daemon RSS was 32.35 MiB
(32.31–32.38) at global concurrency four and 32.26 MiB (32.04–32.39) at global
concurrency 64. Median acceptance took 176 ms / 170 ms, and median drain time
after release was 5.24 s / 5.02 s.

The 32 MiB touched tool allocation used 43.77 MiB (43.65–43.77) of supervised
process RSS at global concurrency four, separately from 30.54 MiB
(30.39–30.65) in the daemon. It produced 262,145 stdout bytes and a 16,384-byte
public preview with durable artifacts. Every completion/cancellation check in
both profiles left zero tool processes, including captured PIDs that might have
been reparented. Median cancellation-to-terminal time was 62 ms / 111 ms.

After 100 completed churn conversations, median daemon RSS was 31.41 MiB
(31.18–31.52) at concurrency four and 31.25 MiB (31.16–31.27) at concurrency
64, from baselines of 28.23 MiB in both profiles. Durable completed histories
were retained.
