# Workspaces and explicit Git worktrees

**Unified workspace design, updated 2026-10-16.** Every session has a workspace selection.
A workspace is a registered directory on its owning daemon. Opening a workspace
records an existing directory in place; it does not copy files or create a
worktree. The registry is independent of sessions, so several sessions can
select the same workspace and execute concurrently, including model requests
and tools.

The built-in workspace ID `default` is named **No workspace** and has no path.
It lets text-only conversations satisfy the required workspace selection without
granting filesystem tools. CLI chat selects it when no directory or ID is given;
`--no-workspace` makes that choice explicit. Task creation also uses it by
default and accepts `--no-workspace`. API requests always carry a
`WorkspaceSelection`; use `{"workspace_id":"default","allowed_tools":[]}`
for directory-less work.

The older project registry and session-owned lazy worktree design has been
removed. Historical offline validation records describe that earlier
implementation. They remain useful evidence for the historical code, not for
the unified workspace API below.

## Open and create workspaces

Open an existing absolute directory on the daemon's machine:

```sh
hivectl workspace open /absolute/repository --command-id open-repo
hivectl workspace list
hivectl workspace show WORKSPACE_ID
hivectl workspace rename WORKSPACE_ID "Readable name" --command-id rename-workspace
hivectl workspace events WORKSPACE_ID
```

Workspace identity is based on the canonical path. Reopening the same path
returns the same workspace. Distinct checkout paths remain distinct workspaces,
even when Git reports a shared common directory. `git_common_dir` is optional
Git metadata, not an identity key or a project registration.

Create a new directory under an existing absolute parent, optionally initializing
Git:

```sh
hivectl workspace create /absolute/parent new-folder --command-id create-folder
hivectl workspace create /absolute/parent new-repo --git --command-id create-repo
```

The daemon uses the paths on its own machine. A client on another machine must
send a path meaningful to the owner; the CLI does not reinterpret local paths.
Folder and optional Git repository creation are durable asynchronous operations.
The command returns an `allocating` workspace; poll `workspace show` or its events
until it becomes `ready` or `failed` before selecting it for a session. Restart
does not replay an unfinished filesystem or Git operation: it records the
workspace as `failed` with `effects_unknown: true`. Inspect the target path and
workspace record before deciding what to do next. Created ordinary directories
have `managed_worktree: false`; removing their workspace record only unregisters
them and leaves the directory intact.

## Explicit worktrees

A Git worktree is a separate workspace created only by an explicit command or
API call. It freezes the selected revision when the operation is accepted and
returns an `allocating` workspace record. Poll `workspace show` or its events
until the status is `ready` or a terminal failure is recorded.

```sh
hivectl workspace worktree WORKSPACE_ID --base main --name "fix failing test" \
  --command-id create-fix-worktree
hivectl workspace show WORKTREE_ID
hivectl workspace diff WORKTREE_ID
```

Omitting `--base` selects the source workspace's current `HEAD`. The resulting
`base_commit` is the exact commit frozen for that worktree. `source_workspace_id`
records where it came from, and `managed_worktree` says whether Hive owns the
worktree lifecycle. These are provenance fields; every directory is still a
workspace.

Creating a session, task, batch member, child, history fork, or retry never
creates a worktree. Those operations retain or select an existing workspace.
This keeps workspace creation and execution admission as separate, explicit
commands.

## Session selection and execution

`POST /v1/sessions` requires a workspace selection:

```json
{
  "command_id": "session-1",
  "provider": "opencode-go",
  "model": "glm-5.3-flash",
  "workspace": {
    "workspace_id": "WORKSPACE_ID",
    "allowed_tools": ["read", "write", "edit", "bash"]
  }
}
```

There is no project selection or caller-supplied raw execution root. The daemon
resolves the selected ID, validates its owner and state, and builds the effective
execution policy. An optional `limits` object on `WorkspaceSelection` sets
`shell_timeout_ms`, `max_output_bytes`, `max_tool_calls`, and
`max_model_requests`; omitted values use the documented defaults. The daemon
returns the effective execution policy. `SessionResponse.workspace_id` always
identifies the selected workspace, including `default`.

Several sessions may use the same registered workspace and execute model
requests and tools concurrently, subject to global, batch, and child admission
limits. Turns within each session remain ordered. Agents are responsible for
coordinating conflicting filesystem changes; explicitly created worktrees
provide independent filesystem state when needed. Each session remains
independently durable and keeps its own messages, turns, and policy. Follow-ups,
retries, and history forks retain the existing selected workspace unless a new
operation explicitly accepts another selection.

