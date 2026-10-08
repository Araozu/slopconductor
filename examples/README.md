# Design examples

[batch-request.json](batch-request.json) illustrates a proposed batch submission
format for M2. It is not currently accepted by the CLI or daemon, and its schema
is not frozen.

The matrix has two prompts, two placeholder provider/model identifiers, and two
runtime turn-limit settings: eight combinations. Replace node/project/model
identifiers with real configured values once that feature exists.

The proposed preview operation resolves `HEAD` to one exact commit, validates
settings/caps, and reports the expansion before any worktree or inference is
started. Admission then creates workspaces lazily with at most two active runs
for this batch, within tighter daemon/account limits if configured.

The runtime-setting axis is provider-independent. Provider-specific axes must
be validated against each selected model rather than silently ignored.
