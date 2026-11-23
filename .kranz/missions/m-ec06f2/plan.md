# Mission plan — m-ec06f2

**Goal:** Make every fix feature id unique within its milestone, whatever created it (validator finding, final gate, tier escalation, or operator-directed unblock-add-fix repair), so a validator finding after a repair gets its own fix feature and the mission carries on, without changing fix-cycle spending, the event schema, or replay of existing logs.

Branch `kranz/mission-m-ec06f2` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Authority granted on approval

Approving this plan grants the following to the mission's sessions with no further prompt.

**Write paths** (3; glob patterns the mission may change):
- `crates/engine/src/findings.rs`
- `crates/engine/src/orchestrator.rs`
- `crates/engine/tests/mission_test.rs`

## Cost estimate

Estimated **$3.86 – $19.28** (expected ~$8.83). Rough estimate — live usage is authoritative; based on 50 completed mission(s).

Context-fit check (corpus p90 over 61 plan(s): 4556 chars / 24 files):
- **f-1-1** (Next-free-cycle rule for fix feature ids, with unit tests) — spec is 5608 chars (corpus p90: 4556); spec points at 25 files (corpus p90: 24)

## Considered alternatives

**Chosen approach:** A single next-free-cycle rule inside emit_fix_features, the one function every fix path goes through: cycle = max(fix_cycles+1, 1 + highest existing fix-id cycle). It reproduces today's ids whenever no repair or escalation has happened, so existing logs and their continuations are unchanged. It covers repairs, validator fixes, final-gate fixes and the post-escalation reset with one tested rule, and it needs no schema or reducer change.

Rejected shapes:
- **Give repairs their own `{milestone_id}-repair-{n}` namespace** — More readable, but it leaves the tier-escalation reset (fix_cycles back to 0) still reusing `-fix-1-*`, so a second guard would be needed anyway: two rules instead of one.
- **Make unblock-add-fix spend a fix cycle, or stop TierEscalated resetting fix_cycles** — The scoping answers forbid changing repair cycle spending, and touching the reducer's fold would alter the state that existing logs replay to.
- **Have the reducer rename or accept colliding fixfeature.created events** — It changes replay semantics for existing logs and weakens the duplicate guard that protects started work.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** Anti-vacuity: the test filter `fix_id_unique` matches at least 5 tests in kranz-engine, and the name did not exist anywhere under crates/ at the mission base commit, so every match is a new test.
  `list=$(cargo test -p kranz-engine fix_id_unique -- --list 2>/dev/null | grep ': test$'); printf '%s\n' "$list"; n=$(printf '%s\n' "$list" | grep -c 'fix_id_unique.*: test$'); [ "$n" -ge 5 ] && ! git grep -q fix_id_unique "$KRANZ_BASE_SHA" -- crates`
- **[a2]** All the new fix_id_unique tests pass.
  `cargo test -p kranz-engine fix_id_unique`
- **[a3]** Mission-level regression for m-882080: a milestone blocks, the orchestrator chooses unblock-add-fix, the repair runs and passes, the next validation round raises a finding, the finding becomes a fix feature whose id differs from the repair's, and the mission reaches Complete.
  `cargo test -p kranz-engine --test mission_test fix_id_unique_repair_then_finding_completes -- --list 2>/dev/null | grep -q 'fix_id_unique_repair_then_finding_completes: test$' && cargo test -p kranz-engine --test mission_test fix_id_unique_repair_then_finding_completes`
- **[a4]** Two unblock-add-fix repairs on one milestone get different feature ids, and neither spends a fix cycle.
  `cargo test -p kranz-engine fix_id_unique_two_repairs_get_distinct_ids -- --list 2>/dev/null | grep -q 'fix_id_unique_two_repairs_get_distinct_ids: test$' && cargo test -p kranz-engine fix_id_unique_two_repairs_get_distinct_ids`