## Diff and removal

`workspace diff` compares tracked changes against the current `HEAD` for an
ordinary Git workspace and against the frozen `base_commit` for a managed
worktree. A non-Git workspace has no Git diff and returns an unsupported
operation result. Git status also reports untracked and ignored paths; the patch
contains tracked changes.

```sh
hivectl workspace diff WORKSPACE_ID
hivectl workspace remove WORKSPACE_ID --command-id remove-workspace
```

The built-in `default` workspace is always present, has `path: null`, enables no
tools, and cannot be renamed or removed. Removing an ordinary workspace
unregisters it and leaves its directory intact.
Removing a managed worktree asks Git to remove its directory and is conservative:
dirty paths or commits beyond its frozen base prevent cleanup. Cleanup is
refused while queued, running, paused, or waiting turns still use that workspace.
An accepted removal can be observed as `removing`, then `removed` or `failed`;
the command is not automatically replayed after an uncertain outcome.

## Public API

All workspace routes require local bearer authentication and advertise the
`unified-workspaces` capability. There are no `/v1/projects` routes or aliases.
The public DTOs live in `hive_protocol::workspaces`.

| Route | Behavior |
| --- | --- |
| `GET /v1/workspaces?after=...&limit=...` | Paginated workspace records |
| `POST /v1/workspaces` | Open an existing directory with `{ "command_id", "path" }` |
| `POST /v1/workspaces/create` | Start creating a directory with `{ "command_id", "parent", "name", "git" }`; returns `allocating`, then poll the workspace record/events |
| `GET /v1/workspaces/{id}` | Inspect workspace identity, owner, provenance, and state |
| `POST /v1/workspaces/{id}/rename` | Rename a workspace with `{ "command_id", "name" }` |
| `POST /v1/workspaces/{id}/remove` | Unregister an ordinary workspace or conservatively remove its managed worktree |
| `GET /v1/workspaces/{id}/diff` | Compare Git changes to current `HEAD` or the frozen worktree base |
| `GET /v1/workspaces/{id}/events?after=...&limit=...` | Read paginated workspace lifecycle events |
| `POST /v1/workspaces/{id}/worktrees` | Explicitly create a worktree with `{ "command_id", "base_ref", "name" }` |

Workspace records include `id`, `name`, `owner_node_id`, optional canonical `path`,
`is_default`,
optional `git_common_dir`, `managed_worktree`, optional
`source_workspace_id` and `base_commit`, `status`, optional `error_code`, and
`effects_unknown`. Worktree creation and removal are durable supervised
operations. If the daemon restarts while Git may have changed files, it records
the failure and uncertain effects instead of replaying the command.

## Supervision and recovery

Directory creation, worktree creation, and worktree removal are daemon-owned
operations, with at most two outstanding jobs. Git has two command slots, one
serialized worktree mutation, a 30-second command deadline, and a 1 MiB combined
output bound. Git inherits the tool environment allowlist; provider credentials,
interactive authentication, hooks, pagers, fsmonitor, and external diff converters
are excluded. Checkout filters remain repository configuration.

Windows keeps canonical verbatim paths for directory identity and translates
arguments passed to Git to conventional drive/UNC paths. Git commands enable
`core.longpaths`; Git for Windows still limits its metadata directory paths, so
deep data-directory overrides can fail worktree creation. Native Windows fixtures
use short private temporary roots. Worktrees provide filesystem separation,
not an OS security sandbox.

The registry admits at most 256 non-removed workspaces, including the default.
This bounds admission rather than disk use; historical records remain queryable.
Creation reserves capacity and an idempotent command receipt before touching the
filesystem. Interrupted `allocating` or `removing` records become `failed` with
`daemon_restarted` and `effects_unknown: true`; startup never repeats the operation
or adopts a partial directory. Retrying a command returns its original acceptance.
Clients inspect the workspace record to obtain the current result. Only persistence
writes are retried after a known Git outcome. Failed outcome persistence stops new
execution admissions.

## Historical validation

The pre-redesign Git implementation was exercised by
[`native-validation.md`](native-validation.md) and its old project/workspace
smoke checks. Those reports are retained as dated historical records. They do
not establish validation of the unified workspace contract introduced on
2026-10-15. The same validation document records the redesign's local Linux
checks separately.

Git behavior follows upstream [worktree](https://git-scm.com/docs/git-worktree),
[revision resolution](https://git-scm.com/docs/git-rev-parse), and
[diff](https://git-scm.com/docs/git-diff) contracts. Worktrees provide
filesystem separation, not an OS security sandbox.
