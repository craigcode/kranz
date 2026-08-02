---
title: "Reclaim sweep: stop re-dispatching claimed-and-spooled beads; fix lease pid classification"
priority: 1
schedule: once
---

# Reclaim sweep: stop re-dispatching claimed-and-spooled beads; fix lease pid classification

Source: mission m-83d1ed's final validation round (2026-08-01, verified by
the orchestrator reading the shipped code — "established, not hypotheses").
The beads bridge merged to main at 8526dea with these two verified defects
deferred here rather than paid for in further fix cycles.

## Problem (verified defect 1)

The `--reclaim` sweep can re-dispatch a bead that is already claimed AND
spooled. A bead mid-pipeline (claimed by a worker whose lease is live, or
lease-less but inside the TTL) should never be re-dispatched; the sweep's
current classification can put it back on the dispatch path, producing
duplicate in-flight work for one bead. The claim layer's idempotency bounds
the blast radius (a re-dispatched claim fails 'already claimed' and skips
silently) — so this is bounded noise, not corruption — but the sweep's
classification is wrong.

## Problem (verified defect 2)

The lease pid classification in the sweep releases claims on the wrong
signal in at least one class: a lease whose recorded pid is alive but NOT
the claimant's (recycled pid, or a heartbeat written by the wrong process)
is treated as live-and-ours, and the inverse case (same-user kill probe
failing on another uid's live process, EPERM) can be read as dead. The
klas queue's ClaimPidLiveness three-way verdict (Alive/Dead/Unknown with
identity-token comparison) is the proven shape to mirror — the sweep
should reuse or exactly mirror it rather than its current pid check.

## Acceptance hints

- A claimed-and-spooled bead is never re-dispatched by the sweep (fixture:
  claimed + live lease + spool present → untouched; claimed + dead lease →
  released exactly once).
- Liveness uses the queue's three-way verdict shape: Alive-by-token stands,
  Dead releases, Unknown hits only the TTL backstop.
- The round-trip gains a case for each; all existing cases keep passing.

## Context

The bridge merged with these open because they are bounded (claim-layer
idempotency absorbs the worst case) and the mission's fix-cycle cap was
exhausted. The final round's full finding list is in the mission events
(seq ~11062-11063); the two fix-feature specs the orchestrator drafted are
in `.kranz/missions/m-83d1ed/` events around the `fixFeatures` decision
text — mine them for the exact repro shapes.
