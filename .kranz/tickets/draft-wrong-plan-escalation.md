---
title: Draft-stage 'this plan is likely wrong' escalation, distinct from NeedsContext
priority: 3
schedule: once
---

## Goal
Give the planner a third voice beside "plan ready" and "underspecified"
(NeedsContext): a structured WRONG-PLAN escalation — "I can produce a
plan, but it is likely wrong / the goal is misframed / the premise is
broken" — that parks the ticket for the operator with the reasoning
attached, before any approval. Today a wrong plan is discovered
mid-mission (blocked, revision) at mission prices; the wrong-spec gap
(the model that grills also writes the spec) means nothing currently
catches it at draft time.

## Context
From docs/reviews/mattpocock-skills-flow.md (idea 4, ADOPT). NeedsContext
means "I lack information"; wrong-plan means "I have the information and
the shape still doesn't hold." Surfaces: the draft loop's structured
outcome (draft.rs DraftOutcome), ticket status (a new state or a
NeedsContext variant with a distinct reason label), dashboard/Slack
rendering of the parked reason. Keep it additive and opt-in by the
planner only — never inferred.

## Acceptance hints
- The planner can emit the escalation; the ticket parks with the reason
  visible in CLI/dashboard/Slack; the operator resolves it by editing,
  re-scoping, or abandoning the ticket.
- Distinct from NeedsContext in the status file and in rendering.
- cargo test --workspace green with a parked-escalation fixture.
