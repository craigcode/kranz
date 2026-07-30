---
title: Divergence between sibling streams as a first-class event
priority: 2
schedule: once
blocked-by: [heterogeneous-dispatch-pool]
---

## Goal
When sibling streams for one unit disagree, append a divergence event
carrying the disagreement, references to the candidate diffs, and — when it
arrives — the resolution (which candidate, why, decided by whom or by which
gate).

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-304). Design rule to
carry verbatim: **agreement between models is a signal to log, never a
criterion to trust** — a unit is done when gates are green and no
escalation is open, not when streams stop disagreeing. events.rs is
additive-only (contract file discipline). Divergence records feed the
escalation ledger (outcomes.rs fold) and the training-corpus export; the
resolution reference must survive provenance replay.

## Acceptance hints
- A fixture with two divergent candidate diffs produces a divergence event
  referencing both, and a later resolution event naming the decider.
- The outcomes fold surfaces divergence count and resolution kind per
  mission.
- Identical candidates produce an agreement record (logged, not trusted):
  no gate is skipped because streams agreed (test).
- Old logs without divergence events fold cleanly.
- Anti-vacuity grep on a named filter unique to this work.
