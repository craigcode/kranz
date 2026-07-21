# Research — m-2d5583

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/orchestrator.rs
- crates/engine/src/contract_sweep.rs
- crates/engine/src/runner.rs
- .kranz/tickets/contract-smoke-test-at-approval.md
- .kranz/missions/index.md

## External sources

- git show 65d3201:.kranz/missions/m-8b3ec3/plan.json
- docs/knowledge/lessons.md (m-c6882a nested-runtime lesson)

## Facts

- approve_plan is a synchronous pub fn called from many sync sites, so it cannot be made async without a large ripple; the lint must be synchronous. — `orchestrator.rs:1232 `pub fn approve_plan`; callers exec.rs:187, host.rs:503, commands.rs:767, planning_tui.rs:1229, draft.rs:273, plus ~70 tests.`
- There is an m-c6882a-aware synchronous command-probe pattern that avoids the 'Cannot start a runtime from within a runtime' panic; the lint must reuse it, not a nested tokio Runtime. — `orchestrator.rs:6834 `fn run_with_timeout` (std::process poll loop) with doc at 6808; used by sandbox_command_preflight; the async run_shell_command at 7069 and run_bounded_gate_command build runtimes and must be avoided here.`
- The final gate runs command assertions with KRANZ_BASE_SHA set via runner::contract_env; the lint reuses this env so worker/validator/gate/lint never diverge, and base_sha is pinned at approval. — `orchestrator.rs:4204 run_shell_command with `runner::contract_env(base_sha)`; runner.rs:548 contract_env sets KRANZ_BASE_SHA only for a non-empty SHA; base_sha pinned at orchestrator.rs:1288 (comment 1281-1288).`
- plan.md is produced by render_plan_markdown, which has additional callers on the revised-plan path, so its signature should not change; append the lint section to the plan.md body string in approve_plan instead. — `render_plan_markdown at orchestrator.rs:5592; other callers orchestrator.rs:1901 and crates/server/src/rest.rs:273; contract list rendered at 5657.`
- The abandoned tungstenite mission's buggy assertion is the platform-inverted lockfile grep; the ticket's canonical m-0c885b a6 is the inverted-polarity shape that can only pass while the requirement is unmet — i.e. it passes on the untouched base. — `git show 65d3201:.kranz/missions/m-8b3ec3/plan.json a7 `grep -L tokio-tungstenite.0.24 Cargo.lock`; merge commit a080aea 'abandoned on platform-inverted grep in contract'; ticket context lines 18-28.`
- Approval-time contract probing already exists (PATH resolution, anti-vacuity) and is surfaced via an advisory emit_decision, giving a precedent to extend rather than a new surface to invent. — `orchestrator.rs:734-769 preflight command probes; contract_sweep.rs:73 cargo_test_has_anti_vacuity (`[1-9]` guard, AGENTS.md rule 5); run_loop:2313 emit_decision(preflight summary).`

## Ambiguities & stale docs

- The exact m-0c885b a6 command is not on disk (mission cleaned); it is reconstructed from the ticket as an inverted grep that passes on the base. The lint's pass-on-base=suspect rule catches that shape; the classification is validated with equivalent fixtures (`true`/`false`, sleep, inverted grep).
- The ticket's 'hooks-disabled' mechanism is unspecified; chosen approach injects core.hooksPath=/dev/null via GIT_CONFIG_* env for any git invoked by a contract command.
- The m-0c885b a8 bug (a git diff that failed to exclude the harness's own plan-artifact commits) is not catchable by a pre-branch base run because the base diff is empty; it is scoped out of this dynamic lint and left for a possible future static check.

## Candidate knowledge updates

- docs/knowledge/validation: document the approval-time contract lint — command assertions that PASS against the untouched base are polarity/vacuity suspects (a correctly-scoped 'work landed' assertion must fail before the work lands); the lint is advisory, synchronous (std::process, no nested tokio Runtime), and runs before the branch exists.
- A repo lesson: when authoring a command assertion, expect it to FAIL on the pristine base; a green-on-base command is either a legitimate invariant or an inverted/vacuous bug — review it at approval.
