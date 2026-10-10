---
state: open
title: Trajectory board 3: a Board lens listing everything waiting on the operator
priority: 3
schedule: once
blocked-by: [trajectory-board-2-fleet-model]
---

## Goal

Add a Board lens to the pipeline view. In this slice it shows the counts and a
complete list of everything waiting on the operator across the repository's
pipeline, including asks inside running missions, each with a link to the
surface that holds the act. It loads on demand, keeps itself fresh by polling
while visible, and never sends a mutation. Nothing is drawn on a canvas yet.

## Context

Design of record: `docs/scoping/trajectory-board.md`. Read it in full before
planning and do not contradict a decided or proposed section. This is slice 3,
and it implements D-A, the polling limits in D-B, and option 1 of D-C.

The lens. `Lens` and `LENSES` in `apps/dashboard/src/lib/lensFilter.ts` gain
`board`, labelled Board. `PipelineView` keeps its title bar, lens bar and Run
queue control; when the Board lens is selected it renders the board body in
place of the row list and of the two metrics panels below it.

Routes. `apps/dashboard/src/lib/routes.ts` parses `#/board` and
`#/r/<repo>/board` to the pipeline view with the Board lens selected, the way
it already treats the backlog route.

Loading. The board body is a separate component brought in with `React.lazy`
and `Suspense`, so its code and the fleet model from slice 2 are not in the
main chunk.

Data. While the body is mounted and the document is visible, refresh the
store's ticket and mission lists through `loadTickets` and `loadMissions` no
more often than every ten seconds, and fetch `api.missionState(id)` no more
often than every five seconds for each mission whose pipeline stage is
`running`. Fetch nothing while the document is hidden, and refresh at once
when it becomes visible again. Discard a mission's state when it leaves the
`running` stage and discard everything when the repository changes, following
the generation guards in `apps/dashboard/src/lib/store.ts`. When a fetch
fails, keep the last good data and say so quietly; the existing list error
banners already cover the two lists. Go through `api`, so the read-token
handling is inherited.

Body. A header with three counts from the fleet model (waiting on you, in
flight, landed), then the list of asks in the model's urgency order. Each
entry names the kind of ask, the work item, its title, the detail that makes
the ask concrete (the grant's kind and target, the question's text, the gate's
id), and one link to where the act lives. When nothing is waiting, say so in
one line. Use the dashboard's existing classes and tokens in
`apps/dashboard/src/styles.css`; add no colors.

Rules. Server-derived text is rendered as React text that has passed through
`visible()`. No `dangerouslySetInnerHTML`. No POST of any kind from the lens.
Wording follows D-G: the list uses the pipeline's stage names and decided
verbs.

Out of this slice: any canvas, any animation, any action button.

## Scoping answers

## Acceptance hints

- `cd apps/dashboard && npx vitest run src/components/BoardLens.test.tsx 2>&1 | grep -qE 'Tests +[1-9][0-9]* passed'` succeeds.
- `cd apps/dashboard && npx vitest run src/lib/routes.test.ts -t trajectory 2>&1 | grep -qE 'Tests +[1-9][0-9]* passed'` succeeds; the new route tests carry the word trajectory in their names because the existing describe block already contains the substring board.
- `cd apps/dashboard && npx vitest run src/lib/lensFilter.test.ts -t trajectory 2>&1 | grep -qE 'Tests +[1-9][0-9]* passed'` succeeds.
- A component test with `api` mocked renders fixtures holding one ask of every kind and asserts one list entry per ask, in urgency order, each with the href the fleet model gave it, and the three counts.
- A component test asserts the one-line empty state when nothing is waiting, and that a zero-width character in a title is shown in its escaped form.
- A fake-timer test asserts that over sixty seconds the two list loaders run at most seven times each, that `api.missionState` is called only for missions in the `running` stage, that no request is made while `document.hidden` is true, and that a `visibilitychange` back to visible triggers an immediate refresh.
- A test replaces `globalThis.fetch` with a spy, mounts the lens, runs the timers for sixty seconds, and asserts every request used the GET method.
- `cd apps/dashboard && npm run build && ! grep -l "trajectory-board-lens" dist/assets/index-*.js && grep -l "trajectory-board-lens" dist/assets/*.js` succeeds, where `trajectory-board-lens` is the class name on the lens body's root element.
- `cd apps/dashboard && npx tsc -b && npm run test && npm run sync-embedded && npm run check-embedded && npm run lint` all succeed, and the refreshed bundle under `crates/cli/assets/dashboard/dist` is committed.

## Operator notes

Confirm D-A and D-C in the design note before drafting. With live QA enabled
(docs/validating-uis.md), the validator can open the dev server on the board
route against a running serve and compare the list with the mission pages.
