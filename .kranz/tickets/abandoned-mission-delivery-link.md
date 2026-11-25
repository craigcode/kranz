---
state: done
state-note: "Decided D-X1 on 2026-11-24: count only, which is what already ships. No link is recorded. Reopen with a scoping pass if the link is wanted later."
title: Link an abandoned mission to the change that finished its work
priority: 3
schedule: once
---

## Goal

An abandoned mission that nonetheless delivered every feature (its branch
merged by hand as a pull request) currently shows up only as a count. The
operator decision this ticket records, D-X, is whether — and how — to link
such a mission to the change that actually finished its work.

## Context

`crates/engine/src/escalation_metrics.rs` and `crates/engine/src/outcomes.rs`
now report a separate `.abandoned` count beside the autonomy ratio, with
each abandoned mission's interventions by class and whether a feature had
completed before the abandon. That tells an operator *that* human effort
was spent and salvaged, but not *where* the resulting change landed.

## D-X options (DECIDED 2026-11-24: D-X1)

- **D-X1 — count only (shipped default).** Keep reporting the abandoned
  count and its per-mission detail with no link to the delivering change.
  Simplest, and what ships today.
- **D-X2 — optional field on `mission.abandoned`.** Add an optional
  `#[serde(default)]` field recorded via `kranz abandon --delivered-by
  <branch|PR>`. Forward-only: an already-terminal mission cannot be
  re-abandoned to backfill the field after the fact. About 14
  `MissionAbandoned { .. }` struct literals across engine/cli/server would
  need a sweep to stay exhaustive — that sweep is its own feature, not a
  side effect of this ticket.
- **D-X3 — derive at fold time from `merged.rs` ancestry.** Walk git
  history to find the commit that merged the mission's branch, instead of
  recording anything at abandon time. Reads as unknown once the branch is
  deleted, as already observed for m-caf631 and m-882080.

## Acceptance hints

- This ticket records the decision; it does not implement D-X2 or D-X3
  without a follow-up scoping pass naming the chosen option.
- Anti-vacuity: no test is required until an option other than D-X1 is
  chosen.
