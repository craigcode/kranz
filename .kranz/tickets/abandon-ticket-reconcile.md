---
state: done
state-note: "Done: abandon_mission flushes the event log, then calls work::reconcile_ticket_for_mission exactly as the run/drain paths do (Abandoned maps to ticket failed), warn-not-fail on reconcile error. Regression test mission_catalog::tests::abandon_reconcile_marks_the_linked_ticket_failed (filter abandon_reconcile_ matches only it); full workspace gates green."
title: kranz abandon must reconcile the linked ticket state
priority: 3
schedule: once
---

## Goal

`kranz run` and the queue drain call
`work::reconcile_ticket_for_mission` on terminal mission states (mapping
Abandoned/Failed → ticket `failed`, Blocked → `needs-context`), but
`kranz abandon` does not — a directly-abandoned mission leaves its ticket
stuck in `running` forever (observed 2026-08-08 on m-a5a8fd /
flight-rules-pack-contract; corrected by hand).

## Acceptance hints

- `kranz abandon` (CLI and REST abandon paths) reconciles the linked ticket
  exactly as the drain does, with the same warn-not-fail posture on
  reconcile error.
- Regression test: abandon a queued mission, ticket state becomes `failed`
  naming the mission.
- Anti-vacuity: filter `abandon_reconcile_` matches only the new test(s).
