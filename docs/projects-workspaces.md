# Registered projects and managed workspaces

**Implemented, 2026-10-10.** A registered project identifies a repository on the
authoritative daemon's machine. A new session can reserve a detached Git
worktree, freeze an exact base commit, and use the existing four coding tools in
that workspace. Allocation happens after turn admission. CLI/API consumers can
inspect workspace records, project events, and separate diffs, and deliberately
request cleanup. [Tasks and batches](tasks-batches.md) now build durable attempts and deterministic
matrices on these session/workspace primitives. [Native child tasks](child-tasks.md)
build bounded delegation and durable waits on the same primitives.

## CLI workflow

Git must be on the daemon's PATH. Register an absolute checkout path:

```sh
slop project register /absolute/repository --command-id register-demo
slop project list
slop project show PROJECT_ID
slop chat --project PROJECT_ID --base main --prompt "Fix the failing test." --detach --command-id job-a
slop chat --project PROJECT_ID --base main --prompt "Improve error handling." --detach --command-id job-b
slop project workspaces PROJECT_ID
slop session show SESSION_ID
slop workspace show WORKSPACE_ID
slop workspace diff WORKSPACE_ID
slop project events PROJECT_ID
slop workspace remove WORKSPACE_ID --command-id cleanup-a
```

Use `--json` for structured output. Lists support `--after` and `--limit`, with at
most 200 records per page. Session inspection exposes `managed_workspace_id`
and the execution policy's absolute root. Continue that workspace through
`chat --session SESSION_ID` or `session send`.

`--project` selects a new managed workspace; `--workspace` selects an existing
explicit directory. Both are creation-time choices. `--base` requires
`--project`; omission selects the registered checkout's HEAD. `--tool` restricts
the default `read`, `write`, `edit`, `bash` allowlist with either workspace choice.
Codex remains text-only and rejects project tool sessions before acceptance.

## Identity and admission

Registration canonicalizes the checkout root and Git common directory. Checkouts
sharing one common directory map to one project; independent clones map to
separate projects. The first registration retains the checkout used for future
revision resolution. Roots and common directories are revalidated before Git
operations, and ready workspaces are validated before each admitted turn.
Moved/replaced repositories require inspection rather than silent remapping.
Bare repositories are outside this checkout-based surface.

Session acceptance resolves a revision to a full commit hash and atomically
commits the session, reservation, command receipt, and semantic events. Retrying
the same command and payload returns its original receipt even if HEAD moves or
the source checkout disappears. Changing the payload or reusing a command ID for
another operation conflicts. Allocation uses the committed hash. Uncommitted
source changes are not copied into managed worktrees.

Schema 5 reservations store metadata and policy without creating a directory,
worktree, or provider context. An admitted turn commits `workspace_allocating`
before `git worktree add --detach`, then commits `workspace_ready` before
inference/tools use it. The existing admission slot covers preparation. The
canonical-root lease serializes turns on that workspace, including explicit
directory sessions pointing to it; separate roots can run concurrently.

Paths are `<data-dir>/workspaces/<project-id>/<workspace-id>`. Preexisting paths
and symlink replacements are rejected. The daemon retains workspaces for later
turns and inspection. Detached HEADs avoid branch-name allocation. Workspaces
share Git objects and repository metadata, with separate indexes and working
files. Automatic merging, publishing, and submodule initialization remain future
policies. Worktrees provide filesystem separation, not an OS security sandbox.

## Diff and cleanup

Diff inspection returns the frozen base, current HEAD, tracked patch against the
base (including staged changes and commits), and porcelain status showing
tracked, untracked, and ignored paths. Untracked contents are represented by
status paths until tracked; binary changes use Git's ordinary binary summary.
A running turn makes inspection conflict. These bounded reads are not an atomic
snapshot against external editors or a concurrently admitted turn.

Cleanup is an idempotent command returning a durable workspace record. A ready
workspace enters `removing`; a daemon-owned task performs cleanup independently
of the CLI. Poll `workspace show` or project events for the outcome. Queued,
running, and paused turns using the root block cleanup. Once cleanup is accepted,
new messages on that root are rejected. Unused reservations become `removed`
without filesystem work.

