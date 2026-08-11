---
title: Every security(1) invocation needs a hard timeout — locked keychains hang, not fail
priority: 2
schedule: once
state: done
state-note: Fixed — both security call sites route through one bounded helper (10s kill deadline, Err(TimedOut)); regression test pins the locked-keychain hang.
---

## Goal
On a host whose login keychain is locked and never GUI-approved, `security`
subcommands can block forever (observed 2026-08-10 on m-eee81f: the pre-
692d92f `create-keychain -p ""` empty-passphrase path prompted interactively
and hung the branch's whole `cargo test --workspace` gate for 20+ minutes;
stray `find-generic-password -s gh:github.com` lookups from other tools pile
up hung the same way). 692d92f removed the empty-passphrase prompt on main,
but ANY future `security` call site (or a securityd authorization prompt
from another trigger) hangs gates the same way. Wrap every `security`
invocation in the engine (backend_cursor keychain seeding, any probes) with
a bounded wait (kill after N seconds, treat timeout as
unavailable-not-authorized), and add a regression pin: a fake `security`
that sleeps forever must make the caller fail fast, never hang the suite.

## Context
The hang mechanism: `security` talks to securityd, which requests GUI user
interaction when the keychain is locked; with no approval the call parks
indefinitely (no errno, no timeout). The mission-side manifestation was
version skew (branch predated the hardening), but the engine-side lesson is
that `security` is an unbounded-blocking external dependency and must be
treated like a network call. Found while un-wedging m-eee81f's f-2-4 gate
(killed 12 hung `create-keychain` children by hand; kill-watch bash-o3fvg7mf
held the line for the remainder of the mission).

## Scoping answers

## Acceptance hints
- Every `security` spawn in the engine goes through one bounded helper.
- Test: stub `security` that sleeps; caller fails within the bound naming
  the timeout; anti-vacuity grep on the filter.
