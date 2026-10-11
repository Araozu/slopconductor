# Headless Codex connection

**Structured provider integration, 2026-10-11.** Codex implements the same
`ProviderClient::infer` path as Go and Zen: structured requests, file/shell and
native child tools, refusals, streamed text/tool arguments, private continuation,
and model-specific reasoning effort. Legacy collected/text-only completion
methods remain available for runtime callers. The daemon owns runtime login and private XDG
credential storage through [provider commands/endpoints](provider-credentials.md).
The daemon selects `codex/model` through the public session/task API and CLI.
Workspaces, managed worktrees, tasks, matrices, cancellation, steering,
pause/resume, and bounded delegation use the shared native runtime. See the
[provider checklist](provider-checklist.md) for required behavior and evidence.
Account entitlement remains an upstream decision.

## ChatGPT subscription login

The native Rust integration follows OpenAI's documented
[Sign in with ChatGPT flow](https://developers.openai.com/siwc/token-sharing-open-source/sign-in).
It creates a dynamic registration for this app, uses a loopback callback with
random state/nonce and S256 PKCE, exchanges the code with the issued client ID,
and validates the ID token's signature, issuer, audience, expiry and nonce.
Inference requires the granted plan-usage scopes. App/account/workspace
eligibility and available models remain upstream decisions.

Start the daemon, then authorize through its public API using the native CLI:

```sh
cargo run -p slop-daemon --locked
# In another terminal:
cargo run --locked -- provider login codex --command-id my-login-1
```

The daemon supplies its persisted opaque node ID as the stable host ID. Open the
printed **Continue with ChatGPT** URL and
complete authorization within five minutes. The callback is on `127.0.0.1` with
an available port. If the browser is on another machine, forward that loopback
port to the daemon host before completing login.

The daemon saves `credentials/codex-chatgpt.json` beneath its XDG data directory
(default `~/.local/share/slopconductor`), or beneath its protected
`LOCALAPPDATA/slopconductor` directory on Windows. Explicit supported data-directory
overrides apply. Unix credential files have mode `0600`; records are replaced
atomically on both platforms. Never commit the credential record.

Running a new login command with the same record reauthorizes that registration,
retains its issued client/host IDs, and rejects a different returned account
identity. The daemon keeps at most sixteen saved registrations under distinct local
account labels. `default` preserves the original credential filename; named
registrations use `credentials/codex-chatgpt-<account>.json`. Labels are 1–64
ASCII letters, digits, hyphens or underscores; labels are case-insensitive and
stored in lowercase to avoid filesystem collisions on Windows. They are local identifiers, not
email-based account identity. Each record retains its own issued client/host
identity and tokens. A different validated subject cannot replace a returning
registration. A low-level `codex_login` runtime example
also exists for development with an explicitly supplied private file and host ID.

Load the record into a daemon-owned connection and share it across sessions:

```rust,ignore
use std::sync::Arc;
use slop_runtime::providers::{ChatMessage, ChatRequest, ProviderClient};
use slop_runtime::providers::{chatgpt_auth::ChatGptConnection, codex::CodexClient};

let connection = Arc::new(ChatGptConnection::load(credential_path).await?);
let client: Box<dyn ProviderClient> = Box::new(CodexClient::from_chatgpt(connection)?);
let request = ChatRequest {
    model: "gpt-6.1-sol".to_owned(),
    messages: vec![ChatMessage::user("Reply with exactly: ok")],
    max_tokens: None,
    session_id: "session-1".to_owned(),
};
let response = client.complete(&request).await?;
```

The connection refreshes near expiry through the documented token endpoint,
serializes concurrent refreshes, and atomically saves replacement access/refresh
tokens before using them. One daemon must exclusively own the credential record;
share one `Arc<ChatGptConnection>` for that registration. A rejected or uncertain
refresh blocks further renewal in that connection until a new sign-in. There is
no automatic inference retry or fallback. HTTP error bodies and token values are
excluded from diagnostics.

## Request restrictions

Subscription inference uses `POST https://api.openai.com/v1/responses` with
`store: false`, `stream: true` and an explicit input array. `complete` collects
the same SSE stream used by `complete_streaming`; it makes one inference
attempt. A non-SSE subscription response is rejected. The stable session ID is
sent as `prompt_cache_key`, without assigning conversation ownership upstream.

The documented [preview restrictions](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations)
exclude `max_output_tokens`, so subscription requests must use
`max_tokens: None`. An explicit cap fails before dispatch. OpenCode Go still
requires `Some(limit)`. Omission is not a billing cap: request duration and
retained response data remain bounded, but upstream generation usage is unknown
until reported.

Subscription requests accept user, assistant and explicit developer messages.
System messages fail validation rather than changing their priority; callers
can explicitly choose the developer role for supported instructions. The structured daemon path accepts ordinary function tools through
`additional_tools` developer input items in subscription mode; Platform mode
uses normal Responses tool definitions. Both preserve text/refusal blocks and
paired calls/results. The daemon assembles bounded canonical history and sends
it explicitly on every request. `include: ["reasoning.encrypted_content"]`
requests replay material; completed reasoning items (including summaries),
assistant `phase`, and associated output items are stored privately and replayed
only within the same provider/model/endpoint/registration scope. Token refresh
does not change that scope; changing a key, account, or endpoint does. Required
incompatible continuation fails without dispatch. Interrupted/incomplete
continuation is discarded.

Attachments, media, custom/free-form tools, hosted tools, arbitrary provider
parameters, public reasoning-summary rendering, and upstream conversation
storage remain unsupported. Unknown output types fail explicitly. The legacy
`complete`/`complete_streaming` methods remain strict text-only interfaces; use
`infer` for structured content. Completion requires a recognized terminal event; incomplete
turns retain their reason and partial text, and missing usage stays unknown.

The static executable catalog includes `gpt-6.1-sol`, `gpt-6-sol`, `gpt-6-luna`
and `gpt-6-astra`, verified against the official
[Codex model catalog](https://learn.chatgpt.com/docs/models) and corresponding
API model pages. Subscription discovery reads the selected account's live
`models`/`slug` catalog and includes only `visibility: list`. The public `provider discover codex` command performs this explicit live probe
and returns auth/account scope, observation time, and executable capabilities.
The ordinary `models` command stays offline. Discovery does not enable unmapped
models or establish successful inference. No model is substituted.

Platform API keys are also supported by `CodexClient::new`/`from_env`
(`OPENAI_API_KEY`), with optional output caps and genuine non-streaming
Responses requests. They use separate Platform billing. Neither auth mode reads
Codex CLI cached credentials or uses ChatGPT `backend-api` endpoints.

## Verification and remaining work

Local HTTP fixtures cover subscription discovery, shared completion methods,
request restrictions, terminal failures, token rotation/persistence, signed
ID-token validation, callback binding and safe diagnostics. Existing OpenCode Go
decoder tests exercise the shared bounded HTTP/SSE implementation.

The separately selected live checks use `SLOP_CODEX_CREDENTIAL_FILE` and
optionally `SLOP_CODEX_MODEL`. The text check makes one discovery and one inference
request. The structured check makes one discovery and two inference requests,
using a synthetic manifest tool/result with no local filesystem changes. Both
omit generation-token caps. Use a private credential record that is not in use
by a daemon during the check. Select a live credential and usage budget before
running either check explicitly:

```sh
cargo test -p slop-runtime --test codex_live --locked live_subscription_stream_uses_shared_interface -- --ignored
cargo test -p slop-runtime --test codex_live --locked live_subscription_tool_result_round_trip -- --ignored
```

Neither ignored live check was run for this change.

The Codex daemon/CLI fixture covers four coding tools, private reasoning/phase
replay, refusals, incomplete/malformed output, local cancellation, native child
creation/wait, managed worktrees, account selection, explicit live-catalog fixtures, restart history,
and stale environment-key suppression. Structured adapter fixtures also cover
streamed refusals and contradictory terminal text/tool arguments. Auth fixtures cover successful and
unconfirmed revocation and reject untrusted revocation destinations. They use
synthetic credentials and local HTTP servers, not upstream login or inference.

Local cancellation drops the inference future, closes its HTTP stream, and
records the canceled request/turn. This applies to Codex through the common
runtime; it does not confirm that upstream generation or billing stopped.
Client detach leaves accepted work running. No live login, entitlement, or
inference is established by offline checks. Credentialed upstream login and a
structured tool/result round trip remain a separate validation gate.

## Reasoning settings

Checked against the official model pages on 2026-10-11:

| Model | Supported `--effort` values |
| --- | --- |
| [gpt-6.1-sol](https://developers.openai.com/api/docs/models/gpt-6.1-sol) | low, medium, high, xhigh, max |
| [gpt-6-astra](https://developers.openai.com/api/docs/models/gpt-6-astra) | low, medium, high, xhigh, max |
| [gpt-6-sol](https://developers.openai.com/api/docs/models/gpt-6-sol) | none, low, medium, high, xhigh, max |
| [gpt-6-luna](https://developers.openai.com/api/docs/models/gpt-6-luna) | none, low, medium, high, xhigh, max |

Omission keeps the provider default. Explicit unsupported values fail before
acceptance/dispatch. The API exposes these same capabilities and freezes the
effective settings for every model request in a turn.

```sh
slop chat --model codex/gpt-6.1-sol --workspace /absolute/workspace \
  --effort high --prompt "Inspect the project, make the change, and run checks."
```

## Accounts and sign-out

These commands use authenticated public daemon endpoints; clients never read
the provider credential records:

```sh
slop provider accounts
slop provider login codex --account work --command-id login-work
slop provider select chatgpt --account work
slop provider select api_key
slop provider discover codex
slop provider logout work
slop provider remove-key codex
```

A new account label starts a distinct dynamic registration. An existing label
reauthorizes its issued registration. Login completion validates identity and
persists tokens before activation; selection affects new admissions while
existing turns retain their connection snapshot. No billing fallback occurs.
Sign-out disables the selected subscription connection and attempts the
[documented revocation flow](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions),
using the trusted OIDC `revocation_endpoint`. It clears access, refresh, and ID
tokens locally while retaining registration identity. The response explicitly
reports whether remote revocation was confirmed; if not, disconnect the app in
ChatGPT Settings. Signed-out registrations can be reauthorized using the same
client/host IDs. Already dispatched HTTP inference has no confirmed remote
cancellation guarantee.

Removing an API key disables that billing path and suppresses import of a stale
environment key after restart. It does not revoke a Platform key remotely or
automatically select another saved credential. `provider set-key` can explicitly
configure it again. Public `models` readiness remains a local configuration
check; `provider discover` verifies a catalog only.
