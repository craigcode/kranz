# Mission plan — m-a11f17

**Goal:** Add a lens filter bar (Actionable | Backlog | Missions | All) to the pipeline view so operators see actionable work by default instead of a wall of terminal 'landed' rows, and restore a visible affordance to the full #/backlog list.

Branch `kranz/mission-m-a11f17` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$3.81 – $19.03** (expected ~$7.61). Rough estimate — live usage is authoritative; based on 34 completed mission(s).

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** On initial render the default lens is Actionable: rows in terminal stages (landed, abandoned) are not rendered, while captured, needs-you, reviewable, delivered and failed rows are rendered. 
  `cd apps/dashboard && npx vitest run -t "default lens is Actionable"`
- **[a2]** Selecting the All lens renders every row across all stages including landed and abandoned (today's unfiltered behavior). 
  `cd apps/dashboard && npx vitest run -t "All lens shows every row"`
- **[a3]** The pure lens filter yields the correct membership per lens: Backlog = ticket-kind rows only, Missions = mission-backed rows with active stages always plus terminal missions capped to the 10 most recent by createdAt, Actionable excludes landed and abandoned, All is identity. 
  `cd apps/dashboard && npx vitest run src/lib/lensFilter.test.ts`
- **[a4]** Under the Missions lens, a mission-backed row still renders an affordance linking to that mission's detail view (#/m/<id>). 
  `cd apps/dashboard && npx vitest run -t "Missions lens row links to mission detail"`
- **[a5]** The pipeline view renders a visible affordance whose href is #/backlog, reaching the full BacklogPanel. 
  `cd apps/dashboard && npx vitest run -t "links to #/backlog"`
- **[a6]** The dashboard typechecks, the full test suite passes with no regressions, and the production build succeeds. 
  `cd apps/dashboard && npx tsc --noEmit && npm run test && npm run build`
- **[a7]** The mission changes no files outside apps/dashboard relative to the mission base commit. 
  `[ -z "$(git diff --name-only $KRANZ_BASE_SHA -- ':(exclude)apps/dashboard' ':(exclude).kranz')" ]`
- **[a8]** Opening the dashboard, an operator immediately sees actionable work rather than a wall of terminal 'landed' history: the Landed count is surfaced (not silently dropped), and the full backlog is reachable via a discoverable affordance. *(agent judgement)*

## Milestone 1 — Lens filter bar: signal over history, backlog reachable

### 1.1 Pure lens-filter model (lib/lensFilter.ts)

Create apps/dashboard/src/lib/lensFilter.ts — a PURE module (no React import) that defines the pipeline view's lenses and filters rows. Context: the pipeline view (src/components/PipelineView.tsx) reduces each work item to one of the stages in src/lib/pipelineStage.ts via `pipelineStage(item: WorkItem): PipelineStage` where PipelineStage = 'captured' | 'drafting' | 'needs-you' | 'reviewable' | 'queued' | 'running' | 'delivered' | 'landed' | 'failed' | 'abandoned'. WorkItem is a discriminated union (kind: 'ticket' | 'mission'); a ticket row may carry an optional joined `mission`. Import types from './pipelineStage' and './types'.

Export:
1. `export type Lens = 'actionable' | 'backlog' | 'missions' | 'all';`
2. `export const LENSES: ReadonlyArray<{ id: Lens; label: string }>` in this exact order/labels: {actionable,'Actionable'}, {backlog,'Backlog'}, {missions,'Missions'}, {all,'All'}.
3. `export interface LensRow { item: WorkItem; missionCreatedAt?: string; }` — the minimal shape the filter needs (the real PipelineView Row will structurally satisfy this).
4. `export function filterLensRows<T extends LensRow>(rows: T[], lens: Lens): T[]` — a generic that preserves the caller's row type and returns the subset for the lens, in the SAME relative order as the input for every lens except where a cap applies.

Lens semantics (compute stage via pipelineStage(row.item); treat a row as mission-backed when row.item.kind==='mission' OR row.item.mission!==undefined):
- 'all': identity (return all rows unchanged).
- 'actionable': keep rows whose stage is one of exactly {captured, needs-you, reviewable, delivered, failed}. Exclude everything else (drafting, queued, running, landed, abandoned).
- 'backlog': keep rows where row.item.kind==='ticket'. (Ticketless mission rows excluded.)
- 'missions': keep mission-backed rows only. Within those: ALWAYS keep rows whose stage is one of {running, queued, reviewable, delivered} (active). For rows whose stage is one of {landed, failed} (terminal), keep only the 10 most recent by missionCreatedAt (ISO-8601 string; sort descending, missing/undefined createdAt sorts last; ties keep input order). Exclude 'abandoned' rows entirely. Preserve original input order in the returned array (do not globally re-sort — only use the createdAt ordering to decide which terminal rows survive the cap).

Also export a small helper `export function landedCount(rows: LensRow[]): number` returning the number of rows whose stage is 'landed' (used by the view to show a 'Landed (N)' hint).

Write apps/dashboard/src/lib/lensFilter.test.ts (vitest) BEFORE/ALONGSIDE the implementation, following the style of src/lib/pipelineStage.test.ts. Build WorkItem fixtures directly (kind:'ticket' with ticket.state, and kind:'mission' with mission {status, merged}). The test file MUST contain tests whose names include these exact substrings so the validation contract can target them:
  - "All lens shows every row" — identity returns all rows.
  - "Missions lens shows only mission-backed rows" — ticket-only rows (no joined mission) are excluded; also assert the terminal cap: given 12 landed mission rows with distinct missionCreatedAt, exactly the 10 most recent survive, and an active (running) mission with no/old createdAt is always kept.
  - "actionable excludes landed and abandoned" — a landed row and an abandoned row are filtered out while a captured, a needs-you, a reviewable, a delivered and a failed row are kept.
  - "Backlog lens shows only ticket-kind rows" — ticketless mission rows excluded, ticket rows (any stage) kept.
Ensure `cd apps/dashboard && npx vitest run src/lib/lensFilter.test.ts` and `npx tsc --noEmit` both pass. Do not touch any file outside apps/dashboard.

Done when:
- filterLensRows(rows,'actionable') excludes landed and abandoned rows and includes captured, needs-you, reviewable, delivered, failed.
- filterLensRows(rows,'backlog') returns only rows where item.kind==='ticket'.
- filterLensRows(rows,'missions') returns only mission-backed rows, always keeps active (running/queued/reviewable/delivered), caps terminal (landed/failed) to the 10 most recent by missionCreatedAt, and excludes abandoned.
- filterLensRows(rows,'all') is identity.
- landedCount returns the number of landed-stage rows.
- lensFilter.test.ts contains tests named with the substrings 'All lens shows every row', 'Missions lens shows only mission-backed rows', 'actionable excludes landed and abandoned', and 'Backlog lens shows only ticket-kind rows', and all pass under vitest.
- npx tsc --noEmit passes; no file outside apps/dashboard is modified.

### 1.2 Lens bar UI + backlog link in PipelineView

Wire the lens filter into apps/dashboard/src/components/PipelineView.tsx, add styles in apps/dashboard/src/styles.css, and preserve/adapt apps/dashboard/src/components/PipelineView.test.tsx. This depends on src/lib/lensFilter.ts (already implemented) which exports: `type Lens = 'actionable'|'backlog'|'missions'|'all'`, `LENSES` (ordered [{id:'actionable',label:'Actionable'},{id:'backlog',label:'Backlog'},{id:'missions',label:'Missions'},{id:'all',label:'All'}]), `filterLensRows<T extends {item:WorkItem; missionCreatedAt?:string}>(rows, lens): T[]`, and `landedCount(rows)`. Import them from '../lib/lensFilter'.

Current behavior to preserve: PipelineView calls buildRows(tickets, missions) to produce a flat `Row[]` (each Row has item:WorkItem, id, title, missionId?, slug?, etc.) and renders `rows.map(row)` inside `<ul className="picker-list">`. The row/action rendering (renderPrimary/renderSecondary/inline plan/report panels) MUST stay exactly as-is.

Changes:
1. Thread createdAt onto rows: in buildRows, add `missionCreatedAt?: string` to the Row interface and populate it from the joined/own MissionSummary.createdAt (for ticket rows, use the joined mission's createdAt when present; for mission rows, the mission's createdAt). MissionSummary has a `createdAt: string` field.
2. Add lens state: `const [lens, setLens] = useState<Lens>('actionable');` (default Actionable).
3. Render a lens bar inside the picker-box, above the list (after the picker-title / error blocks, near RunQueueButton). Render one button per LENSES entry: `<button className={"lens-tab" + (lens===l.id?' lens-tab--active':'')} aria-pressed={lens===l.id} onClick={()=>setLens(l.id)}>{l.label}</button>` wrapped in `<div className="lens-bar" role="tablist">`. Give the active one aria-pressed="true".
4. Apply the filter: `const visibleRows = filterLensRows(rows, lens);` and render `visibleRows.map(row)` instead of `rows.map(row)`.
5. Landed hint: when lens==='actionable' and landedCount(rows) > 0, render a hint element `<button type="button" className="lens-landed-hint" onClick={()=>setLens('all')}>Landed ({landedCount(rows)}) — view all</button>` (a one-click path to reveal hidden landed history). Text must contain the pattern 'Landed (N)'.
6. Backlog link: add a VISIBLE anchor to the full backlog list — `<a className="btn-small pipeline-backlog-link" href="#/backlog">Backlog ↗</a>` — in the picker-title alongside the existing '+ new ticket' / '+ new mission' buttons. This is SEPARATE from the Backlog lens (the lens filters in place; this link routes to BacklogPanel).
7. Empty-state: keep the existing 'No work items found' block, but base it on visibleRows.length===0 only when lens==='all' OR rows.length===0 (so an empty Actionable view with landed history present still shows the Landed hint rather than a misleading 'no work items'). Keep it simple: show the existing empty message when rows.length===0; you do not need a per-lens empty message beyond that.
8. Styles (styles.css): add .lens-bar (horizontal flex, gap, margin), .lens-tab (reuse the visual language of existing .btn-small / status pills — small, clickable, clear active state via .lens-tab--active with a distinct background/border), .lens-landed-hint (dim, small, clickable), .pipeline-backlog-link. Match the existing dark dashboard aesthetic; do not restyle unrelated elements.

Tests (PipelineView.test.tsx): PRESERVE every existing assertion. Because the default lens is now Actionable, any existing test that asserts on a non-actionable stage row (running, queued, landed, abandoned — e.g. the ticketless running mission, the queued primary-action test, the UNMERGED landed row, the abandoned-row test, the direct-fixed done→landed test) MUST first switch the lens to All before asserting: after render, `fireEvent.click(screen.getByText('All'))` (or query the .lens-tab whose text is 'All'). Add NEW tests named with these exact substrings (targeted by the contract):
  - "default lens is Actionable" — render with a mix of a captured ticket and a landed mission (complete, merged:true) and an abandoned mission; assert the landed and abandoned rows are NOT in the document initially, and the captured row IS.
  - "All lens shows every row" — from that same mix, click the All lens and assert the landed and abandoned rows now appear.
  - "Missions lens row links to mission detail" — use a delivered mission (complete, merged:false) which renders the Merge link to #/m/<id>; click the Missions lens and assert its row contains an anchor whose href is '#/m/<id>'.
  - "links to #/backlog" — assert an anchor with getAttribute('href')==='#/backlog' is present on the pipeline view.
  - "Backlog lens shows only ticket rows" — with one ticket and one ticketless mission, click the Backlog lens and assert the ticket row is present and the ticketless mission row is absent.
ALSO: in the same test file's 'App default route' describe block, the existing assertion `expect(screen.queryByText('Missions')).toBeNull()` will now FAIL because 'Missions' is a legitimate lens label. Update that assertion: replace it with one asserting the pipeline view is shown and the lens bar's 'Missions' control exists (e.g. assert a .lens-tab with text 'Missions' is present), preserving the test's original intent (pipeline is the default route) rather than deleting the test.

Verify all of these pass: `cd apps/dashboard && npx tsc --noEmit && npm run test && npm run build`. Do not modify any file outside apps/dashboard.

Done when:
- PipelineView renders a lens bar with four controls labeled Actionable, Backlog, Missions, All; Actionable is active on load.
- On load (Actionable) landed and abandoned rows are not rendered; captured/needs-you/reviewable/delivered/failed rows are; clicking All reveals every row.
- When landed rows exist and the lens is Actionable, a 'Landed (N)' hint is shown and clicking it switches to the All lens.
- The Backlog lens shows only ticket-kind rows; the Missions lens shows only mission-backed rows and their #/m/<id> affordance still works.
- A visible anchor with href='#/backlog' is present on the pipeline view.
- All previously-existing PipelineView tests still pass (adapted to switch to the All lens where they assert on non-actionable stages), and the App default-route test no longer asserts 'Missions' is absent.
- cd apps/dashboard && npx tsc --noEmit && npm run test && npm run build all succeed; no file outside apps/dashboard is modified.

