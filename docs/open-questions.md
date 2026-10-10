# Open questions and proposed defaults

These questions do not block the M0 scaffold. Resolve them near the milestone
that needs them, and record consequential decisions in an ADR.

## Before privileged local execution

1. **Installation scope:** one user-owned daemon per OS installation is the
   proposed initial scope. A machine-wide multi-user service would need separate
   authentication, filesystem ownership, and account isolation.
2. **Data locations:** Linux XDG config/data defaults and Windows local
   application-data defaults are implemented. Workspace-root overrides and
   backup/retention behavior remain open.
3. **Credential store:** choose OS keyring integration and an explicit alternative
   for headless systems. Credentials remain separate from conversations.
4. **Default execution policy:** define permitted file roots, shell/network
   capabilities, resource caps, and when a task waits for human input.
5. **Providers:** OpenCode Go was chosen first, with Zen and headless Codex added
   afterward through the shared `ProviderClient`. Do not hardcode a particular
   model into the domain; account-specific catalog/capability validation remains
   open.
6. **SQLite binding:** bundled `rusqlite` and one bounded database worker are
   selected and implemented for node identity. Backup/export behavior remains
   future work.
7. **Uncertain side effects:** interrupted thinking is discarded, and unfinished
   tools are failed due to daemon/process failure without restoration/replay.
   The agent decides its next action, with possible unknown effects preserved.
   Define the future recovery UI/CLI operations and evidence used to inspect
   those effects.

## Before orchestration and remote delivery

8. **Project identity:** decide how remotes, local-only repositories, moved
   directories, submodules, and separate checkouts map to one logical project.
9. **Workspace lifecycle:** select branch naming, retained worktree limits,
   user-owned versus managed directories, and cleanup policy.
10. **Budget semantics:** cumulative task/tree/batch model-request and tool-call
    limits are implemented as reservations before dispatch, retained across
    attempts and restart. Existing per-turn limits remain. Account-specific
    admission and monetary estimates remain open; operation counts are not
    unconditional billing caps.
11. **Offline instructions:** choose expiry defaults, revision checks, and whether
    a forwarder stores pending commands at all.
12. **Peer trust:** choose pairing credentials, revocation, delegated read/write
    capabilities, and the mapping from tailnet identity to app authorization.

## Before presentation and distribution

13. **Web framework and browser renderer:** choose them when the interface track
    starts, without affecting native builds.
14. **Browser sidebar:** evaluate Electron-native views and remote streaming;
    prove the shown context is the controlled context.
15. **Distribution/license:** decide whether this remains personal/local, becomes
    open source, or becomes a paid/hosted product. Current packages are unpublished.
16. **Subscriptions:** recheck each provider's supported third-party flow and
    eligibility for the chosen distribution/hosting model.
17. **Updates/releases:** choose signing, binary packaging, schema upgrade/rollback
    rules, and CLI/daemon compatibility support windows.
18. **Numerical memory targets:** measure a real release-build workload before
    selecting daemon and per-session budgets.

## Before migration

19. **Checkpoint portability:** define supported provider context and tool/workspace
    subsets rather than promising arbitrary live-state migration.
20. **Ownership proof/recovery:** validate handoff grants, restored-backup behavior,
    cloned data directories, and failure handling without a mandatory coordinator.
21. **Cross-OS support:** choose the first portable project/toolchain subset.
22. **Unavailable owner:** establish deliberate recovery semantics for data copies;
    an offline cache must not imply authority to resume.

## Already settled

Rust owns execution. JavaScript is permitted in web/Electron clients only.
The CLI/API ship first. Tailscale is required for initial remote access.
Each node owns its local state. Aggregation and mirroring are optional.
Direct provider integration is required. Browser tools and session migration
are future tracks, not first-release dependencies.