- **[a5]** Replaying a log that holds a validator-driven fix (ms-1-fix-1-1) and no repair folds to the same feature ids as today, and the next validator-driven fix for that milestone gets the same id today's code gives (ms-1-fix-2-1).
  `cargo test -p kranz-engine fix_id_unique_legacy_log_replays_unchanged -- --list 2>/dev/null | grep -q 'fix_id_unique_legacy_log_replays_unchanged: test$' && cargo test -p kranz-engine fix_id_unique_legacy_log_replays_unchanged`
- **[a6]** After a local-tier milestone has used fix cycles 1 and 2 (ms-1-fix-1-1 and ms-1-fix-2-1 exist) and the cap triggers a tier escalation, which resets fix_cycles to 0, the fix features created in the same round get a fresh id (ms-1-fix-3-1) with no duplicate-fixfeature error.
  `cargo test -p kranz-engine fix_id_unique_after_tier_escalation -- --list 2>/dev/null | grep -q 'fix_id_unique_after_tier_escalation: test$' && cargo test -p kranz-engine fix_id_unique_after_tier_escalation`
- **[a7]** The id rule never yields an id already held by a feature of the milestone, and with no repair or escalation it yields exactly `{milestone_id}-fix-{fix_cycles+1}-{n}` as before.
  `cargo test -p kranz-engine fix_id_unique_rule_never_reuses_existing_id -- --list 2>/dev/null | grep -q 'fix_id_unique_rule_never_reuses_existing_id: test$' && cargo test -p kranz-engine fix_id_unique_rule_never_reuses_existing_id`
- **[a8]** An operator-directed repair still does not spend a fix cycle: the existing unblock-add-fix integration test passes unchanged.
  `cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation -- --list 2>/dev/null | grep -q 'unblock_add_fix_schedules_repair_before_revalidation: test$' && cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation`
- **[a9]** The event schema and the fold are untouched (events.rs, types.rs and reducer.rs are unchanged from the base commit), so replaying any existing log gives the same state as today.
  `git diff --quiet "$KRANZ_BASE_SHA" -- crates/engine/src/events.rs crates/engine/src/types.rs crates/engine/src/reducer.rs`
- **[a10]** The whole workspace test suite passes.
  `cargo test --workspace`
