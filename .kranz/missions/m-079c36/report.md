# Mission report — m-079c36

**Goal:** M7 tier 1: when workerIsolation=worktree, every worker and validator session runs in a dedicated git worktree and all mission-branch mutations happen in a mission integration worktree, so the primary checkout never changes branches and is byte-untouched across an entire mission; workerIsolation=checkout preserves today's behavior byte-for-byte and is the default this mission.

Branch `kranz/mission-m-079c36` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 2h 01m 52s
**Tokens:** 57482 in / 318107 out / 38315765 cache read / 1369344 cache write
**Cost:** $124.70 actual vs $12.22–$61.12 estimated (expected $24.45)

## What shipped

### Milestone 1 — Isolation config + mission integration-worktree primitive ✅

- ✅ **Add workerIsolation config (worktree|checkout, default checkout)** — 1 run
  - `2802998` [f-1-1] add workerIsolation mission-config key (worktree|checkout, default checkout)
- ✅ **Mission integration-worktree lifecycle primitive** — 1 run
  - `f8b0066` [f-1-2] add mission integration-worktree lifecycle primitive
  - `688a914` [f-1-2] checkpoint (engine commit)
- ✅ **ms-1 cleanup: derive WorkerIsolation Default, remove out-of-scope docs, cover resume-time worktree reaping** *(fix)* — 1 run
  - `3fd1ecd` [ms-1-fix-1-1] derive WorkerIsolation Default, remove out-of-scope docs, cover resume-time worktree reaping

### Milestone 2 — Route all mission-branch work through the integration worktree in worktree mode ✅

- ✅ **Sequential worker + run() loop route to the integration worktree** — 2 runs, 1 respawn
  - `b520da9` [f-2-1] verify integration-worktree routing, add worktree/checkout mode tests
- ✅ **Validators, parallel merges, milestone tags, and final gate route to the integration worktree** — 2 runs, 1 respawn
- ✅ **Committed artifacts + approval route to the worktree; deliverables stay readable** — 1 run
  - `5e98bbd` [f-2-3] route approve_plan and mission report commits through the integration worktree

### Milestone 3 — End-to-end guarantees, cleanup, and regression ✅

- ✅ **End-to-end worktree guarantees + checkout-mode regression + leak-free cleanup** — 1 run
  - `5f149cb` [f-3-1] end-to-end worktree guarantees, checkout-mode regression, leak-free cleanup
- ✅ **Strengthen ms-3 e2e coverage: worktree-mode approve_revised_plan test + positive parallel-engagement assertion** *(fix)* — 1 run
  - `dd4166d` [ms-3-fix-1-1] worktree-mode approve_revised_plan test + positive parallel-batch assertion

## Validation history

### ms-1 round 1 — Isolation config + mission integration-worktree primitive

