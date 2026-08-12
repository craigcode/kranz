# M5.5 Flight Rules closure proof

Date: 2026-08-11
Scope: KRZ-341 through KRZ-349

M5.5 is closed as one repo/pack-owned engineering-standards system. The proof
uses the same schema, resolver, checker, event, waiver, report, and UI paths as
production. It does not create a test-only enforcement path or infer policy
from prompt prose.

## Executable evidence matrix

| Done-when claim | Executable evidence |
|---|---|
| The schema-4 corpus is strict, bounded, stable, and digest-pinned | `flight_rules_contract_synthetic_pack_loads_byte_stable_manifest_and_digest`; the remaining `flight_rules_contract_*` tests cover malformed metadata, transitions, caps, no-follow reads, tracked-source trust, and old schemas |
| Draft and approval expose exact consent | `flight_rules_pin_approve_plan_pins_manifest_and_emits_resolved`; `flight_rules_pin_approval_pins_from_trusted_base_and_rejects_stale`; dashboard `flight_rules_dashboard_groups_exact_consent_by_rfc_with_digest_and_waiver_posture` |
| Worker and validators receive only their stage projection, with its identity recorded | `flight_rules_projection_worker_prompt_projects_implementation_stage_only`; `flight_rules_projection_validator_prompts_project_validation_stage_only`; `flight_rules_projection_sections_carry_boundary_digests_and_sources` |
| Approved and enforced-SHOULD failures are visible but nonblocking | `flight_rules_enforcement_lifecycle_level_matrix_is_exact`; `flight_rules_enforcement_enforced_should_failure_is_advisory`; `flight_rules_projection_approved_rules_are_never_labelled_blocking` |
| Deterministic and contextual enforced MUST failures are authoritative | `flight_rules_enforcement_merge_reruns_pinned_gate_on_integration_tree`; `flight_rules_enforcement_contextual_emits_exactly_one_linked_report_per_rule`; `flight_rules_enforcement_contextual_missing_and_duplicate_fail_closed` |
| A human waiver is exact, replayable, and invalidated by relevant change | `flight_rules_waiver_record_path_appends_and_the_fold_renders_waived`; `flight_rules_waiver_diff_digest_tracks_only_affected_paths`; `flight_rules_waiver_mismatched_revision_digest_or_pin_joins_nothing`; `flight_rules_waiver_expiry_restores_the_block` |
| A mission cannot change the policy judging it | `flight_rules_pin_mission_branch_pack_edit_is_ignored_and_surfaced`; `flight_rules_pin_merge_ignores_mission_branch_pack_edit`; `flight_rules_enforcement_checker_uses_only_pinned_gate_binding` |
| A live-base enforcement or checker change refuses merge | `flight_rules_pin_merge_mission_refuses_on_enforced_policy_drift`; `flight_rules_enforcement_merge_detects_checker_command_drift` |
| Replay and evidence explain every disposition; absence is never green | `flight_rules_provenance_replay_folds_the_coverage_matrix`; `flight_rules_provenance_bundle_renders_coverage_byte_identically`; `flight_rules_provenance_absent_evidence_is_never_pass`; `flight_rules_provenance_pre_flight_rules_chain_is_unchanged` |
| Metrics keep revision and denominator honesty | `flight_rules_metrics_keeps_revision_counts_denominators_and_false_greens_honest`; `flight_rules_metrics_suppresses_small_sample_conclusions_not_raw_counts`; `flight_rules_metrics_no_evidence_is_not_evaluated_never_green` |
| Spec and incident review reuse the same evidence contract | `flight_rules_review_class_selects_only_its_artifact_policy`; `flight_rules_review_class_deliverable_is_nonempty_and_source_is_immutable`; `flight_rules_review_class_context_rule_binds_the_review_diff` |

The focused Rust proof command was run raw as required:

```text
cargo test --workspace flight_rules_
```

It selected 128 tests across the workspace and completed with zero failures.
The dedicated dashboard tests cover approval consent and live coverage/drift/
waiver rendering. The release closure additionally requires the raw full
workspace test, Clippy, format, build, dashboard, strict audit, and secret-scan
gates; their successful run is recorded in the closing commit and review.

The proof is intentionally a seam matrix rather than one giant mocked mission.
Each claim is pinned at its authoritative boundary, while the full workspace
suite checks their composition. A monolithic happy-path test would make the
fail-closed cases and old/no-pack compatibility less observable, not more.

## Scope boundary retained

This milestone does not add a hosted organization catalog, pack signing,
inheritance/RBAC, semantic retrieval, editor integration, or an incident
management client. Those remain control-plane integrations around the shipped
repo/pack contract. Kranz still governs evidence and approval; it does not try
to become the system that authors code, stores incidents, or distributes
company policy.
