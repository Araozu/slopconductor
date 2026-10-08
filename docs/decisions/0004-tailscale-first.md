# 0004: Tailscale for initial remote reachability

Status: accepted. Date: 2026-10-08.

## Context

The user is comfortable requiring Tailscale. The project does not need to solve
internet rendezvous, NAT traversal, or relay infrastructure to deliver its first
remote-control workflow.

## Decision

Build initial remote access over a tailnet using the same public daemon API.
Pairing, node identities, API authorization, and freshness remain application
responsibilities. A VPS gateway is optional.

The bootstrap remains loopback-only until privileged remote access has its
required authentication and policy implementation.

## Consequences

Participating remote client devices need private-network access. A responsive
web client can be used from a phone on the tailnet.

Built-in P2P remains an optional transport track. Command/event semantics and
local ownership must not depend on whether networking is direct or relayed.
