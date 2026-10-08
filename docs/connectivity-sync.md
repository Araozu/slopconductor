# Connectivity, peer visibility, and optional sync

## Principle

Local execution is complete on one node. Remote connectivity adds access to that
node; replication adds availability of its last-known data elsewhere. Neither
requires a central application server to become the execution owner.

The first remote milestone requires Tailscale on the participating machines and
client devices. The application still defines its own node identities, trust,
API authorization, and session ownership.

## Separate four concerns

| Concern | Responsibility |
| --- | --- |
| Discovery | Know which application nodes exist and their endpoints |
| Reachability | Establish a network connection to a selected endpoint |
| Authorization | Decide which client/peer can read or mutate which records |
| Replication | Copy selected owner-authored data and track its freshness |

A tailnet provides useful networking identities and connectivity, but the app
still needs a registry of application nodes. Start with explicit peer registration
and pairing; automatic discovery can be added if useful.

## Direct access over Tailscale

A native CLI/TUI can call a trusted daemon endpoint through the private network.
A phone with Tailscale can open a daemon's HTTPS web entrypoint when that client
exists.

Tailscale can connect devices directly and fall back to encrypted relay paths.
It also uses a coordination service. Thus this deployment avoids a required
central Slop Conductor server while still relying on networking infrastructure.
See [connection types](https://tailscale.com/docs/reference/connection-types)
and [control/data planes](https://tailscale.com/docs/concepts/control-data-planes).

[Tailscale Serve](https://tailscale.com/docs/features/tailscale-serve) can expose
a local HTTP service as private HTTPS. It is a possible deployment integration,
not a substitute for application authorization. The initial remote deployment
does not require a public Funnel endpoint.

M0 listens on loopback only. Remote privileged API access must wait for pairing
and capability enforcement. A private HTTPS proxy to the health-only service can
be explored independently without implying task control exists.

## Aggregate views

A client can query several configured owners itself, or one reachable daemon can
query its trusted peers and present a combined view. The latter is convenient for
a browser and gives it one authenticated entrypoint.

Aggregate records include source node, owner, OS, workspace, model, state, and
last observed time. Unreachable is distinct from stopped, failed, or completed.
Cached records are labeled with freshness and source cursor.

An aggregate node forwards mutations to the owner. It preserves command IDs and
acknowledgement provenance. If it stores an offline command, the UI reports
pending delivery until the owner accepts it. Commands can have expiry and
expected-revision conditions so stale instructions do not apply unexpectedly.

## Optional VPS role

The VPS can run the same daemon and additionally serve as an always-on aggregate
entrypoint, event/artifact mirror, or later notification service. Its own tasks
remain locally owned there. It is not mandatory for home-only execution.

When the VPS is unavailable, local execution and direct access to other reachable
owners continue. Cached remote views at a client or peer remain explicitly stale.
A gateway outage cannot trigger automatic execution failover.

## History replication

Begin with selective owner-authored events, session snapshots, and referenced
artifacts. Each replica stores per-session sequence cursors and source ownership
metadata. Transfers are authenticated, bounded, idempotent, and resumable.

Replicate only selected data. Credentials, arbitrary project contents, ignored
files, and entire workspaces are separate categories with explicit rules. Event
mirroring must not accidentally upload an entire repository.

A mirror is a read-only projection. It can display historical messages when the
owner is offline, but new execution waits for the owner or a deliberate handoff.
This model avoids requiring a general multi-writer merge algorithm for active
agent conversations.

Retention differences can create gaps. Source snapshots and manifests provide a
recovery route. Artifact integrity is checked by size/hash, with download limits
and storage quotas.

## Dual boot

Each OS install has its own node ID, credentials, project mappings, and data
directory. Group them under a physical-machine label for display. Only the booted
OS environment is normally available on that device.

Switching OS interrupts the old environment's process. Replicated history can
remain visible, but it cannot establish that the old task is running or that a
queued instruction has been applied. Shared disk contents do not imply shared
execution ownership.

## Alternative transport later

Built-in P2P is a separate reachability feature. Browser P2P commonly uses
WebRTC data channels, signaling, and possible TURN relay support; the
[WebRTC guide](https://webrtc.org/getting-started/peer-connections) documents those
pieces. Native daemon transports could use other libraries.

Keep command/event semantics independent of transport. Do not build NAT
traversal, relays, and internet rendezvous into the first CLI release when
Tailscale is an accepted deployment requirement.
