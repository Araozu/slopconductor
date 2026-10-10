# Example inputs

[batch-request.json](batch-request.json) matches the implemented `BatchSpec`.
Replace its project ID with one returned by `slop project register`, then run:

```sh
slop --json batch preview examples/batch-request.json --output frozen-batch.json
slop batch submit frozen-batch.json --command-id example-sweep
```

Preview creates no jobs or worktrees. See [tasks and batches](../docs/tasks-batches.md)
for settings, bounds, results, and explicit selective retries.
