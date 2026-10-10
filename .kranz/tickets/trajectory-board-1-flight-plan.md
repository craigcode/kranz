---
state: open
title: Trajectory board 1: the flight plan as pure typed modules
priority: 3
schedule: once
---

## Goal

Port the trajectory board's flight plan from the reference prototype into the
dashboard as pure, typed, tested modules under `apps/dashboard/src/lib/board/`:
the geometry of the figure-8 route, a craft's timeline of key times, and its
pose at a given time. No rendering, no React, no network, no new dependency.
Nothing imports these modules yet; later slices do.

## Context

Design of record: `docs/scoping/trajectory-board.md`. Read it in full before
planning and do not contradict a decided or proposed section. This is slice 1
of its build slicing.

Source to port: `docs/scoping/trajectory-board-prototype/src/profile.js`, in
particular `makeGeom`, `timeline`, `poseAt`, `trail`, `extent`, `clock` and
`bez`. Its behavior is the specification. The soak mode of
`docs/scoping/trajectory-board-prototype/test/logic.mjs` shows the invariants
the prototype was checked against. Port those checks; do not port its
simulator.

Keep the numbers. The constants, angles and curve construction in `profile.js`
were tuned by eye. Copy them exactly, so that a later slice draws what the
reference draws.

Deliberate differences from the source:

- TypeScript under the dashboard's strict config, with named exports and
  exported types. No `globalThis.KB`.
- The craft argument is a small exported interface holding only what the
  flight plan reads: the timestamps `createdAt`, `padAt`, `assembledAt`,
  `startedAt`, `completedAt`, `mergedAt`, `failedAt` and `abandonedAt` in
  milliseconds, plus `pad`, `lane` and `site`. Slice 2 produces values of this
  type.
- `describe` is not ported. Wording belongs to the fleet model in slice 2.
- A failed craft's stay at the splashdown point is no longer a fixed nine
  seconds. The craft interface carries an `afloat` flag: while it is true the
  craft holds at splashdown; when it is false the source's hold and fade
  apply.

Constraints: pure functions only, so no `Date.now()`, no DOM and no module
state. Follow the style of the existing pure model modules and their tests in
`apps/dashboard/src/lib` (`pipelineStage.ts`, `lensFilter.ts`). Do not edit
anything under `apps/dashboard/src/components`, do not add a dependency, and do
not edit the reference prototype.

## Scoping answers

## Acceptance hints

- `cd apps/dashboard && npx vitest run src/lib/board/flightPlan.test.ts 2>&1 | grep -qE 'Tests +[1-9][0-9]* passed'` succeeds.
- The tests assert, for the wide and the tall layout and for every lane: the Earth is orbited counter-clockwise and the Moon clockwise; the outbound crossing starts on that lane's Earth orbit and ends on that lane's lunar orbit; lane order is preserved all the way round, so the innermost lane at the Earth is the outermost at the Moon.
- The tests step a craft through each life story at 50 ms intervals (pad to landed; failed in Earth orbit through to splashdown; scrubbed on the pad; scrubbed in orbit) and assert that every numeric pose field is finite, that consecutive positions never differ by more than 0.45 world units, and that the nose never turns more than 0.5 radians in one step while the craft is alive.
- The tests assert that at touchdown a landing craft is at its site on the lunar surface with its nose along the surface normal, and that a failed craft takes the outbound crossing, sweeps round the Moon from the arrival angle to the departure angle, takes the return crossing and ends at the splashdown point on the Earth's surface.
- The tests assert that a failed craft with `afloat` true is still drawn at the splashdown point ten minutes after splashdown, and that with `afloat` false it is gone by then.
- The tests assert that `poseAt` returns null before the craft exists and for a ground craft with no pad, and that calling it twice with the same arguments gives deeply equal results.
- `cd apps/dashboard && npx tsc -b && npm run test && npm run lint` all succeed.
- `cd apps/dashboard && npm run build && npm run check-embedded` succeed with no change under `crates/cli/assets/dashboard/dist`, because nothing imports the new modules yet.
- The mission's diff touches only files under `apps/dashboard/src/lib/board/`.

## Operator notes

No decision is needed for this slice. It adds no dependency and nothing that
renders.
