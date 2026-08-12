---
state: done
state-note: Done: gate_scores.rs series fold + kranz gate-scores [--json]; vacuous-filter is the honest scored demo (determination coverage, threshold 1.0, absent when no graded surface). gate_score_series filter: 14 green; full gates green. Unblocks gate-score-distribution-flags.
title: Gate results carry an optional confidence score and threshold
priority: 1
schedule: once
blocked-by: [gate-plugin-interface, gate-results-first-class-events]
---

## Goal
Gate results may carry a gate-supplied confidence score and the threshold
the gate evaluated against, alongside the authoritative verdict — persisted
through gate events and queryable by gate identity across missions.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-315; scored-gates
addendum — source: Litera's ARGO/Mates programme post, Jul 2026, where
confidence-scored review agents stand in for sign-offs and low scores route
back to a human). Contract rules: score and threshold are gate-supplied —
kranz records what the gate reported, never normalises or reinterprets;
the verdict stays authoritative (a gate may pass with a low score or fail
with a high one; kranz must not derive the verdict from the score); the
score is optional, and absence is the normal case downstream — boolean-only
gates emit nothing and nothing special-cases them. gate-plugin-interface
already reserves the optional field in the result type (do-first coupling:
the contract is born extensible so no gate is ever reworked off a boolean
shape); this ticket is the persistence and query half. Persistence rides
the gate event additively (events.rs contract-file discipline). The gate
contract doubles as the clean-room boundary — no consumer-specific
vocabulary in the type (positioning ADR).

## Acceptance hints
- Boolean-only gates work unchanged and emit no score (regression).
- A gate event replays to verdict + score + threshold; a score-absent event
  replays cleanly; old logs fold.
- Gate results are queryable by gate identity across missions, returning
  the score series (consumed by gate-score-distribution-flags).
- Anti-vacuity grep on a named filter unique to this work.
