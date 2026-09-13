---
state: done
state-note: shipped in 952cbc0
title: Blast-radius-gated Considered Alternatives section in parked plans
priority: 3
schedule: once
---

## Goal

Large-scope drafts must weigh alternatives before committing to a plan
shape, and the approve gate must see what was rejected. When a draft's
estimated scope crosses a threshold (touch-set breadth and/or estimate
high bound — thresholds configurable), the planning turn is required to
produce a "Considered alternatives" section in plan.md: the chosen
approach plus at least two rejected shapes, each with a one-line
trade-off. Small drafts are exempt — forcing three approaches on a $5
fix is ceremony, not rigor. Plans below the threshold may include the
section voluntarily; above it, a parked plan without one is bounced
back to drafting.

## Context

Receipt: m-079c36 (worktree isolation) blew its estimate ceiling 2x;
the approve gate saw one plan shape and had no way to know whether a
cheaper decomposition was considered. This applies the policy-scaling
design note (docs/scoping/worker-sandboxing.md, "Design note — policy
scaling"): rigor as a function of blast radius x autonomy. Surfaces:
drafting prompt + plan structured-output schema gain the section;
plan.md rendering includes it; the dashboard/Slack plan review shows
it (it is exactly what the human gate needs to be informed). The
reviewable artifact is the point — this is consent quality, not
planner busywork.

## Acceptance hints

- A draft whose estimate/touch-set crosses the threshold parks with a
  Considered Alternatives section (>=2 rejected shapes with
  trade-offs); one below parks fine without it.
- The section renders in plan.md and in the pending-plan review
  surfaces.
- Threshold configurable; cargo test --workspace green.
