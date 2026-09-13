---
state: done
state-note: landed in 8da93ce (Craig); reviewed + tested + gates green
title: Merge gate omits dashboard test + lint (weaker than AGENTS.md / CI)
priority: 2
schedule: once
---

## Goal
merge_gate.rs DASHBOARD_GATES runs only `npm ci`, `npx tsc --noEmit`,
`npm run build` (~line 52) — missing `npm run test` (vitest) and
`npm run lint` (oxlint), which AGENTS.md requires and apps/dashboard/
package.json defines. A dashboard change can merge through the gated Merge
button with failing tests or lint. Add both to DASHBOARD_GATES, add them to
.github/workflows/ci.yml's dashboard job, and update the merge_gate↔ci.yml
pinning test so the two stay aligned by contract (they are asserted equal).

## Context
Found by a Codex code review 2026-07-07, citing the AGENTS.md added the same
day. Keep merge_gate.rs and ci.yml in lockstep (there is a test pinning them).

## Acceptance hints
- DASHBOARD_GATES includes npm run test and npm run lint after build; ci.yml
  dashboard job runs the same; the pinning test passes.
- cargo test --workspace green.
