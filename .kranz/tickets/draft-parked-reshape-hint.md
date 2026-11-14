---
title: Correct the reshape hint printed for a parked draft
priority: 3
schedule: once
---

## Goal

`kranz draft <slug>` parks a plan for review and prints a hint that says `kranz plan --mission <id>` reshapes it. That command refuses the mission. Make the hint name a route that works, or make the route work.

## Context

Found on 2026-11-13. A parked draft is a mission whose status is already approved, with its ticket in REVIEW. `kranz plan --mission <id>` only accepts a mission that is still in planning and answers that the mission is approved and should be run. `kranz revise` extends the touch set and command grants of an active mission and cannot change an existing assertion.

The route that does work today is to record the operator's answers in the ticket, abandon the parked draft and draft again. The hint does not mention it.

The hint is printed in `crates/cli/src/backlog.rs` for `DraftOutcome::ParkedForReview`.

## Scoping answers

- Whether a parked draft should be reshapable in place is an operator decision (flag it as a D-X decision). The smaller change is to correct the text.
- If the text is corrected, it names the ticket route: add the answers to the ticket, `kranz abandon <id>`, `kranz draft <slug>`.
- Check the dashboard, Slack and `docs/tickets.md` for the same wording.
- Contract files change additively only.

## Acceptance hints

- The hint printed for a parked draft names only commands that accept a parked draft.
- A test runs the named command against a parked draft and it is not refused, or asserts the printed text matches the documented route.
- `docs/tickets.md` describes how to reshape a parked draft.
