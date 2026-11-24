# Mission report — m-dd7d22

**Goal:** Each mission report states how many times the operator intervened, split by class, and both `kranz escalation-metrics` and `kranz outcomes` show a separate count of abandoned missions, with each one's interventions by class and whether it had delivered features. Neither autonomy ratio changes.

Branch `kranz/mission-m-dd7d22` (from `main`). Plan of record: [plan.md](plan.md).

## The plan

2 milestones, 7 features, gated by 17 contract assertions (16 command, 1 judgement).

1. **Mission report states operator interventions by class**
   - Per-class intervention counts in the escalation fold — Repository: kranz (Rust workspace).
   - Interventions line on the mission report — Repository: kranz (Rust workspace).
2. **Abandoned missions are visible beside the autonomy ratio**
   - Abandoned-mission fold in escalation_metrics and EscalationMetrics.abandoned — Repository: kranz (Rust workspace).
   - Outcomes.abandoned through the memoized outcomes fold — Repository: kranz (Rust workspace).
   - CLI text sections for abandoned missions — Repository: kranz (Rust workspace).
   - metrics.md companion figure, changelog, and D-X / divergence tickets — Repository: kranz.

**Chosen approach:** Build one per-class intervention fold and one abandoned fold in escalation_metrics.rs.
**Elapsed:** 3h 48m 54s
**Tokens:** 620 in / 153807 out / 19411674 cache read / 1060971 cache write
**Cost:** $11.41 actual vs $9.98–$49.90 estimated (expected $22.87)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-dd7d22-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Provider:** local-worktree (source isolation) · template: worktree · no workspace contract
- **Workspace contract:** no workspace contract (source isolation only)
- **Preflight:** preflight: 1 issue(s): [warn] command assertion [a8] uses '!', which was not found on PATH

## What shipped

### Milestone 1 — Mission report states operator interventions by class ✅

- ✅ **Per-class intervention counts in the escalation fold** — 1 run
  Repository: kranz (Rust workspace).
  - `1e43889` [f-1-1] add per-class intervention counts to escalation fold
  - ✓ intervention_classes_counts_each_class_separately: a log with plan.approved, 2 post-approval user.message, 1 grant.approved, 1 grant.denied, 1 plan.revised, 1 plan.revision.rejected and 1 operator milestone.unblocked yields steers 2, grant_decisions 2, revision_rulings 2, operator_unblocks 1, total 7
  - ✓ intervention_classes_exclude_engine_lift_and_drafting: a pre-approval user.message and a workspace-gate-lift milestone.unblocked (GATE_LIFT_REASON, no block_context) count zero in every class
  - ✓ intervention_classes_total_matches_mission_escalation: over the existing anti_vacuity_missions fixture and the revision/unblock fixtures, mission_interventions(..).total() equals mission_escalation(..).interventions for every mission
  - ✓ intervention_classes_format_pluralises: format_intervention_classes renders the three example strings in the spec exactly
  - ✓ all pre-existing escalation_metrics tests pass unchanged
- ✅ **Interventions line on the mission report** — 1 run
  Repository: kranz (Rust workspace).
  - `8e19207` [f-1-2] show operator interventions by class on the mission report
  - ✓ report_interventions_line_two_steers_one_grant_shows_three passes, asserting the exact line with count 3 split as 2 steers and 1 grant decision
  - ✓ report_interventions_line_zero_renders_zero passes, asserting the zero line is present (not missing)
  - ✓ report_interventions_line_follows_cost passes
  - ✓ report_render.rs contains no match arm on grant/revision/unblock/user-message event kinds and calls crate::escalation_metrics
  - ✓ the whole report_test target passes

### Milestone 2 — Abandoned missions are visible beside the autonomy ratio ✅

- ✅ **Abandoned-mission fold in escalation_metrics and EscalationMetrics.abandoned** — 1 run
  Repository: kranz (Rust workspace).
  - `d50ea4c` [f-2-1] add abandoned-mission fold to escalation_metrics
  - ✓ abandoned_missions_fold_escalation_ratio_unchanged passes with the exact autonomy and abandoned values in the spec
  - ✓ abandoned_missions_fold_no_feature_is_not_delivered passes
  - ✓ abandoned_missions_fold_feature_after_abandon_not_counted passes
  - ✓ abandoned_missions_fold_completed_and_failed_excluded passes
  - ✓ every pre-existing escalation_metrics test passes unchanged, including escalation_metrics_abandoned_is_not_a_closed_mission
  - ✓ kranz-server lib tests pass
- ✅ **Outcomes.abandoned through the memoized outcomes fold** — 1 run
  Repository: kranz (Rust workspace).
  - `fa43471` [f-2-2] surface abandoned missions in the outcomes fold
  - ✓ abandoned_missions_fold_outcomes_ratio_unchanged passes, pinning closed 2 / interventions 1 / zero 1 / share 0.5 and the abandoned row
  - ✓ abandoned_missions_fold_outcomes_json_field passes (field present; legacy JSON without it deserializes)
  - ✓ all pre-existing outcomes tests pass unchanged
  - ✓ kranz-slack formatting tests and kranz-server lib tests pass
