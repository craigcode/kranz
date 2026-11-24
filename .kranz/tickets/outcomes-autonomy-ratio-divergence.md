---
title: kranz outcomes autonomy ratio diverges from the published definition
priority: 3
schedule: once
---

## Goal

`crates/engine/src/outcomes.rs` computes an `AutonomyRatio` that disagrees,
in three ways, with the definition `docs/metrics.md` publishes and that
`crates/engine/src/escalation_metrics.rs` implements. This ticket records
the divergence as an operator decision: whether to align `kranz outcomes`
with the published definition. Aligning it would change its numbers, so it
is not done as a drive-by fix.

## Context

- `crates/engine/src/outcomes.rs`: `AutonomyRatio`'s `is_closed` counts
  ABANDONED as closed (the published ratio excludes it), omits operator
  unblocks from its intervention count, and classifies steers by
  timestamp.
- `crates/engine/src/escalation_metrics.rs`: the ratio `docs/metrics.md`
  publishes excludes abandoned missions, counts operator unblocks as
  interventions, and classifies steers by seq.

Three independent divergences, same underlying metric, two different
answers depending on which command you run.

## D-X options

- Leave `kranz outcomes`'s `AutonomyRatio` as-is; treat it as a distinct,
  unpublished figure.
- Align `kranz outcomes` with `escalation_metrics.rs` (and therefore
  `docs/metrics.md`): exclude ABANDONED from `is_closed`, count operator
  unblocks, classify steers by seq. This changes `kranz outcomes`'s
  reported numbers.

## Acceptance hints

- This ticket records the decision; it does not change either file's
  behavior.
- A follow-up feature, once the operator decides, would need a regression
  test comparing the two folds over the same log.
