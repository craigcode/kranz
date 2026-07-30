---
title: Cost per merged change, grouped by repo
priority: 2
schedule: once
---

## Goal
The outcomes report gains cost per merged change, grouped by repo across
the M8 host catalog, sitting beside autonomy ratio — turning the
consumption-cost worry into a number.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-329; scored-gates
addendum). The numerator exists (the cost fold); the denominator is merged
changes — the derived landed/ancestry check (merged.rs), derived at fold
time, never stored. Grouping rides the M8 multi-root catalog. Coordinate
with outcomes-report-task-class: cost-per-change-per-task-class and
cost-per-merged-change-per-repo are the same fold with different
groupings — extend it, do not build a second one.

## Acceptance hints
- Derivable from event logs alone; a fixture proves no external data
  source is consulted.
- Repos with no merged changes in the window are absent, not zero (house
  rule: no fabricated numbers).
- The time window is a parameter with a documented default; boundary
  behavior tested.
- Anti-vacuity grep on a named filter unique to this work.
