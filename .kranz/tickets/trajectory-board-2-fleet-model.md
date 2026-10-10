---
state: open
title: Trajectory board 2: fleet model and one definition of a pending ask
priority: 3
schedule: once
blocked-by: [trajectory-board-1-flight-plan]
---

## Goal

Add the trajectory board's fleet model to the dashboard as a pure, typed,
tested module. Given the pipeline's rows and the folded state of each live
mission, it yields one craft per work item with its place and label, every ask
that is waiting on the operator with a link to the surface that holds the act,
the counts, and the stage changes worth animating since the previous model.
Put the conditions that decide whether an ask is pending into one module and
make the existing panels use it, with no change to what they show. No
rendering of the board, no network, no new dependency.

## Context

Design of record: `docs/scoping/trajectory-board.md`. Read it in full before
planning and do not contradict a decided or proposed section. This is slice 2.
The tables under "The board" and the rules under D-B and D-D are the
specification for this module.

Inputs: the same `TicketSummary[]` and `MissionSummary[]` the pipeline view
uses, a map from mission id to `MissionState` that may be missing entries, the
current time in milliseconds, and the previous model or null.

Rows and stages. `buildRows` and its `Row` type live inside
`apps/dashboard/src/components/PipelineView.tsx` today. Move them to a module
under `apps/dashboard/src/lib` and import them from both places, as a pure
refactor. A craft's stage comes only from `pipelineStage()`.

Pending asks. Create `apps/dashboard/src/lib/pendingAsks.ts` holding the six
in-flight conditions from the design note's table, each exactly as the panel
that renders it computes it today: `GrantRequestPanel`,
`PermissionRequestPanel`, `GateReviewPanel`, `RevisionPanel`,
`QuestionRequestPanel`, and the blocked status that `StatusStrip` shows. Then
make those panels take their condition from the new module. Their rendering,
their actions and their existing tests do not change.

Stage asks are `needs-you`, `wrong-plan`, `reviewable`, `delivered` and
`failed`. `captured` and `queued` never produce an ask.

Where each ask's act lives, using the helpers in
`apps/dashboard/src/lib/routes.ts`: an in-flight ask and `delivered` link to
the mission page; `needs-you` and `wrong-plan` link to the ticket page;
`reviewable` links to the pipeline's Actionable lens when the work has a
ticket, because that row shows the plan, the estimate and Queue, and to the
mission page when it has none; `failed` links to the ticket page when there is
a ticket and to the mission page otherwise.

Urgency order for the list: one-call permission, grant, blocked, gate review,
plan revision, question, then `needs-you`, `wrong-plan`, `reviewable`,
`delivered`, `failed`. Within a kind, oldest mission first.

Places and capacity, exactly as the design note states them: four pads with
ringed work first and the rest counted in the hangar, five orbit lanes shared
at different angles beyond five craft, twelve landing sites holding the most
recent landings, and the three most recently created failed items afloat at
the splashdown point. The list of asks is never truncated by capacity.

Time. The model emits, for each craft, a value of the craft interface exported
by slice 1. On first sight of a work item, choose timestamps that put it in
the steady state of its stage, so nothing animates. When a work item moves by
exactly one step along the route between two consecutive models (ground to
running, running to delivered, delivered to landed, running to failed, or any
stage to abandoned), stamp that change with the current time and report it as
a transition. Any other change snaps to the steady state of the new stage and
reports nothing.

Text. Every string that came from the server and ends up in a label or a list
entry passes through `visible()` from `apps/dashboard/src/lib/presentation.ts`.

Constraints: the fleet model imports nothing from `api.ts` or `store.ts`
except types. Do not change `pipelineStage.ts` or `lensFilter.ts`. Do not add
a dependency. Do not edit the reference prototype;
`docs/scoping/trajectory-board-prototype/src/fold.js` is useful for its pad,
lane and site allocation, but its event fold is not to be ported.

## Scoping answers

## Acceptance hints

- `cd apps/dashboard && npx vitest run src/lib/board/fleet.test.ts 2>&1 | grep -qE 'Tests +[1-9][0-9]* passed'` succeeds.
- `cd apps/dashboard && npx vitest run src/lib/pendingAsks.test.ts 2>&1 | grep -qE 'Tests +[1-9][0-9]* passed'` succeeds.
- A table-driven test covers all eleven pipeline stages, for ticket-backed and ticketless rows, and asserts the place and the ring state given in the design note's table.
- A test asserts that `captured` and `queued` items never carry an ask, that a running mission carries an ask exactly when `pendingAsks` returns one, and that the flattened list follows the urgency order above.
- For each of the six in-flight kinds, fixtures for present, resolved, closed and expired records show `pendingAsks` agreeing with what the corresponding panel renders, and every pre-existing panel test file passes unmodified.
- A test asserts each ask's link equals the `routes.ts` helper named above for its kind, for a repo-scoped and an unscoped hash.
- Capacity tests: six ground items of which five are ringed put ringed items on all four pads and report the rest in the hangar, including how many of those are ringed; seven running craft use lane indexes 0 to 4 only and no two share both a lane and an angle slot; fifteen landed items leave exactly the twelve most recently created with a site; five failed items leave exactly the three most recently created afloat.
- A test builds a model with no previous model and asserts it reports no transitions and that, through slice 1's `poseAt` at the current time, each craft is in the phase its stage's steady state requires.
- Tests assert that each single-step change is reported once with the current time, that a change of more than one step and a first sighting report nothing, and that building twice from identical inputs reports nothing and yields deeply equal craft.
- A test with a zero-width character in a ticket title and in a grant target asserts the label and the list entry contain the escaped form produced by `visible()`.
- `cd apps/dashboard && npx tsc -b && npm run test && npm run build && npm run sync-embedded && npm run check-embedded && npm run lint` all succeed, and the refreshed bundle under `crates/cli/assets/dashboard/dist` is committed.

## Operator notes

Confirm D-B and D-D in the design note before drafting; this slice encodes
both. The refactor of the five ask panels is the only part that touches
shipped behavior, so it is the part to read closely at review.
