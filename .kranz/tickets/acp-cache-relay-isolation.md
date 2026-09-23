---
state: open
title: Qualify isolated ACP cache inputs and relay listener
priority: 3
schedule: once
blocked-by: [acp-release-review-followups]
---

## Goal

Design and separately qualify a profile that does not expose operator cache
contents or relay authority to unrelated Docker peers.

## Context

The release follow-up establishes the current trust model: mounted caches are
readable and the relay listens on both its internal and default-bridge interfaces.
Those are supported single-operator host assumptions, not hostile multi-tenant
isolation. See [ACP containment](../../docs/acp-containment.md#host-trust-and-dns-prerequisite).
Do not silently change the already qualified profile's mount or network shape.

## Scoping answers

- Decide whether to introduce a new explicit profile revision with isolated, immutable per-project cache inputs; inventory source confidentiality, credential exclusions, disk cost and cache-miss behavior before implementation.
- Design a relay listener reachable only from its owned worker network, retaining authenticated host forwarding, bounded ownership, crash recovery and denial attribution; include an unrelated default-bridge peer as an adversary.
- Retain the current profile and its documented trust assumptions until replacement qualification is approved; no automatic migration or widened grants.
- Run positive and negative probes for cache reads/writes, host-side mutation, cross-network relay use, DNS, cleanup and exact provider startup pins before making new qualification claims.
- Out of scope: client filesystem/terminal RPCs, provider credential acquisition, a new scheduler, model routing or live-provider calls without separate bounded authorization.

## Acceptance hints

- An operator-reviewed design names the new profile boundary and migration choice before implementation begins.
- A subsequent implementation has independent boundary review and retained exact-runtime proof; a version number or green mock test alone cannot qualify it.