- **[a11]** The workspace is lint-clean and formatted.
  `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
- **[a12]** The change only fixes how fix feature ids are chosen. It adds no command, flag, or code path to recover stuck missions (including m-882080), and it does not change when fix cycles are spent. *(agent judgement)*
## Contract lint

Each `check: command` assertion above was run once against the untouched base tree at approval time. Suspects are assertions that already pass (or could not reach a verdict) before this plan's work lands — a possible polarity/vacuity bug in the assertion itself. This never blocks approval.

author-bug suspects (already pass / no verdict on the untouched base): [a2] cargo test -p kranz-engine fix_id_unique; [a8] cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation -- --list 2>/dev/null | grep -q 'unblock_add_fix_schedules_repair_before_revalidation: test$' && cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation; [a9] git diff --quiet "$KRANZ_BASE_SHA" -- crates/engine/src/events.rs crates/engine/src/types.rs crates/engine/src/reducer.rs
base-expected-to-fail (benign): [a1] list=$(cargo test -p kranz-engine fix_id_unique -- --list 2>/dev/null | grep ': test$'); printf '%s\n' "$list"; n=$(printf '%s\n' "$list" | grep -c 'fix_id_unique.*: test$'); [ "$n" -ge 5 ] && ! git grep -q fix_id_unique "$KRANZ_BASE_SHA" -- crates; [a3] cargo test -p kranz-engine --test mission_test fix_id_unique_repair_then_finding_completes -- --list 2>/dev/null | grep -q 'fix_id_unique_repair_then_finding_completes: test$' && cargo test -p kranz-engine --test mission_test fix_id_unique_repair_then_finding_completes; [a4] cargo test -p kranz-engine fix_id_unique_two_repairs_get_distinct_ids -- --list 2>/dev/null | grep -q 'fix_id_unique_two_repairs_get_distinct_ids: test$' && cargo test -p kranz-engine fix_id_unique_two_repairs_get_distinct_ids; [a5] cargo test -p kranz-engine fix_id_unique_legacy_log_replays_unchanged -- --list 2>/dev/null | grep -q 'fix_id_unique_legacy_log_replays_unchanged: test$' && cargo test -p kranz-engine fix_id_unique_legacy_log_replays_unchanged; [a6] cargo test -p kranz-engine fix_id_unique_after_tier_escalation -- --list 2>/dev/null | grep -q 'fix_id_unique_after_tier_escalation: test$' && cargo test -p kranz-engine fix_id_unique_after_tier_escalation; [a7] cargo test -p kranz-engine fix_id_unique_rule_never_reuses_existing_id -- --list 2>/dev/null | grep -q 'fix_id_unique_rule_never_reuses_existing_id: test$' && cargo test -p kranz-engine fix_id_unique_rule_never_reuses_existing_id; [a10] cargo test --workspace
not linted (never run on the base): [a11] cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check

Lint commands ran under shell: sh

named contract gates (defect classes):
- vacuous-filter: FAIL
  [a3] test-runner pipeline's grep anchors no nonzero count (missing the `[1-9]` guard — zero matching tests still print `test result: ok.`): `cargo test -p kranz-engine --test mission_test fix_id_unique_repair_then_finding_completes -- --list 2>/dev/null | grep -q 'fix_id_unique_repair_then_finding_completes: test$' && cargo test -p kranz-engine --test mission_test fix_id_unique_repair_then_finding_completes`
  [a4] test-runner pipeline's grep anchors no nonzero count (missing the `[1-9]` guard — zero matching tests still print `test result: ok.`): `cargo test -p kranz-engine fix_id_unique_two_repairs_get_distinct_ids -- --list 2>/dev/null | grep -q 'fix_id_unique_two_repairs_get_distinct_ids: test$' && cargo test -p kranz-engine fix_id_unique_two_repairs_get_distinct_ids`
  [a5] test-runner pipeline's grep anchors no nonzero count (missing the `[1-9]` guard — zero matching tests still print `test result: ok.`): `cargo test -p kranz-engine fix_id_unique_legacy_log_replays_unchanged -- --list 2>/dev/null | grep -q 'fix_id_unique_legacy_log_replays_unchanged: test$' && cargo test -p kranz-engine fix_id_unique_legacy_log_replays_unchanged`
  [a6] test-runner pipeline's grep anchors no nonzero count (missing the `[1-9]` guard — zero matching tests still print `test result: ok.`): `cargo test -p kranz-engine fix_id_unique_after_tier_escalation -- --list 2>/dev/null | grep -q 'fix_id_unique_after_tier_escalation: test$' && cargo test -p kranz-engine fix_id_unique_after_tier_escalation`
  [a7] test-runner pipeline's grep anchors no nonzero count (missing the `[1-9]` guard — zero matching tests still print `test result: ok.`): `cargo test -p kranz-engine fix_id_unique_rule_never_reuses_existing_id -- --list 2>/dev/null | grep -q 'fix_id_unique_rule_never_reuses_existing_id: test$' && cargo test -p kranz-engine fix_id_unique_rule_never_reuses_existing_id`
  [a8] test-runner pipeline's grep anchors no nonzero count (missing the `[1-9]` guard — zero matching tests still print `test result: ok.`): `cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation -- --list 2>/dev/null | grep -q 'unblock_add_fix_schedules_repair_before_revalidation: test$' && cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation`
  [a8] cargo test filter `unblock_add_fix_schedules_repair_before_revalidation` collides with an existing test at crates/engine/tests/mission_test.rs:4230 (`fn unblock_add_fix_schedules_repair_before_revalidation`) — the gate can pass on pre-existing tests with zero implementation; the filter must match ONLY the not-yet-written tests: `cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation -- --list 2>/dev/null | grep -q 'unblock_add_fix_schedules_repair_before_revalidation: test$' && cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation`
  [a8] cargo test filter `unblock_add_fix_schedules_repair_before_revalidation` collides with an existing test at crates/engine/tests/mission_test.rs:4230 (`fn unblock_add_fix_schedules_repair_before_revalidation`) — the gate can pass on pre-existing tests with zero implementation; the filter must match ONLY the not-yet-written tests: `cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation -- --list 2>/dev/null | grep -q 'unblock_add_fix_schedules_repair_before_revalidation: test$' && cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation`
- wrong-polarity: PASS
- host-shell-syntax: PASS
- passes-on-base: FAIL
  [a2] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo test -p kranz-engine fix_id_unique`
  [a8] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation -- --list 2>/dev/null | grep -q 'unblock_add_fix_schedules_repair_before_revalidation: test$' && cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation`
  [a9] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `git diff --quiet "$KRANZ_BASE_SHA" -- crates/engine/src/events.rs crates/engine/src/types.rs crates/engine/src/reducer.rs`
- env-sensitive: PASS


## Milestone 1 — Fix feature ids are unique per milestone

### 1.1 Next-free-cycle rule for fix feature ids, with unit tests

CONTEXT. Kranz is a Rust workspace; the engine crate is `kranz-engine` (crates/engine). Fix features are created by `emit_fix_features` in crates/engine/src/findings.rs (around lines 257-303). Today it names each feature `{milestone_id}-fix-{cycle}-{n}` with `let cycle = self.state.mission.milestones[mi].fix_cycles + 1;` (line ~276). Four paths call it:
(1) validator findings: crates/engine/src/orchestrator.rs ~6794 and ~6815;
(2) final-gate findings: crates/engine/src/orchestrator/finalization.rs ~935 and ~974;
(3) the tier-escalation valve: `escalate_or_block` in findings.rs emits `TierEscalated`, whose fold in reducer.rs:616-625 resets `fix_cycles` to 0, and the caller then calls emit_fix_features;
(4) the operator-directed repair: the `"unblock-add-fix"` arm in orchestrator.rs ~3421-3448. This arm deliberately does NOT spend a fix cycle, because the reducer only increments fix_cycles when a fixfeature.created arrives while the milestone is Validating.
As a result, paths (3) and (4) leave fix_cycles unchanged or reset, and the next fix reuses an existing id. The reducer (reducer.rs:539-613) rejects a different payload on an existing id whose feature started, committed or completed, with `duplicate fixfeature.created for feature '<id>' with a different payload`. That is exactly what wedged mission m-882080.

TASK. Change ONLY how the cycle number in the id is chosen, inside emit_fix_features (a small private helper in findings.rs is fine). New rule: `cycle = max(fix_cycles + 1, 1 + the highest c among existing feature ids of this milestone of the form {milestone_id}-fix-<c>-...)`. Parse c by `strip_prefix(&format!("{milestone_id}-fix-"))`, then the first '-'-separated segment, then parse as u32. Ignore anything that doesn't parse. This mirrors the existing `replan_cycle` computation at orchestrator.rs ~2786-2798. All features from one emit_fix_features call share the cycle, and n starts at 1 as today. In a log with no repair and no escalation the highest existing c always equals fix_cycles, so ids are identical to today's. Also update the comments that describe fix ids: orchestrator.rs ~2819-2822 (re-plan namespace comment) and the unblock-add-fix comment ~3422-3427. Say that the repair takes the next free fix-id cycle but still spends no fix cycle.

HARD CONSTRAINTS.
- Do NOT modify crates/engine/src/events.rs, crates/engine/src/types.rs or crates/engine/src/reducer.rs at all.
- Do NOT change when fix_cycles is incremented.
- Do NOT change re-plan (`-replan-`) or conflict (`-conflict-`) id schemes.
- Do NOT add any recovery command or path for stuck missions.
- Only touch crates/engine/src/findings.rs and crates/engine/src/orchestrator.rs; put tests in their existing `#[cfg(test)] mod tests` modules.
- Every new test name MUST start with `fix_id_unique_`, and that string must not appear anywhere else.

