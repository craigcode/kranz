---
title: Flight Rules effectiveness, calibration, and exception metrics
priority: 3
schedule: once
blocked-by: [flight-rules-dashboard-report]
state: done
state-note: Implemented — deterministic cross-mission rule/revision folds expose honest raw denominators, advisory/fail/block/waiver/not-evaluated/false-green outcomes, resolution time and score distributions, minimum-sample suppression, and calibration smells through CLI JSON/text and REST. Dedicated flight_rules_metrics_* tests are green.
---

## Goal
Fold rule-level outcomes across missions so owners can see whether a standard
prevents deviations, blocks everything, produces noise, attracts waivers, or
is later contradicted by linked false-green defects.

## Context
KRZ-348; design D-H/D-K. Build on gate-score distributions, contract health,
outcomes, and defect linkage. This evaluates policy/checkers, not individual
engineers or agent backends. Preserve honest denominators and minimum sample
thresholds; absence is unknown, not success.

## Acceptance hints
- Per stable rule/revision: applicable/evaluated counts, advisory/fail/block/waiver rates, resolution time, optional score distribution, and false-green count.
- Minimum samples suppress conclusions while still showing raw counts; rules with no checker/evidence render not-evaluated rather than green.
- Flag near-constant score, never-near-threshold, high-waiver, always-fail, and never-selected smells with evidence and definitions inline.
- Revision boundaries remain visible; trends never silently merge rules whose meaning changed.
- Outputs are deterministic pure folds over existing events/tickets and machine-readable; no second analytics store.
- Anti-vacuity: unique filter `flight_rules_metrics_` reports a nonzero pass count.

## Wrong plan (from orchestrator)
KRZ-348 is a pure fold over rule-level outcomes, and every dimension it folds along is absent from the base this mission branches from (main @ 90a4bdc, re-verified this turn): `git grep -ci standards main -- crates apps` returns exactly one hit, a doc comment at crates/engine/src/pack.rs:1; crates/engine/src/pack/ contains only toml.rs with no standards module, no StandardsManifest, no rule/revision/lifecycle/checker type and no normalized digest; pack.rs:68,72,197 still define only SCHEMA_BASE= … (truncated)
