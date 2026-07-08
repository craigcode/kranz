---
title: Add a live sandbox-exec test proving macOS fs+net egress actually blocks a disallowed host
priority: 2
schedule: once
---

## Goal
The Tier-2 fs+net egress (sandbox.rs, landed in 8da93ce) generates a correct
macOS SBPL profile — deny-default + scoped `(allow network-outbound
(remote tcp "<host>:<port>"))`, NOT `(allow network*)` — verified by a
profile-STRING test. But there is no LIVE test that `sandbox-exec -f <profile>`
actually BLOCKS a connection to a non-allowlisted host at runtime. Seatbelt's
hostname-based `remote tcp "host:port"` filtering has known semantic caveats
(it is not DNS-aware the way an app-layer allowlist is). Add a live test
mirroring the fs tier's `sandbox_enforcement_macos_allows_inside_denies_outside`:
under a fs+net profile, a connection to an allowlisted host:port succeeds and a
connection to a NON-allowlisted host:port is refused. If Seatbelt can't enforce
hostname egress at runtime, fail closed / document the limitation loudly rather
than imply containment the OS doesn't deliver.

## Context
Found in the 8da93ce code review 2026-07-07. fs+net is opt-in (enforce defaults
off), so dormant — but a containment feature that looks enforced but isn't is a
false-security trap (same class the sandbox tiers exist to avoid). Verify before
anyone relies on fs+net on macOS. Linux (`--unshare-net`) already fails closed.

## Acceptance hints
- A macos-gated live test: under a generated fs+net profile, an allowlisted
  destination connects and a non-allowlisted one is refused; OR the profile is
  proven to fail closed and the hostname-egress limitation is documented.
- cargo test --workspace green.