TESTS TO ADD (exact names; run with `cargo test -p kranz-engine fix_id_unique`):
(a) `fix_id_unique_rule_never_reuses_existing_id` in findings.rs tests. Cover these cases:
  - no existing fixes and fix_cycles 0 → `ms-1-fix-1-1`;
  - fixes fix-1-* and fix_cycles 1 → `fix-2-*`, the same as the old formula;
  - existing `ms-1-fix-1-1` with fix_cycles 0 (the repair case) → `fix-2-1`;
  - existing fix-1-* and fix-2-* with fix_cycles 0 (post-escalation) → `fix-3-*`;
  - a different milestone `ms-10` whose ids must not affect `ms-1`;
  - for every case, assert that no generated id equals an existing feature id.
(b) `fix_id_unique_two_repairs_get_distinct_ids`. Drive the real unblock-add-fix arm twice on one milestone, not emit_fix_features directly. Find the function containing the `"unblock-add-fix" =>` match arm in orchestrator.rs and how existing tests block a milestone and feed an orchestrator decision; the MockBackend in crates/engine/src/backend_mock.rs scripts orchestrator replies, and helpers like `lessons_test_repo`, `tier_escalation_*_script` live in orchestrator.rs tests. If the first repair's feature is marked Complete between the two (e.g. by emitting feature events), the second repair must still succeed. Assert:
  - the two repair ids differ;
  - both are present in the milestone;
  - fix_cycles is the same before and after both repairs.
