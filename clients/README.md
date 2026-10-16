# Independent clients

The initial native CLI lives in `crates/hive-cli`. A future native TUI should be
added as `crates/hive-tui` and use `hive-client`. Future browser and Electron
projects belong in `clients/web/` and `clients/desktop/` respectively when work on
those interfaces actually starts.

There are deliberately no frontend package manifests or build dependencies yet.
Running `cargo build -p hive-cli` does not require a web toolchain, Electron, or
the daemon implementation. Every client uses the public daemon API; none owns
agent execution.

See [client architecture](../docs/clients.md) and
[implementation plan](../docs/implementation.md) before adding a frontend.
