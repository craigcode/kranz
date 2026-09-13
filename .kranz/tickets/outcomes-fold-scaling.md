---
state: done
state-note: per-(path,len,mtime) memoization in outcomes.rs cached_mission_outcomes: unchanged logs not re-parsed; append-only logs invalidate deterministically via len; per-entry computes/hits counters prove it in the invalidation test (global counters race across parallel tests). Single-source fold unchanged; skip semantics unchanged.
title: Outcomes fold scales with total event-log bytes — add caching or projection
priority: 3
schedule: once
---

## Goal
The flight-surgeon outcomes fold (crates/engine/src/outcomes.rs, landed in
m-d1e3c3) re-reads and re-parses every mission's entire events.jsonl on each
request — including worker.message stream deltas, which dominate log size.
Fine at ~48 missions with a fetch-on-mount dashboard; degrades linearly from
there. Add one of: (a) a per-(mission, lastSeq) memoized fold in the server,
(b) a streaming parser that projects only the event kinds the fold consumes,
or (c) an incrementally maintained fold updated on event append. Keep the
single-source architecture: the engine-owned fold stays the only math;
caching must not become a second source of truth (invalidate on lastSeq).

## Context
Pre-merge review of m-d1e3c3 finding (outcomes.rs fold + REST handler in
crates/server/src/rest.rs): no pagination, no byte cap, no cache. The plan
sanctioned the pure-fold shape for correctness-first; this ticket is the
scheduled scaling pass, not a correctness fix. The dashboard fetches once on
mount today, so there is no polling amplification — the trigger for this
work is mission-count growth or a polling consumer.

## Acceptance hints
- Repeated outcomes requests do not re-parse unchanged event logs (memo key
  or incremental update demonstrably skips unchanged input; a test proves
  invalidation on new events).
- cargo test --workspace passes with a pin that fold results are identical
  before/after the change on a fixture log.
