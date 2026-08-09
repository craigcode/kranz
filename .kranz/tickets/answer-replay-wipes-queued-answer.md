---
title: Duplicate answer-question drain wipes a successfully queued answer
priority: 1
schedule: once
---

## Goal
Fix the ControlCommand::AnswerQuestion error path in orchestrator.rs (~2727-2732): it emits an OrchestratorDecision even when the answer was already durably queued (question.answered appended) and only a post-write step failed. The OrchestratorDecision fold clears pending_user_messages, so a crash-replayed duplicate control file (same or next drain batch) clears the just-queued answer before the consult consumes it — the durable question.answered remains but the mission never sees it. The success path already skips the decision event for exactly this reason; the error path must not clear the queue either. Found by the 14th-pass review (commit 226ad4e).

## Context

The review notes the test near orchestrator.rs:10150 currently documents the
wipe as acceptable — that expectation gets reversed by this fix. The FYI
companion fact (structured questions deliberately do not park; the UI
"pending decision" affordance can race completion) is the D-X design and is
NOT part of this ticket.

## Scoping answers

## Acceptance hints

- Narrating an ignored/failed answer-application must not clear
  pending_user_messages: either no OrchestratorDecision on that path or a
  decision variant whose fold leaves the queue intact.
- Regression test: a duplicate AnswerQuestion control file drained after the
  answer was queued leaves pending_user_messages non-empty and the consult
  still consumes the answer.
- The genuine error case (answer for an unknown/closed question) still
  narrates and still clears nothing that was legitimately queued.
- Anti-vacuity: test-name filters must match only the new tests.
