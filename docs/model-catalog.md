# Model catalogs and reasoning efforts

**Implemented, 2026-10-16.** Go and Zen's live `GET /models` endpoints return
model identities, not reasoning controls. Their public Models.dev entries expose
`reasoning_options`, API package mappings, tools, and context/input limits.
The Rust runtime uses those explicit entries for provider-specific discovery
and validation. It never infers effort levels from a model name or from
`reasoning: true` alone.

`GET /v1/models` keeps its existing `ModelResponse[]` contract, including
`capabilities.reasoning_efforts`. Its executable catalog combines verified
compiled mappings with Models.dev entries whose API formats the adapter supports.
The authenticated `GET /v1/providers/{provider}/models` probe keeps its existing
`ProviderCatalog` contract: live advertised IDs are enriched with the same
metadata. An advertised ID with no supported API mapping remains unavailable.
Public metadata describes adapter support; it does not establish account access,
quota, or current upstream availability. Native `child_discover`, session/turn
validation, tasks, matrices, children, and inference use the same runtime catalog.

The source keys are `opencode-go` for Go and `opencode` for Zen. A model's explicit
`provider.npm`, or its provider's default `npm`, selects an implemented API shape:
`@ai-sdk/openai-compatible` means Chat Completions, `@ai-sdk/openai` means
Responses, and `@ai-sdk/anthropic` means Messages. Verified compiled mappings
remain available, including provider-specific exceptions. New entries must
explicitly advertise text output. New entries using
other packages, such as Google or System One, are not executable. Deprecated
entries do not add new execution mappings.

Only explicit `reasoning_options` entries of type `effort` populate
`reasoning_efforts`. Values retain provider ordering and are deduplicated.
Empty options, reasoning booleans, toggles, and token budgets do not manufacture
an effort menu. Null and `default` represent no override. Future bounded effort
names are accepted from metadata without updating a compiled enum. Explicit
unsupported settings fail before acceptance and dispatch.

Selected efforts use the declared API format:

- Chat Completions: `reasoning_effort`.
- Responses: `reasoning.effort`.
- Messages: `output_config.effort`.

An omitted effort leaves provider defaults in effect. `none`, when advertised,
is an explicit wire value; it is distinct from clearing the override. Thinking
toggles and token budgets are separate controls and are not implemented by this
effort-only contract. Codex retains its existing model-specific implementation.

The daemon refreshes `https://models.dev/api.json` at startup and every hour.
Fetches have a five-second timeout and a 16 MiB body cap, send no provider
credentials, and follow no redirects or automatic HTTP retries. Replacement is
atomic; malformed, missing, oversized, or failed responses retain the previous
in-memory snapshot. Startup falls back to the bundled public subset when offline.
Live refreshes are not persisted across restarts.

Set the optional TOML `models_dev_url` to an explicit Models.dev-shaped endpoint
for a controlled gateway. HTTPS and loopback HTTP are accepted; embedded
credentials, queries, and fragments are rejected. A custom Go or Zen inference
endpoint disables the default remote refresh unless `models_dev_url` is supplied,
so offline fixtures continue to use only bundled data. The selected default model
is validated after startup discovery and before storage or execution starts.

The bundled subset was retrieved from the public API on 2026-10-16; the upstream
repository revision observed at that time was
[`ab9af7ba4aa16ee65e9f42c01544393f4f49ec44`](https://github.com/anomalyco/models.dev/tree/ab9af7ba4aa16ee65e9f42c01544393f4f49ec44).
It contains only the two providers' model IDs, names, API mappings, reasoning
options, tool support, modalities, limits, and deprecation status. It includes no credentials
or inference responses. See the upstream [metadata schema](https://github.com/anomalyco/models.dev/blob/dev/packages/core/src/schema.ts)
and [Anthropic effort encoder](https://github.com/anomalyco/opencode/blob/dev/packages/ai/src/protocols/anthropic-messages.ts).

Offline Rust tests cover parsing, per-provider wire/effort differences, fixed
reasoning, unsupported formats, bounded transport, and wire encoding. The real
daemon smoke check uses synthetic catalogs to discover and execute new models
over all three API formats on both gateways, and verifies invalid efforts never
dispatch. No paid live inference was used for this change.
