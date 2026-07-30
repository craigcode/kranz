---
title: Flag mis-specified gates from their score distributions
priority: 2
schedule: once
blocked-by: [gate-confidence-score]
---

## Goal
Per gate, fold the score distribution across missions and flag two smells:
scores that never approach the threshold over a meaningful sample, and
near-constant scores regardless of input — surfaced on the outcomes report
beside block-to-grant timing, and fed into the escalation ledger.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-316; scored-gates
addendum). Complement to rubber-stamp-grant-flag, not an alternative —
present them together: block-to-grant timing catches an inattentive human;
this catches a mis-specified gate that passes everything because its
threshold is meaningless. A gate whose scores cluster far above threshold
and never approach it is either genuinely safe or measuring nothing, and
the verdict alone cannot distinguish the two. Pure-fold over gate events
(house pattern, no second persisted source of truth).

## Acceptance hints
- A minimum sample count gates assessment; below it, no flags (boundary
  tested).
- Gates that emit no score are excluded, never flagged.
- A flag names the gate and carries the distribution summary that
  triggered it.
- Flagged gates appear in the escalation ledger fold.
- Anti-vacuity grep on a named filter unique to this work.
