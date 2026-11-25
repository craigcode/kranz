---
state: done
state-note: "Mission m-87118c. `kranz outcomes` now computes its autonomy ratio by the published definition, and a test folds one log through both `outcomes.rs` and `escalation_metrics.rs`. The per-repo share in `kranz outcomes --all` still counts abandoned missions in its denominator."
title: kranz outcomes autonomy ratio diverges from the published definition
priority: 3
schedule: once
---

## Goal

`crates/engine/src/outcomes.rs` computes an `AutonomyRatio` that disagrees,
in three ways, with the definition `docs/metrics.md` publishes and that
`crates/engine/src/escalation_metrics.rs` implements. The operator decided
on 2026-11-24 to align `kranz outcomes` with the published definition. This
changes the numbers `kranz outcomes` reports, so the change says so in the
changelog.

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

## D-X options (DECIDED 2026-11-24: align)

The operator chose the second option. The first is kept for the record.

- Leave `kranz outcomes`'s `AutonomyRatio` as-is; treat it as a distinct,
  unpublished figure.
- Align `kranz outcomes` with `escalation_metrics.rs` (and therefore
  `docs/metrics.md`): exclude ABANDONED from `is_closed`, count operator
  unblocks, classify steers by seq. This changes `kranz outcomes`'s
  reported numbers.

## Acceptance hints

- `kranz outcomes` excludes abandoned missions from closed missions, counts
  operator unblocks as interventions and classifies steers by seq. Prefer
  calling the shared `mission_interventions` rule over a second copy of it.
- A regression test folds one log through both `outcomes.rs` and
  `escalation_metrics.rs` and asserts the two autonomy ratios agree. The log
  includes an abandoned mission, an operator unblock and a steer whose
  timestamp and seq order differ.
- The existing test `abandoned_missions_fold_outcomes_ratio_unchanged` pins
  the old behaviour (closed 2, share 0.5). Update it to the aligned numbers
  and say so in the pull request.
- The `.abandoned` section is unchanged.
- The changelog entry states that `kranz outcomes` autonomy figures change
  and why. `docs/metrics.md` needs no new definition.
- Anti-vacuity: the test filter matches only the new test.