- ✅ **CLI text sections for abandoned missions** — 1 run
  Repository: kranz (Rust workspace).
  - `444b559` [f-2-3] show abandoned missions beside the autonomy ratio in CLI text
  - ✓ abandoned_missions_cli_outcomes_section_lists_rows passes with the exact header/count/row text
  - ✓ abandoned_missions_cli_escalation_section_lists_rows passes with the exact header/count/row text
  - ✓ abandoned_missions_cli_absent_when_none passes for both renderers
  - ✓ all pre-existing kranz --lib tests pass unchanged (existing text output byte-identical when nothing is abandoned)
- ✅ **metrics.md companion figure, changelog, and D-X / divergence tickets** — 1 run
  Repository: kranz.
  - `9742e3a` [f-2-4] document abandoned-mission companion figure and open D-X tickets
  - ✓ docs/metrics.md still contains 'Closed missions (`completed` + `failed`). Abandoned' and now contains '.abandoned'
  - ✓ CHANGELOG.md Unreleased has an Added entry for the interventions line and the abandoned count
  - ✓ .kranz/tickets/abandoned-mission-delivery-link.md exists with valid frontmatter and contains 'D-X' and 'delivered-by'
  - ✓ .kranz/tickets/outcomes-autonomy-ratio-divergence.md exists with valid frontmatter and names both outcomes.rs and escalation_metrics.rs
- ✅ **Outcomes abandoned section discloses that this view's ratio counts abandoned missions as closed** *(fix)* — 1 run
  Repository: kranz (Rust workspace).
  - `f80431e` [ms-2-fix-1-1] note the autonomy-ratio divergence in kranz outcomes abandoned section
  - ✓ abandoned_missions_cli_outcomes_notes_ratio_divergence passes: kranz outcomes text with one abandoned mission contains the exact note line pointing to .kranz/tickets/outcomes-autonomy-ratio-divergence.md
  - ✓ abandoned_missions_cli_escalation_has_no_divergence_note passes: escalation-metrics text shows the abandoned section but not the note
  - ✓ abandoned_missions_cli_absent_when_none and all pre-existing kranz --lib tests pass unchanged (no note and no section when nothing was abandoned)
  - ✓ only crates/cli/src/output.rs changes; fmt --check and clippy -D warnings are clean

## Validation history

### ms-1 round 1 — Mission report states operator interventions by class

- [critical] a15 — grep -q 'D-X' .kranz/tickets/abandoned-mission-delivery-link.md && grep -q 'delivered-by' .kranz/tickets/abandoned-mission-delivery-link.md && grep -q 'outcomes.rs' .kranz/tickets/outcomes-autonomy-ra… [truncated]
- [critical] a14 — grep -qF 'Closed missions (`completed` + `failed`). Abandoned' docs/metrics.md && grep -qF '.abandoned' docs/metrics.md && ! git diff --quiet $KRANZ_BASE_SHA -- docs/metrics.md -> FAIL. Direct inspect… [truncated]

Disposition: waived.
- a14: Not an ms-1 defect. The metrics.md companion entry with the `.abandoned` reproduce path is planned work: feature f-2-4 in ms-2, still pending. This assertion has no milestone tag, so it runs at every milestone gate and fails until f-2-4 lands. A fix-feature now would duplicate f-2-4 before the abandoned fields it documents exist. This waiver covers the ms-1 gate only. a14 still gates ms-2 and the mission.
- a15: Not an ms-1 defect. The two tickets, abandoned-mission-delivery-link.md and outcomes-autonomy-ratio-divergence.md, are planned work: feature f-2-4 in ms-2, still pending. This assertion has no milestone tag, so it is checked early here. A fix-feature now would duplicate f-2-4. This waiver covers the ms-1 gate only. a15 still gates ms-2 and the mission.

### ms-2 round 1 — Abandoned missions are visible beside the autonomy ratio

No findings.

### Final gate

