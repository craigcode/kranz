# Mission plan — m-2d5583

**Goal:** At approve_plan, lint each command assertion by running it once against the untouched base tree (bounded, sync, hooks-disabled) and surface author-bug suspects (pass-on-base) distinctly from benign base-expected-to-fail warnings to the operator and in plan.md, without ever blocking approval.

Branch `kranz/mission-m-2d5583` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$3.85 – $19.25** (expected ~$8.98). Rough estimate — live usage is authoritative; based on 46 completed mission(s).

## Considered alternatives

**Chosen approach:** Two features in one milestone: (f-1-1) a pure, engine-independent contract_lint module — classification polarity plus a synchronous std::process base-tree runner, unit-tested in isolation exactly like contract_sweep.rs — then (f-1-2) wire it into the synchronous approve_plan and surface results via an advisory emit_decision (mirroring preflight) and an appended plan.md section. approve_plan stays synchronous and render_plan_markdown's signature is untouched (the lint section is appended to the plan.md body string), keeping the blast radius to a new module + orchestrator + tests.

Rejected shapes:
- **Make approve_plan async so it can reuse the final gate's async run_shell_command runner directly.** — approve_plan is a sync pub fn called from ~80 sites including synchronous contexts (draft.rs:273, exec.rs:187, server host.rs:503, cli commands.rs:767, planning_tui.rs:1229) and dozens of tests; making it async ripples everywhere for zero behavioral gain, when the synchronous run_with_timeout pattern already runs contract commands safely at preflight.
- **Run the lint in a throwaway detached git worktree checked out at base_sha for exact-base fidelity regardless of a dirty primary.** — A fresh worktree has a cold target/ dir, so every cargo assertion recompiles the whole workspace from scratch — minutes of real compute at every approval; running in the primary checkout (which equals base in the normal not-yet-run case) with a dirty-tree advisory note is cheap (warm target) and good enough for a linter.
- **Extend render_plan_markdown with a lint parameter to interleave per-assertion lint annotations into the contract list.** — render_plan_markdown has other callers (orchestrator.rs:1901 and server rest.rs:273 on the revised-plan path); changing its signature drags those into scope, whereas appending a lint section to the plan.md body inside approve_plan gives the same visibility with no ripple.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The full workspace test suite passes, including the new approval-lint tests (no regressions). 
  `cargo test --workspace --no-fail-fast 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a2]** The approval-time contract lint classifies base outcomes correctly: a provably-buggy command that PASSES against the untouched base (the inverted m-0c885b lockfile-grep shape) is flagged as an author-bug suspect; a normal not-yet-landed command that FAILS on base is marked base-expected-to-fail (not a bug); a command that cannot produce a verdict within the bound is recorded distinctly; and over-budget commands are recorded as not-linted. 
  `cargo test -p kranz-engine approval_lint 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a3]** The lint never blocks approval and never triggers the nested-runtime panic: a #[tokio::test] approves a command-bearing contract from within the process tokio runtime, approval returns Ok, and lint results are recorded. 
  `cargo test -p kranz-engine -- approval_lint_never_blocks approval_lint_no_nested_runtime_panic 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a4]** No clippy warnings across the workspace and all targets. 
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[a5]** All code is rustfmt-clean. 
  `cargo fmt --all --check`
- **[a6]** The lint output visibly distinguishes author-bug suspects (pass-on-base / could-not-verdict) from base-expected-to-fail warnings in BOTH the operator-facing approval decision and the committed plan.md; it is advisory only (approval always proceeds); and it probes the base tree via the synchronous std::process path (mirroring run_with_timeout) rather than constructing any new tokio Runtime inside the synchronous approve_plan. *(agent judgement)*

## Milestone 1 — approve_plan lints command assertions against the base and surfaces suspects vs base-expected-to-fail to the operator and in plan.md, without blocking

### 1.1 Pure classification + synchronous base-tree runner in a new contract_lint module

Create a new engine module `crates/engine/src/contract_lint.rs` holding the pure classification logic and a synchronous, nested-runtime-safe runner that executes contract command assertions against a checkout. Declare the module in `crates/engine/src/lib.rs` exactly the way the existing `contract_sweep` module is declared (same visibility keyword — grep lib.rs for `mod contract_sweep;` and add `mod contract_lint;` beside it). This module must NOT depend on `MissionEngine`; it takes plain inputs so it is unit-testable in isolation, mirroring `crates/engine/src/contract_sweep.rs`.

WHY THIS EXISTS: at approval time the engine must run each `check: command` assertion once against the untouched base tree and tell the operator whether each already passes (a polarity/vacuity SUSPECT — a correctly-scoped 'the work landed' assertion must FAIL before the work lands; the abandoned m-0c885b mission shipped an `a6` grep that could only pass while the requirement was unmet) or fails as expected (the feature simply hasn't landed yet — the usual, benign case).

CRITICAL RUNTIME CONSTRAINT (do not violate): `approve_plan` is a synchronous `pub fn` called from inside the process `#[tokio::main]` runtime. Constructing a new `tokio::runtime::Runtime` and calling `block_on` from there panics unconditionally ('Cannot start a runtime from within a runtime'). Therefore this runner MUST use `std::process::Command` with a poll/try_wait+sleep timeout loop — copy the proven pattern of `run_with_timeout` in `crates/engine/src/orchestrator.rs` (around line 6834, with the explanatory doc at ~6808). Do NOT call the async `run_shell_command`, do NOT build a tokio Runtime, do NOT use `run_bounded_gate_command` (it builds a current-thread runtime and would panic under the ambient runtime).

