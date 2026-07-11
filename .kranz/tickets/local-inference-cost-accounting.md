---
title: Local-run cost accounting so calibration stays clean
priority: 4
schedule: once
---

## Goal
Account for local-inference runs distinctly from paid frontier runs: a local
turn is ~$0 marginal (fixed hardware + electricity, not per-token). Flag local
runs as "local: $0 marginal" (with optional amortized hardware/watt) so they
do not pollute the calibration corpus or the mission cost trailers when mixed
with paid missions.

## Context
From the review addendum §5 of docs/scoping/local-inference-executor-tier.md —
a gap the original workstream missed. cost.rs prices per-token per-model; a
local mission would show ~$0 next to a $30–160 frontier mission and distort
both the estimate calibration (the corpus is small and hard-won — see the
calibration history) and the per-mission cost receipts. Study: cost.rs pricing
table + how cost lands in events and report.md, the calibration corpus and
shape-aware estimator, the mission-cost-trailers work. Decide: is a local turn
priced $0, or amortized (hardware $ / expected-lifetime-tokens + watts)? Start
with a distinct "$0 marginal (local)" tag that the calibrator EXCLUDES from
frontier-cost calibration; amortized accounting is a later refinement.

## Acceptance hints
- A local-backed mission's cost is tagged local/marginal-zero and is excluded
  from (or bucketed separately in) the frontier-cost calibration corpus.
- Mission cost trailers / receipts distinguish local from paid spend rather
  than reporting a misleading $0.
- cargo test --workspace passes with a pin that a local run does not enter the
  frontier calibration set.
