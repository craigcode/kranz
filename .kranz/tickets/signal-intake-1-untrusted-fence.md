---
state: open
title: Signal intake 1: fence external text in ticket seeds
priority: 2
schedule: once
---

## Goal

Wrap externally sourced text that reaches a drafter or worker through a ticket in an explicit untrusted-data fence, starting with the GitHub webhook trigger's excerpts. The text stays readable as evidence and is never presented as instructions.

## Context

Design of record: `docs/scoping/signal-intake.md`. Read it in full before planning and do not contradict a decided or proposed section. Slice 1, independent of the rest.

`trigger_ticket_template` (`crates/engine/src/hooks.rs`) writes the bounded, scrubbed CI or comment excerpt into the ticket's Context as plain text, and the whole ticket becomes the drafter's seed (`draft.rs`). The established fence is the validator's `<<<BEGIN/END KRANZ UNTRUSTED RUNTIME EVIDENCE>>>` block with its warning (`runner.rs`).

## Scoping answers

- Reuse the existing fence wording and markers, or a sibling marker for ticket evidence; do not invent a second convention.
- A fence inside the excerpt must not close the block early: neutralise marker text in the excerpt.
- Ticket notes appended by `ticket_notes::draft_context` are operator-authored and out of scope.

## Acceptance hints

- A trigger ticket's excerpt appears only inside the fence, and the drafter's seed carries the warning.
- An excerpt containing the closing marker cannot end the fence (test).
- Existing trigger tests pass; the ticket's title, goal and provenance lines are unchanged.
