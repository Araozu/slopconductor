# Example inputs

[batch-request.json](batch-request.json) matches the implemented `BatchSpec`.
Replace its project ID with one returned by `slop project register`, then run:

```sh
slop --json batch preview examples/batch-request.json --output frozen-batch.json
slop batch submit frozen-batch.json --command-id example-sweep
```

Preview creates no jobs or worktrees. See [tasks and batches](../docs/tasks-batches.md)
for settings, bounds, results, and explicit selective retries.

[child-policy.json](child-policy.json) authorizes bounded delegation in a selected
project. [child-request.json](child-request.json) is input for `run create-child`;
replace its project ID and supply `--command-id` on the CLI. See
[child tasks](../docs/child-tasks.md) for the parent policy, aggregate budgets,
native tools, and durable waits.
