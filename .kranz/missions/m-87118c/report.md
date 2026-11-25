# Mission report — m-87118c

**Goal:** Make `kranz outcomes`' AutonomyRatio follow the published definition in docs/metrics.md, the one escalation_metrics.rs implements: abandoned missions leave the denominator, operator unblocks count as interventions, and steers are classified by seq. Prove the two folds agree with a regression test, and record the change in the figures in CHANGELOG.md.

Branch `kranz/mission-m-87118c` (from `main`). Plan of record: [plan.md](plan.md).

## The plan

1 milestone, 2 features, gated by 12 contract assertions (11 command, 1 judgement).

1. **kranz outcomes autonomy ratio follows the published definition**
   - Align outcomes.rs AutonomyRatio with escalation_metrics and add the cross-fold regression test — Repository: kranz, a Rust workspace.
   - Changelog entry for the changed kranz outcomes autonomy figures — Repository: kranz (Rust workspace).

**Chosen approach:** Use the shared mission_interventions rule for every reader of MissionOutcomes.interventions, add a separate first-terminal completed/failed…
**Elapsed:** 2h 14m 27s
**Tokens:** 184 in / 61037 out / 5626134 cache read / 446294 cache write
**Cost:** $4.80 actual vs $3.58–$17.88 estimated (expected $8.16)
**Interventions:** 0 (0 steers, 0 grant decisions, 0 revision rulings, 0 operator unblocks)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-87118c-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Provider:** local-worktree (source isolation) · template: worktree · no workspace contract
- **Workspace contract:** no workspace contract (source isolation only)
- **Preflight:** preflight: 4 issue(s): [warn] command assertion [a1] runs `cargo test` without anti-vacuity (`ok. [1-9]`); a zero-test filter would pass vacuously; [warn] command assertion [a2] uses ''/fn', which was… [truncated]

## What shipped

### Milestone 1 — kranz outcomes autonomy ratio follows the published definition ✅

- ✅ **Align outcomes.rs AutonomyRatio with escalation_metrics and add the cross-fold regression test** — 2 runs, 1 respawn
  Repository: kranz, a Rust workspace.
  - `5a7fb82` [f-1-1] checkpoint (engine commit)
  - ✓ `cargo test -p kranz-engine --lib autonomy_ratio_outcomes_matches_escalation_metrics` runs exactly 1 test and it passes. Over one fixture repo (an abandoned mission, an operator unblock on a failed mission, a steer whose seq is after plan.approved but whose ts is before it, and a clean completion), compute_outcomes and compute_escalation_metrics both report closed 3, zero-intervention 1, share 1/3, and outcomes reports total_interventions 2
  - ✓ abandoned_missions_fold_outcomes_ratio_unchanged asserts closed_missions 1, total_interventions 0, zero_intervention_missions 1, zero_intervention_share 1.0, keeps its .abandoned assertions, and passes
  - ✓ mission_outcomes counts an operator milestone.unblocked as an intervention but not the workspace-gate lift (GATE_LIFT_REASON)
  - ✓ mission_outcomes counts a user.message whose seq is at or after plan.approved even when its ts is earlier
  - ✓ For an abandoned-only mission, is_closed is true and autonomy_closed is false; for a completed mission and for a failed mission, both are true
  - ✓ is_closed_true_for_each_terminal_event still passes unchanged
  - ✓ escalation_metrics.rs diff is additions only; no line containing abandoned_mission_row, summarize_abandoned or abandoned_rows is added or removed in outcomes.rs
  - ✓ the string 'the same closed set as' no longer appears in outcomes.rs, and the string 'e.ts >= approved_ts' no longer appears either
  - ✓ `cargo test -p kranz-engine --lib outcomes::`, `--lib escalation_metrics::`, `--lib evidence_bundle`, `--test outcomes_report_merged_test` and `--test report_test` all pass; `cargo fmt --all --check` and `cargo clippy -p kranz-engine --all-targets -- -D warnings` are clean
- ✅ **Changelog entry for the changed kranz outcomes autonomy figures** — 2 runs, 1 respawn
  Repository: kranz (Rust workspace).
  - `79b0f97` [f-1-2] document kranz outcomes autonomy ratio definition change
  - `181892e` [f-1-2] note operator decision in autonomy ratio changelog entry
  - ✓ `git diff $KRANZ_BASE_SHA -- CHANGELOG.md` shows only added lines, all in one new `- Changed:` bullet at the top of `## Unreleased`
  - ✓ the added lines contain `kranz outcomes`, 'autonomy', 'abandoned' and 'unblock'
  - ✓ the entry says why (aligned with docs/metrics.md / `kranz escalation-metrics` per the operator decision), names all three rule changes, names the evidence-bundle Interventions figure and the `kranz outcomes --all` / cost-per-merged-change share as also affected, says that share's denominator still counts abandoned missions, and says .abandoned is unchanged
  - ✓ no file other than CHANGELOG.md is modified by this feature

