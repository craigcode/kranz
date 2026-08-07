---
state: done
state-note: aa8e4ef: plan-time context-fit check at approve+revise — per-feature spec score (chars + file mentions) vs corpus p90 from historical plan.json (documented defaults unfitted); over-anchor emits orchestrator.decision + plan.md Context-fit note, advisory only, never a gate. plan_fit.rs with unit tests at both boundaries.
title: Plan-time feature context-fit check (warn when a feature won't fit one session)
priority: 3
schedule: once
---

## Goal
At plan approval, estimate each feature's expected session footprint and
warn (or suggest a split) when a feature looks bigger than one worker
session's context budget — the part of Pocock's "context-window-sized work
unit" kranz lacks. kranz already enforces fresh-context-per-feature at
run time; this moves the sizing signal to plan time, where splitting is
cheap. v1 can be a heuristic (spec length + expected file count from
touch_set + historical per-feature token p50 from the calibration corpus),
surfaced as a planning decision note, NOT a hard gate.

## Context
From docs/reviews/mattpocock-skills-flow.md (idea 1, ADOPT-partial). The
worker already stops cleanly at its turn budget with an honest partial —
that is the safety net, not the plan. Data sources: cost.rs calibration
corpus per-feature token means, plan.json feature specs, the plan-approved
estimate flow (approve_plan, orchestrator.rs).

## Acceptance hints
- Over-budget features produce an operator-visible warning at approval
  (decision event + plan.md note), never a silent split or a hard block.
- A fixture plan with one oversized feature warns; a right-sized plan
  stays quiet.
- cargo test --workspace green.