- [minor] clippy cleanliness (allowed validation command `cargo clippy --workspace --tests`) — The milestone introduces a new clippy `derivable_impls` warning: crates/engine/src/types.rs:360 — `impl Default for WorkerIsolation { fn default() -> Self { WorkerIsolation::Checkout } }` triggers `wa… [truncated]
- [minor] f-1-2: resume() crash-recovery sweep reaps a leaked integration worktree — orchestrator.rs:408-412 adds new resume() logic that removes a leaked mission_worktree_path on lock acquisition, but no test exercises this path — setup_and_teardown_mission_worktree_round_trip covers… [truncated]
- [critical] a1 — cargo test --workspace --test worktree_isolation primary_checkout_untouched_in_worktree_mode 2>&1 output: 'running 0 tests ... test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered ou… [truncated]
- [critical] a2 — cargo test --workspace --test worktree_isolation worker_session_cwd_is_worktree 2>&1: 'running 0 tests ... test result: ok. 0 passed; ... 5 filtered out'. Test does not exist; grepping for 'test resul… [truncated]
- [critical] a3 — cargo test --workspace --test worktree_isolation validator_session_cwd_is_worktree 2>&1: 'running 0 tests ... test result: ok. 0 passed; ... 5 filtered out'. Test absent; same vacuous-pass issue as a1… [truncated]
- [critical] a5 — cargo test --workspace --test worktree_isolation checkout_mode_runs_worker_in_primary_root 2>&1: 'running 0 tests ... test result: ok. 0 passed; ... 5 filtered out'. Test absent.
- [critical] a6 — cargo test --workspace --test worktree_isolation mission_branch_carries_deliverables_in_worktree_mode 2>&1: 'running 0 tests ... test result: ok. 0 passed; ... 5 filtered out'. Test absent.
- [critical] a7 — cargo test --workspace --test worktree_isolation base_sha_reaches_sessions_in_worktree_mode 2>&1: 'running 0 tests ... test result: ok. 0 passed; ... 5 filtered out'. Test absent.
- [critical] a8 — cargo test --workspace --test worktree_isolation worktrees_removed_at_mission_end_in_worktree_mode 2>&1: 'running 0 tests ... test result: ok. 0 passed; ... 5 filtered out'. Test absent. Note: setup_m… [truncated]
- [major] a9 — Source inspection (agent-judgement): `grep -rn "worker_isolation" crates/engine/src/ | grep -v tests` shows the worker_isolation field is only defined/defaulted/read in types.rs (as a plain getter) an… [truncated]
- [minor] f-1-2 (mission_worktree_path/setup/teardown wiring) — The feature criteria for f-1-2 (git_ops has add_worktree_checkout; orchestrator has mission_worktree_path/setup_mission_worktree/teardown_mission_worktree; resume() reaps a leaked integration worktree… [truncated]

Disposition: 1 fix feature(s) created.

### ms-1 round 2 — Isolation config + mission integration-worktree primitive

- [minor] f-1-2 add_worktree_checkout tests — cargo fmt --check regression — cargo fmt --check reports a diff at crates/engine/tests/git_ops_test.rs:792 (and one just below it), on lines added by f-1-2 (`repo.add_worktree_checkout(&advance_wt, "feature-x").unwrap();` and `let … [truncated]
- [critical] a1 — `cargo test --workspace --test worktree_isolation primary_checkout_untouched_in_worktree_mode 2>&1` → `running 0 tests` / `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out`. … [truncated]
- [critical] a2 — `cargo test --workspace --test worktree_isolation worker_session_cwd_is_worktree` → `running 0 tests` / `test result: ok. 0 passed; 0 failed; ... 5 filtered out`. No such test exists.
- [critical] a3 — `cargo test --workspace --test worktree_isolation validator_session_cwd_is_worktree` → `running 0 tests` / `test result: ok. 0 passed; 0 failed; ... 5 filtered out`. No such test exists.
- [critical] a5 — `cargo test --workspace --test worktree_isolation checkout_mode_runs_worker_in_primary_root` → `running 0 tests` / `test result: ok. 0 passed; 0 failed; ... 5 filtered out`. No such test exists.
- [critical] a6 — `cargo test --workspace --test worktree_isolation mission_branch_carries_deliverables_in_worktree_mode` → `running 0 tests` / `test result: ok. 0 passed; 0 failed; ... 5 filtered out`. No such test ex… [truncated]
- [critical] a7 — `cargo test --workspace --test worktree_isolation base_sha_reaches_sessions_in_worktree_mode` → `running 0 tests` / `test result: ok. 0 passed; 0 failed; ... 5 filtered out`. No such test exists.
- [critical] a8 — `cargo test --workspace --test worktree_isolation worktrees_removed_at_mission_end_in_worktree_mode` → `running 0 tests` / `test result: ok. 0 passed; 0 failed; ... 5 filtered out`. No such test exist… [truncated]
- [critical] a9 — Read-only code audit of crates/engine/src/orchestrator.rs: `WorkerIsolation`/`isolation()` (types.rs:355,390,446) is never consulted in orchestrator.rs. run() (line ~1093-1105) unconditionally calls `… [truncated]
- [minor] cargo fmt --check — `cargo fmt --check` reports a diff in crates/engine/tests/git_ops_test.rs:792, inside code added by this milestone's diff (the new `add_worktree_checkout_existing_branch_at_its_tip` test, confirmed vi… [truncated]

Disposition: waived.
- a1: Premature: full-mission primary-untouched behavior is implemented by f-2-1/f-2-3 + f-3-1 (ms-2/ms-3); its named test is required by those features and re-run at final gate. Cannot be satisfied inside the primitives milestone.
- a2: Worker-cwd worktree routing is f-2-1 (ms-2) scope; worker_session_cwd_is_worktree is in that feature's validationCriteria.
- a3: Validator-cwd worktree routing is f-2-2 (ms-2) scope; validator_session_cwd_is_worktree is required there.
- a5: checkout_mode_runs_worker_in_primary_root regression test is authored in f-2-1/f-3-1 alongside the run-path branch it guards; nothing to regress-test until routing exists.
- a6: Mission-branch merge-back deliverables are f-2-2/f-3-1 scope; the named test lands with the merge routing.
- a7: KRANZ_BASE_SHA-under-worktree verification is f-2-2 scope (validators/final-gate re-pointing); not implementable before routing.
- a8: End-of-mission teardown is wired into the run lifecycle in ms-2 and leak-tested in f-3-1; the primitive exists now but the lifecycle call is later scope by design.
- a9: The audit correctly shows routing isn't wired yet — that IS ms-2/ms-3's job (f-2-1/f-2-2/f-2-3 wire run()/approve_plan/merge-back/final_gate); judged for real at final gate. Not an ms-1 defect.
- f-1-2 add_worktree_checkout tests — cargo fmt --check regression: Out-of-contract (no assertion covers cargo fmt) and cosmetic; trivial line-wrap that f-3-1's cleanup will fix by running cargo fmt before merge.
- cargo fmt --check: Duplicate of the fmt regression above — out-of-contract cosmetic; folded into f-3-1 cleanup (run cargo fmt on git_ops_test.rs) so the branch is fmt-clean for the CI gate at merge.

### ms-2 round 1 — Route all mission-branch work through the integration worktree in worktree mode

- [critical] a8 — worktrees_removed_at_mission_end_in_worktree_mode — The contract command `cargo test --workspace --test worktree_isolation worktrees_removed_at_mission_end_in_worktree_mode 2>&1 | grep -qE 'test result: ok\.'` references a test that does not exist. gre… [truncated]
- [critical] a8 — Command: `cargo test --workspace --test worktree_isolation worktrees_removed_at_mission_end_in_worktree_mode 2>&1 | grep -qE 'test result: ok\.'` exits 0, but `cargo test --workspace --test worktree_i… [truncated]

