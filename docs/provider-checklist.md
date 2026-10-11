# Provider behavior checklist

This is the delivery checklist for a provider in Slop Conductor's current
native agent runtime. It applies to Go, Zen, and Codex. The larger
[`ProviderAdapter` proposal](provider-interface.md) is not a claim that its
illustrative DTOs or endpoints exist. A capability may be unsupported, but it
must be declared accurately and rejected before dispatch. Checkboxes record the
implemented Codex status; registration behavior applies to subscription auth.
Live verification is
a separate gate and is never inferred from local fixtures.

## Required execution behavior

- [x] Stable provider/model identities and explicit executable model mappings;
  unknown advertised models never become executable implicitly.
- [x] Supported authentication uses direct APIs in Rust and a shared connection
  pool; clients do not execute agents or own credentials.
- [x] Authentication and endpoint selection never silently switch billing paths.
- [x] Local readiness is distinct from an account catalog and successful inference.
- [x] Model capabilities drive configuration; explicit unsupported settings fail
  before network dispatch instead of being dropped.
- [x] Preserve instruction roles, canonical text/refusal blocks, tool proposals,
  paired results, provider call IDs, and their order.
- [x] Execute authorized tools only after a completed, validated proposal;
  partial, malformed, failed, canceled, and incomplete requests execute no tools.
- [x] Keep tool execution, child-task authority, scheduling, admission, and
  aggregate budgets in the daemon's existing runtime.
- [x] Preserve private completed continuation within its provider, model,
  endpoint, and credential-registration scope. Block required incompatible
  continuation; discard incomplete continuation and exclude it from public data.
- [x] Streaming preserves canonical block IDs and requires recognized terminal
  evidence. Bound requests, response bodies, UTF-8/SSE frames and assembled output.
- [x] Keep requested/reported model IDs distinct. Missing usage is unknown;
  cumulative snapshots are not summed repeatedly.
- [x] Local cancellation, steering, shutdown, client detach, and provider failures
  have distinct outcomes. Do not advertise confirmed upstream cancellation.
- [x] No transport retries, redirect following, implicit model substitution, or
  replay of tool operations with uncertain outcomes.
- [x] Persist accepted work, requests, tools, artifacts, and events through the
  common session/task APIs. Restart and reconnect never create another attempt automatically.
- [x] Credentials, authorization headers, upstream error bodies, and private
  reasoning never appear in public transcripts or diagnostics.
- [x] Unsupported media and hosted tools are explicit exclusions. Supporting
  ordinary function tools does not imply supporting every Responses API feature.

## Discovery and credential lifecycle

- [x] Expose a bounded, explicit live catalog probe with provider/account/auth
  scope and observation time; keep the offline executable catalog available.
- [x] Allow selecting API-key or subscription mode without resupplying a secret.
- [x] Keep multiple subscription registrations separate and allow selection,
  new-account login, and returning login with stable client/host identity.
- [x] Stop using signed-out tokens, attempt documented remote revocation, clear
  local tokens, and disclose unconfirmed revocation while retaining registration.
- [x] Permit explicit API-key removal without importing a stale startup key.

## Evidence required before calling a provider complete

- [x] Adapter fixtures cover every advertised setting/auth mode, structured
  translation, continuation scope, refusal, tools, stream failure, and cancellation.
- [x] Real daemon/CLI fixtures cover coding tools, account selection, live
  discovery, restart, cancellation, and credential privacy.
- [x] Formatting, Clippy with warnings denied, workspace tests, and bootstrap smoke pass.
- [ ] Credentialed live login, discovery, and a tool/result inference round trip
  pass against the supported upstream service. **Outstanding without credentials.**

See [Codex connection](codex-connection.md) for provider-specific restrictions and
the exact validation scope. Future media, remote clients, hosted tools, and public
reasoning-summary presentation remain separate product work.
