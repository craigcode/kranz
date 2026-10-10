---
state: open
title: Trajectory board 5: the 3D scene under the plot
priority: 3
schedule: once
blocked-by: [trajectory-board-4-plot]
---

## Goal

Add a WebGL layer beneath the Board lens's 2D plot: a faceted Earth and Moon,
launch pads, spacecraft drawn in code, and a descent stage and flag at each
landing site. Use three.js, loaded only with the lens. Wherever WebGL or the
library is unavailable, the board keeps working exactly as slice 4 left it.
Ship no third-party models.

## Context

Design of record: `docs/scoping/trajectory-board.md`. Read it in full before
planning and do not contradict a decided or proposed section. This is slice 5.
It may be drafted only after the operator has decided D-E in favor of the
dependency. D-F is deliberately left open: this slice omits the kit props, so
it needs no model loader and no binary asset.

Source to port: `docs/scoping/trajectory-board-prototype/src/scene.js`.
`buildStack`, `buildLM`, `buildCSM` and `buildCM` build the spacecraft from
primitives. `planet`, `paintEarth` and `paintMoon` build the faceted bodies
with vertex colors, including a sea patch at the splashdown point. `build`
places pads, towers and site markers. `frame` applies a pose to a craft,
switching between the stack, the joined command module and lander, the lander
alone, the ascent stage and the command module under parachutes. Leave out
`loadKit` and everything placed from `EARTH_SITE`, `MOON_SITE` and
`MOON_EXTRAS`.

The two layers must agree. The camera is orthographic and uses the same
projection as the 2D layer (`project` in the geometry from slice 1), so a
craft's model sits under its ring and label. The reference was checked to the
pixel on this.

When the scene is active, the 2D layer stops drawing its stand-in bodies, craft
triangles and flags and keeps the route, trails, rings and leaders. When the
scene is not active, slice 4's behavior is unchanged.

The dependency. Add `three` at its current stable release as an exact version
in `apps/dashboard/package.json`, and the matching `@types/three` as a dev
dependency. Import named classes from the package; there is no `THREE` global.
The reference targets release 0.147 through the removed `examples/js` build,
so expect differences: color management and the output color space, vertex
colors being interpreted as linear, and light intensities changing units in
release 155. Retune lights and colors until the result matches the picture in
the reference's `README.md`; do not port the old numbers blindly.

Installing it needs the npm registry, which workers cannot reach by default.
Expect a grant request for that egress, or stop and report if it is refused.

Loading. three.js must end up only in lazily loaded chunks. Import it from the
scene module alone, and load the scene module on demand from the lens after
the 2D plot is up, so a slow or failed load never delays the list or the
plot.

Lifecycle. Create the renderer only when a WebGL context is available. On
unmount dispose of the renderer, geometries and materials. On
`webglcontextlost` fall back to the 2D stand-ins. Render on the plot's frame
loop; with `prefers-reduced-motion` render once per model change.

Notices. The dashboard build writes `THIRD_PARTY_NOTICES.txt` through Vite's
license option; after the build it must carry three.js's MIT notice, and the
synced copy under `crates/cli/assets/dashboard/dist` with it.

Constraints: no model loader, no binary assets, no POST, no change to the
list, and nothing from three.js in the main chunk.

## Scoping answers

## Acceptance hints

- `cd apps/dashboard && npx vitest run src/lib/board/scene.test.ts 2>&1 | grep -qE 'Tests +[1-9][0-9]* passed'` succeeds.
- The scene tests build each spacecraft without a renderer and assert that the stack is four separable parts (three stages and the escape tower), that the lander separates into a descent stage and an ascent stage, that the command module carries its parachutes as a separate group, and that the bounding heights of the stack and of the joined command module and lander match the flight plan's `HEIGHT` table.
- A test applies poses for each craft kind and asserts the model's position equals the flight plan's projection of that pose and that exactly the parts belonging to that kind and stage are visible.
- A component test under jsdom, where no WebGL context exists, asserts the lens renders the header, the list and the plot path of slice 4 without error and never constructs a renderer.
- A test asserts that unmounting an active scene calls dispose on its renderer and that a `webglcontextlost` event returns the lens to the 2D stand-ins.
- `cd apps/dashboard && npm run build && ! grep -l "WebGLRenderer" dist/assets/index-*.js && grep -l "WebGLRenderer" dist/assets/*.js` succeeds.
- `cd apps/dashboard && grep -q "^## three - " dist/THIRD_PARTY_NOTICES.txt && grep -q "^## three - " ../../crates/cli/assets/dashboard/dist/THIRD_PARTY_NOTICES.txt` succeeds after the build and sync; neither file names three.js today.
- `cd apps/dashboard && npm audit --audit-level=high` succeeds.
- The mission's diff adds exactly one runtime dependency and one dev dependency to `apps/dashboard/package.json`, both with exact versions, and adds no binary file.
- `cd apps/dashboard && npx tsc -b && npm run test && npm run sync-embedded && npm run check-embedded && npm run lint` all succeed, and the refreshed bundle under `crates/cli/assets/dashboard/dist` is committed.
- With live QA enabled: on the dev server the demo route shows shaded bodies and spacecraft models under the rings and labels, and each ringed label still sits on its craft.

## Operator notes

Decide D-E before drafting. Expect one egress grant for the npm registry.
Measured before this ticket was written: the scene's share of three.js 0.186.1
is 534 KB minified and 133 KB gzipped in its own chunk, against 375 KB for the
whole dashboard today, and Vite prints its large-chunk warning for it.

At Delivered, compare the demo route with the reference prototype side by
side. Lighting and color are the parts most likely to need another pass,
because the library's defaults changed between the reference's release and the
current one.
