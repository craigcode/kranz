# Trajectory board: a Board lens on the pipeline view (scoping)

Status: scoped 2026-10-08, DESIGN-FIRST. The decisions marked OPERATOR
DECISION are the operator's; the ones marked PROPOSED are recommendations that
stand unless the operator changes them. Review this note before drafting any
`trajectory-board-*` ticket. Reference prototype:
[`trajectory-board-prototype/`](trajectory-board-prototype/README.md).

## Why

The pipeline view answers "where is it, what's next, whose move is it" one row
at a time. It does not show, in one place, every ask that is waiting on the
operator. A running mission that is blocked, parked on a grant, holding a
question, or waiting on a one-call permission still reads as `running` in the
list; the ask itself is on the mission page or in Slack.

The board is one picture of the whole pipeline with one signal: **a red ring
means this is waiting on you.** It is meant to be read from across the room.

It is also deliberately playful: Kranz is named after a flight director, and
the pipeline's stages map onto an Apollo flight plan without strain. That is
why it is an optional lens. The default view, the list, and the "clean and
data-dense; no decorative chrome" rule in `docs/dashboard-reference.md` stay as
they are for anyone who never opens it.

## What it inherits (already decided elsewhere)

- **One data source.** Lenses are filtered views of the pipeline's rows, not
  new list components; standalone lists drifted and were deleted
  (`pipeline-view-surface-actionable`). The board is a lens.
- **One stage model.** Surfaces render `pipelineStage()`
  (`apps/dashboard/src/lib/pipelineStage.ts`), never raw ticket or mission
  states (`docs/scoping/pipeline-view.md`, "The stage model").
- **The UI is a pure view.** The Rust reducer is the single source of truth;
  the dashboard replaces `MissionState` wholesale and does not re-derive it
  (`apps/dashboard/src/lib/store.ts`).
- **Gates stay informed and human.** Queueing a reviewed plan is contract
  review as much as cost consent, and an uninformed gate is theater
  (pipeline-view D-D). Merge is human-triggered and never pushes
  (pipeline-view D-C).
- **Verbs are fixed.** Approve is plan approval, Queue is ticket queueing,
  Start and Merge keep their names (pipeline-view D-A).
- **A surface may only mutate what it can show in full.** The glasses client
  excludes plan approval, merge, and anything it cannot fully review
  (`even-realities-g2-demo`). The same boundary applies here.
- **No new execution primitive.** This is a view over state the server already
  publishes. It adds no endpoint, no event, and no engine behavior.

## The board

Earth on the left, the Moon on the right, one figure-8 route between them.
Each work item is one craft. Its place comes from its pipeline stage.

| Pipeline stage | Place | What the operator sees | Ring |
|---|---|---|---|
| `captured` | hangar | not drawn as a craft; shown as a count beside the pads | no |
| `drafting` | pad | the stack rising while the plan is drafted | no |
| `needs-you`, `wrong-plan` | pad | the stack, stopped | yes |
| `reviewable` | pad | the complete stack | yes |
| `queued` | pad | the complete stack, cleared and waiting for the queue | no |
| `running` | Earth orbit | the craft circling; progress in its label | only for an in-flight ask |
| `delivered` | the crossing, then lunar orbit | command module and lander circling the Moon | yes |
| `landed` | lunar surface | the lander descends; a flag stays | no |
| `failed` | free return | round the Moon, back to Earth, splashdown; it stays afloat until redrafted or cleaned up | yes |
| `abandoned` | wherever it was | fades out | no |

In-flight asks ring a craft in Earth orbit. Each uses the same condition as the
panel that already shows it on the mission page:

| Ask | Condition in `MissionState` | Existing panel |
|---|---|---|
| Grant | `pendingGrantRequest` is present | `GrantRequestPanel` |
| One-call permission | a `permissions` record with no resolution, not closed, before its deadline | `PermissionRequestPanel` |
| Blocked | `mission.status` is `blocked` | `StatusStrip` |
| Gate review | a `gateEvaluations` record, not closed or consumed, resolved to anything but `proceed` | `GateReviewPanel` |
| Plan revision | `pendingRevision` is present | `RevisionPanel` |
| Question | `pendingQuestions` is non-empty | `QuestionRequestPanel` |

