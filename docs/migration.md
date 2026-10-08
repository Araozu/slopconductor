# Future session and workspace migration

## Status and goal

Migration is an exploratory future milestone. It must not delay local CLI
execution, but the data model should support portable session identity,
serializable checkpoints, and relocatable workspace references from the start.

The intended experience is: pause work at home, move it to work, reopen the same
conversation, and continue under the same session ID with a new owner.

## Three distinct operations

| Operation | Identity and ownership | Contents |
| --- | --- | --- |
| Export / clone | New session ID for a copy; source remains owner | Selected history, attachments, and context |
| Move idle conversation | Original ID changes owner | History, configuration, and required artifacts |
| Move paused task | Original ID changes owner after validated handoff | Conversation plus checkpoint and restorable workspace state |

A clone is useful for experiments but must not pretend it is a migrated execution
attempt. Restore from backup also requires its own ownership policy.

## Portable foundations

Session/task IDs are independent of node IDs. Checkpoints are versioned
application data rather than raw stacks, futures, or process snapshots.
Workspaces use logical references with destination mappings. Artifacts have
content identity and provenance.

Separate portable model/context data from provider-account credential handles.
The target authenticates using its own supported account configuration and
checks compatibility. Required opaque provider continuation fields travel only
when the selected provider/account permits their supported reuse.

## Safe execution boundary

First implement idle conversation moves. A later task move starts from an
explicitly paused, persisted boundary with no unaccounted-for active tools.

An in-flight tool must finish, be canceled with known effects, or be reconciled
before that move is accepted. A shell process or browser context does not
automatically become portable. A future remote-tool attachment is another
capability and must identify which node still owns it.

A network interruption during an inference request can leave partial context.
Record the interrupted outcome and reconstruct a supported next turn; do not
represent that as transparent live process migration.

## Proposed transfer package

A package contains:

- Transfer ID, source and target node IDs, session ID, source ownership epoch.
- Protocol/checkpoint schema versions, required runtime/tool capabilities.
- Session/task/run records, durable event watermark, and checkpoint digest.
- Selected message/context records and artifact hashes/sizes.
- Logical project identity, base commit, workspace manifest, and path mappings.
- Tool/inference uncertainty records that must be resolved before activation.
- Explicit exclusions and a validation report.

Actual provider credentials are excluded. Packages are authenticated, integrity
checked, size limited, and staged outside active workspaces.

## Workspace reconstruction

For a Git task, identify the repository, exact base commit, relevant branch/ref
metadata, staged and unstaged changes, untracked files selected for transfer,
submodules, large-file dependencies, and toolchain expectations.

A Git bundle can carry repository objects and refs, but a workspace manifest must
also describe uncommitted and selected untracked state. It cannot assume a
committed-history transfer contains the active working directory. See
[Git bundle documentation](https://git-scm.com/docs/git-bundle).

If the target already has the project, validate its identity and create a new
managed workspace. Do not overwrite a user's existing dirty checkout. When
necessary, transfer missing Git objects and reconstruct changes in staging.

Ignored files, local secrets, caches, build output, databases, and background
services are not automatically included. A target may rebuild dependencies and
then run explicit validation commands. Environment compatibility is checked
rather than inferred from matching repository names.

Linux/Windows moves require path, case-sensitivity, executable-bit, symlink,
line-ending, shell, and toolchain handling. The first cross-OS transfer can
support a constrained subset and return a clear unsupported-capability result
for the remainder.

## Conservative ownership handoff

The following is a protocol proposal, not a proven implementation:

1. **Prepare source:** record transfer intent, stop new execution, settle tools,
   and persist the checkpoint. Source still owns the session but is paused.
2. **Stage target:** validate trust, package integrity, schemas, destination
   mappings, and capabilities. Target is prepared but cannot execute.
3. **Confirm readiness:** target records durable staged state and returns its
   checkpoint digest and validation result.
4. **Commit source deactivation:** source durably records that it has relinquished
   execution, advances the ownership epoch, and creates an authenticated handoff
   grant tied to the target and checkpoint.
5. **Activate target:** target persists the grant and new ownership before
   scheduling a resumed attempt.
6. **Acknowledge completion:** peers update routing; source retains a tombstone
   and read-only history, with artifact cleanup following retention policy.

Commands are idempotent by transfer ID. A timeout cannot automatically roll
ownership back after a grant may have reached the target. Before source
deactivation, a prepared target has no authority to execute.

## Failure behavior

| Failure | Required behavior |
| --- | --- |
| Target rejects capabilities or workspace | Leave source paused/owned; report blockers |
| Upload interrupted | Resume staging by manifest; target remains inactive |
| Source crashes before deactivation | Reconcile preparation state; target has no grant |
| Grant delivery uncertain | Keep source deactivated; query/retransmit the same durable grant |
| Target activates but acknowledgement is lost | Reconcile ownership; source cannot resume on timeout |
| Target fails after receiving the grant | Keep ownership explicit; recover target or perform another deliberate transfer |
| Old source backup is restored | Quarantine stale ownership until transfer history is reconciled |

Ownership epochs help route/reject stale commands, but an epoch alone does not
fence an offline process or an old restored database. Source quiescence, durable
deactivation, startup reconciliation, and rules for cloned data directories are
necessary. Prefer blocked progress to allowing two owners.

## Validation before shipping

Fault-inject every transfer boundary. Verify source and target cannot both
schedule a run, packages can resume without duplicate records, corrupted
artifacts fail validation, old schemas are handled explicitly, and interrupted
source/target restarts recover the same transfer state.

Begin with idle conversations on the same OS, then paused Git tasks, then
selected cross-OS paths. Live process migration, transparent failover, and
portable arbitrary browser state remain outside the initial proposal.
