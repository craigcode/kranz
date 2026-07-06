# Mission plan — m-836e99

**Goal:** Delete the unrouted dead MissionPicker component and its now-orphaned helper/tests, and scrub every remaining source reference to MissionPicker, without regressing the abandon-mission affordance that lives on the mission detail surface.

Branch `kranz/mission-m-836e99` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$2.52 – $12.62** (expected ~$5.05). Rough estimate — live usage is authoritative; based on 27 completed mission(s).

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** MissionPicker.tsx and MissionPicker.test.tsx no longer exist in the repository. 
  `cd apps/dashboard && test ! -e src/components/MissionPicker.tsx && test ! -e src/components/MissionPicker.test.tsx`
- **[a2]** The orphaned missionCounts helper (imported only by MissionPicker) no longer exists. 
  `cd apps/dashboard && test ! -e src/lib/missionCounts.ts`
- **[a3]** No source file under apps/dashboard/src contains any reference to the string MissionPicker (imports, comments, or test strings). 
  `cd apps/dashboard && ! grep -rn 'MissionPicker' src`
- **[a4]** The missionCounts symbol is no longer imported anywhere in the dashboard source. 
  `cd apps/dashboard && ! grep -rn 'missionCounts' src`
- **[a5]** The dashboard typechecks with no errors after the deletions. 
  `cd apps/dashboard && npx tsc --noEmit`
- **[a6]** The dashboard test suite passes after the deletions. 
  `cd apps/dashboard && npm run test`
- **[a7]** The dashboard lints clean after the deletions (no unused imports or dead references left behind). 
  `cd apps/dashboard && npm run lint`
- **[a8]** The confirm-gated abandon-mission affordance remains present and wired on the live mission detail surface (StatusStrip), so deleting MissionPicker does not remove the only path to abandon a mission. 
  `cd apps/dashboard && npm run test -- StatusStrip`
- **[a9]** Only the intended files are touched: MissionPicker.tsx/.test.tsx and missionCounts.ts are deleted, and the only modified source files are PipelineView.tsx, BacklogPanel.tsx, styles.css, and PipelineView.test.tsx (no unrelated files changed relative to the pinned base). 
  `cd "$(git rev-parse --show-toplevel)" && test -z "$(git diff --name-only $KRANZ_BASE_SHA -- apps/dashboard/src | grep -vE '^apps/dashboard/src/(components/(MissionPicker\.tsx|MissionPicker\.test\.tsx|PipelineView\.tsx|PipelineView\.test\.tsx|BacklogPanel\.tsx)|lib/missionCounts\.ts|styles\.css)$')"`
- **[a10]** Updated comments accurately describe the current architecture and do not leave dangling references to the removed component. *(agent judgement)*

## Milestone 1 — MissionPicker and its orphaned helper are removed, all references scrubbed, and the suite stays green

### 1.1 Delete MissionPicker, its orphaned missionCounts helper, and scrub every remaining source reference

Context: apps/dashboard/src/components/MissionPicker.tsx is a dead, unrouted React component. App.tsx renders <PipelineView/> at the root, <BacklogPanel/> at #/backlog, and <StatusStrip/> as the mission detail view; it does NOT import MissionPicker. The only import of MissionPicker in the entire repo is inside its own test file. The confirm-gated abandon-mission affordance that MissionPicker offered ALSO exists on the live mission detail surface (StatusStrip.tsx, arm→'confirm abandon' flow wired to the store's abandonMission) — so removing MissionPicker does not remove the only abandon path. Do NOT touch StatusStrip.

Before making changes, re-verify the safety facts with: `cd apps/dashboard && grep -rn 'MissionPicker' src` and confirm App.tsx does not import it and StatusStrip.tsx still contains the abandon button. If App.tsx imports MissionPicker, or StatusStrip has no abandon affordance, STOP and report instead of deleting.

Do exactly the following:

