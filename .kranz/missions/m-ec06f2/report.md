# Mission report — m-ec06f2

**Goal:** Make every fix feature id unique within its milestone, whatever created it (validator finding, final gate, tier escalation, or operator-directed unblock-add-fix repair), so a validator finding after a repair gets its own fix feature and the mission carries on, without changing fix-cycle spending, the event schema, or replay of existing logs.

Branch `kranz/mission-m-ec06f2` (from `main`). Plan of record: [plan.md](plan.md).

## The plan

1 milestone, 2 features, gated by 12 contract assertions (11 command, 1 judgement).

1. **Fix feature ids are unique per milestone**
   - Next-free-cycle rule for fix feature ids, with unit tests — CONTEXT.
   - End-to-end mission test for the m-882080 sequence — CONTEXT.

**Chosen approach:** A single next-free-cycle rule inside emit_fix_features, the one function every fix path goes through: cycle = max(fix_cycles+1, 1 + highest…
**Elapsed:** 1h 04m 55s
**Tokens:** 198 in / 68972 out / 5671692 cache read / 380980 cache write
**Cost:** $4.36 actual vs $3.86–$19.28 estimated (expected $8.83)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-ec06f2-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Provider:** local-worktree (source isolation) · template: worktree · no workspace contract
- **Workspace contract:** no workspace contract (source isolation only)
- **Preflight:** preflight: 2 issue(s): [warn] command assertion [a1] runs `cargo test` without anti-vacuity (`ok. [1-9]`); a zero-test filter would pass vacuously; [warn] command assertion [a2] runs `cargo test` with… [truncated]

## What shipped

### Milestone 1 — Fix feature ids are unique per milestone ✅

- ✅ **Next-free-cycle rule for fix feature ids, with unit tests** — 2 runs, 1 respawn
  CONTEXT.
  - `bd580e3` [f-1-1] checkpoint (engine commit)
  - `2c8fce9` [f-1-1] fmt: align assert_eq args in fix_id_unique_two_repairs_get_distinct_ids
  - ✓ emit_fix_features picks cycle = max(fix_cycles+1, 1 + highest existing `{milestone_id}-fix-<c>-` cycle on the milestone); with no repair or escalation it produces the same ids as before
  - ✓ fix_id_unique_rule_never_reuses_existing_id passes and covers: empty milestone, normal second cycle, post-repair, post-escalation, and a prefix-sharing milestone id (ms-10 vs ms-1)
  - ✓ fix_id_unique_two_repairs_get_distinct_ids drives the unblock-add-fix arm twice and asserts distinct ids and an unchanged fix_cycles
  - ✓ fix_id_unique_legacy_log_replays_unchanged asserts folded ids equal the logged ids and the next validator fix is ms-1-fix-2-1
  - ✓ fix_id_unique_after_tier_escalation asserts TierEscalated, Ok return, and a new id ms-1-fix-3-1 with no duplicate error
  - ✓ events.rs, types.rs and reducer.rs are byte-identical to the base commit
  - ✓ existing test unblock_add_fix_schedules_repair_before_revalidation still passes
- ✅ **End-to-end mission test for the m-882080 sequence** — 1 run
  CONTEXT.
  - `787732e` [f-1-2] add end-to-end mission test for unique fix ids after a repair
  - ✓ fix_id_unique_repair_then_finding_completes exists in crates/engine/tests/mission_test.rs and passes
  - ✓ the test drives block → unblock-add-fix repair → passing repair → validation finding → fix feature → passing validation → MissionStatus::Complete
  - ✓ the test asserts the repair's fix feature id and the finding's fix feature id differ and all milestone feature ids are unique
  - ✓ only crates/engine/tests/mission_test.rs is changed by this feature

## Validation history

### ms-1 round 1 — Fix feature ids are unique per milestone

No findings.

## Contract outcomes

- ✅ **[a1]** Anti-vacuity: the test filter `fix_id_unique` matches at least 5 tests in kranz-engine, and the name did not exist anywhere under crates/ at the mission base commit, so every match is a new test. *(command: `list=$(cargo test -p kranz-engine fix_id_unique -- --list 2>/dev/null | grep ': test$'); printf '%s\n' "$list"; n=$(printf '%s\n' "$list" | grep -c 'fix_id_unique.*: test$'); [ "$n" -ge 5 ] && ! git grep -q fix_id_unique "$KRANZ_BASE_SHA" -- crates`)*
- ✅ **[a2]** All the new fix_id_unique tests pass. *(command: `cargo test -p kranz-engine fix_id_unique`)*
- ✅ **[a3]** Mission-level regression for m-882080: a milestone blocks, the orchestrator chooses unblock-add-fix, the repair runs and passes, the next validation round raises a finding, the finding becomes a fix feature whose id differs from the repair's, and the mission reaches Complete. *(command: `cargo test -p kranz-engine --test mission_test fix_id_unique_repair_then_finding_completes -- --list 2>/dev/null | grep -q 'fix_id_unique_repair_then_finding_completes: test$' && cargo test -p kranz-engine --test mission_test fix_id_unique_repair_then_finding_completes`)*
- ✅ **[a4]** Two unblock-add-fix repairs on one milestone get different feature ids, and neither spends a fix cycle. *(command: `cargo test -p kranz-engine fix_id_unique_two_repairs_get_distinct_ids -- --list 2>/dev/null | grep -q 'fix_id_unique_two_repairs_get_distinct_ids: test$' && cargo test -p kranz-engine fix_id_unique_two_repairs_get_distinct_ids`)*
- ✅ **[a5]** Replaying a log that holds a validator-driven fix (ms-1-fix-1-1) and no repair folds to the same feature ids as today, and the next validator-driven fix for that milestone gets the same id today's code gives (ms-1-fix-2-1). *(command: `cargo test -p kranz-engine fix_id_unique_legacy_log_replays_unchanged -- --list 2>/dev/null | grep -q 'fix_id_unique_legacy_log_replays_unchanged: test$' && cargo test -p kranz-engine fix_id_unique_legacy_log_replays_unchanged`)*
- ✅ **[a6]** After a local-tier milestone has used fix cycles 1 and 2 (ms-1-fix-1-1 and ms-1-fix-2-1 exist) and the cap triggers a tier escalation, which resets fix_cycles to 0, the fix features created in the same round get a fresh id (ms-1-fix-3-1) with no duplicate-fixfeature error. *(command: `cargo test -p kranz-engine fix_id_unique_after_tier_escalation -- --list 2>/dev/null | grep -q 'fix_id_unique_after_tier_escalation: test$' && cargo test -p kranz-engine fix_id_unique_after_tier_escalation`)*
- ✅ **[a7]** The id rule never yields an id already held by a feature of the milestone, and with no repair or escalation it yields exactly `{milestone_id}-fix-{fix_cycles+1}-{n}` as before. *(command: `cargo test -p kranz-engine fix_id_unique_rule_never_reuses_existing_id -- --list 2>/dev/null | grep -q 'fix_id_unique_rule_never_reuses_existing_id: test$' && cargo test -p kranz-engine fix_id_unique_rule_never_reuses_existing_id`)*
- ✅ **[a8]** An operator-directed repair still does not spend a fix cycle: the existing unblock-add-fix integration test passes unchanged. *(command: `cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation -- --list 2>/dev/null | grep -q 'unblock_add_fix_schedules_repair_before_revalidation: test$' && cargo test -p kranz-engine --test mission_test unblock_add_fix_schedules_repair_before_revalidation`)*
- ✅ **[a9]** The event schema and the fold are untouched (events.rs, types.rs and reducer.rs are unchanged from the base commit), so replaying any existing log gives the same state as today. *(command: `git diff --quiet "$KRANZ_BASE_SHA" -- crates/engine/src/events.rs crates/engine/src/types.rs crates/engine/src/reducer.rs`)*
- ✅ **[a10]** The whole workspace test suite passes. *(command: `cargo test --workspace`)*
- ✅ **[a11]** The workspace is lint-clean and formatted. *(command: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`)*
- ✅ **[a12]** The change only fixes how fix feature ids are chosen. It adds no command, flag, or code path to recover stuck missions (including m-882080), and it does not change when fix cycles are spent. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
