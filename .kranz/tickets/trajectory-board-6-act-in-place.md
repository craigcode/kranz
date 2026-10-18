---
state: open
title: Trajectory board 6: answer an ask beside the board by reusing its owner
priority: 3
schedule: once
blocked-by: [trajectory-board-3-lens]
---

## Goal

Let the operator answer an ask without leaving the Board lens, by showing
beside the board the component that already owns that act, with everything it
already shows. Add no new call site for any POST and no second implementation
of any action.

## Context

Design of record: `docs/scoping/trajectory-board.md`. Read it in full before
planning and do not contradict a decided or proposed section. This is slice 6
and it implements option 2 of D-C. It may be drafted only after the operator
has decided D-C and said which acts may be reached in place. As written, this
ticket covers all of them; strike the ones the decision excludes.

It works from the list alone, so it does not depend on the plot. If slice 4
has landed, selecting a craft on the plot selects its list entry too.

In-flight asks. Selecting one connects that mission through the store's
`connectMission` and renders, in a column beside the board, the existing
`GrantRequestPanel`, `PermissionRequestPanel`, `GateReviewPanel`,
`RevisionPanel` and `QuestionRequestPanel`. They already read the connected
mission's state and show nothing when their ask is absent. The pipeline route
keeps a connection open by design (see the comment on `connectMission` in
`apps/dashboard/src/App.tsx`).

A reviewable ticket. The pipeline row for it shows the plan, the estimate and
Queue. That row is built inside `PipelineView.tsx` today. Extract it into an
exported component as a pure refactor and render the same component beside
the board for the selected item.

A delivered mission. Render the existing `DeliveredPanel` for the selected
mission: report, diff stat and the gated Merge.

Everything else stays a link, as in slice 3: `needs-you`, `wrong-plan`,
`failed`, and a reviewable mission with no ticket are full pages of their own.

After an act completes, refresh the lists and the selected mission's state at
once instead of waiting for the next poll.

Rules. The lens's own files call no mutating method of `api` and never call
`postJson`; every mutation in this slice happens inside a component that
existed before it. Wording and verbs are unchanged, per D-G and pipeline-view
D-A. Text shown by the reused components is theirs and is not re-rendered by
the lens.

## Scoping answers

## Acceptance hints

- `cd apps/dashboard && npx vitest run src/components/BoardLens.test.tsx -t "trajectory act" 2>&1 | grep -qE 'Tests +[1-9][0-9]* passed'` succeeds.
- Tests select a list entry of each in-flight kind and assert that `connectMission` was called with that mission's id and that the owning panel's own heading is present beside the board.
- A test selects a reviewable ticket and asserts the plan, the estimate line and the Queue for run button are present, and that pressing it calls the store's `approveTicket` with the ticket's slug.
- A test selects a delivered mission and asserts the report, the diff stat and the Merge control from `DeliveredPanel` are present.
- A test asserts that entries for `needs-you`, `wrong-plan`, `failed` and a ticketless reviewable mission are still plain links with the hrefs from slice 3.
- Every pre-existing test in `src/components/PipelineView.test.tsx` passes unmodified after the row is extracted.
- A test reads the lens's own non-test source files (`src/components/Board*.tsx` and `src/lib/board/*.ts`) and asserts that none mentions `postJson` and that the only member of `api` any of them references is `missionState`.
- `cd apps/dashboard && npx tsc -b && npm run test && npm run build && npm run sync-embedded && npm run check-embedded && npm run lint` all succeed, and the refreshed bundle under `crates/cli/assets/dashboard/dist` is committed.
- With live QA enabled: against a serve holding a mission parked on a grant, selecting it on the board route shows the grant panel beside the board with the same target the mission page shows.

## Operator notes

Decide D-C first. The question to settle is whether plan queueing and merge
may be reached from beside the board at all, or only the bounded asks (grant,
one-call permission, question), which is the line the glasses client draws.
