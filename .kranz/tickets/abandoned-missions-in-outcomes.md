---
title: Show operator interventions in the mission report, and count missions that were abandoned and finished by hand
priority: 2
schedule: once
---

## Goal

A mission's own report says how many times the operator had to step in. The outcomes fold also shows missions that were abandoned after delivering work, so the missions that needed the most human effort are visible and not left out.

## Context

`crates/engine/src/escalation_metrics.rs` already defines an operator intervention (grant decisions, steers after approval, revision rulings, operator unblocks) and folds the autonomy ratio from it. `kranz outcomes` reports that ratio, cost per change and cycle time per task class. Two gaps remain.

First, the per-mission report (`report_render.rs`) shows elapsed time and cost and says nothing about interventions. An operator reading one report cannot tell a clean run from one that needed five steers.

Second, an ABANDONED mission is outside the autonomy denominator by design: the operator retired it, so it is neither a completion nor an honest failure. Missions m-caf631 and m-882080 (2026-11-19) each delivered every feature, took several operator messages and unblocks, were abandoned for harness reasons, and had their branches merged by hand as pull requests. They appear in no outcome figure. The cases with the most human effort are the ones the fold cannot see.

## Scoping answers

- Keep the autonomy ratio's definition and denominator unchanged. `docs/metrics.md` publishes it with reproducible numbers.
- The mission report gains an interventions line: the count, split by the existing classes. Reuse the intervention set in `escalation_metrics.rs`; do not define a second one.
- The outcomes fold gains a separate count of abandoned missions, with their intervention counts and whether any feature had completed before the abandon. It sits beside the autonomy ratio and does not feed it.
- Flag as a D-X operator decision whether an abandoned mission can be linked to the change that finished its work (for example a field on `kranz abandon` naming a branch or pull request), or whether the count alone is enough.
- Everything is a fold over existing events. `events.rs` and `types.rs` are additive-only; a new optional field on the abandon event, if the D-X answer needs one, takes `#[serde(default)]`.
- Update `docs/metrics.md` if a published definition gains a companion figure.

## Acceptance hints

- A test: a mission log with two steers and one grant decision renders a report whose interventions line shows three, split by class.
- A test: a mission with no interventions renders zero, not a missing line.
- A test: the outcomes fold over logs holding one completed and one abandoned mission leaves the autonomy ratio as it is today and reports the abandoned mission in the new count.
- Anti-vacuity: the test filter matches only the new tests.