(c) `fix_id_unique_legacy_log_replays_unchanged`. Build an event log as today's engine would write it: milestone validating, then fixfeature.created for `ms-1-fix-1-1`, and no repair. Fold it with crate::reducer::fold. Assert:
  - the milestone's feature ids are exactly the plan features plus `ms-1-fix-1-1`;
  - fix_cycles == 1;
  - the next validator-driven emit_fix_features (milestone back in Validating) produces `ms-1-fix-2-1`.
(d) `fix_id_unique_after_tier_escalation` in orchestrator.rs tests. Model it on the existing local-tier escalation test that uses `max_fix_cycles_per_milestone: 2` and a milestone with `fix_cycles: 2` (search for `tier_escalation_finding_script` near lines 12064-12140). Seed the milestone's features with Complete fix features `ms-1-fix-1-1` and `ms-1-fix-2-1`. Run `validation_round(0)` with a finding. Assert:
  - TierEscalated is emitted;
  - the call returns Ok;
  - the new fix feature id is `ms-1-fix-3-1` and is unique within the milestone.
Confirm by reasoning or a temporary local revert that tests (b) and (d) would fail on the old formula.

FINISH. Run and report: `cargo test -p kranz-engine fix_id_unique`, `cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`. Paste the test-runner output as evidence.

Done when:
- emit_fix_features picks cycle = max(fix_cycles+1, 1 + highest existing `{milestone_id}-fix-<c>-` cycle on the milestone); with no repair or escalation it produces the same ids as before
- fix_id_unique_rule_never_reuses_existing_id passes and covers: empty milestone, normal second cycle, post-repair, post-escalation, and a prefix-sharing milestone id (ms-10 vs ms-1)
- fix_id_unique_two_repairs_get_distinct_ids drives the unblock-add-fix arm twice and asserts distinct ids and an unchanged fix_cycles
- fix_id_unique_legacy_log_replays_unchanged asserts folded ids equal the logged ids and the next validator fix is ms-1-fix-2-1
- fix_id_unique_after_tier_escalation asserts TierEscalated, Ok return, and a new id ms-1-fix-3-1 with no duplicate error
- events.rs, types.rs and reducer.rs are byte-identical to the base commit
- existing test unblock_add_fix_schedules_repair_before_revalidation still passes