PUBLIC API to implement:
1. `pub enum AssertionLintOutcome` with variants: `PassedOnBase` (SUSPECT — already green on the untouched base), `FailedOnBase` (base-expected-to-fail — benign), `CouldNotVerdict` (the command hit the per-command timeout and never produced an exit code), `NotLinted` (skipped because the overall wall-clock budget was already spent). Document each variant with the meaning above. Add a helper `pub fn is_author_bug_suspect(&self) -> bool` returning true for `PassedOnBase` and `CouldNotVerdict`, false for `FailedOnBase` and `NotLinted`.
2. `pub struct AssertionLint { pub id: String, pub command: String, pub outcome: AssertionLintOutcome, pub output_tail: String }`.
3. `pub struct ContractLintReport { pub results: Vec<AssertionLint>, pub tree_clean_at_base: bool }` with: `pub fn suspects(&self) -> Vec<&AssertionLint>` (results whose outcome.is_author_bug_suspect()), `pub fn is_empty(&self) -> bool` (no command assertions were linted), and `pub fn summary(&self) -> String` — a compact operator-facing string that lists suspects first under a clear label (e.g. 'author-bug suspects (already pass / no verdict on the untouched base): [a6] <cmd>; ...') and base-expected-to-fail assertions under a separate label, and, when `tree_clean_at_base` is false, prepends a note that the lint ran against a working tree with uncommitted changes so results may not reflect the pristine base.
4. `pub fn classify(ran_to_completion: bool, exited_success: bool) -> AssertionLintOutcome`: `!ran_to_completion` → `CouldNotVerdict`; `ran_to_completion && exited_success` → `PassedOnBase`; `ran_to_completion && !exited_success` → `FailedOnBase`. (The `NotLinted` outcome is assigned by the runner when the budget is spent, not by classify.)
5. `pub fn lint_env(base_sha: Option<&str>) -> std::collections::HashMap<String, String>`: start from `crate::runner::contract_env(base_sha)` (so `KRANZ_BASE_SHA` is set exactly as the final gate sets it, and only when a non-empty base SHA is supplied), then ADD git-hook-disabling keys so any `git` invoked by a contract command runs with hooks off: `GIT_CONFIG_COUNT=1`, `GIT_CONFIG_KEY_0=core.hooksPath`, `GIT_CONFIG_VALUE_0=/dev/null`.
6. `pub fn run_contract_lint(cwd: &std::path::Path, base_sha: Option<&str>, contract: &[crate::types::Assertion], tree_clean_at_base: bool) -> ContractLintReport`: iterate the assertions; SKIP any whose `check` is not `AssertionCheck::Command` and any `command` that is `None`; for each command assertion, if the overall budget is already exhausted record `NotLinted`, otherwise run `sh -c <command>` (unix) via the std::process poll loop with a per-command timeout, in `cwd`, with env from `lint_env`, stdin null, stdout/stderr piped; classify via the exit result (timeout → `ran_to_completion=false`); keep an output tail (~1500 chars). Provide an inner `run_contract_lint_with_limits(cwd, base_sha, contract, tree_clean_at_base, per_command: Duration, overall: Duration)` that `run_contract_lint` calls with module const defaults `PER_COMMAND_TIMEOUT = Duration::from_secs(600)` and `OVERALL_BUDGET = Duration::from_secs(180)`, so tests can inject tiny limits without waiting.

Use `crate::types::{Assertion, AssertionCheck}` for the assertion type (see how `final_gate` in orchestrator.rs reads `assertion.check == AssertionCheck::Command` and `assertion.command.as_deref()`).

