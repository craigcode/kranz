---
state: done
state-note: Shipped in #172: planner instructions request touchSet, review surfaces/CLI/Slack/plan.md state when a plan declares no write paths, and queue --timing reports coverage.
title: Declare write paths on every plan, or flag their absence at review
priority: 2
schedule: once
---

## Goal

Make a plan with no declared write paths the visible exception. A plan whose `touchSet` is empty has no bound on what the mission may change, and nothing in plan review says so today.

## Context

Measured on this repository on 2026-11-11: 14 of 50 plans declared a `touchSet`. Write paths are shown in full at plan approval when present (`PlanReview.tsx`, the Slack plan cards); when absent the section is simply missing.

This is a consent gap in its own right. It is also the precondition for ever admitting missions by declared paths: see [Concurrent missions](../../docs/scoping/concurrent-missions.md), D-D rule 1 and the D-A revisit trigger.

## Scoping answers

- Do not change what an empty `touchSet` permits at run time in this ticket. Changing enforcement is a separate decision.
- The planner is asked for a `touchSet`; a plan without one is still a valid plan.
- Every review surface (dashboard plan review, Slack plan card, CLI plan output) states plainly when a plan declares no write paths.
- No new prompt routing or context features: the request for write paths belongs in the existing static plan instructions.

## Acceptance hints

- Plan review for a plan with no `touchSet` shows a line saying the mission's writes are not bounded by the plan, on all three surfaces.
- A plan with a `touchSet` renders as it does today.
- A metric or report line gives the share of approved plans that declared write paths, so coverage can be tracked.
