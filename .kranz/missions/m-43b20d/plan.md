# Mission plan — m-43b20d

**Goal:** A successful approve on any surface consumes the host-parked plan, and the dashboard approves through the unified pending-plan cache instead of posting a client-side copy.

Branch `kranz/mission-m-43b20d` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$3.68 – $18.40** (expected ~$7.36). Rough estimate — live usage is authoritative; based on 19 completed mission(s).

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** A direct POST /api/missions/:id/approve (the old web path, plan in body) commits the plan AND clears the host-parked plan: afterwards GET /pending-plan returns pending:false and a follow-up POST /approve-pending returns 409 (honest 'no reviewed plan pending'), never a stale re-approve error — so a later Slack /kranz approve cannot retry a consumed plan. 
  `set -o pipefail; cargo test -p kranz-server --test host_test approve_clears_parked_plan 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a2]** The dashboard approve action issues its request to the approve-pending endpoint (via api.approvePending), not to the /approve endpoint — verified by a mocked-api network assertion in a store/handler test. 
  `cd apps/dashboard && set -o pipefail && npx vitest run src/lib/store.approve.test.ts 2>&1 | grep -qE '[1-9][0-9]* passed'`
- **[a3]** The full Rust workspace test suite passes, with a passed-count guard so a compile-that-runs-zero-tests cannot masquerade as success. 
  `set -o pipefail; cargo test --workspace 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a4]** The dashboard TypeScript project typechecks with no errors after the api/store changes. 
  `cd apps/dashboard && npx tsc --noEmit`

## Milestone 1 — A successful approve always consumes the parked plan; the dashboard approves through the unified cache

### 1.1 MissionHost::approve clears the host-parked plan on success

In crates/server/src/host.rs, MissionHost::approve (around line 445) commits the plan via `engine.approve_plan(plan)?` but never clears the mission's parked `pending_plan` slot, so a direct POST /api/missions/:id/approve leaves a stale copy that a later Slack /kranz approve (via try_approve_pending, host.rs:863) will re-run against an already-approved mission and error.

CHANGE: after `engine.approve_plan(plan)?` returns Ok (and only on success), clear the parked plan for this mission. The helper `set_pending_plan(id, Some/None)` already exists (host.rs:851) and takes the `missions` registry lock — which is INDEPENDENT of the engine `cell` lock — so call `self.set_pending_plan(id, None)` after dropping/finishing with the engine guard to avoid holding two locks at once. The registry entry is still `HostedMission::Planning` at this point (only `start` consumes the engine), so `set_pending_plan` will match and clear it. Do not change the returned branch value or any error behaviour. Note: the try_approve_pending path already `.take()`s the plan before calling approve, so this extra clear is idempotent there (already None) and harmless — it exists to close the DIRECT /approve path.

TEST: add a host-level integration test in crates/server/tests/host_test.rs named `approve_clears_parked_plan_so_no_stale_slack_approve` (any name beginning `approve_clears_parked_plan`). Model it closely on the existing `pending_plan_parks_on_ready_and_approve_pending_commits` test in that file (same harness: `setup()` early-return guard, `init_repo()`, `MockScript`/`MockBackend`/`hosted_app`, and the `post_json`/`get_json`/`TOKEN` helpers). Sequence:
  1. POST /api/missions to create a hosted mission; capture id.
  2. POST /api/missions/:id/planning/turn once, then POST /api/missions/:id/planning/request-plan and assert 200 with ready:true (this parks the plan). Assert GET /api/missions/:id/pending-plan returns pending:true.
  3. POST /api/missions/:id/approve with body `{ "plan": <the plan from the request-plan response's body["plan"]> }` — this reproduces the OLD dashboard web path. Assert 200 and body["branch"] == format!("kranz/mission-{id}").
  4. Assert GET /api/missions/:id/pending-plan now returns pending:false (the parked plan was cleared).
  5. Assert a follow-up POST /api/missions/:id/approve-pending returns StatusCode::CONFLICT (409) — the honest 'no reviewed plan pending' state, proving a later Slack approve cannot retry a stale plan.

Do NOT weaken any existing test. Completion gate: `cargo test -p kranz-server --test host_test` COMPILES and passes (not just the new test).

Done when:
- After a successful POST /approve, GET /pending-plan for that mission returns pending:false.
- A POST /approve-pending issued after a successful /approve returns HTTP 409 (not a 2xx re-approve, not a 500).
- The new test named approve_clears_parked_plan* runs and passes, and `cargo test -p kranz-server --test host_test` compiles and is green.

### 1.2 Dashboard approves via approve-pending{start:false} through the unified cache

Switch the dashboard's plan-approval to consume the host-parked plan instead of posting a client-side copy.

1. apps/dashboard/src/lib/api.ts: add a new method `approvePending(id: string): Promise<{ branch: string; started: boolean }>` that POSTs to `/api/missions/${encodeURIComponent(id)}/approve-pending` with body `{ start: false }` (use the existing `postJson` helper, exactly like the neighbouring lifecycle methods). The server route (crates/server/src/host.rs approve_pending_route) returns `{ branch, started }`.
2. apps/dashboard/src/lib/store.ts: change the `approvePlan` store action (~line 490) so that instead of `api.approvePlan(id, review.plan)` it calls `api.approvePending(id)`. Keep the action's no-arg signature, keep the leading `if (id === null || review === null) return;` guard (the local `review` still gates the Approve button in PlanReview.tsx; the server's parked copy is what actually commits), keep `patchPlanning({ error: null })`, and on success still read `{ branch }` and set `patchPlanning({ approvedBranch: branch })`. Keep the existing `.catch` failPlanning behaviour and the `get().missionId !== id` staleness guard unchanged.
3. Remove the now-unused `api.approvePlan` method from api.ts (a repo-wide grep confirms the store action at store.ts was its only caller). If removing it surfaces any other reference, update that reference instead of leaving dead code. Ensure `npx tsc --noEmit` stays clean.

TEST: add a vitest test file apps/dashboard/src/lib/store.approve.test.ts modeled on the existing apps/dashboard/src/lib/store.tickets.test.ts (same `vi.mock('./api', …)` pattern, importing `{ api }` after the mock, resetting mocks in beforeEach). In the mocked `api` object include `approvePending: vi.fn()` (returning a resolved `{ branch: 'kranz/mission-m-test', started: false }`). Drive the store via `useKranzStore.setState` into a state where `missionId` is a valid id and `planning.review` holds a plan+estimate, invoke `useKranzStore.getState().approvePlan()`, await the pending promise, and assert: `api.approvePending` was called once with the mission id; and that the /approve path is NOT taken (if you keep an `approvePlan` mock at all, assert it was not called — or simply assert approvePending is the method invoked). This is the network assertion required by the acceptance hints.

Run tests from apps/dashboard with vitest; ensure `npx tsc --noEmit` passes.

Done when:
- Invoking the approvePlan store action calls api.approvePending with the mission id and does not call the /approve method (asserted via mocked api).
- `npx vitest run src/lib/store.approve.test.ts` in apps/dashboard runs at least one test and passes.
- `npx tsc --noEmit` in apps/dashboard exits 0 with no errors after the api.ts and store.ts changes.

