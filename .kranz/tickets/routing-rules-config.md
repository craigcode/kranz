---
state: done
state-note: Done: .kranz/routing-rules.json (taskClassRules + ordered patternRules, tiers never model ids) validated at draft/approve, base-branch-owned with mission-branch edit ignored+surfaced, executorRoute provenance on worker.spawned (fold-derived, effective tier honest). routing_rules_config filter: 19 green; full gates green.
title: Tracked routing rules — declarative task-class routing config
priority: 3
schedule: once
blocked-by: [backend-routing-abstraction]
---

## Goal
The routing floor's rules become a tracked, base-branch-owned config
artifact: complexity-tier rules (task class → tier) and ordered pattern
rules, validated at draft/approve, with the effective route recorded on
the mission's events.

## Context
The config-surface slice of backend-routing-abstraction (KRZ-331).
Reference design from the Warp scan (2026-08-04): their custom model
routers ship exactly two forms — complexity tiers and ordered rules — as
config files, with admin-published team routers. Kranz's version stays
deterministic (no LLM-judged routing in the floor), follows merge-gates
ownership (base-branch-owned; a mission cannot edit the rules that route
it), and resolves capability classes rather than hardcoded model ids
(docs/reviews/local-llm-and-triumvirate.md §1). The effective route and
matching rule are recorded per session — routing is provenance, not a
hidden implementation detail.

## Acceptance hints
- Rules file parses/validates at draft/approve; an invalid rule fails
  closed naming it.
- Determinism: same inputs → same route (test); rules read from the live
  base branch, a mission-branch edit is ignored and surfaced.
- Session events record the effective route and the rule that matched.
- No rules file ⇒ today's per-role config behavior (regression).
- Anti-vacuity grep on a named filter unique to this work.