Rules that keep the picture honest:

- **Nothing waiting is hidden.** A list beside the plot names every ask, in
  urgency order, with a link to the surface that holds the act. The plot can
  run out of room; the list cannot.
- **First sight is steady state.** When the lens opens, every craft is already
  where its stage puts it. Only changes observed while the lens is open are
  animated: liftoff, the crossing, the descent, the free return.
- **Capacity is fixed and overflow is defined.** Four pads, five orbit lanes,
  twelve landing sites. Ringed work takes pads first; ground work beyond four
  waits in the hangar and is counted there. Craft beyond five share lanes at
  different angles. The surface keeps the twelve most recent flags, and the
  sea keeps the three most recent failures afloat. The list has them all.
- **The plot is not the only way in.** The list is the accessible form of the
  board. With reduced motion, or without a canvas, the lens still works.

## D-A. Where it lives (PROPOSED)

A fifth lens on the pipeline view, `Board`, with `#/board` and
`#/r/<repo>/board` as aliases in the way `#/backlog` already is. The lens body
is loaded on demand, so its code is not in the main chunk.

It cannot be a separate page. A real serve approves browser requests only from
its own origin, the two dev-server ports and the Tauri shell (`origin_allowed`
in `crates/server/src/lib.rs`), and that allowlist should not be widened for a
toy. A separate app under `apps/` would have to be served by `kranz serve`
anyway to be usable.

## D-B. Where its state comes from (PROPOSED)

Server folds only: the pipeline's `tickets` and `missions`, plus
`GET /api/missions/:id/state` for each mission in the `running` stage. The lens
keeps a small in-memory record of the stage changes it has observed, stamped
when it saw them, to drive the animations.

The prototype instead folds the raw event log in the browser
(`trajectory-board-prototype/src/fold.js`, a reduced mirror of the reducer).
That buys replay of history, and costs a second reducer that must track the
event schema forever. Not proposed. Replay is out of scope for this build.

Polling is the cost. `GET /api/missions` folds every mission log on each
request (`docs/protocol.md`), so the lists refresh no more often than every
ten seconds, mission states every five, and nothing is fetched while the tab is
hidden.

## D-C. Acting from the board (OPERATOR DECISION)

The prototype's simulated mode has GO and NO-GO buttons on the board. Against a
real server those would be a second, uninformed copy of gates this project has
already made informed on purpose.

Options:

1. **The board never acts.** Every ask links to the surface that holds the
   act: the ticket, the pipeline row, or the mission page. The lens makes no
   POST at all. This is slices 3 to 5.
2. **Act in place by reuse.** Selecting a craft shows, beside the plot, the
   components that already own the act: the existing ask panels for an
   in-flight ask, the existing reviewable row (plan, estimate, Queue) and
   `DeliveredPanel` (report, diff stat, Merge) for the others. No new call
   site for any POST. This is slice 6.
3. **Board-native buttons that POST directly.** Not proposed.

Recommendation: 1, then 2 as its own slice once the read-only lens has been
used for a while. The operator decides whether slice 6 is built at all, and
whether plan queueing and merge may be reached in place or must always
navigate.

## D-D. What rings (PROPOSED)

The Actionable lens's stages without `captured` (`needs-you`, `wrong-plan`,
`reviewable`, `delivered`, `failed`), plus the six in-flight asks above.

`captured` tickets are inventory, not something stopped; ringing each one would
make the signal meaningless. `queued` work is not ringed either: the existing
Run queue control stays above the plot and says whether anything is draining.

## D-E. three.js as a dependency (OPERATOR DECISION)

The 3D layer needs three.js (MIT). It would be the dashboard's fifth runtime
dependency, beside `react`, `react-dom`, `zustand` and `@tauri-apps/api`, and
the bundle is embedded in the `kranz` binary and its crates.io package.

