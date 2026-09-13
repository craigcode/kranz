---
title: Make OpenSpec a first-class authoring front end for kranz tickets
priority: 2
schedule: once
state: open
---

## Goal

Let an operator author work in OpenSpec and execute it in kranz without
hand-copying, by importing `openspec/changes/<name>/` into a kranz ticket
that carries the spec's intent and refuses to fake its acceptance criteria.

## Context

OpenSpec (github.com/Fission-AI/OpenSpec) is a spec-driven convention for AI
coding assistants. Each change is a folder holding `proposal.md` (rationale
and scope), `specs/` (requirements as SHALL-style scenarios), `design.md`
(technical approach), and `tasks.md` (an implementation checklist), moving
through explore, propose, apply, archive. Its README states plainly that it
avoids rigid phase gates and does not enforce a workflow.

That non-goal is exactly kranz's goal, which is why the two compose instead
of competing. OpenSpec produces intent. Kranz makes intent binding: approval
pins the base SHA, a worker implements against it with no memory of the
conversation, and a separate validator session re-runs the named commands.
`docs/tickets.md` already carries `task-class: spec-review` with
`review-artifact`, which consumes exactly this shape of tracked document, so
the zero-code path exists today.

## Acceptance hints

- `kranz ticket import-openspec <change-dir>` writes `.kranz/tickets/<slug>.md`
  and a run of the CLI test filter for it reports
  `result: ok\. [1-9][0-9]* passed`.
- `proposal.md` becomes `## Goal`; `design.md` and the spec scenarios become
  `## Context`.
- `tasks.md` is NOT imported, and a test asserts its content is absent from
  the generated ticket.
- No scenario text reaches `## Acceptance hints`. A test asserts the generated
  hints section names no SHALL sentence and instead states that executable
  criteria are still required.
- Importing a directory that is missing `proposal.md` fails closed with the
  path it looked for, and a test covers it.
- Re-importing the same change without `--force` refuses rather than
  overwriting operator edits to the ticket.

## Design notes

Two mappings carry the whole risk.

**`tasks.md` is dropped on purpose.** It is the assistant's own
decomposition, self-reported and ungraded. Importing it would smuggle an
unvalidated plan past the orchestrator, which is the one step that should be
doing that thinking.

**Scenarios must not become acceptance criteria.** `docs/tickets.md` requires
acceptance hints to be concrete and testable with a passed-count guard,
because a bare test-name filter exits 0 on zero matches. "The app SHALL
default to the system preference" cannot fail. Copying scenarios into hints
would ship vacuous assertions on every imported mission, which is the defect
class the positioning ADR calls the wedge. Carry them as intent, and let the
orchestrator propose executable commands or return NEEDS-CONTEXT.

One direction only. `openspec/changes/` explains why the work exists; the
approved plan is what the validator judges. Syncing back would create two
sources of truth that drift the moment someone edits a spec mid-mission.
