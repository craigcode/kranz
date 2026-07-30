---
title: Industry-comparison metrics beside the kranz-native outcomes
priority: 3
schedule: once
---

## Goal
The outcomes report gains a clearly-separated comparison set — assisted-
change share (proportion of merged changes with agent involvement), defect
density per unit of change, and defect resolution time — each stating its
definition inline. The kranz-native metrics (autonomy ratio, cost per
change, false-green rate, escalation ledger) stay primary.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-333; scored-gates
addendum). The point is contrast for an outside reader: the
industry-legible metrics are weak volume proxies, but their absence makes
the report unreadable rather than rigorous — and publishing both sets
shows that volume metrics can improve while false greens go unmeasured,
which makes the case better than either set alone. Definitions render
inline because these metrics are self-defined across the industry; the
definition is the whole argument. Defect density depends on
false-green-defect-linkage (priority raised for this dependency): while
unlanded, ship the other two and leave the slot visibly empty rather than
approximating it.

## Acceptance hints
- The report renders the two groups structurally distinct: native primary,
  comparison secondary.
- Each comparison metric carries its inline definition in the output
  (tested as content, not just presence).
- With linkage unlanded, the defect-density slot renders empty and names
  its dependency — never an approximation.
- Anti-vacuity grep on a named filter unique to this work.
