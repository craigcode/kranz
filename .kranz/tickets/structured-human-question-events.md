---
state: done
state-note: Done: additive question.opened/answered/cleared + WorkerReport.questions + AnswerQuestion control; pending_questions projection replaying after restart; dashboard QuestionRequestPanel + Slack QuestionReady in the shared your-move chrome; prose fallback intact. 28 Rust + 5 vitest green; dashboard chain green; full gates green.
title: Structured human-question events for dashboard and Slack
priority: 3
schedule: once
---

## Goal
When an agent needs a human choice, represent that decision as a structured,
replayable pending-decision projection rendered by dashboard and Slack —
without creating a third competing human-input inbox beside grants,
NeedsContext, blocked+msg, and revision approve/reject.

## Context
Mission Control's AskUserQuestion overlay is a UX contract reference (caps,
options, free text), not a PTY key-injection port.

## D-X — unify channels (accepted for this ticket)
1. **Permission / deny-rule / allow-set / touch-path prompts** → existing
   `grant.requested` / approve / deny flow. Do **not** invent parallel
   question events for grants.
2. **Ticket underspecification** → existing NeedsContext on the ticket.
3. **Orchestrator/worker "ask the human" tool payloads** (structured choices)
   → additive mission events (`question.opened` / `question.answered` /
   `question.cleared` or equivalent) that feed **one** pending-decision
   projection consumed by dashboard + Slack. Answers submit through existing
   control paths (`msg` / dedicated control kind), not a new server.
4. **Milestone blocked prose** may *link* to a structured question when one
   is open; blocked alone remains valid for backends that only emit prose.

## Acceptance hints
- Additive event kinds only (`#[serde(default)]`); old logs still fold.
- Dashboard and Slack render at most one clear "your move" surface for
  grants vs questions (distinct kinds, shared projection chrome).
- Answers replay after restart from the event log.
- Question text/options/answers size-capped and scrubbed.
- Backends without structured ask tools keep working via prose guidance.
- Anti-vacuity grep on the named filter.