## Validation history

### ms-1 round 1 — kranz outcomes autonomy ratio follows the published definition

No findings.

## Contract outcomes

- ✅ **[a1]** A regression test writes one fixture repo with an abandoned mission, an operator unblock, and a steer whose seq is after plan.approved but whose timestamp is before it. It shows that compute_outcomes' autonomyRatio and compute_escalation_metrics' autonomy agree, with closed 3, zero-intervention 1 and share 1/3. The filter matches exactly that one test. *(command: `out=$(cargo test -p kranz-engine --lib autonomy_ratio_outcomes_matches_escalation_metrics 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. 1 passed'`)*
- ✅ **[a2]** The existing test abandoned_missions_fold_outcomes_ratio_unchanged now pins the aligned numbers (closed_missions 1, total_interventions 0, zero_intervention_share 1.0) and passes. *(command: `body=$(awk '/fn abandoned_missions_fold_outcomes_ratio_unchanged\(/,/^        }$/' crates/engine/src/outcomes.rs) && printf '%s\n' "$body" | grep -qF 'assert_eq!(ratio.closed_missions, 1);' && printf '%s\n' "$body" | grep -qF 'assert_eq!(ratio.total_interventions, 0);' && printf '%s\n' "$body" | grep -qF 'assert_eq!(ratio.zero_intervention_share, 1.0);' && out=$(cargo test -p kranz-engine --lib abandoned_missions_fold_outcomes_ratio_unchanged 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. 1 passed'`)*
- ✅ **[a3]** mission_outcomes counts interventions through the shared escalation_metrics::mission_interventions rule. The old timestamp-based steer comparison is gone from outcomes.rs. *(command: `grep -q 'mission_interventions' crates/engine/src/outcomes.rs && ! grep -qF 'e.ts >= approved_ts' crates/engine/src/outcomes.rs`)*
- ✅ **[a4]** Every engine lib test in the outcomes module, including outcomes/reasons_tests and the unchanged is_closed_true_for_each_terminal_event, passes. *(command: `out=$(cargo test -p kranz-engine --lib outcomes:: 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a5]** Every engine lib test in the escalation_metrics and evidence_bundle modules passes. *(command: `out=$(cargo test -p kranz-engine --lib escalation_metrics:: 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && out=$(cargo test -p kranz-engine --lib evidence_bundle 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a6]** The engine integration tests for the merged-change report and the mission report pass. *(command: `out=$(cargo test -p kranz-engine --test outcomes_report_merged_test 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && out=$(cargo test -p kranz-engine --test report_test 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a7]** The .abandoned section is unchanged: the abandoned-missions fold tests pass, escalation_metrics.rs gets additions only, and no outcomes.rs line that builds the abandoned rows or their summary is added or removed. *(command: `out=$(cargo test -p kranz-engine --lib abandoned_missions_fold 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && ! git diff $KRANZ_BASE_SHA -- crates/engine/src/escalation_metrics.rs | grep -qE '^-([^-]|$)' && ! git diff $KRANZ_BASE_SHA -- crates/engine/src/outcomes.rs | grep -E '^[-+]([^-+]|$)' | grep -qE 'abandoned_mission_row|summarize_abandoned|abandoned_rows'`)*
- ✅ **[a8]** docs/metrics.md is unchanged, so no new definition is published. *(command: `git diff --quiet $KRANZ_BASE_SHA -- docs/metrics.md`)*
- ✅ **[a9]** CHANGELOG.md gains an entry that names `kranz outcomes`, the autonomy figures and abandoned missions. *(command: `d=$(git diff $KRANZ_BASE_SHA -- CHANGELOG.md | grep -E '^\+([^+]|$)') && printf '%s\n' "$d" | grep -qF 'kranz outcomes' && printf '%s\n' "$d" | grep -qi 'autonomy' && printf '%s\n' "$d" | grep -qi 'abandon' && printf '%s\n' "$d" | grep -qi 'unblock'`)*
- ✅ **[a10]** The CostPerMergedChange doc comments no longer claim the window uses the same closed set as AutonomyRatio. *(command: `! grep -qF 'the same closed set as' crates/engine/src/outcomes.rs`)*
- ✅ **[a11]** The workspace is rustfmt-clean, and kranz-engine has no clippy warnings in any target. *(command: `cargo fmt --all --check && cargo clippy -p kranz-engine --all-targets -- -D warnings`)*
- ✅ **[a12]** The CHANGELOG entry says that the `kranz outcomes` autonomy figures change and why: they now follow docs/metrics.md and `kranz escalation-metrics`, per an operator decision. It names the three rule changes. It names the side effects: the evidence bundle's Interventions figure and the intervention count behind the `kranz outcomes --all` / cost-per-merged-change autonomy share now use the same rule, while that share's denominator still counts abandoned missions. It also says the .abandoned section is unchanged. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
