# Product idea

## The application

Slop Conductor is a personal control surface for AI work executing across several
machines. Each active operating-system installation runs one native daemon that
owns local conversations, tasks, workspaces, and agent execution. Clients attach
to those daemons to create work, observe it, and send instructions.

The initial environment is a home PC, a work PC, and a VPS. The PCs dual boot
Linux and Windows. A phone is primarily a client. The application should work on
one machine on its own and add remote visibility when the user connects devices.

The daemon implements the agent loop and integrates directly with model
providers. A chat is durable conversation data plus execution state scheduled
inside a shared runtime. The product presents messages, tool calls, file changes,
artifacts, and progress through a structured protocol.

## Why build it

The user frequently starts many AI jobs, sometimes from a script that expands
prompts, models, and other parameters into combinations. These jobs may need
isolated Git worktrees. A running agent may need to create another session and
delegate a bounded task to it.

The user also changes devices while work is running. A task started at home
should remain accessible from a work PC or phone. Attaching a client should
restore the same conversation and allow further instructions without restarting
execution.

A major constraint is resource efficiency. Existing per-chat processes were
observed to consume roughly 300 MiB each, with a batch reaching roughly 10 GiB.
These are the user's observations, not benchmarks for this repository. The new
runtime must avoid replicating an entire application runtime and service stack
for each conversation.

## Primary user stories

### One task, several devices

The user starts a coding task in the CLI on home Linux. The daemon acknowledges
the task and persists its identity. The CLI can exit. Later, a phone opens a
client, selects home Linux, catches up on events, and sends a correction. The
owner daemon acknowledges the correction and applies it according to the chosen
steering mode. The same session is visible from the work PC.

Success means that execution is independent of every client's connection and
that the UI accurately distinguishes pending instructions from accepted ones.

### Prompt and model experiments

A script submits a matrix of prompts, model identifiers, and execution settings.
The system reports the number of combinations, validates settings against the
selected providers, and creates an identifiable batch. A scheduler admits only
the configured amount of work. Results retain the input parameters so they can
be compared or exported.

Success means deterministic expansion, bounded admission, useful partial
results, and a way to retry individual failures without rerunning successes.

### Parallel work in one repository

Several tasks target the same project. Each writable coding task gets its own
worktree and branch or an explicit exclusive workspace reservation. The user can
inspect diffs and test results per task. The daemon records which base commit and
workspace produced each result.

Success means separate edits, clear provenance, and deliberate merging or cleanup.
Creating a worktree alone does not isolate ports, databases, external services,
or subprocess privileges.

### An agent starts another session

An agent invokes a native orchestration tool to create a child task, selecting
its model, target, workspace mode, allowed tools, initial context, and budget.
The child is admitted through the same scheduler used by human clients and
scripts. The parent can await a result, receive an artifact, or continue with
other work.

Success means visible parent/child relationships and accountable resources.
Context and credentials do not implicitly expand with every level of delegation.

### Optional future handoff

The user asks to move a conversation or paused coding task from home to work.
The source prepares a checkpoint; the target validates dependencies and restores
necessary data. The session keeps its identity and history while execution
ownership changes. If a handoff is ambiguous, execution waits for reconciliation.

This is an ambitious future capability. It should influence identity and
checkpoint design now without blocking the first useful CLI release.

## Product vocabulary

| Concept | Meaning |
| --- | --- |
| Physical machine | A device such as the home PC; useful for display/grouping |
| Node / execution environment | One daemon installation and its OS-local data directory |
| Project | A registered repository or directory, with logical identity and local mappings |
| Workspace | The concrete directory, worktree, or other environment used by a run |
| Session | A persistent conversation with context, messages, and an owner |
| Task | A durable goal or work item associated with a session |
| Run | One execution attempt for a task; restarts create explicit attempts |
| Turn | A model interaction and associated tool-processing steps |
| Batch | A named collection of tasks with recorded parameter combinations |
| Artifact | A durable output such as a diff, report, log, or attachment |
| Client | An interface that sends commands and renders daemon state |

Initially, a session has one primary task at a time. A follow-up after a terminal
task can create a new task in the same conversation. Detailed task/session
relationships can evolve without making clients own the execution loop.

## Scope and product character

The initial product serves the user and their trusted devices. Team accounts,
commercial hosting, organizational tenancy, and complex shared editing are open
questions. A distribution license and public branding have not been chosen.

The first tools are filesystem operations, shell commands, and Git. Browser
automation with a visible client sidebar is a later direction. Cloud APIs and
supported provider subscription flows are primary model access methods. Local
models are optional future integrations.

The CLI and public API are the first delivery surface. A native TUI, responsive
web interface, and optional Electron application are separate consumers of that
API and can mature at different speeds.
