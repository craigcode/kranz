---
title: Price block/resume cycles in the cost model (calibration slices 2-3 follow-up)
priority: 4
schedule: once
---

## Goal
Fold block/resume and fix-cycle activity into the cost estimate: missions
that hit checkpoint refusals, grant parks, or validation fix-cycles cost a
measurable multiple of the happy path, and the model currently prices none
of it. Extend the calibration features with per-mission counts of
milestone.blocked / grant.requested / fixfeature.created / resume events
(already in the event log) and fit a multiplier term so estimates widen
honestly for plumbing-heavy or gate-heavy shapes.

## Context
Three consecutive missions blew their estimates on this axis (2026-07-19):
m-9dc8c1 $93.41 vs $16.39 expected (2.5× high), m-0f1abd $75.17 vs $16.39
(2× high), m-b66d34 $248.13 vs $28.51 (≈4× high) — all driven by secret-scan
blocks, grant parks, and validation rounds rather than plan scope. The
estimate-calibration scoping doc already records slices 2 (turn-driven
orchestrator term) and 3 (shape/size buckets) as open; this ticket adds the
gate-activity signal to whichever slice fits. Corpus: 40+ completed missions
with full event logs, including these three outlier cases with named causes.

## Acceptance hints
- Calibration harvest shows the gate-activity features correlate with
  estimate error (report the fit for the three named outliers).
- New estimates for gate-heavy shapes widen (p90 covers the m-b66d34 case
  without making every estimate uselessly wide).
- `cargo test --workspace cost` green; docs/scoping/estimate-calibration.md
  updated with the measured result.
