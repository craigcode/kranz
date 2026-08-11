---
title: Instant worker CLI auth-death must not burn the respawn budget
priority: 2
schedule: once
state: done
state-note: Fixed — spawn_auth_death classifier parks the milestone (backend unauthenticated) instead of consuming the respawn budget; feature stays Active and re-runs on re-auth.
---

## Goal
A worker session whose backend CLI dies in seconds with an authentication
error (e.g. cursor-agent `Error: Authentication required`, exit 1, no
terminal event) is an infrastructure failure, not a worker-quality failure.
Today it consumes the feature's respawn budget: on m-eee81f (2026-08-09)
cursor auth expired mid-mission and three ~1-second spawns exhausted the
ms-1-fix-3-1 budget ("respawn budget exhausted"), failing a feature whose
worker never ran. Classify spawn-time auth/dead-binary failures separately
from genuine worker failures: do not decrement the respawn budget, surface
`backend unauthenticated` as the block reason, and park for operator action
(or requeue with backoff) instead of failing the feature.

## Context
`backend-readiness-quota-preflight` (done) gates queue drain before claim;
this is the in-flight complement — auth can expire mid-mission (observed:
cursor token died between fix rounds 3 and 4 of m-eee81f, ~17:44). The
signal is detectable: sub-N-second exit, nonzero status, empty/missing
terminal event, stderr matching the backend's auth-error pattern (cursor:
"Authentication required"; codex: 401; claude: OAuth expired). Keep the
classifier per-backend and conservative — a slow genuine failure must still
consume budget normally.

## Scoping answers

## Acceptance hints
- A worker spawn that exits within a bounded window with a recognized
  auth/dead-binary signature does not consume respawn budget and surfaces a
  distinct block reason naming the backend and the re-auth action.
- Genuine worker failures (ran, produced a verdict, failed) unchanged.
- Tests: simulated instant auth-death spawn vs simulated genuine failure;
  anti-vacuity grep on the named filter.
