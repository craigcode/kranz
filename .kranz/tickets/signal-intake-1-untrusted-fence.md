---
state: done
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

- Use a per-instance nonced sibling marker for ticket/signal evidence: `<<<BEGIN KRANZ UNTRUSTED SIGNAL EVIDENCE [<nonce>]>>>` and `<<<END KRANZ UNTRUSTED SIGNAL EVIDENCE [<nonce>]>>>` with a fresh 128-bit CSPRNG hex nonce per envelope, plus pre-scrubbing any literal `<<<BEGIN KRANZ UNTRUSTED` or `<<<END KRANZ UNTRUSTED` substrings from the body before wrapping so an excerpt can never close or spoof a fence.
- `Ticket::parse_sections` must ignore `## ` headings inside `<<<BEGIN KRANZ UNTRUSTED ...>>>` .. `<<<END KRANZ UNTRUSTED ...>>>` blocks so untrusted excerpts cannot inject `## Goal`, `## Review artifact`, or other ticket sections.
- Ticket notes appended by `ticket_notes::draft_context` are operator-authored and out of scope.

## Acceptance hints

- A trigger ticket's excerpt appears only inside the nonced `<<<BEGIN KRANZ UNTRUSTED SIGNAL EVIDENCE [<nonce>]>>>` .. `<<<END KRANZ UNTRUSTED SIGNAL EVIDENCE [<nonce>]>>>` fence, and the drafter's seed carries the warning.
- An excerpt containing literal `<<<BEGIN KRANZ UNTRUSTED` / `<<<END KRANZ UNTRUSTED` markers or `## Goal` / `## Review artifact` headings cannot close the fence early or inject parsed ticket sections (test).
- Existing trigger tests pass; the ticket's title, goal and provenance lines are unchanged.