- [critical] a17 *(final gate)* — Most of a17 holds; one part fails. What holds: in escalation_metrics.rs, the rows `aggregate` builds with `abandoned_mission_row` go only into `summarize_abandoned`. The `Some(TerminalOutcome::Abandon… [truncated]

Disposition: 1 fix feature(s) created.

Disposition: milestone blocked — validator command denied: `git diff 8e19207814a17c2b2605b2d900b3473b1facd1f6..HEAD -- crates/engine/src/escalation_metrics.rs crates/engine/src/outcomes.rs crates/cli/src/output.rs | grep -nE "^\+.{101,}" | head -20` — grant request timed out after 3600s with no operator decision (deny-default)

### ms-2 round 2 — Abandoned missions are visible beside the autonomy ratio

No findings.

## Contract outcomes

- ✅ **[a1]** A mission log with two steers after plan approval and one grant decision renders a report whose interventions line reads `**Interventions:** 3 (2 steers, 1 grant decision, 0 revision rulings, 0 operator unblocks)`. *(command: `out=$(cargo test -p kranz-engine --test report_test report_interventions_line_two_steers_one_grant_shows_three 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a2]** A mission with no interventions renders the interventions line with a zero count, `**Interventions:** 0 (0 steers, 0 grant decisions, 0 revision rulings, 0 operator unblocks)`, not a missing line. *(command: `out=$(cargo test -p kranz-engine --test report_test report_interventions_line_zero_renders_zero 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a3]** The per-class intervention fold in escalation_metrics.rs counts grant decisions, post-approval steers, revision rulings and operator unblocks separately, and its total equals the existing `interventions` count (engine-owned workspace-gate lifts and pre-approval messages still do not count). *(command: `out=$(cargo test -p kranz-engine --lib intervention_classes_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a4]** Over one completed mission (no interventions) and one abandoned mission (one post-approval steer, one operator unblock, one feature completed before the abandon), `escalation-metrics` keeps its autonomy ratio at closed 1, zero-intervention 1, share 1.0, and its `abandoned` figure reports 1 mission with 2 interventions (1 steer, 1 operator unblock) that had delivered features. *(command: `out=$(cargo test -p kranz-engine --lib abandoned_missions_fold_escalation_ratio_unchanged 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a5]** Over the same two-mission fixture repo, `kranz outcomes`' autonomy ratio stays exactly as it is today (closed 2, total interventions 1, zero-intervention 1, share 0.5), and `Outcomes.abandoned` reports the abandoned mission with the same per-class counts and delivered flag as escalation-metrics. *(command: `out=$(cargo test -p kranz-engine --lib abandoned_missions_fold_outcomes_ratio_unchanged 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a6]** The abandoned-mission fold tests all pass: an abandoned mission with no completed feature reads as not delivered, a feature completed after the abandon does not count, and completed and failed missions never appear in the abandoned count. *(command: `out=$(cargo test -p kranz-engine --lib abandoned_missions_fold_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [3-9]|test result: ok\. [1-9][0-9]+ passed'`)*
- ✅ **[a7]** The text views of `kranz escalation-metrics` and `kranz outcomes` print an `Abandoned (outside the autonomy ratio)` section with the count and a per-mission row of interventions by class when any mission was abandoned, and print no such section when none was. *(command: `out=$(cargo test -p kranz --lib abandoned_missions_cli_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a8]** Anti-vacuity: none of the new test-name prefixes existed at the base commit, so every filter above matches only tests this mission added. *(command: `! git grep -qE 'fn (intervention_classes_|report_interventions_line_|abandoned_missions_fold_|abandoned_missions_cli_)' $KRANZ_BASE_SHA -- crates`)*
- ✅ **[a9]** The full kranz-engine library test suite passes, including every existing escalation_metrics and outcomes autonomy test, unchanged. *(command: `out=$(cargo test -p kranz-engine --lib 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a10]** The existing report renderer tests still pass. *(command: `out=$(cargo test -p kranz-engine --test report_test 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a11]** The CLI library tests, the Slack formatting tests and the server library tests (which build or serialize `Outcomes`) all pass. *(command: `out=$(cargo test -p kranz --lib 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && out=$(cargo test -p kranz-slack --test formatting 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && out=$(cargo test -p kranz-server --lib 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a12]** `events.rs` and `types.rs` are unchanged (D-X1: the count alone, no new event field). *(command: `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs`)*
- ✅ **[a13]** The report renderer defines no intervention classification of its own: it holds no match on grant, revision, unblock or user-message event kinds and calls the escalation_metrics per-class fold instead. *(command: `! grep -qE 'EventKind::(GrantApproved|GrantDenied|PlanRevised|PlanRevisionRejected|MilestoneUnblocked|UserMessage)' crates/engine/src/report_render.rs && grep -q 'escalation_metrics::' crates/engine/src/report_render.rs`)*
- ✅ **[a14]** `docs/metrics.md` keeps the published autonomy-ratio denominator text and gains a companion entry for the abandoned count that names its reproduce path `.abandoned`. *(command: `grep -qF 'Closed missions (`completed` + `failed`). Abandoned' docs/metrics.md && grep -qF '.abandoned' docs/metrics.md && ! git diff --quiet $KRANZ_BASE_SHA -- docs/metrics.md`)*
- ✅ **[a15]** The D-X operator decision (whether an abandoned mission can be linked to the change that finished its work) and the existing divergence between the two autonomy ratios are each recorded as a ticket. *(command: `grep -q 'D-X' .kranz/tickets/abandoned-mission-delivery-link.md && grep -q 'delivered-by' .kranz/tickets/abandoned-mission-delivery-link.md && grep -q 'outcomes.rs' .kranz/tickets/outcomes-autonomy-ratio-divergence.md && grep -q 'escalation_metrics.rs' .kranz/tickets/outcomes-autonomy-ratio-divergence.md`)*
- ✅ **[a16]** Formatting and clippy are clean on the touched crates. *(command: `cargo fmt --all --check && cargo clippy -p kranz-engine -p kranz -p kranz-slack -p kranz-server --all-targets -- -D warnings`)*
- ✅ **[a17]** The abandoned figures never enter either autonomy ratio's numerator or denominator, and the new report line and CLI section read honestly to an operator: they name the abandoned count as outside the ratio, and they don't present an abandoned mission as a completion or a failure. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
