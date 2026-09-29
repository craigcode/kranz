---
state: done
state-note: Done: docs/metrics.md — four metrics defined with reproduce-commands and dogfood numbers (89.8%% autonomy, 0 sub-10s grants, 0 false greens); linked from the-trust-interval.md.
title: "Publish the trust-metric vocabulary (docs/metrics.md) with dogfood receipts"
priority: 1
schedule: once
---

# Publish the trust-metric vocabulary

These metrics describe mission authorization and outcomes. Publish their
definitions alongside reproducible examples from the event log.

## Problem

The flight-surgeon console (fc93a23) computes the metrics — autonomy ratio,
rubber-stamp block-to-grant timing, false greens, the escalation ledger —
but the definitions live in `escalation_metrics.rs` and a dashboard panel.
They are not a published, citable vocabulary with worked examples.

## Design

`docs/metrics.md`, in the the-trust-interval.md register:

1. **Autonomy ratio** — share of closed missions with zero operator
   interventions; definition, denominator choice (closed missions, and why
   abandoned is excluded), and the dogfood number from this repo's log.
2. **Block-to-grant timing / rubber-stamp signal** — p50/p90 grant latency
   and the sub-ten-second share; why a wall of fast grants is the
   tight-boundary failure made visible (the habituation failure Boris
   Cherny described removing the human over).
3. **False green** — a mission that closed clean and later had a defect
   traced back to it; the defect→mission linkage data entry.
4. **Escalation ledger** — every block/grant with what the worker proposed
   and what the operator decided; also the fine-tune corpus
   (training-corpus-export).
5. A worked example of each computed from this repo's own event log (the
   receipts), plus a paragraph explaining how the metrics help an operator
   inspect authorization decisions and their outcomes.

## Test gate

- `docs/metrics.md` exists; every number in it is reproducible by running
  `kranz escalation-metrics --json` (and `kranz outcomes` where cited) on
  this repo — the doc states the command beside each figure.
- the-trust-interval.md links to it.

## Out of scope

New metric computation (all four exist), dashboard changes, the LinkedIn
post itself (operator's voice, operator's account).
