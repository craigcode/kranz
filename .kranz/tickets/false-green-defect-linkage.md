---
state: done
state-note: Superseded by the flight-surgeon console (fc93a23); marked 2026-07-30 by operator. Do not work.
title: False-green detection via defect→mission linkage
priority: 1
schedule: once
---

## Goal
When a later defect ticket links to the mission that shipped the defect,
that mission is retro-marked false-green in the outcomes fold and report —
a validation pass that a defect later disproved is recorded as such.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-324). The linkage
mechanism exists: the `traced-from-mission` ticket frontmatter field
(consumed by escalation_metrics.rs). The retro-mark is DERIVED at fold time
from the link — the mission's own event log is append-only and is never
rewritten (invariant). False-green rate joins the outcomes report; it is
also the honest denominator for any autonomy claim. Priority raised 2→1 on
2026-07-29: outcomes-comparison-metrics' defect-density slot depends on
this linkage (scored-gates addendum's rule — raise this rather than
duplicate it).

## Acceptance hints
- Fixture: completed mission + defect ticket carrying traced-from-mission →
  fold marks the mission false-green; removing the link unmarks it
  (derived, not stored).
- The mission's events.jsonl is byte-identical before/after (test).
- False-green rate appears in the outcomes report with the linking
  ticket(s) named.
- Anti-vacuity grep on a named filter unique to this work.
