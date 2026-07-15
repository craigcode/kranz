---
title: Structured human-question events for dashboard and Slack
priority: 2
schedule: once
---

## Goal
Represent agent questions as first-class, structured kranz events instead of
only prose in transcripts or Slack text. When an agent needs a human choice,
the question, options, multi-select flag, free-text affordance, and answer
should round-trip through dashboard and Slack as mission events that can be
replayed, audited, and resumed.

## Context
Mission Control's `AskUserQuestion` overlay is a useful UX reference: it
defensively parses the tool payload, caps question and option sizes, renders a
native choice UI, and records whether the user answered with options, free
text, or "chat about this". Kranz should borrow the contract, not the PTY key
injection.

Kranz already has the product rule that blocked work must converge to a human.
Structured questions make that rule more precise: a blocker can name exactly
what decision is needed, Slack can render buttons or choices, and the dashboard
can show the pending decision without scraping prose.

## Acceptance hints
- New additive event types record `question.opened`, `question.answered`, and
  `question.cleared` or equivalent without breaking old logs.
- Dashboard and Slack both render pending structured questions and can submit
  answers back through existing control paths.
- Answers are replayable from the event log after a server restart.
- Question text, headers, options, and free-text answers are size-capped and
  scrubbed before persistence.
- Existing prose guidance still works for backends that cannot emit structured
  questions.