TESTS (write them in a `#[cfg(test)] mod tests` in this file; PREFIX every test name with `approval_lint_` so the contract's `cargo test -p kranz-engine approval_lint` filter picks them up):
- `approval_lint_classify_polarity`: classify(true,true)=PassedOnBase, classify(true,false)=FailedOnBase, classify(false,_)=CouldNotVerdict; assert `PassedOnBase.is_author_bug_suspect()` and `CouldNotVerdict.is_author_bug_suspect()` are true, the other two false.
- `approval_lint_env_has_base_sha_and_disables_hooks`: lint_env(Some("deadbeef")) contains KRANZ_BASE_SHA=deadbeef and GIT_CONFIG_KEY_0=core.hooksPath with GIT_CONFIG_VALUE_0=/dev/null and GIT_CONFIG_COUNT=1; lint_env(None) omits KRANZ_BASE_SHA but still sets the hook keys.
- `approval_lint_runner_buckets_true_false`: build a small contract of command assertions using `true` (→ PassedOnBase/suspect) and `false` (→ FailedOnBase/expected) plus one `agent-judgement` assertion; assert the judgement one is skipped and the two commands land in the right buckets; assert `report.suspects()` contains only the `true` one.
- `approval_lint_runner_times_out_slow_command`: with an injected tiny per-command timeout (e.g. 200ms) via run_contract_lint_with_limits, a `sleep 5` command classifies as CouldNotVerdict (a suspect), and it does NOT wait 5 seconds.
- `approval_lint_runner_budget_skips_remainder`: with an injected tiny overall budget, after the budget is spent the remaining command assertions are recorded NotLinted (not suspect, not expected).
- `approval_lint_runner_safe_under_tokio` as a `#[tokio::test]`: call run_contract_lint (or the with_limits form) from within the tokio runtime over a tiny `true`/`false` contract and assert it returns without panicking (regression for the nested-runtime trap).

FILES YOU MAY TOUCH: `crates/engine/src/contract_lint.rs` (new) and `crates/engine/src/lib.rs` (module declaration only). Run `cargo test -p kranz-engine approval_lint`, `cargo clippy -p kranz-engine --all-targets -- -D warnings`, and `cargo fmt --all --check` before finishing (CI enforces fmt).

Done when:
- classify() returns PassedOnBase for ran+success, FailedOnBase for ran+nonzero, and CouldNotVerdict when the per-command timeout elapses — each covered by a unit test; is_author_bug_suspect() is true for PassedOnBase and CouldNotVerdict only.
- lint_env(Some(sha)) contains KRANZ_BASE_SHA=sha plus the git-hook-disabling keys (GIT_CONFIG_COUNT=1, core.hooksPath→/dev/null); lint_env(None) omits KRANZ_BASE_SHA but still disables hooks.
- run_contract_lint classifies `true` as PassedOnBase (suspect) and `false` as FailedOnBase (expected), skips agent-judgement assertions, and report.suspects() returns only the pass-on-base one.
- With an injected short per-command timeout a `sleep`ing command classifies as CouldNotVerdict without blocking for the full sleep; with an injected tiny overall budget the remaining command assertions are recorded NotLinted.
- A #[tokio::test] runs the runner from inside the process tokio runtime and returns without panicking (no nested tokio Runtime is constructed anywhere in the module).
- cargo fmt --all --check and cargo clippy -p kranz-engine --all-targets -- -D warnings are clean.

### 1.2 Invoke the lint in approve_plan and surface suspects vs base-expected-to-fail in the approval decision and plan.md

Wire the `contract_lint` module (built in the sibling feature) into `MissionEngine::approve_plan` in `crates/engine/src/orchestrator.rs` so that every approval runs the command assertions against the base tree, records the result, surfaces it to the operator, and embeds it in the committed plan.md — WITHOUT ever blocking approval and WITHOUT changing `approve_plan`'s synchronous signature.

CONTEXT — current approve_plan flow (orchestrator.rs ~line 1232): it validates milestones/features, assigns assertion ids, computes the cost estimate, then (line ~1274) creates the mission branch, resolves `base_sha` (line ~1288; the comment at 1281–1288 explains resolving it anywhere here pins the base tip — moving the resolution EARLIER is safe because creating the branch does not move the base ref), renders plan.md via `render_plan_markdown` (line ~1295) into `plan_md_body`, writes/commits plan.json + plan.md + index.md (+ research.md) as the '[kranz] approved plan' commit, then emits `EventKind::PlanApproved`.

WHAT TO ADD:
1. Resolve `base_sha` (via `self.repo.rev_parse(&base)`) BEFORE the branch is created, so the lint runs 'before the branch exists' as the mission requires. Keep the existing single pinned base_sha value used for the PlanApproved event (do not resolve it twice).
2. Determine `tree_clean_at_base`: whether the active checkout has no tracked/uncommitted modifications relative to base (use the existing repo API used elsewhere for cleanliness — see how `contract_sweep::primary_checkout_finding` callers obtain cleanliness, or `self.repo` status helpers in git_ops). If you cannot cheaply determine it, default to `true` but prefer a real check; this only controls an advisory note.
3. Call `contract_lint::run_contract_lint(self.paths.repo_root.as_path(), Some(&base_sha), &plan.validation_contract, tree_clean_at_base)`. Wrap it so ANY failure is non-fatal (advisory): the lint must never turn approve_plan into an Err.
4. Emit the lint as an advisory operator-facing decision using the same mechanism preflight uses (`self.emit_decision(&summary, None)` — see run_loop ~line 2313 and PREFLIGHT_CLEAR_SUMMARY). The summary is `report.summary()`: it must list author-bug SUSPECTS (pass-on-base / could-not-verdict) under a clear label distinct from the base-expected-to-fail assertions, and note a dirty tree if applicable. When there are no command assertions, emit a short 'no command assertions to lint' note. This decision must be emitted so it reaches every surface (CLI/Slack/dashboard) exactly like the preflight summary.
5. Embed the lint into the committed plan.md. DO NOT change `render_plan_markdown`'s signature (it has other callers — orchestrator.rs ~1901 and crates/server/src/rest.rs:273 for revised plans). Instead, after `render_plan_markdown` returns `plan_md_body`, APPEND a '## Contract lint (approval-time, against the base tree)' section built from the report: one bullet per linted command assertion showing its id, outcome label (suspect / base-expected-to-fail / could-not-verdict / not-linted), and command, with suspects called out first; include the dirty-tree note when relevant. Append to the same `plan_md_body` string that is written and committed in BOTH the worktree-mode and non-worktree branches of approve_plan, so the committed plan.md carries the lint in both isolation modes.
6. Approval MUST still succeed regardless of lint outcomes (linter, not a verdict). Do not add any early-return / error path driven by the lint.

TESTS — add to `crates/engine/tests/mission_test.rs` as `#[tokio::test]`s (these approve from within the tokio runtime, which also regression-guards the nested-runtime panic). PREFIX names with `approval_lint_`, and name two of them EXACTLY `approval_lint_never_blocks` and `approval_lint_no_nested_runtime_panic` (the contract's a3 filters on these). Look at existing approve_plan tests in this file (e.g. `preflight_flags_missing_program_and_ignores_present_ones` ~line 3588 and the simple_plan helper) for the setup idiom (init_repo, MockBackend, MissionConfig, building a Plan with a validation_contract). Cover:
- `approval_lint_flags_pass_on_base_suspect`: approve a Planning mission whose contract includes a command that PASSES on the untouched base (an inverted-grep shape or simply `true`) AND a command that FAILS on base (`false` or a grep for a not-yet-present string). Assert approval returns Ok, and the emitted decision (inspect the event log / decisions) flags the pass-on-base command as an author-bug suspect while labeling the fail-on-base one base-expected-to-fail.
- `approval_lint_writes_section_to_plan_md`: after approval, the committed plan.md (read it from the mission branch or the primary twin, matching how other tests read plan.md) contains the '## Contract lint' section listing the assertions' base outcomes with suspects separated from base-expected-to-fail.
- `approval_lint_never_blocks`: a contract whose command assertions ALL pass on base still approves successfully (Ok), proving the lint is advisory.
- `approval_lint_no_nested_runtime_panic`: approving a command-bearing contract from within the #[tokio::test] runtime does not panic and returns Ok (the lint used the synchronous path).

FILES YOU MAY TOUCH: `crates/engine/src/orchestrator.rs` and `crates/engine/tests/mission_test.rs`. Before finishing, run `cargo test -p kranz-engine approval_lint`, `cargo test --workspace --no-fail-fast`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all --check` (CI enforces fmt).

Done when:
- A #[tokio::test] approves a Planning mission whose contract has one pass-on-base command and one fail-on-base command; approval returns Ok and the emitted approval decision flags the pass-on-base one as an author-bug suspect and the fail-on-base one as base-expected-to-fail.
- The committed plan.md contains a '## Contract lint' section listing each command assertion's base outcome with suspects visibly separated from base-expected-to-fail warnings (verified in both worktree and non-worktree isolation paths that write plan.md).
- approve_plan never returns Err because of lint outcomes and never blocks: a contract whose commands all pass on base still approves successfully (test named approval_lint_never_blocks).
- The lint executes before the mission branch is created and does not panic under the process tokio runtime (test named approval_lint_no_nested_runtime_panic).
- When the working tree is not clean at base, the lint decision and plan.md section note that results may not reflect the pristine base, still without blocking approval.
- cargo test --workspace --no-fail-fast, cargo clippy --workspace --all-targets -- -D warnings, and cargo fmt --all --check are all clean.

