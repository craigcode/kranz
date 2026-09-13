# Pipeline view + action layer (backlog-pipeline-view scoping)

Status: scoped 2026-07-05 night, DESIGN-FIRST — the three flagged decisions
are the operator's; review this doc before any mission is drafted against
it. Companion ticket: `.kranz/tickets/backlog-pipeline-view.md` (p1).

## Why

One day of real operation (2026-07-05) produced this confusion inventory:
approved-idle read as RUNNING on every surface (32-minute stall); plans,
reports, and estimates live on mission branches and in terminal scrollback;
"approve" names two unrelated acts; complete-but-unmerged is invisible
(m-bc11fb sat unmerged for hours while its stale build was being clicked);
the drain is invisible to every UI; stale chips and ghost rows compounded
distrust (since fixed — fix-blocked-chip-derived, fix-missions-list-ghosts).

Root cause, one sentence: **the product is a pipeline; the UI shows two
disconnected lists.**

And the end-to-end gap map (post those fixes):

| Stage                    | Web UI                  | Slack            |
|--------------------------|-------------------------|------------------|
| Capture ticket           | none                    | none             |
| Draft plan               | yes                     | yes              |
| Review plan + estimate   | plan buried on branch; estimate unrecorded | none |
| Approve / queue          | yes (blocked-aware)     | yes (by slug)    |
| RUN THE QUEUE            | none                    | none             |
| Monitor                  | yes                     | yes              |
| Review deliverable       | none (git only)         | completion ping  |
| Merge                    | none (git only)         | none             |

## Design principles (DECIDED 2026-07-05)

1. **Simple lists, easy buttons.** Every surface is a flat list with one
   obvious action per row — the operator's next move is always one tap.
   No nested navigation to reach a gate; detail views are for reading,
   never the only place an action lives.
2. **Iterate is always on offer.** Work is never a dead end: Reviewable
   offers **Reshape** (a feedback line → planning turn → re-parked plan);
   Delivered and Landed offer **Iterate** (one tap creates a follow-up
   ticket pre-seeded with the mission's report and the operator's one-line
   direction); Failed offers **Redraft**. The pipeline loops; the UI must
   show the loop.

## The stage model

Canonical stages of one piece of work, each with its artifact and its one
next action. Surfaces render THIS model — never raw internal states.

| Stage      | Backing state              | Artifact shown inline        | Operator action    |
|------------|----------------------------|------------------------------|--------------------|
| Captured   | ticket NEW                 | the ticket itself            | Draft              |
| Drafting   | ticket DRAFTING            | live orchestrator feed (WS)  | (watch / answer)   |
| Needs you  | ticket NEEDS-CONTEXT       | the orchestrator's questions | Answer + redraft   |
| Reviewable | ticket REVIEW              | plan.md + estimate           | Queue / Reshape    |
| Queued     | ticket QUEUED              | queue position               | (reorder later)    |
| Running    | mission Running + live run | mission feed, cost ticker    | steer / pause      |
| Delivered  | mission Complete, unmerged | report.md + diff stat        | Merge / Iterate    |
| Landed     | branch merged to base      | merge commit link            | Iterate            |
| Failed     | mission Failed/ticket FAIL | report + failure note        | Redraft / abandon  |

