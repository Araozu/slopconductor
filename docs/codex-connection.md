# Headless Codex connection

**Implemented runtime slice, 2026-10-08.** Codex implements the same
`ProviderClient` as OpenCode Go: validation, model listing, collected completion
and incremental text completion. The daemon exposes health and an authenticated node query;
session execution and public provider/account endpoints remain proposed in the
[shared provider interface](provider-interface.md).

## ChatGPT subscription login

The native Rust integration follows OpenAI's documented
[Sign in with ChatGPT flow](https://developers.openai.com/siwc/token-sharing-open-source/sign-in).
It creates a dynamic registration for this app, uses a loopback callback with
random state/nonce and S256 PKCE, exchanges the code with the issued client ID,
and validates the ID token's signature, issuer, audience, expiry and nonce.
Inference requires the granted plan-usage scopes. App/account/workspace
eligibility and available models remain upstream decisions.

The short-lived helper prints a URL for the user to open; it does not launch a
browser, invoke an agent CLI, or perform inference. On Linux, from this checkout:

```sh
install -d -m 700 .slop/credentials
cargo run -p slop-runtime --example codex_login --locked -- \
  .slop/credentials/chatgpt.json slop-local-host-1
```

Choose a stable opaque host ID for this daemon host and reuse it. A different
host needs a distinct ID. Open the printed **Continue with ChatGPT** URL and
complete authorization within five minutes. The callback is on `127.0.0.1` with
an available port. If the browser is on another machine, forward that loopback
port to the daemon host before completing login.

On Windows, supply a credential path in an existing directory whose ACL grants
access only to the intended user/service. The helper accepts the same path and
host-ID arguments. Unix credential files are written with mode `0600`; records
are replaced atomically on both platforms. Never commit the credential record.

Running the helper again with the same record reauthorizes that registration,
retains its issued client/host IDs, and rejects a different returned account
identity. Use separate paths and connections for separate account registrations.

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
can explicitly choose the developer role for supported instructions. Tools,
attachments, structured refusals, reasoning summaries, opaque continuation and
automatic history assembly remain unsupported. Unsupported response content
fails explicitly. Completion requires a recognized terminal event; incomplete
turns retain their reason and partial text, and missing usage stays unknown.

The static executable catalog includes `gpt-6.1-sol`, `gpt-6-sol`, `gpt-6-luna`
and `gpt-6-astra`, verified against the official
[Codex model catalog](https://learn.chatgpt.com/docs/models) and corresponding
API model pages. Subscription discovery reads the selected account's live
`models`/`slug` catalog and includes only `visibility: list`. Discovery does not
enable unmapped models or guarantee entitlement. No model is substituted.

Platform API keys are also supported by `CodexClient::new`/`from_env`
(`OPENAI_API_KEY`), with optional output caps and genuine non-streaming
Responses requests. They use separate Platform billing. Neither auth mode reads
Codex CLI cached credentials or uses ChatGPT `backend-api` endpoints.

## Verification and remaining work

Local HTTP fixtures cover subscription discovery, shared completion methods,
request restrictions, terminal failures, token rotation/persistence, signed
ID-token validation, callback binding and safe diagnostics. Existing OpenCode Go
decoder tests exercise the shared bounded HTTP/SSE implementation.

The separately selected live check uses `SLOP_CODEX_CREDENTIAL_FILE` and optionally
`SLOP_CODEX_MODEL`. It makes one discovery request and one streamed inference
request without a generation-token cap. Select a live credential and usage
budget before running:

```sh
cargo test -p slop-runtime --test codex_live --locked -- --ignored
```

No live login or inference is established by the offline suite. Account-picker
UI, public login/configuration endpoints, remote sign-out/revocation, cancellation,
capability snapshots and the persisted agent loop remain planned work.
