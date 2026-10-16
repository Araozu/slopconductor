# Independent clients

The existing native CLI lives in `crates/hive-cli`. Additional TUI, browser,
Electron/desktop, mobile, and other presentation clients belong in separate
repositories outside this repository. Their source, manifests, assets, tests,
and build/release tooling live with those projects. This directory documents
the boundary; it is not a location for future client implementations.

There are deliberately no frontend package manifests or build dependencies yet.
Running `cargo build -p hive-cli` does not require a web toolchain, Electron, or
the daemon implementation. Every client uses the public daemon API; none owns
agent execution.

External native clients may reuse `hive-client` and `hive-protocol` through a
pinned repository dependency. Browser clients consume the HTTP/OpenAPI contract.
See [client architecture](../docs/clients.md) and the
[implementation plan](../docs/implementation.md).
