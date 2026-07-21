---
title: Deterministic tier routing + executor escalation (validator-local split out)
priority: 2
schedule: once
---

## Goal
Map task-class tags on backlog tickets to execution tiers (local vs frontier),
deterministically — no learned classifier. Route execution-class subtasks
(tests, mechanical refactors, doc generation, bounded diffs) to the local
executor; on two failed validations, escalate the ticket to the frontier tier.
Surface the escalation rate per mission on the dashboard — it is the live
metric of local-model competence.

## Context
From docs/scoping/local-inference-executor-tier.md (KRZ-206a). Deterministic
routing first (task-class → tier); learned/LLM routing is a later
optimization that needs the trace data anyway. Escalation builds on existing
validation-outcome plumbing: today the fix-cycle cap routes a stuck milestone
to Blocked (orchestrator.rs) — tier-escalation is a NEW branch (re-route up a
tier) rather than block, but reuses the same validation-failure signal. Study:
the fix-cycle / MilestoneBlocked flow, the ticket frontmatter schema (add a
task-class / tier tag), the estimate/decision event vocabulary, StatusStrip /
dashboard for the escalation-rate surface.

DELIBERATELY OUT OF SCOPE (see the sibling local-validator ticket, addendum
§4): this ticket routes the EXECUTOR local only. The validator stays frontier
here. Routing the executor local is low-risk because the validator still
catches bad work; routing the validator local is a separate, harder-gated
change because a weak local validator that wrongly PASSES bad work is a silent
green the escalation valve never catches.

## Acceptance hints
- A ticket tagged execution-class routes to the local executor tier; two
  failed validations escalate it to frontier, recorded as a decision event.
- The validator tier is unchanged (frontier) — no local validation in this
  ticket.
- Escalation rate is queryable per mission and rendered on the dashboard.
- cargo test --workspace + dashboard suite pass with routing/escalation pins.
