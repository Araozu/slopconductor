# 0001: Native shared agent runtime

Status: accepted. Date: 2026-10-08.

## Context

The user needs many concurrent tasks and has observed substantial memory
consumption from an application/runtime process per chat. The user explicitly
rejects a JavaScript backend, external agent CLI wrappers, and terminal-output
presentation as the product's agent interface.

## Decision

Use Rust for the daemon and native clients. Implement the agent loop directly
with supported model-provider APIs. Schedule admitted sessions inside a shared
native runtime, using Tokio for asynchronous work.

One coordinating daemon owns all sessions on one execution environment. Tool
subprocesses are allowed for actual shell/Git/test/browser work. JavaScript is
allowed in a browser or optional Electron frontend.

## Consequences

Provider/context/tool/recovery behavior is owned by this project and requires
real implementation effort. The architecture avoids duplicated language
runtimes per conversation, but still needs bounded retained data and measured
memory.

The daemon must operate without any frontend toolchain. A desktop client is
optional presentation, not an execution dependency.
