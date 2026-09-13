# Mission report — m-642a1a

**Goal:** Add a Start action to the mission page's management row for an approved-idle mission (approved plan, no live run), wired to POST /api/missions/:id/start, surfacing the host's conflict message on a race.

Branch `kranz/mission-m-642a1a` (from `kranz/mission-m-468277`). Plan of record: [plan.md](plan.md).

**Elapsed:** 1h 09m 55s
**Tokens:** 17892 in / 91275 out / 7597066 cache read / 370637 cache write
**Cost:** $22.83 actual vs $4.07–$20.37 estimated (expected $8.15)

## What shipped

### Milestone 1 — Start an approved-idle mission from its page ✅

- ✅ **Test harness + approved-idle predicate** — 2 runs, 1 respawn
  - `bee1ff2` [f-1-1] fix: isApprovedIdle should key off MissionStatus::Approved
- ✅ **Wire Start into StatusStrip with host-conflict surfacing** — 1 run
  - `985e76d` [f-1-2] wire Start into StatusStrip with host-conflict surfacing

## Validation history

### ms-1 round 1 — Start an approved-idle mission from its page

- [critical] [a3] / [f-1-1] — isApprovedIdle predicate semantics — Contract [a3] and criterion [f-1-1] require isApprovedIdle to return true iff mission status is 'running' with zero worker runs, and false for all non-running statuses. The shipped predicate (apps/das… [truncated]
- [major] a3 / f-1-1 contract wording vs implementation — apps/dashboard/src/lib/startAffordance.ts: `export function isApprovedIdle(state) { return state.mission.status === 'approved'; }`. The contract (a3) states the predicate should return true iff status… [truncated]

Disposition: waived.
- [a3] / [f-1-1] — isApprovedIdle predicate semantics: Waived as stale contract wording, not a code defect: the engine folds plan.approved→Approved and Approved→Running on the first milestone.started/worker.spawned (reducer.rs:82,92-93,146-147), so approved-idle is precisely status==='approved' and 'running with zero runs' is unreachable. Both validators concur the shipped predicate is the functionally-correct one; reverting to the a3/f-1-1 literal wording would make Start never render — a regression against the mission goal. The a3 command still passes; only the assertion prose (drafted before confirming MissionStatus::Approved had landed in the base) is obsolete.
- a3 / f-1-1 contract wording vs implementation: Duplicate of the above — same wording-vs-implementation drift. Waived for the same reason: MissionStatus enumerates 'approved' as distinct from 'running' (types.rs / dashboard types.ts), the implemented status==='approved' check is correct and validator-endorsed, and no code change is warranted.

## Contract outcomes

- ✅ **[a1]** The dashboard type-checks cleanly, including the new predicate, store changes, and test files. *(command: `cd apps/dashboard && npx tsc --noEmit`)*
- ✅ **[a2]** The dashboard production build passes. *(command: `cd apps/dashboard && npm run build`)*
- ✅ **[a3]** The approved-idle predicate returns true iff the mission status is 'running' with zero worker runs, and false for a running mission that has worker runs and for every non-running status (planning, paused, blocked, validating, complete, failed, abandoned). *(command: `cd apps/dashboard && npx vitest run src/lib/startAffordance.test.ts`)*
- ✅ **[a4]** StatusStrip renders a Start control for an approved-idle mission and no Start control for a running mission that has worker runs; clicking Start when the host rejects it (409) renders the host's conflict message in the management row rather than failing silently. *(command: `cd apps/dashboard && npx vitest run src/components/StatusStrip.test.tsx`)*
- ✅ **[a5]** On the double-start race the host's conflict text is presented legibly to the user in the management row (not only logged to the console or swallowed), and double-start remains refused by the host — no engine/host behavior was changed to enforce it client-side. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
