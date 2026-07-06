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

## The stage model

Canonical stages of one piece of work, each with its artifact and its one
next action. Surfaces render THIS model — never raw internal states.

| Stage      | Backing state              | Artifact shown inline        | Operator action    |
|------------|----------------------------|------------------------------|--------------------|
| Captured   | ticket NEW                 | the ticket itself            | Draft              |
| Drafting   | ticket DRAFTING            | live orchestrator feed (WS)  | (watch / answer)   |
| Needs you  | ticket NEEDS-CONTEXT       | the orchestrator's questions | Answer + redraft   |
| Reviewable | ticket REVIEW              | plan.md + estimate           | Queue (or reshape) |
| Queued     | ticket QUEUED              | queue position               | (reorder later)    |
| Running    | mission Running + live run | mission feed, cost ticker    | steer / pause      |
| Delivered  | mission Complete, unmerged | report.md + diff stat        | Merge              |
| Landed     | branch merged to base      | merge commit link            | —                  |
| Failed     | mission Failed/ticket FAIL | report + failure note        | Redraft / abandon  |

Notes: "Delivered ≠ Landed" is the distinction today's UI erases — the
UNMERGED badge is mandatory. `isBlocked` renders at Captured/Reviewable.
Estimates must be PERSISTED at plan-park time (into plan.md's header) so
Reviewable can show them — today they die in draft stdout.

## D-A — verb renames (OPERATOR DECISION)

"Approve" currently names both plan-commit and ticket-queueing. Proposal:

- Plan approval (the spend/contract consent) keeps **Approve** — it is the
  product's one sacred gate and the word should stay ceremonial.
- Ticket-queueing becomes **Queue** everywhere (button copy "Queue for
  run"; CLI alias `kranz ticket queue` with `approve` kept as a deprecated
  alias one release).
- Start (an approved mission with no run) stays **Start**.

**Recommendation:** as above. Foreclosed if rejected: nothing — copy is
cheap; deciding twice is not.

## D-B — who drains the queue (resolves gascity-citizenship D5)

Options: (1) serve grows an OPT-IN background drain (`autoWork: true` in
config); (2) `kranz work` stays the only dispatcher, run manually or under
a supervisor (launchd/Gas City).

**Recommendation: (1), opt-in, default off.** Serve already hosts runs and
holds the registry; the queue is crash-safe (atomic claims, dead-claim
recovery); opt-in preserves the human-on-the-trigger posture for anyone
who wants it, and the Gas City pack keeps using the external dispatcher
unchanged. With autoWork on, "Queue" from any surface IS end-to-end: the
run starts when the repo frees up, no terminal anywhere. Foreclosed:
nothing — the external dispatcher path remains fully supported (the two
dispatchers already contend safely via the claim machinery).

## D-C — does kranz merge? (OPERATOR DECISION)

The standing rule — **kranz never pushes** — saved main twice this week
and is not on the table. Proposal: a gated **Merge** action (dashboard
button + `/kranz merge <id>`; POST /api/missions/:id/merge, token-gated)
that: refuses on a dirty tracked tree; runs the full gate suite (workspace
tests, clippy -D warnings, fmt, dashboard tsc+build when touched); merges
--no-ff to the base branch on success; NEVER pushes. Push remains a human
git command. Failure shows the failing gate verbatim.

**Recommendation: yes.** This converts the merge from git archaeology to a
reviewed click while keeping publication human. Foreclosed if rejected:
the Delivered stage keeps a copy-paste command block instead of a button
(acceptable fallback; the view ships either way).

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
3. **autoWork drain** (if D-B accepted): config-gated serve background
   drain reusing the dispatcher's claim/skip logic verbatim from
   cli backlog (hoist to engine if needed — same pattern as the draft
   hoist).
4. **Merge affordance** (if D-C accepted): the gated merge, REST + button
   + Slack verb.
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
