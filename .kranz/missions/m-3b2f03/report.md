# Mission report — m-3b2f03

**Goal:** Add a human-triggered, token/allowlist-gated Merge action at the Delivered stage on both the dashboard and Slack that refuses on a dirty tracked tree, runs the full CI gate suite, merges --no-ff into the base branch on green, surfaces the failing gate verbatim on red, and never pushes.

Branch `kranz/mission-m-3b2f03` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 2h 56m 02s
**Tokens:** 63995 in / 161693 out / 20801070 cache read / 852357 cache write
**Cost:** $83.18 actual vs $10.20–$51.00 estimated (expected $20.40)

## What shipped

### Milestone 1 — The gated merge, end-to-end via REST ✅

- ✅ **Gate-suite runner (pure, injectable)** — 1 run
  - `553c4de` [f-1-1] add pure, injectable gate-suite runner
- ✅ **Dashboard-touched diff probe + merge orchestration** — 1 run
  - `617494d` [f-1-2] add dashboard-touched diff probe and gated merge orchestration
- ✅ **MissionHost::merge + token-gated POST /api/missions/:id/merge** — 1 run
  - `2702558` [f-1-3] add MissionHost::merge + token-gated POST /api/missions/:id/merge

### Milestone 2 — Merge on both human surfaces ✅

- ✅ **Dashboard Delivered panel: report + diff + Merge button** — 1 run
  - `5d3a9a8` [f-2-1] add Delivered panel: report + diff-stat + gated Merge button
- ✅ **Slack Delivered card: report summary + diff stat + deep link + Merge button** — 2 runs, 1 respawn
  - `a6bd610` [f-2-2] fix kranz-slack compile: missing diff_stat field and classify(repo_root) arg
- ✅ **Slack /kranz merge verb + Merge button, allowlist-gated via PlanningHost seam** — 2 runs, 1 respawn
  - `7455f6a` [f-2-3] add Action::Merge to no-op exhaustive match arm in bridge.rs

## Validation history

### ms-1 round 1 — The gated merge, end-to-end via REST

- [critical] a8/a9 (kranz-slack merge verb + Delivered card Merge button) — cargo test -p kranz-slack passes 13+4+26 tests, all pre-existing (slash_command_ticket, work_run, tickets.rs, etc.). `grep -rn 'merge|Merge' crates/slack/src` returns no matches at all — there is no /… [truncated]
- [critical] a10 (dashboard Merge button, verbatim gate-failure display) — `cd apps/dashboard && npm ci && npx tsc --noEmit && npm run test && npm run build` all succeed (tsc clean, 7 test files / 38 tests pass, vite build succeeds), but grep for merge/Merge in apps/dashboar… [truncated]
- [minor] a6/a7 (POST /api/missions/:id/merge token gating and outcomes) — cargo test -p kranz-server passes all 68 tests including merge_route_requires_token, merge_route_merges_on_green_gates_and_flips_the_merged_bit, merge_route_surfaces_a_failing_gates_verbatim_output_an… [truncated]

Disposition: waived.
- a8/a9 (kranz-slack merge verb + Delivered card Merge button): Not an ms-1 defect — a8/a9 are ms-2 assertions delivered by planned pending features f-2-2 (Slack Delivered card) and f-2-3 (/kranz merge verb + button); the whole-contract validator ran them early against unbuilt code. No fix-feature; f-2-2/f-2-3 will implement them.
- a10 (dashboard Merge button, verbatim gate-failure display): Not an ms-1 defect — a10 is an ms-2 assertion delivered by planned pending feature f-2-1 (Dashboard Delivered panel + Merge button); flagged early because the whole contract runs at every milestone gate. f-2-1 will implement it.
- a6/a7 (POST /api/missions/:id/merge token gating and outcomes): Validator itself states 'no fix needed' — a6/a7 are fully satisfied by four passing server_test.rs tests (token 401, green+merged-bit flip, verbatim gate failure, dirty-tree refusal); flagged only for milestone-verdict completeness.

### ms-2 round 1 — Merge on both human surfaces

No findings.

## Contract outcomes

- ✅ **[a1]** The engine merge orchestration refuses to merge when the working tree has a dirty TRACKED file: it does not run any gate and does not create a merge commit, returning a refusal outcome. *(command: `cargo test -p kranz-engine`)*
- ✅ **[a2]** When every gate in the suite passes, the merge orchestration integrates the mission branch into its base branch with a --no-ff merge commit (a commit with two parents) such that the mission-branch tip becomes an ancestor of the base-branch tip. *(command: `cargo test -p kranz-engine`)*
- ✅ **[a3]** When a gate fails, the merge orchestration returns that gate's verbatim captured output, runs no subsequent gate, creates no merge commit, and leaves the base branch tip unchanged. *(command: `cargo test -p kranz-engine`)*
- ✅ **[a4]** The dashboard tsc+build gate is included in the suite only when the mission diff (base_sha..mission_branch) touches a path under apps/dashboard/, and is omitted otherwise. *(command: `cargo test -p kranz-engine`)*
- ✅ **[a5]** No code path in the merge orchestration ever invokes `git push` under any outcome (refused, gate-failed, conflict, or merged); merging is a purely local operation. *(command: `cargo test -p kranz-engine`)*
- ✅ **[a6]** POST /api/missions/:id/merge is rejected (401) when the x-kranz-token header is missing or wrong, and is accepted when the correct token is supplied. *(command: `cargo test -p kranz-server`)*
- ✅ **[a7]** The merge endpoint returns a merged result on green (after which the missions-list `merged` bit reads true for that mission) and, on a failing gate, returns a non-2xx response whose body carries the failing gate's verbatim output without advancing the base branch. *(command: `cargo test -p kranz-server`)*
- ✅ **[a8]** The Slack `/kranz merge <slug|id>` slash verb and the Delivered card's Merge button are denied for a user not on the spend allowlist (no host merge call is made) and, when the user is authorized, trigger the merge through the in-process PlanningHost seam (like work run) rather than on the socket read loop. *(command: `cargo test -p kranz-slack`)*
- ✅ **[a9]** The Slack Delivered card built for a Complete-and-unmerged mission contains the report summary, the diff stat, an Open-in-dashboard deep link, and a Merge button. *(command: `cargo test -p kranz-slack`)*
- ✅ **[a10]** The dashboard type-checks, builds, and its unit tests pass, covering: a Merge button that appears only for a Complete-and-unmerged mission and POSTs to the merge endpoint, inline rendering of report.md and the diff stat at the Delivered affordance, and verbatim display of a failed gate's output. *(command: `cd apps/dashboard && npm ci && npx tsc --noEmit && npm run test && npm run build`)*
- ✅ **[a11]** Across the full mission diff, the standing 'kranz never pushes' rule is preserved: nothing in the merge feature calls, enables, or reaches the push path on any surface. *(agent judgement)*
- ✅ **[a12]** The operator-facing Delivered surfaces (dashboard panel and Slack card) present the report, the diff stat, and an explicit unmerged distinction with a single Merge action per the design of record (docs/scoping/pipeline-view.md D-C), and a gate failure is shown to the operator verbatim rather than as a generic error. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