Cleanup refuses tracked modifications, untracked/ignored paths, and HEADs that
differ from the frozen base. A refusal returns the workspace to `ready` with
`workspace_dirty` or `workspace_has_commits`, and commits
`workspace_cleanup_failed`. Preserve or deliberately resolve the changes before
issuing a new cleanup command. Retrying the previous command returns its original
acknowledgement and never launches cleanup again. Successful Git removal commits
`workspace_removed`; history and provenance remain queryable. Forced cleanup,
retention automation, and failed-workspace repair/adoption remain future work.

## Supervision and recovery

Git uses supervised subprocesses in the shared native runtime, with two command
slots and one serialized worktree mutation. Each command has a 30-second deadline
and a 1 MiB combined stdout/stderr bound. Commands inherit only the existing tool
environment allowlist; interactive credentials, pagers, hooks, fsmonitor, and
external diff/text converters are disabled. Provider credentials are excluded.
Windows keeps canonical verbatim paths for workspace identity and containment
checks, but passes conventional drive/UNC paths to Git and enables
`core.longpaths` for each command. This supports long tracked paths inside a
worktree without changing the user's Git configuration. Git for Windows still
applies a shorter `$GIT_DIR` metadata-path limit; deep data-directory overrides
can exceed that limit and fail allocation. CI uses short per-user fixture roots
to leave room for the managed workspace IDs.
Checkout filters remain repository configuration and can execute subprocesses
under the user's permissions. Unix process groups and Windows Job Objects
supervise descendants; hard daemon death on Unix can leave subprocesses alive.

The registry accepts at most 256 projects and 256 workspace records whose state
is not `removed`, including at most 32 per project. These are admission bounds,
not disk quotas or performance measurements. Historical records/events need
future retention controls.

Known outcomes and semantic events commit together, with bounded result-write
retries that never repeat Git. An outcome that cannot be committed stops new
turn/session admissions. Failures after a Git mutation starts preserve possible
unknown effects. Startup marks unfinished `allocating`
or `removing` records `failed`, with `daemon_restarted` and
`effects_unknown: true`. It never repeats Git operations, adopts an existing
destination, or deletes a possible partial checkout. Reserved and ready records
survive restart. Failed workspaces block new instructions and remain inspectable;
diffs can inspect them when their Git identity is valid. Existing turn/tool
recovery still applies.

## Public API

All routes require local bearer authentication and API v1. Health advertises
`managed-workspaces`; native clients check it before sending project selections
or workspace mutations to older daemons.

| Route | Behavior |
| --- | --- |
| `POST /v1/projects` | Register `{"command_id":"register-1","path":"/absolute/repo"}` |
| `GET /v1/projects` | Paginated registered projects |
| `GET /v1/projects/{id}` | Project identity/local mapping |
| `GET /v1/projects/{id}/workspaces` | Paginated workspace provenance/states |
| `GET /v1/projects/{id}/events` | Paginated durable project/workspace events |
| `GET /v1/workspaces/{id}` | Workspace state and failure details |
| `GET /v1/workspaces/{id}/diff` | Bounded diff/status against the frozen base |
| `POST /v1/workspaces/{id}/remove` | Accept `{"command_id":"cleanup-1"}` |

`POST /v1/sessions` accepts an optional project selection:

```json
{
  "command_id": "session-1",
  "provider": "opencode-go",
  "model": "glm-5.3-flash",
  "project": {
    "project_id": "PROJECT_ID",
    "base_ref": "main",
    "allowed_tools": ["read", "write", "edit", "bash"]
  }
}
```

`project` and `execution` are mutually exclusive. Omitting project selection
preserves existing request serialization and command deduplication. Workspaces
are application operations; the advertised model tool set remains exactly four.

## Validation

`scripts/projects_smoke.py`, included in `scripts/smoke.py`, uses real Git,
daemon, and CLI binaries with offline inference. It verifies registration and
deduplication, lazy allocation, moving HEAD after acceptance, concurrent
independent edits, untouched source files, diffs, authentication, pagination,
cleanup refusal, detached commit preservation, preexisting destination failure,
and persisted state after restart. Storage fault injection checks both unfinished
Git operation states, preserved paths, no replay, and schema-4 migration preserving
session receipts. A runtime test creates, edits, inspects, and safely removes a
real worktree containing a tracked file beyond the legacy Windows path limit.
[Recorded offline validation](native-validation.md) passes on Linux and native
Windows. Live-provider checks remain a separate validation gate.

Git behavior follows upstream [worktree](https://git-scm.com/docs/git-worktree),
[revision resolution](https://git-scm.com/docs/git-rev-parse), and
[diff](https://git-scm.com/docs/git-diff) contracts.