1. DELETE the file `apps/dashboard/src/components/MissionPicker.tsx`.
2. DELETE the file `apps/dashboard/src/components/MissionPicker.test.tsx`. Note this test file contains a `describe('missionCounts')` block — those tests go away with the file; that is intended.
3. DELETE the file `apps/dashboard/src/lib/missionCounts.ts`. Rationale: `missionCounts` is imported ONLY by MissionPicker.tsx and MissionPicker.test.tsx. Once both are gone it has zero consumers and zero test coverage — orphaned dead code. Before deleting, confirm no other importer with `cd apps/dashboard && grep -rn 'missionCounts' src` (expect matches only in the two MissionPicker files you are deleting). If any OTHER file imports missionCounts, STOP and report — do not delete it in that case.
4. Update the comment at the top of `apps/dashboard/src/components/PipelineView.tsx` (around line 3) so it no longer names MissionPicker. It currently reads that the default route 'Replaces the old two-list landing (MissionPicker + BacklogPanel) — the picker/backlog panels stay reachable at their own hashes for now'. Rewrite it to accurately describe the current architecture: the pipeline view is the default route (#/), it is a single flat list reduced through the nine-stage model; the browsable ticket backlog lives at #/backlog (BacklogPanel). Do not reference MissionPicker. Keep the comment concise and truthful — BacklogPanel is still real and routed; only MissionPicker is gone.
5. Update the comment at the top of `apps/dashboard/src/components/BacklogPanel.tsx` (around line 2) which currently says 'Mirrors MissionPicker's list styling: rows use .picker-item / .picker-row'. Rewrite so it no longer names MissionPicker — e.g. describe that rows use the shared .picker-item / .picker-row list styling and clicking one navigates to #/backlog/<slug>. Do not invent new behavior; just drop the MissionPicker reference.
6. Update the CSS comment in `apps/dashboard/src/styles.css` (around line 1442, above the `.picker-item--stacked` rule) which currently says 'Scoped via modifier — MissionPicker/BacklogPanel keep the plain row layout.' Rewrite so it no longer names MissionPicker (e.g. '... — the plain .picker-item rows keep the flat layout; only the pipeline view opts into the stacked modifier.'). Do NOT change any CSS selectors or rules — `.picker-item` and `.picker-item--stacked` are still used by PipelineView and BacklogPanel and must be preserved.
7. Update the stale test-description string in `apps/dashboard/src/components/PipelineView.test.tsx` (around line 356): the `it('renders the pipeline view (not the old MissionPicker landing) at the default hash', ...)` description names MissionPicker. Reword the description string so it no longer says MissionPicker (e.g. 'renders the pipeline view at the default hash'). Do NOT change the test body or its assertions — only the description text.

After the edits, run the full gate from the apps/dashboard directory and paste the output as test evidence: `npx tsc --noEmit`, then `npm run test` (vitest run), then `npm run lint` (oxlint). All three must pass. Also run `grep -rn 'MissionPicker' src` and `grep -rn 'missionCounts' src` and confirm BOTH return no matches. Do not add any new dependencies. Do not modify App.tsx, StatusStrip.tsx, or any file not listed above.

Done when:
- MissionPicker.tsx and MissionPicker.test.tsx are deleted from apps/dashboard/src/components/
- missionCounts.ts is deleted from apps/dashboard/src/lib/ (confirmed to have no importers other than the deleted MissionPicker files)
- `grep -rn 'MissionPicker' apps/dashboard/src` returns no matches
- `grep -rn 'missionCounts' apps/dashboard/src` returns no matches
- The top-of-file comments in PipelineView.tsx and BacklogPanel.tsx, the CSS comment near .picker-item--stacked in styles.css, and the test-description string near line 356 in PipelineView.test.tsx no longer reference MissionPicker and remain accurate
- The .picker-item and .picker-item--stacked CSS rules and selectors are unchanged (still used by PipelineView and BacklogPanel)
- `cd apps/dashboard && npx tsc --noEmit` exits 0
- `cd apps/dashboard && npm run test` passes with the full suite green
- `cd apps/dashboard && npm run lint` exits 0
- StatusStrip.tsx is unchanged and still contains the confirm-gated abandon-mission affordance

