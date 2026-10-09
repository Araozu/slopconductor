# Runtime provider credentials

**Implemented, 2026-10-09.** Credentials can be configured through the running
daemon's authenticated public API and native CLI. All secrets are daemon-owned
files under `<data-dir>/credentials`, separate from SQLite conversations,
command receipts, events, and logs. Setting a provider key enables new requests
immediately; active requests retain their original connection. No restart is
required. Codex keeps both configured credentials when present and persists
which mode is active (`api_key` or `chatgpt`) across restart.

## User workflow

Start the daemon without a provider key:

```sh
cargo run -p slop-daemon --locked
```

In another terminal, supply a key on stdin or from a file. The CLI forwards it to
the daemon; it does not store credentials or call the provider. Keys are not
accepted as command-line arguments or printed in responses.

```sh
cargo run --locked -- provider set-key opencode-go --key-file /path/to/key
cat /path/to/key | cargo run --locked -- provider set-key opencode-go
cargo run --locked -- --json provider status
cargo run --locked -- models
cargo run --locked -- chat --prompt "Explain Rust ownership."
```

`provider set-key` supports `opencode-go`, `opencode-zen`, and `codex`. A Codex
API key uses OpenAI Platform billing. Credential status separately reports
`api_key_configured`, `chatgpt_configured`, `execution_supported`, and Codex's
`active_auth_mode`. All three providers support daemon text chat; Go and Zen
also support structured workspace tools. Readiness describes local credentials,
not upstream entitlement or availability.

Authorize a ChatGPT subscription with:

```sh
cargo run --locked -- provider login codex --command-id my-login-1
cargo run --locked -- provider login-status my-login-1
```

The first command prints an authorization URL and follows the login. Open it in
a browser on the daemon host and authorize within five minutes. The daemon owns
the loopback listener, code exchange, ID-token validation, and credential write.
A browser on another machine needs the callback port forwarded to the daemon
host. Ctrl-C detaches the CLI while the bounded daemon login continues. The
daemon's durable node ID supplies the stable host identity. Returning login
reuses the same registration and validates the returned account. This command
enables Codex subscription chat after authorization succeeds; see
[Codex connection](codex-connection.md) for text-only restrictions.

## Location, privacy, and restart

Linux uses `$XDG_DATA_HOME/slopconductor/credentials`, defaulting to
`~/.local/share/slopconductor/credentials`. Relative/empty XDG values follow the
existing startup fallback. Windows uses
`%LOCALAPPDATA%/slopconductor/credentials`. An absolute `--data-dir` or
`SLOP_DATA_DIR` override changes the credential location with the rest of the
daemon data. Windows overrides must remain beneath canonical `LOCALAPPDATA`.
There is no source-checkout or top-level home credential directory.

Files are `opencode-go-api-key`, `opencode-zen-api-key`, `codex-api-key`,
`codex-chatgpt.json`, and the non-secret `codex-auth-mode`. Linux directories are owner-only `0700`, and files are
`0600`; Windows uses the protected profile directory's inherited ACLs. Reads
reject symlinks and insecure files. API-key replacements use synchronized,
same-directory temporary files and atomic replacement; directory metadata is
synchronized on Unix. Codex records use the native adapter's corresponding
atomic storage. The data-directory lock keeps one daemon authoritative.

Saved records load before readiness. For compatibility, `OPENCODE_GO_API_KEY`,
`OPENCODE_ZEN_API_KEY`, and `OPENAI_API_KEY` are imported on startup only when
their respective saved record is absent. Saved runtime updates take precedence
over stale environment values on restart. Invalid/insecure saved records fail
startup instead of silently falling back to another credential.

Keys are bounded to 16 KiB after trimming and contain printable ASCII without
internal whitespace. Credential HTTP bodies are capped at 32 KiB. Updates are
serialized; the daemon persists before activation/acknowledgment and owns both
steps even if the caller disconnects. A failed write leaves the active connection
unchanged. Credential status is local configuration, not a live authentication
check. Account management, credential deletion, and remote revocation remain
planned.

## Public API

Every route requires the same local bearer authentication as chat. Health
advertises `provider-credentials`; the listener remains loopback-only.

| Method and route | Request / response |
| --- | --- |
| `GET /v1/providers` | Safe credential status for the three supported adapters |
| `PUT /v1/providers/{provider}/api-key` | `{"api_key":"..."}`; `200` with safe provider status after persistence and activation |
| `POST /v1/providers/codex/login` | `{"command_id":"my-login-1"}`; `202` with `login_id` and ephemeral `authorization_url` |
| `GET /v1/providers/codex/login/{login_id}` | `pending`, `succeeded`, or `failed`, with a safe optional error code |

PUT is an idempotent credential replacement. There is no endpoint to retrieve
stored secrets. Login startup reuses the current attempt for the same command
ID and rejects a different ID while pending. Only the latest login attempt is
retained in memory; a later attempt replaces the completed status. Pending
authorization is canceled on daemon shutdown, with any started credential write
drained before directory ownership is released. After restart, old login IDs
return `404`; start a new explicit attempt. Unknown code/token exchanges are
never replayed automatically. Successful credential records survive restart.

## Verification

The bootstrap smoke check exercises real binaries without provider credentials
or paid calls: missing-key readiness, authenticated configuration via stdin/file
and PUT, rejected input and insecure storage, active-turn credential snapshots,
restart precedence, credential isolation from transcripts/logs, callback denial,
duplicate/conflicting login IDs, stable host identity, and pending-login shutdown.
It also runs real daemon/CLI text turns through offline Zen Chat Completions and
Codex Responses fixtures, including saved-key activation after restart.
Existing native auth fixtures cover signed ID tokens and token rotation/storage.
No live ChatGPT login or inference is established by these checks.
