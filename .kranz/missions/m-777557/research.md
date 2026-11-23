# Research — m-777557

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/command_exec.rs
- crates/engine/src/contract_lint.rs
- crates/engine/src/orchestrator.rs
- crates/engine/src/orchestrator/finalization.rs
- crates/engine/src/types.rs
- crates/engine/src/config.rs
- crates/engine/src/planning.rs
- crates/engine/src/contract_health.rs
- crates/engine/src/contract_gates.rs
- crates/engine/prompts/orchestrator.md
- docs/knowledge/validation/child-containment.md
- docs/knowledge/validation/gates.md
- docs/knowledge/CONVENTIONS.md
- .kranz/missions/m-882080/plan.json

## External sources

- crates/engine/src/command_exec.rs:149
- crates/engine/src/contract_lint.rs:27
- crates/engine/src/orchestrator.rs:1820
- crates/engine/src/planning.rs:554
- crates/engine/src/config.rs:377

## Facts

- A single 600s constant caps every contract command run, with no config override. — `crates/engine/src/command_exec.rs:149 COMMAND_TIMEOUT; run_contract_command_sandboxed passes it at :1438`
- The final gate and both validation-round passes use the fixed-timeout wrapper. — `orchestrator/finalization.rs:247; orchestrator.rs:5975 and :6018`
- A timeout is returned as (None, "timed out after Ns"), so callers cannot tell it from a spawn or wrap failure without matching text. — `command_exec.rs:1890 and :1359/:1510`
- The lint classes any no-exit-code result as CouldNotVerdict, an author-bug suspect, and the headline then says suspects 'already pass'. — `contract_lint.rs:194-202 classify; orchestrator.rs:1828-1832`
- contract_health counts lint decisions by matching the 'contract lint:' prefix and the 'all command assertions correctly fail' text. — `contract_health.rs:113-117`
- Revised plans are refused if an approved assertion's fields change, compared field by field in two places. — `planning.rs:562-566; reducer.rs:1096`
- Config has an operator-only mechanism that refuses keys set in the project layer. — `config.rs:367-563 CONFIG_TRUST_RULES`
- The plan JSON schema given to planners has additionalProperties false, so timeoutSecs must be added there. — `planning.rs ~640-720 (ptyScript/negativeControl schema)`
- About 53 Assertion struct literals across 26 files will need the new field. — `grep -c 'negative_control: None' crates`
- child-containment.md is a human-owned note: agents may only add a Rechecked line. — `docs/knowledge/validation/child-containment.md front matter; docs/knowledge/CONVENTIONS.md 'Who may change a note'`
- A 4-hour maximum already exists as a precedent elsewhere in the repo. — `docs/scoping/acp-resource-budgets.md:254 'maxima ... 14,400 seconds'`

## Ambiguities & stale docs

- The operator did not answer D-X1..D-X5 before requesting the plan. The plan assumes my stated defaults: per-assertion timeoutSecs wins over operator-only global contractCommandTimeoutSecs, which wins over 600s; a fixed, non-configurable 14,400s ceiling; plan values out of range refused with the assertion id; config read at each run but not runtime-patchable; the lint keeps a 600s cap and labels timeouts separately; timeouts stay critical command-assertion findings with timeout wording; the planning prompt gains a timeoutSecs line. If the operator wants different answers, revise before approval.
- Merge-gate (docs/merge-gates.md) and workspace-hook 600s limits are treated as out of scope.
- This mission's own contract deliberately omits `cargo test --workspace`: the running engine still kills contract commands at 600s, so that assertion would block exactly as in m-882080/m-caf631. Coverage comes from targeted filters plus clippy --workspace --all-targets.
- A per-assertion timeoutSecs that is set too low cannot be rescued by raising config, because the per-assertion value wins.

## Candidate knowledge updates

- docs/knowledge/validation/gates.md: replace 'The timeout is 10 minutes (COMMAND_TIMEOUT)' with the resolution order, the ceiling and the timeout evidence wording (done by the docs feature).
- docs/knowledge/lessons.md or a new validation note: a contract assertion expected to run longer than 600s must set timeoutSecs, or the approval lint will flag it as timing out.
