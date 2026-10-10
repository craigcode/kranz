---
state: open
title: Trajectory board 4: the 2D plot and a scripted demo route
priority: 3
schedule: once
blocked-by: [trajectory-board-1-flight-plan, trajectory-board-3-lens]
---

## Goal

Draw the board inside the Board lens on a 2D canvas: the figure-8 route, the
Earth and the Moon, one marker per craft at the pose the flight plan gives it,
trails, a red ring on everything that is waiting, labels, and flags for landed
work. Animate only the stage changes the fleet model reports. Add a demo route
that plays a fixed fleet through every stage without contacting the server.
No new dependency.

## Context

Design of record: `docs/scoping/trajectory-board.md`. Read it in full before
planning and do not contradict a decided or proposed section. This is slice 4.
It ends with a complete board that needs nothing from D-E or D-F.

Sources to port, both under `docs/scoping/trajectory-board-prototype/src/`:

- `chart.js` is the 2D layer: the route built once into cached layers and
  reused, the halos, trails, the steady and pulsing ring, the selection ring,
  the leaders from craft to labels, and the stand-ins it draws when WebGL is
  missing (shaded discs for the two bodies, a triangle per craft pointing
  along its nose, flags). In this slice the stand-ins are the board.
- `ui.js` holds the label placement (`placeChips` and the obstacle boxes), the
  choice between the wide and tall layouts from the plot's aspect ratio, the
  fit of the board's bounds into the plot, and the frame loop.

`docs/scoping/trajectory-board-prototype/README.md` has a picture of the
result and says how to run the reference.

Do not port from `ui.js`: its clock and replay bar, its decision buttons, its
flight log, or any use of `innerHTML`. The lens already has its list.

Feeding it. Each frame takes the craft from the fleet model, computes
`timeline` and `poseAt` from slice 1 at the current time, and draws. Trails
and the dotted course ahead come from `trail`. Flags come from the model's
landing sites.

Labels are React elements positioned over the canvas, one per drawn craft: the
work item's name, and in the hold color the phrase for its most urgent ask.
D-G allows flight-controller phrases here: GO FOR LAUNCH? for `reviewable`,
GO FOR LANDING? for `delivered`, and short plain words for the rest. Text goes
through `visible()`. Selecting a craft or its label highlights its entries in
the list, and selecting a list entry highlights its craft.

Motion. Run the frame loop only while the document is visible. With
`prefers-reduced-motion`, draw once per model change and drop the pulse.

Color. The plot keeps the reference's dark chart palette, declared as custom
properties scoped to the lens root in `apps/dashboard/src/styles.css`; take
the values from the light scheme in the reference's `template.html`. The lens
header and list keep the dashboard's own tokens.

No canvas. In jsdom, and anywhere `getContext('2d')` returns null, the lens
shows its header and list and nothing else, without throwing.

Demo. `#/board/demo` and `#/r/<repo>/board/demo` drive the same lens from a
deterministic script instead of the store: a fixed fleet that passes through
every pipeline stage and raises every kind of ask, including one failure that
flies the free return and one abandonment, then repeats. It makes no network
request and shows the word DEMO in the lens header for as long as it runs.
`docs/scoping/trajectory-board-prototype/src/sim.js` has a scripted backstory
to borrow from, but the script here produces successive fleet-model inputs,
not events.

Constraints: no new dependency, no POST, no change to slices 1 and 2 beyond
what a discovered defect requires, and the board's code stays in the lazily
loaded chunk.

## Scoping answers

## Acceptance hints

- `cd apps/dashboard && npx vitest run src/lib/board/plot.test.ts 2>&1 | grep -qE 'Tests +[1-9][0-9]* passed'` succeeds.
- `cd apps/dashboard && npx vitest run src/lib/board/demo.test.ts 2>&1 | grep -qE 'Tests +[1-9][0-9]* passed'` succeeds.
- `cd apps/dashboard && npx vitest run src/components/BoardPlot.test.tsx 2>&1 | grep -qE 'Tests +[1-9][0-9]* passed'` succeeds.
- Drawing is tested against a recording stand-in for the 2D context: for a fixture fleet, the number of ring strokes in the hold color equals the number of drawn craft that are waiting, nothing is drawn for a craft whose pose is null, trails are drawn only for craft in a moving phase, and one flag is drawn per landing site in the model.
- A test draws two frames at the same size and layout and asserts the route layers were built once, then changes the size and asserts they were rebuilt.
- Hit testing is a pure function from a point and the drawn craft to a craft id or null, with tests for a hit, a miss, and two overlapping craft resolving to the nearer one.
- Label placement is a pure function with tests asserting that no two placed labels overlap and that none overlaps an obstacle box, for the wide and the tall layout.
- The demo test steps the script through one full loop and asserts that every one of the eleven pipeline stages and every ask kind occurs at least once, that it contains a failure and an abandonment, that two runs produce deeply equal sequences, and that a `fetch` spy saw no request.
- A component test under jsdom, where the canvas has no context, asserts the lens renders its header and list and logs no error; another asserts the header shows DEMO on the demo route and not on the board route.
- `cd apps/dashboard && npx vitest run src/lib/routes.test.ts -t "trajectory demo" 2>&1 | grep -qE 'Tests +[1-9][0-9]* passed'` succeeds.
- The mission's diff leaves `apps/dashboard/package.json` and `apps/dashboard/package-lock.json` untouched: no dependency was added.
- `cd apps/dashboard && npm run build && ! grep -l "trajectory-board-plot" dist/assets/index-*.js && grep -l "trajectory-board-plot" dist/assets/*.js` succeeds, where `trajectory-board-plot` is the class name on the canvas element.
- `cd apps/dashboard && npx tsc -b && npm run test && npm run sync-embedded && npm run check-embedded && npm run lint` all succeed, and the refreshed bundle under `crates/cli/assets/dashboard/dist` is committed.
- With live QA enabled: on the dev server, `#/board/demo` shows the word DEMO, three non-zero counts, at least one label in the hold color, and a list whose entries match the ringed labels; `#/board` against a running serve shows no DEMO marker.

## Operator notes

This is the first slice whose result has to be looked at. At Delivered, run the
dashboard dev server, open the demo route beside the reference prototype's
simulated feed, and compare: the route, where craft sit at each stage, the
ring, the free return. A mission cannot make that judgement.
