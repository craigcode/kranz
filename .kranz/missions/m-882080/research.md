# Research — m-882080

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/types.rs
- crates/engine/src/planning.rs
- crates/engine/src/runner.rs
- crates/engine/prompts/validator-functional.md
- docs/knowledge/validation/gates.md
- docs/knowledge/decisions/positioning-governance-evidence-layer.md
- docs/knowledge/architecture/mission-pipeline.md

## External sources

- Explore subagent survey of orchestrator.rs, findings.rs, reducer.rs, events.rs, finalization.rs, mission_test.rs, backend_mock.rs

## Facts

- Each validation round runs every command and pty assertion in the contract, whatever the milestone — `crates/engine/src/orchestrator.rs:6071 clones validation_contract; run_contract_commands_for_validation at :5949`
- Round findings are written by the functional validator from engine PASS/FAIL lines, not by the engine — `crates/engine/src/runner.rs:2034-2047; crates/engine/prompts/validator-functional.md:20`
- Any unwaived finding, of any severity, leads to fix features via convert_findings — `crates/engine/src/findings.rs:141,260; orchestrator.rs:6550-6640`
- The final gate runs every command assertion as a non-waivable critical command-assertion finding — `crates/engine/src/orchestrator/finalization.rs:231-269`
- Milestone ids are positional ms-N, assigned at the PlanApproved fold; plan milestones carry only a title — `crates/engine/src/reducer.rs:84-112; types.rs:357`
- The planner schema forbids unknown assertion properties — `crates/engine/src/planning.rs:666-704 additionalProperties false`
- The validator prompt already speaks of 'contract assertions mapped to this milestone' — `crates/engine/prompts/validator-functional.md:7`
- About 69 Assertion struct literals exist across 27 files in four crates — `grep 'Assertion {' over crates/`

## Ambiguities & stale docs

- The operator did not answer the D-X questions before the formal plan request. The plan uses the recommended defaults: D-1 a `milestone` field on Assertion matched by title (trimmed, case-insensitive); D-2 only plan_schema() describes it, and prompts.rs and prompts/*.md stay frozen; D-3 not-yet-due assertions are withheld from validators, recorded as validation.assertion.notdue, and any citing finding is diverted to validation.finding.notdue; D-4 revisions may not change the field, and an unresolvable title counts as due everywhere. Reject at approval if any of these is wrong.
- The goal cites finding class `contract-command-failure`, which does not exist in the repo. Round findings normally carry an empty class, and only the final gate sets `command-assertion`.

## Candidate knowledge updates

- docs/knowledge/validation/gates.md: new 'Milestone-scoped assertions' section
- docs/knowledge/architecture/mission-pipeline.md: due-at rule in the validation-round description
- docs/knowledge/glossary.md: 'not-yet-due assertion'