Disposition: waived.
- a8 — worktrees_removed_at_mission_end_in_worktree_mode: Premature for ms-2: the teardown-at-mission-end behavior IS implemented (teardown_mission_worktree in run() + prune, confirmed by the validator at orchestrator.rs:1965); only the dedicated leak test is missing, and it is explicitly owned by f-3-1 (ms-3), whose spec/validationCriteria already name worktrees_removed_at_mission_end_in_worktree_mode. Creating it here would duplicate the next milestone's charter.
- a8: Duplicate of the finding above — same vacuous-pass on the same absent test. Waived on the same basis: behavior implemented, test is f-3-1's scope; ms-3 validation and the final gate will verify it for real.

### ms-3 round 1 — End-to-end guarantees, cleanup, and regression

- [minor] a1 / f-3-1 primary-checkout-untouched — approve_revised_plan worktree path — The only src change in this milestone (orchestrator.rs:1067-1113) adds a brand-new worktree-mode branch to approve_revised_plan that sets up a mission worktree, writes+commits revised-plan.md there, t… [truncated]
- [minor] f-3-1 'exercises BOTH the sequential and parallel paths' — worktrees_removed_at_mission_end_in_worktree_mode — worktree_isolation.rs:799-806 asserts the per-feature branches kranz/wt/{mission}/f-1-1 and f-1-2 do NOT exist ('must be deleted'), but nothing first asserts they were ever created. If the parallel-ba… [truncated]