Measured with the dashboard's own Vite 8.3.0 against three 0.186.1, importing
only the classes the prototype's scene uses:

| Bundle | Minified | Gzipped |
|---|---|---|
| Whole dashboard today (JS + CSS) | 375 KB | 104 KB |
| 3D chunk, spacecraft and bodies drawn in code | 534 KB | 133 KB |
| 3D chunk, plus the glTF loader for the kit props | 618 KB | 155 KB |

Loaded on demand, it costs nothing until the lens is opened, but it is in the
binary either way, and Vite will warn about a chunk over 500 KB.

The slicing makes this decision separable: slices 1 to 4 deliver a complete 2D
board with no new dependency. Declining D-E loses the models, not the board.

## D-F. Ground props from a third-party kit (OPERATOR DECISION)

The prototype dresses the launch site and the Moon with twelve models from
Kenney's Space Kit 2.0 (CC0 1.0): a hangar, a dish, craters, rocks, a rover,
two astronauts. They are 168 KB and need the glTF loader (84 KB more).

The copies under `trajectory-board-prototype/kit/` came from an unofficial npm
mirror and have not been compared with the original download. Options: verify
them against the original and ship them; or drop them and draw craters and
rocks in code, which removes every third-party asset from the dashboard.

Recommendation: decide after seeing slice 5 without them.

## D-G. Wording (PROPOSED)

Labels on the plot may use flight-controller phrases: GO FOR LAUNCH? on a
reviewable craft, GO FOR LANDING? on a delivered one. Buttons, links and the
list keep the decided verbs and the pipeline's stage names. The label is
flavor; the verb is the contract.

## Build slicing (each a single mission brief)

1. **Flight plan** (`trajectory-board-1-flight-plan`). The geometry of the
   figure 8, a craft's timeline, and its pose at a time, as pure typed modules
   with tests. No UI.
2. **Fleet model** (`trajectory-board-2-fleet-model`). Rows and mission states
   in; craft, places, asks and observed transitions out. The ask conditions
   move into one module that the existing panels also use. No UI. Blocked by
   1, whose craft type it produces.
3. **Board lens** (`trajectory-board-3-lens`). The lens, its routes, on-demand
   loading, polling, the complete list of asks with links, and counts. Useful
   before anything is drawn. Blocked by 2.
4. **Plot** (`trajectory-board-4-plot`). The 2D board: route, craft, trails,
   rings, labels, and a scripted demo route. No new dependency. Blocked by 1
   and 3.
5. **Scene** (`trajectory-board-5-scene`). The 3D layer under the plot. Blocked
   by 4 and by D-E.
6. **Act in place** (`trajectory-board-6-act-in-place`). Blocked by 3 and by
   D-C.

Slices 1 to 4 run in order. Slices 5 and 6 are independent of each other, and
either can be left unbuilt.

## How this work gets validated

- Slices 1 and 2 are pure functions; their contract is tests that can fail.
- Slices 3 to 6 render. Component tests run in jsdom, where a canvas has no
  drawing context, so the lens must degrade to the list and the tests assert
  on the list, on links, and on a recording context for draw calls.
- If the functional validator has a browser tool (`docs/validating-uis.md`),
  the demo route gives it a deterministic page to drive.
- Nobody in a mission can see whether the result looks right. That judgement
  is the operator's at Delivered, against the reference prototype.

## Out of scope

Replay and time-lapse of history. The age of an ask. A fleet-wide board across
repositories. Sound. Anything in Slack or the glasses client. Any server,
protocol, event or engine change. Any second implementation of an existing
action.

## Done when

With the dashboard open on `#/board`, an operator can tell from across the room
whether anything is waiting, read what and where from the list, and reach the
surface that holds the act in one click. Someone who never opens the lens gets
the same default view as before and a main chunk that carries none of the
board's code. The reference prototype folder has been deleted.
