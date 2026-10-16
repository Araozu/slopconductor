# Daemon peering

The local M3 slice connects a CLI to its local daemon, which inspects and controls
an explicitly registered owner's existing conversations. The owner alone runs
inference and tools, persists receipts, and advances its event cursors. Peering
is optional and grants are directional. Each direction is configured separately.

## Deployment and setup

Run the owner with its existing administrator API and an additional loopback peer
listener in the same native process:

```sh
hived --peer-listen 127.0.0.1:7332
```

`peer_listen = "127.0.0.1:7332"` in the daemon TOML file or `SLOP_PEER_LISTEN`
provides the same setting. Both listeners require loopback addresses. With no
peer-listener setting, inbound peering is disabled; outbound peering remains
available. A bind failure occurs before queued execution starts.

Configure [Tailscale Serve](https://tailscale.com/docs/features/tailscale-serve)
separately to publish the **peer listener** as private HTTPS. For example, on
Linux, `sudo tailscale serve --bg http://127.0.0.1:7332`; on Windows, run the
corresponding command without `sudo` in an Administrator terminal. Check the
[current Serve examples](https://tailscale.com/docs/reference/examples/serve)
and the emitted HTTPS origin. Hive does not configure Tailscale.

On the owner, create a grant and save its secret privately before requesting
acceptance. The command ID is required so an ambiguous response can be retried:

```sh
slop peer grant --label work --scope control \
  --token-output /private/work-peer-token --command-id work-grant-1
```

The parent directory must exist. Windows token creation requires a location
beneath `LOCALAPPDATA`. The command prints the grant and owner node IDs, never
the token. An existing token-output file is reused for a retry. The default
scope is `read`; `control` additionally permits messages and turn controls.
A grant covers **all public conversations** on that owner. Control messages can
cause tools to execute under the selected session's existing policy.

Transfer the token file privately to the requesting machine, retaining private
permissions (0600 on Unix). On that machine, import it into the local daemon:

```sh
slop peer add home --origin https://HOME.TAILNET.ts.net \
  --owner-node-id OWNER_NODE_ID --credential-file /private/work-peer-token
slop peer probe home
slop --peer home node
slop --peer home session list
slop --peer home session history SESSION_ID
slop --peer home session follow SESSION_ID --after 0
slop --peer home session send SESSION_ID --text "Focus on the failing test" \
  --delivery next-boundary --detach --command-id correction-1
slop --peer home turn pause TURN_ID --command-id pause-1
slop --peer home turn resume TURN_ID --command-id resume-1
slop --peer home turn cancel TURN_ID --command-id cancel-1
```

The `--daemon` and `--token-file` flags still select the local gateway and its
administrator credential. `--peer` selects a registration on that gateway;
only the gateway uses the remote credential. Existing CLI following and Ctrl-C
detach behavior applies. Steering retains the session's model/settings; changing
those through peering is outside this slice.

`slop peer list`, `slop peer grants`, `slop peer remove home`, and
`slop peer revoke work-grant-1` inspect or remove the corresponding outbound
registration/inbound grant. These administration commands target the selected
local administrator API and reject `--peer`.

## Trust, credentials, and limits

Remote origins must be HTTPS origins without paths, embedded credentials,
queries, or fragments. HTTP is allowed only with literal loopback IP addresses
for offline fixtures. Ordinary plaintext tailnet origins are rejected.

Every remote operation checks API compatibility and the authenticated node ID
against the registration before accessing sessions. Forwarded authenticated
requests also carry `x-slop-expected-owner-node-id`; the owner enforces that
precondition before dispatch, covering a restart between probing and mutation.
Health is only a
compatibility check, not proof of identity. TLS authenticates the configured
origin; a bearer grants application access. A caller's machine identity is not
independently proven by possession of that bearer.

Inbound grants store only SHA-256 digests of random 256-bit tokens in SQLite.
Outbound tokens live in private credential files indexed by digest, separately
from conversations and registry metadata. Distinct immutable credential files
prevent a failed or canceled registration from overwriting an accepted peer's
secret. Private writes retain their serialization guard until the blocking
write actually completes; shutdown drains those writes before releasing storage.

Grant IDs and peer aliases accept 1–128 ASCII letters/digits/hyphens/underscores.
Repeated identical creation returns the existing record; conflicting reuse is
rejected. Revocation is permanent. Removed registrations remain tombstones;
use a new alias to change an origin, owner, or credential. Each daemon supports
128 inbound grants and 128 outbound registrations, including tombstones.
Credential-file retention/garbage collection is future work.

There are 32 network admissions and 16 event-stream admissions shared across
inbound/outbound peering, with eight ordinary requests and four streams per
grant/registration. Excess admissions fail immediately. JSON and event frames
retain the native client's existing byte bounds. Connection and response-header
waits are bounded. Network requests consume no model execution slots.

## Public surface

The administrator listener adds the following authenticated routes:

| Route | Result |
| --- | --- |
| GET /v1/peer-grants | Bounded list of inbound grant metadata |
| PUT /v1/peer-grants/{id} | Idempotent grant creation from label/scope/token |
| POST /v1/peer-grants/{id}/revoke | Permanent revocation |
| GET /v1/peers | Bounded outbound registration list |
| PUT /v1/peers/{id} | Idempotent origin/owner/credential registration |
| DELETE /v1/peers/{id} | Tombstone the registration and close its streams |
| GET /v1/peers/{id}/probe | Verify and return the registered owner's identity |
| /v1/peers/{peer_id}/health, /node | Owner health and authenticated identity |
| /v1/peers/{peer_id}/sessions | GET owner sessions |
| /v1/peers/{peer_id}/sessions/{session_id} | GET owner session |
| /v1/peers/{peer_id}/sessions/{session_id}/messages | GET history; POST steering |
| /v1/peers/{peer_id}/sessions/{session_id}/events, /instructions | GET event stream or instructions |
| /v1/peers/{peer_id}/turns/{turn_id} | GET turn status |
| /v1/peers/{peer_id}/turns/{turn_id}/pause, /resume, /cancel | POST owner controls |
| /v1/peers/{peer_id}/messages/{message_id}, /tools/{tool_id} | GET canonical public message/tool snapshots |

The separate peer listener exposes only the corresponding unprefixed
health/node/session/turn/message/tool routes. Its health advertises this reduced
surface. Provider/admin/filesystem/workspace/history-edit routes and the gateway
routes are absent. The local administrator token is rejected on the peer
listener; peer credentials are rejected on the administrator listener. A peer
cannot transitively access another configured peer.

New DTOs and administrator routes are included in the compile-time OpenAPI
document. The peer listener intentionally has no Swagger or OpenAPI endpoint;
the administrator document describes both the gateway and its restricted owner
operations here, rather than exposing all administrator routes remotely.

## Failure and recovery semantics

Forwarded DTOs and command receipts remain unchanged. The HTTP
`x-slop-owner-node-id` header provides provenance, and sessions retain their owner
field. The CLI reports the selected peer alias on stderr. Event IDs, history
pagination, revisions, remote paths, and event cursors remain the owner's values.
The gateway never interprets remote workspace paths locally or creates replicated
sessions. Events use bounded NDJSON relay and owner cursor replay.

Mutations preserve command IDs, payloads, and revision preconditions. The gateway
returns the owner's acceptance receipt; acceptance does not mean execution has
finished. A lost or invalid response after dispatch returns
`peer_delivery_unknown`. Explicitly repeat the same operation with the original
command ID and payload to recover its receipt. There is no automatic mutation
retry, offline delivery queue, or replay of an uncertain tool operation.

Unreachable, unauthorized, incompatible, removed, and identity-mismatch errors
remain distinct. A failed identity probe is an observation rather than a change
to the owner's task status. Revocation and outbound removal close associated
streams, including idle streams. Already accepted owner work continues through
revocation, CLI exit, gateway shutdown, or network loss; stopping it requires an
explicit owner control command.

Registry state, grant revocation, tokens, and owner receipts survive restart.
Replacing an owner's data directory fails authentication or the expected-node
check; the gateway never silently adopts the new identity. Explicit registration
is required for the replacement owner.

Aggregate views, cached/mirrored history, offline commands, task creation,
per-session sharing, ownership transfer, and cross-node child execution remain
future slices. A deployed two-machine Tailscale HTTPS run remains a separate
validation gate from offline real-binary tests.
