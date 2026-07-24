---
title: Price tier switches as cache-miss events in the cost model
priority: 3
schedule: once
---

## Goal
When the executor escalates off the local tier (tier.escalated), the first
frontier turn re-reads the whole conversation prefix UNCACHED at frontier
rates — a cache-miss event, typically ~10x the cached-prefix price. Today
estimate() does not model escalation at all, so a local-routed mission's
estimate can be wildly wrong in both directions ($0-marginal when it
completes locally, full frontier when it escalates). Extend the cost model:
(1) for local-routed plans, estimate the local path as $0 marginal AND the
escalated path (frontier per-token with the first turn priced as a full
uncached prefix), reporting both rather than a single number; (2) the
escalation point is a feature/milestone edge by design (kranz starts fresh
worker contexts per feature), so the model prices the miss ONCE per
escalation, not per turn — mid-feature switches would be a router policy
change and out of scope.

## Context
Split out of local-inference-cost-accounting.md's cache-miss amendment
(cursor.com/blog/router) — that ticket's acceptance (tag + exclude +
trailer) landed without touching estimate(); this is the modeling half.
Needs: how estimate() composes per-run costs, the router's deterministic
local-tier rule (m-3cda6a), TierEscalated emission (escalate_or_block),
and mission cost trailers for the two-number shape. Data caveat: the
escalated-path calibration corpus is ~zero (escalated_milestones counts
exist; sample before trusting any multiplier).

## Acceptance hints
- A local-routed plan's estimate shows both paths with the cache-miss
  priced explicitly (full prefix at uncached frontier rate for the first
  post-escalation turn).
- A fixture escalation prices the miss once per escalation, never per turn.
- cargo test --workspace green.