### 1.2 End-to-end mission test for the m-882080 sequence

CONTEXT. Kranz is a Rust workspace; the engine crate is `kranz-engine`. A previous feature changed `emit_fix_features` (crates/engine/src/findings.rs) so a fix feature's id cycle is `max(fix_cycles+1, 1 + highest existing {milestone_id}-fix-<c>- cycle on the milestone)`. Before that, an operator-directed `unblock-add-fix` repair (orchestrator.rs `"unblock-add-fix" =>` arm; it deliberately spends no fix cycle) and the next validator-driven fix computed the same id. The event log then rejected the second one with `duplicate fixfeature.created for feature '<id>' with a different payload`, and mission m-882080 was wedged in RUNNING.

TASK. Add ONE integration test to crates/engine/tests/mission_test.rs named exactly `fix_id_unique_repair_then_finding_completes`. Model it closely on the existing `unblock_add_fix_schedules_repair_before_revalidation` (around line 4230) and reuse its helpers: setup(), init_repo(), MockBackend::with_scripts, orch_script, worker_pass, validator_with, fix_features, judgement, dirty_tree_commit_as_is, no_lesson, make_engine, simple_plan, control::enqueue, MissionEngine::resume, read_log.

Scenario:
(1) Phase 1: the milestone blocks at the fix-cycle cap, as in the existing test (max_fix_cycles_per_milestone 1, one fix round, then a second failing round blocks).
(2) Before resuming, make room for one more validator-driven fix round. Enqueue a `ControlCommand::ConfigChange` with patch `{"maxFixCyclesPerMilestone": 2}` (grep orchestrator.rs tests for `ConfigChange` usage). Also enqueue an operator `ControlCommand::Msg` asking for a repair. If ConfigChange is not drained by `run()` on resume, find the equivalent supported path. Do not add engine code for it.
(3) Phase 2: the orchestrator answers `{"action":"unblock-add-fix","note":...,"fix":{...}}`. The repair worker passes. The next validation round returns ONE minor finding. The orchestrator converts it with fix_features(1). That fix worker passes. The following validation round returns no findings, and the mission reaches `MissionStatus::Complete`.
Script the mock backends in the exact order the engine consumes them; read the existing test's comments on script order.

Asserts:
- final status is Complete;
- the events after the MilestoneUnblocked hold exactly two fixfeature.created events, the repair (by its title) and the finding's fix;
- their feature ids differ;
- every feature id in the milestone is unique;
- the repair's id was not reused.
The test must target the old collision: on the old formula, the repair and the finding's fix would both have been `ms-1-fix-2-1`. Confirm by reasoning or a temporary local revert of the id rule that the test fails with the duplicate-fixfeature error on the old code. Do not commit the revert.

HARD CONSTRAINTS.
- Only edit crates/engine/tests/mission_test.rs. Do NOT modify engine source, events.rs, types.rs or reducer.rs.
- Do not add any recovery path for stuck missions.
- The test name must start with `fix_id_unique_`.

FINISH. Run and paste the output of `cargo test -p kranz-engine --test mission_test fix_id_unique_repair_then_finding_completes`, `cargo test -p kranz-engine fix_id_unique`, and `cargo fmt --all --check`.

Done when:
- fix_id_unique_repair_then_finding_completes exists in crates/engine/tests/mission_test.rs and passes
- the test drives block → unblock-add-fix repair → passing repair → validation finding → fix feature → passing validation → MissionStatus::Complete
- the test asserts the repair's fix feature id and the finding's fix feature id differ and all milestone feature ids are unique
- only crates/engine/tests/mission_test.rs is changed by this feature