Notes: "Delivered ≠ Landed" is the distinction today's UI erases — the
UNMERGED badge is mandatory. `isBlocked` renders at Captured/Reviewable.
Estimates must be PERSISTED at plan-park time (into plan.md's header) so
Reviewable can show them — today they die in draft stdout.

## D-A — verb renames (DECIDED 2026-07-06, by operator delegation)

"Approve" currently names both plan-commit and ticket-queueing. Proposal:

- Plan approval (the spend/contract consent) keeps **Approve** — it is the
  product's one sacred gate and the word should stay ceremonial.
- Ticket-queueing becomes **Queue** everywhere (button copy "Queue for
  run"; CLI alias `kranz ticket queue` with `approve` kept as a deprecated
  alias one release).
- Start (an approved mission with no run) stays **Start**.

DECIDED: as recommended — Approve reserved for plan approval, Queue for
ticket-queueing, Start unchanged. (Operator delegated remaining decisions
2026-07-06: "build everything".)

## D-B — who drains the queue (DECIDED 2026-07-05, resolves gascity D5)

DECIDED: running the queue must be available from BOTH web UI and Slack.
Shape: serve owns the drain as a background task; both surfaces get an
explicit trigger — a "Run queue" affordance in the dashboard and
`/kranz work run` in Slack (the bridge NEVER runs missions on the socket
loop; the verb POSTs to serve, which spawns the drain task — same pattern
as start). REST: token-gated POST /api/queue/drain (idempotent: a drain
already running returns its state). Config `autoWork: true` additionally
drains automatically whenever entries queue (opt-in, default off). The
external `kranz work` dispatcher remains fully supported — the claim
machinery already arbitrates concurrent dispatchers safely.

## D-C — does kranz merge? (OPERATOR DECISION)

The standing rule — **kranz never pushes** — saved main twice this week
and is not on the table. Proposal: a gated **Merge** action (dashboard
button + `/kranz merge <id>`; POST /api/missions/:id/merge, token-gated)
that: refuses on a dirty tracked tree; runs the full gate suite (workspace
tests, clippy -D warnings, fmt, dashboard tsc+build when touched); merges
--no-ff to the base branch on success; NEVER pushes. Push remains a human
git command. Failure shows the failing gate verbatim.

DECIDED 2026-07-05: yes — and BOTH deliverable review and the merge act
must be available from web UI and Slack alike. Web: report.md + diff stat
rendered inline at Delivered, Merge button. Slack: a Delivered card
(report summary + diff stat + deep link for the long read) with a gated
Merge button/verb (`/kranz merge <slug|id>`, allowlist-gated — merging
shapes main, so it is spend-adjacent in trust terms). The merge remains
HUMAN-triggered on every surface; kranz still never pushes.

## D-D — the queue gate stays human (DECIDED 2026-07-05)

The Reviewable → Queued transition keeps a human, permanently as the
default. Rationale: the gate is contract review as much as cost consent —
thin contracts must die before they produce unaudited work — and today's
gate is uninformed theater (the operator never sees the estimate). Slice 1
+ slice 2 make it a ten-second informed act: plan, contract, and estimate
WITH calibration confidence, on every surface, one Queue button.

Deferred middle path (explicitly NOT in this build): per-ticket priced
consent — opt-in frontmatter `auto-queue-under: <usd>` meaning "queue
without asking iff the estimate's HIGH bound is under the cap, validators
are on, and blockers are satisfied." Mirrors the scrutiny-floor
philosophy. Hard-gated on fix-calibration-mission-shape landing first:
auto-queueing on 4x-miss estimates is automated surprise.

Also DECIDED 2026-07-05: ticket capture from both web and Slack (slice 5)
is confirmed, not optional.

## Build slicing (each a single mission brief)

1. **Persist estimates + serve stage artifacts.** Estimate into plan.md at
   park time; GET endpoints to render plan.md/report.md/diff-stat for a
   mission; merged/unmerged bit (cheap server-side probe: is the mission
   branch tip an ancestor of base). Pure REST/engine, no UI.
2. **The pipeline view.** One screen, one row per work item, stage chips
   per the model above, artifacts and actions inline. Consumes slice 1.
   Includes the D-A verb copy.
3. **Queue running via serve** (D-B decided): hoist the dispatcher's
   drain/claim/skip loop to be host-callable (same pattern as the draft
   hoist); POST /api/queue/drain; dashboard "Run queue" affordance;
   Slack `/kranz work run` (posts to serve, never runs on the socket
   loop); optional autoWork config.
4. **Deliverable review + merge on both surfaces** (D-C decided): the
   gated merge (gates → --no-ff → never push); report/diff rendered at
   Delivered in the dashboard; Slack Delivered card + allowlist-gated
   /kranz merge.
5. **Ticket capture everywhere:** dashboard new-ticket form; Slack
   `/kranz ticket new <slug> <title...>` opening the modal pattern for
   goal/context. (Small; can ride with slice 2.)

Sequencing: 1 → 2 ship the visibility promise alone; 3 and 4 are
independent of each other and of 2 (REST-level), both gated on decisions.

## Out of scope

Re-planning UI (M2), parallel-worker visualization, multi-repo views,
external tracker sync, any change to the approval spend gate's semantics.

## Done when

An operator with no terminal open can: capture a ticket, draft it, read
the plan AND its estimate, queue it, watch it run (autoWork on), read the
report, see UNMERGED, and land it — from the dashboard alone, and every
one of those steps except reading long artifacts from Slack alone; and at
every moment in between, the pipeline view answers "where is it, what's
next, whose move is it" without a second question.
