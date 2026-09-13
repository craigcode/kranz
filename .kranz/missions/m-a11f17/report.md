# Mission report — m-a11f17

**Goal:** Add a lens filter bar (Actionable | Backlog | Missions | All) to the pipeline view so operators see actionable work by default instead of a wall of terminal 'landed' rows, and restore a visible affordance to the full #/backlog list.

Branch `kranz/mission-m-a11f17` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 25m 50s
**Tokens:** 35062 in / 92374 out / 8825606 cache read / 509829 cache write
**Cost:** $23.63 actual vs $5.10–$25.50 estimated (expected $10.20)

## What shipped

### Milestone 1 — Lens filter bar: signal over history, backlog reachable ✅

- ✅ **Pure lens-filter model (lib/lensFilter.ts)** — 1 run
  - `af48d73` [f-1-1] add pure lens-filter model for pipeline view
- ✅ **Lens bar UI + backlog link in PipelineView** — 1 run
  - `2835520` [f-1-2] add lens filter bar and backlog link to PipelineView
- ✅ **Test the Landed(N) hint and its click-to-All behavior** *(fix)* — 1 run
  - `db8fa8d` [ms-1-fix-1-1] test Landed(N) hint and click-to-All behavior

## Validation history

### ms-1 round 1 — Lens filter bar: signal over history, backlog reachable

- [minor] f-1-2: 'Landed (N)' hint is shown and clicking it switches to the All lens — PipelineView.tsx:452-460 renders `<button className="lens-landed-hint" onClick={() => setLens('all')}>Landed ({landedCount(rows)}) — view all</button>` only when lens==='actionable' && landedCount(row… [truncated]

Disposition: 1 fix feature(s) created.

### ms-1 round 2 — Lens filter bar: signal over history, backlog reachable

No findings.

## Contract outcomes

- ✅ **[a1]** On initial render the default lens is Actionable: rows in terminal stages (landed, abandoned) are not rendered, while captured, needs-you, reviewable, delivered and failed rows are rendered. *(command: `cd apps/dashboard && npx vitest run -t "default lens is Actionable"`)*
- ✅ **[a2]** Selecting the All lens renders every row across all stages including landed and abandoned (today's unfiltered behavior). *(command: `cd apps/dashboard && npx vitest run -t "All lens shows every row"`)*
- ✅ **[a3]** The pure lens filter yields the correct membership per lens: Backlog = ticket-kind rows only, Missions = mission-backed rows with active stages always plus terminal missions capped to the 10 most recent by createdAt, Actionable excludes landed and abandoned, All is identity. *(command: `cd apps/dashboard && npx vitest run src/lib/lensFilter.test.ts`)*
- ✅ **[a4]** Under the Missions lens, a mission-backed row still renders an affordance linking to that mission's detail view (#/m/<id>). *(command: `cd apps/dashboard && npx vitest run -t "Missions lens row links to mission detail"`)*
- ✅ **[a5]** The pipeline view renders a visible affordance whose href is #/backlog, reaching the full BacklogPanel. *(command: `cd apps/dashboard && npx vitest run -t "links to #/backlog"`)*
- ✅ **[a6]** The dashboard typechecks, the full test suite passes with no regressions, and the production build succeeds. *(command: `cd apps/dashboard && npx tsc --noEmit && npm run test && npm run build`)*
- ✅ **[a7]** The mission changes no files outside apps/dashboard relative to the mission base commit. *(command: `[ -z "$(git diff --name-only $KRANZ_BASE_SHA -- ':(exclude)apps/dashboard' ':(exclude).kranz')" ]`)*
- ✅ **[a8]** Opening the dashboard, an operator immediately sees actionable work rather than a wall of terminal 'landed' history: the Landed count is surfaced (not silently dropped), and the full backlog is reachable via a discoverable affordance. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
