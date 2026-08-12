---
state: done
title: Shape-aware cost estimates (doc-heavy vs code missions)
priority: 2
schedule: once
---

## Goal
m-d341a7 cost $163.64 against an $18.35 expected estimate (9x): doc-heavy validator-heavy missions have a different cost shape than the code missions in the calibration corpus. Segment calibration by mission shape (e.g. contract-command mix, docs-only diff scope, validator passes) or at minimum widen estimates with a shape multiplier and label low-confidence estimates as such.

## Context
m-d341a7 (doc-heavy, validator-heavy): $163.64 actual vs $18.35 expected — 9x, 3.6x over the range ceiling; the calibration corpus (crates/engine/src/cost.rs, calibrated from completed missions) contains mostly code missions. Cost drivers observed: 20 sessions, 5 validation passes, 17 orchestrator judgment turns, 21.5M cache-read tokens. Start with an assessment: which observable plan features predict the shape (docs-only diff scope, contract-command mix, milestone/feature count, expected validator passes)? Then either segment calibration by shape or apply a shape multiplier — and when confidence is low (no corpus for the shape), SAY SO in the estimate line instead of printing a tight range.

## Scoping answers

## Acceptance hints
- Estimating m-d341a7's plan post-hoc lands its actual within the produced range (backtest as a unit test with the recorded corpus).
- Estimates carry a confidence/shape indicator when the corpus lacks that mission shape.
- Existing code-mission estimates unchanged within tolerance (regression test).
- cargo test --workspace passes, piped through grep -qE 'result: ok\. [1-9][0-9]* passed'.