Disposition: 1 fix feature(s) created.

### ms-3 round 2 — End-to-end guarantees, cleanup, and regression

No findings.

## Contract outcomes

- ✅ **[a1]** With workerIsolation=worktree, the primary checkout is left byte-untouched across an entire mission (plan approval through completion): its HEAD sha, its current branch, and its tracked-tree porcelain status are identical before the mission starts and after it ends. *(command: `cargo test --workspace --test worktree_isolation primary_checkout_untouched_in_worktree_mode 2>&1 | grep -qE 'test result: ok\.'`)*
- ✅ **[a2]** With workerIsolation=worktree, a sequential worker session is spawned with a cwd that is the mission integration worktree directory, never the primary repo root. *(command: `cargo test --workspace --test worktree_isolation worker_session_cwd_is_worktree 2>&1 | grep -qE 'test result: ok\.'`)*
- ✅ **[a3]** With workerIsolation=worktree, validator sessions are spawned with a cwd inside a git worktree, never the primary repo root. *(command: `cargo test --workspace --test worktree_isolation validator_session_cwd_is_worktree 2>&1 | grep -qE 'test result: ok\.'`)*
- ✅ **[a4]** The workerIsolation config key parses from camelCase JSON, defaults to "checkout" when absent, accepts exactly "worktree" and "checkout", and is rejected by config loading/validation for any other value. *(command: `cargo test --workspace --test worktree_isolation worker_isolation_config 2>&1 | grep -qE 'test result: ok\.'`)*
- ✅ **[a5]** With workerIsolation=checkout (the default), the sequential run path is behaviorally unchanged: workers run with cwd = primary repo root and the mission branch is checked out in the primary tree, exactly as before this mission. *(command: `cargo test --workspace --test worktree_isolation checkout_mode_runs_worker_in_primary_root 2>&1 | grep -qE 'test result: ok\.'`)*
- ✅ **[a6]** With workerIsolation=worktree, the mission branch accumulates the mission's feature commits/merges (deliverables land on the mission branch) even though the primary checkout never checked that branch out. *(command: `cargo test --workspace --test worktree_isolation mission_branch_carries_deliverables_in_worktree_mode 2>&1 | grep -qE 'test result: ok\.'`)*
- ✅ **[a7]** With workerIsolation=worktree, KRANZ_BASE_SHA still reaches the worker session env, the validator session env, and the final-gate contract-command env (the base-sha pin is not regressed by the worktree routing). *(command: `cargo test --workspace --test worktree_isolation base_sha_reaches_sessions_in_worktree_mode 2>&1 | grep -qE 'test result: ok\.'`)*
- ✅ **[a8]** With workerIsolation=worktree, the mission integration worktree and every per-feature worktree/branch are removed and pruned by mission end, leaving no leaked worktrees registered on the repo. *(command: `cargo test --workspace --test worktree_isolation worktrees_removed_at_mission_end_in_worktree_mode 2>&1 | grep -qE 'test result: ok\.'`)*
- ✅ **[a9]** With workerIsolation=worktree, no code path in run(), approve_plan, the parallel merge-back, validation, or the final gate checks out the mission branch in, commits to, or otherwise mutates the tracked contents of the primary checkout; the primary tree is used only for gitignored runtime state (events.jsonl/state.json/runs/control). *(agent judgement)*
- ✅ **[a10]** The entire workspace test suite passes, so the worktree routing introduces no regressions elsewhere in the engine. *(command: `cargo test --workspace 2>&1 | grep -qE 'test result: ok\.'`)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
