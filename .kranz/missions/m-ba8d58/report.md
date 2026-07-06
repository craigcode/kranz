# Mission report — m-ba8d58

**Goal:** Fix five render bugs in the dashboard pipeline view (apps/dashboard): honest inert stages for abandoned/deleted missions and direct-fixed done tickets, a dedicated failure slot so artifact-fetch errors don't collide with the UNMERGED badge, non-truncating mission-id rendering, and a non-JSON fetch guard that reports 'endpoint unavailable — server restart needed?' instead of a raw JSON parse exception.

Branch `kranz/mission-m-ba8d58` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 35m 27s
**Tokens:** 37802 in / 124084 out / 10640770 cache read / 581928 cache write
**Cost:** $32.13 actual vs $7.12–$35.62 estimated (expected $14.25)

## What shipped

### Milestone 1 — Honest stage derivation & inert dead rows ✅

- ✅ **Inert stages for abandoned missions and direct-fixed done tickets** — 1 run
  - `ec0321e` [f-1-1] honest inert stages for abandoned missions and direct-fixed done tickets

### Milestone 2 — Resilient artifact fetch & row layout ✅

- ✅ **Detect non-JSON (HTML fallback) responses in the fetch layer** — 1 run
  - `9fcbddd` [f-2-1] guard non-JSON (HTML fallback) responses in fetch layer
- ✅ **Failure layout slot and non-truncating mission id in the pipeline row** — 1 run
  - `0761a2a` [f-2-2] give inline artifact panels a full-width failure slot; stop id truncation

## Validation history

### ms-1 round 1 — Honest stage derivation & inert dead rows

- [critical] a3 — $ cd apps/dashboard && npx vitest run src/lib/api.test.ts RUN v4.1.9 <operator-home>/Data/kranz/apps/dashboard No test files found, exiting with code 1 filter: src/lib/api.test.ts include: **/*.{te… [truncated]

Disposition: waived.
- a3: Out of scope for ms-1: a3 (api non-JSON guard + api.test.ts) is contracted to ms-2/f-2-1, which is still pending and will implement and validate exactly this; not a defect in ms-1's delivered work (a1/a2).

### ms-2 round 1 — Resilient artifact fetch & row layout

- [minor] f-2-2 mission-id no-truncation component test — apps/dashboard/src/components/PipelineView.test.tsx:344-353 asserts only idEl.textContent === longId and className contains 'picker-id'. jsdom performs no layout, so this passes whether or not the id … [truncated]

Disposition: waived.
- f-2-2 mission-id no-truncation component test: Minor and environment-inherent: jsdom does no layout and vitest doesn't apply external CSS, so no command test can guard pixel-truncation — a7 is deliberately agent-judgement for exactly this, the CSS fix is correct-by-construction, and the suggested getComputedStyle guard would assert on unapplied styles.

## Contract outcomes

- ✅ **[a1]** Abandoned (and deleted/ghost) missions derive to an inert 'abandoned' stage and render zero primary/secondary action controls (no Merge/Iterate/Redraft), while genuinely failed missions and tickets still offer Redraft. *(command: `cd apps/dashboard && npx vitest run src/lib/pipelineStage.test.ts src/components/PipelineView.test.tsx`)*
- ✅ **[a2]** A ticket in state 'done' with no linked mission (direct-fixed) derives to a terminal inert stage ('landed'), rendering no UNMERGED badge and no Draft/Merge/Iterate control — it is neither 'captured' nor 'delivered'. *(command: `cd apps/dashboard && npx vitest run src/lib/pipelineStage.test.ts src/components/PipelineView.test.tsx`)*
- ✅ **[a3]** The api fetch layer (getJson and postJson) throws an ApiError whose message is exactly 'endpoint unavailable — server restart needed?' when a 200 response body is non-JSON HTML, never surfacing a raw JSON-parse SyntaxError; a valid application/json 200 still parses and returns normally. *(command: `cd apps/dashboard && npx vitest run src/lib/api.test.ts`)*
- ✅ **[a4]** The full dashboard test suite, TypeScript typecheck, and production build all pass together. *(command: `cd apps/dashboard && npm ci && npx tsc --noEmit && npm run test && npm run build`)*
- ✅ **[a5]** The dashboard sources lint clean under oxlint. *(command: `cd apps/dashboard && npm run lint`)*
- ✅ **[a6]** When an artifact fetch (plan.md / report.md / diff-stat) fails on a complete/Delivered row, the error message renders in its own dedicated layout slot beneath the row header and does not overlap or collide with the UNMERGED badge. *(agent judgement)*
- ✅ **[a7]** A mission row whose title is absent renders its full mission id with no left-side truncation or clipping — the entire id string is visible in a stable slot. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
