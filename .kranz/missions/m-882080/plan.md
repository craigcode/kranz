# Mission plan — m-882080

**Goal:** Make each milestone validation round judge the milestone only against contract assertions due by that milestone: an assertion can name the plan milestone that must satisfy it, a not-yet-due assertion is still run and its result recorded but produces no blocking finding and no fix feature before its milestone, approval refuses a plan naming an unknown milestone, unscoped plans behave exactly as today, and the final gate still runs every assertion.

Branch `kranz/mission-m-882080` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Authority granted on approval

Approving this plan grants the following to the mission's sessions with no further prompt.

**Commands** (1; each is a prefix, so `x` also allows `x <anything>`):
- `cargo run --locked --package kranz -- knowledge-refresh`

**Write paths** (9; glob patterns the mission may change):
- `crates/engine/src/**`
- `crates/engine/tests/**`
- `crates/cli/src/**`
- `crates/cli/tests/**`
- `crates/server/src/**`
- `crates/server/tests/**`
- `crates/slack/src/**`
- `crates/slack/tests/**`
- `docs/knowledge/**`

## Cost estimate

Estimated **$6.37 – $31.87** (expected ~$14.60). Rough estimate — live usage is authoritative; based on 50 completed mission(s).

Context-fit check (corpus p90 over 60 plan(s): 4350 chars / 24 files):
- **f-1-1** (Add optional Assertion.milestone field (schema only, no behaviour change)) — spec points at 37 files (corpus p90: 24)
- **f-1-2** (Due-at rule, approval refusal for unknown milestones, and plan schema description) — spec is 5164 chars (corpus p90: 4350)
- **f-1-3** (Scope validation rounds to due assertions, record not-yet-due results, divert not-due findings) — spec is 7221 chars (corpus p90: 4350)

## Considered alternatives

**Chosen approach:** An optional `milestone` field (a plan milestone title) on `Assertion`, delivered as one milestone with four sequential features. The tie lives on the frozen contract item, so revisions cannot move it. Titles are what the planner writes, while ms-N ids are engine-assigned by position. The engine itself withholds not-yet-due assertions from the validators and diverts any finding that cites one, so the outcome does not depend on the model. It is one milestone because a multi-milestone plan for this mission would hit the very bug being fixed: later features' test filters would fail the first milestone's round.

Rejected shapes:
- **An `assertions: Vec<String>` id list on PlanMilestone** — `assign_assertion_ids` renames empty or colliding ids at approval, which can silently orphan the list, and revisions would need extra consistency rules.
- **No schema change: show not-yet-due results to the validator labelled 'do not report', or downgrade their severity** — The engine cannot know when an assertion is due without a tie. Any unwaived finding of any severity still triggers convert_findings and fix features, and relying on validator compliance is not deterministic.
- **Split into two milestones (schema and approval, then round scoping)** — The running engine predates this fix, so milestone 1's round would run milestone 2's test-filter assertions, which match nothing yet, and record them as critical findings: exactly the reported failure.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** In a two-milestone mission whose failing command assertion names the second milestone, the first milestone's round records the failing result in a `validation.assertion.notdue` event. That round emits no `validation.finding` and no `fixfeature.created` for the assertion, and the first milestone completes. In the second milestone's round the failure reaches the functional validator as a FAIL line, is emitted as a `validation.finding` for ms-2 and produces a fix feature.
  `cargo test -p kranz-engine --test mission_test assertion_due_round_ 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a2]** A plan in which no assertion names a milestone behaves exactly as today. Every round runs and shows every assertion to the validators, no `validation.assertion.notdue` or `validation.finding.notdue` event is emitted, and the persisted plan JSON carries no `milestone` key.
  `cargo test -p kranz-engine --test mission_test assertion_due_unscoped_ 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a3]** The final gate turns a failing command assertion into a critical `command-assertion` finding whatever milestone the assertion names.
  `cargo test -p kranz-engine --test mission_test assertion_due_final_gate_ 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a4]** Approval refuses a plan whose assertion names a milestone title that matches no plan milestone, or more than one, and the error message contains the assertion id. A revised plan is refused when it names an unknown milestone or changes an existing assertion's milestone.
  `cargo test -p kranz-engine --test mission_test assertion_due_approval_ 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a5]** The planner-facing plan JSON Schema describes the optional `milestone` property on validation-contract items, and a contract assertion without the field deserializes and re-serializes byte-identically (no `milestone` key).
  `cargo test -p kranz-engine --lib assertion_due_schema_ 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a6]** An assertion is due in round i when it names no milestone, or when its milestone sits at position i or earlier. Titles match after trimming and ignoring case, and an unresolvable title counts as due everywhere.
  `cargo test -p kranz-engine --lib assertion_due_rule_ 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a7]** The frozen prompt surfaces are byte-identical to the pinned base: crates/engine/src/prompts.rs and everything under crates/engine/prompts/.
  `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/prompts.rs crates/engine/prompts/`
- **[a8]** The workspace is rustfmt-clean.
  `cargo fmt --all --check`
- **[a9]** The workspace is clippy-clean with warnings denied.
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[a10]** The full workspace test suite passes.
  `cargo test --workspace`
- **[a11]** No knowledge note is stale against the files the mission changed.
  `cargo run --locked --package kranz -- knowledge-refresh`
## Contract lint

Each `check: command` assertion above was run once against the untouched base tree at approval time. Suspects are assertions that already pass (or could not reach a verdict) before this plan's work lands — a possible polarity/vacuity bug in the assertion itself. This never blocks approval.

author-bug suspects (already pass / no verdict on the untouched base): [a7] git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/prompts.rs crates/engine/prompts/; [a8] cargo fmt --all --check; [a9] cargo clippy --workspace --all-targets -- -D warnings
base-expected-to-fail (benign): [a1] cargo test -p kranz-engine --test mission_test assertion_due_round_ 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a2] cargo test -p kranz-engine --test mission_test assertion_due_unscoped_ 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a3] cargo test -p kranz-engine --test mission_test assertion_due_final_gate_ 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a4] cargo test -p kranz-engine --test mission_test assertion_due_approval_ 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a5] cargo test -p kranz-engine --lib assertion_due_schema_ 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a6] cargo test -p kranz-engine --lib assertion_due_rule_ 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a10] cargo test --workspace
not linted (never run on the base): [a11] cargo run --locked --package kranz -- knowledge-refresh

Lint commands ran under shell: sh

named contract gates (defect classes):
- vacuous-filter: PASS
- wrong-polarity: PASS
- host-shell-syntax: PASS
- passes-on-base: FAIL
  [a7] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/prompts.rs crates/engine/prompts/`
  [a8] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo fmt --all --check`
  [a9] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo clippy --workspace --all-targets -- -D warnings`
- env-sensitive: PASS


## Milestone 1 — Validation rounds hold a milestone only to the assertions due by it

### 1.1 Add optional Assertion.milestone field (schema only, no behaviour change)

Context: kranz is a Rust workspace (crates/engine = kranz-engine, crates/cli, crates/server, crates/slack). A mission plan's validation contract is a flat Vec<Assertion> (`pub struct Assertion` in crates/engine/src/types.rs around line 612, serde rename_all camelCase, fields id, statement, check, command, pty_script, negative_control). We are adding a way for an assertion to name the plan milestone that must satisfy it. This feature ONLY adds the data field and keeps every existing behaviour unchanged; later features use it.

Do:
1. Add to `Assertion` a new field, placed after `command`, with a doc comment: `/// The title of the plan milestone that must satisfy this assertion. `None` (every pre-field contract) means the assertion is due at every milestone, as before. Additive: skip_serializing_if keeps old plans byte-identical.` and attributes `#[serde(default, skip_serializing_if = "Option::is_none")] pub milestone: Option<String>,`.
2. Update every struct-literal construction of `Assertion { ... }` across the workspace to add `milestone: None` (about 69 sites in 27 files: crates/engine/src/*.rs including orchestrator.rs, planning.rs, contract_lint.rs, pty_harness.rs, contract_gates.rs, judgement.rs, preflight.rs, pack.rs, pr_handoff.rs, contract_health.rs, reviewer_independence_tests.rs, contract_controls_tests.rs; crates/engine/tests/*.rs including mission_test.rs, worktree_isolation.rs, config_cost_prompts_test.rs, control_digest_test.rs, report_test.rs, reducer_test.rs, runner_test.rs, backend_mock_test.rs; crates/cli/src/output.rs, crates/cli/tests/cli_test.rs; crates/server/src/tickets.rs, crates/server/tests/server_test.rs; crates/slack/src/outbound.rs, crates/slack/src/bridge.rs). Find them with `cargo build --workspace --all-targets` errors or grep for `Assertion {`. A careful scripted bulk edit is allowed, but verify the result compiles and is rustfmt-clean.
3. Do NOT touch crates/engine/src/prompts.rs or anything under crates/engine/prompts/ (frozen surface).
4. Add unit tests in crates/engine/src/types.rs (inside its existing #[cfg(test)] module, or a new one) whose names start with `assertion_due_schema_`: (a) `assertion_due_schema_legacy_roundtrip_is_byte_identical`: a JSON assertion object without `milestone` deserializes with `milestone == None` and re-serializes to exactly the same JSON string (no `milestone` key); (b) `assertion_due_schema_milestone_roundtrips`: an assertion with `"milestone": "M2"` deserializes to Some("M2") and serializes back with the key.

Before finishing run, bare and reading raw exit codes: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`. No new dependencies.

Done when:
- `cargo test -p kranz-engine --lib assertion_due_schema_` runs at least 2 tests and all pass
- A contract assertion JSON without `milestone` round-trips byte-identically (asserted by a test)
- `cargo build --workspace --all-targets` succeeds with every Assertion literal updated
- crates/engine/src/prompts.rs and crates/engine/prompts/ are unchanged
- cargo fmt --all --check and cargo clippy --workspace --all-targets -- -D warnings pass

### 1.2 Due-at rule, approval refusal for unknown milestones, and plan schema description

Context: kranz-engine (crates/engine). `Assertion` (crates/engine/src/types.rs) now has `milestone: Option<String>`, the title of the plan milestone that must satisfy it (added by a previous feature). Plan milestones are `PlanMilestone { title, features }` (types.rs ~357). At approval the reducer turns them into state `Milestone`s with ids `ms-1`, `ms-2`, ... by position (crates/engine/src/reducer.rs ~84-112). Titles are the planner-visible identity. Revisions already compare titles with `planning::norm_title` (trim + lowercase, crates/engine/src/planning.rs ~593).

Do:
1. Create a new module crates/engine/src/contract_scope.rs (register it in crates/engine/src/lib.rs as `pub(crate) mod contract_scope;` or pub if tests need it) with:
   - `pub(crate) fn due_index(assertion: &Assertion, milestone_titles: &[&str]) -> Option<usize>` returns the 0-based position of the milestone whose `norm_title` equals the assertion's `norm_title(milestone)`. It returns None when the field is None, or when the title matches zero milestones or more than one.
   - `pub(crate) fn is_due(assertion: &Assertion, milestone_titles: &[&str], round_index: usize) -> bool` returns true when the field is None, when the title cannot be resolved (fail toward today's behaviour: due everywhere), or when due_index <= round_index. Otherwise it returns false.
   - `pub(crate) fn validate_milestone_refs(contract: &[Assertion], milestone_titles: &[&str], context: &str) -> crate::error::Result<()>`. For each assertion with Some(milestone) matching zero titles, return `EngineError::InvalidState(format!("{context} validation assertion '{id}' names milestone '{m}', which matches no plan milestone"))`. If it matches more than one, return a similar "... matches more than one plan milestone" error. Both messages contain the assertion id.
   - Unit tests named with prefix `assertion_due_rule_` covering: None is due at every index; scoped is not due before its milestone, due at it and due after it; matching is trim and case-insensitive; an unknown title is due everywhere; an ambiguous title is due everywhere; and validate_milestone_refs returns Ok, the unknown error and the ambiguous error (each error containing the id).
2. Initial approval: in `approve_plan_as` (crates/engine/src/orchestrator.rs ~1309), after the existing 'plan milestone has no features' check and before events are emitted, call `contract_scope::validate_milestone_refs(&plan.validation_contract, <plan milestone titles>, "plan")?`. Note that `assign_assertion_ids` runs later (~1449) and may rename empty or colliding ids. That is acceptable; the message uses the id as written.
3. Revised plans: in `validate_revised_plan_for_gate` (planning.rs ~491) call validate_milestone_refs with context "revised plan". In `validate_contract_extends` (planning.rs ~548), also treat a change of `milestone` on an existing assertion as 'revised plan changes validation assertion'. The reducer mirrors these checks in `ensure_contract_extends` (reducer.rs ~1054). Add the same milestone comparison there so the pre-emit gate and the reducer agree (the gate must never accept what the reducer rejects).
4. Planner schema: in `fn plan_schema()` (planning.rs ~621), add to the validationContract item `properties` an entry `"milestone": { "type": "string", "description": "Optional. The exact title of the plan milestone that must deliver this assertion. Validation rounds for earlier milestones still run it and record the result, but do not treat a failure as a finding. Omit it for checks that must hold at every milestone (format, lint, the full test suite). The final gate always checks every assertion." }`. Do NOT edit crates/engine/src/prompts.rs or crates/engine/prompts/** (frozen). The schema description is the only planner-facing change.
5. Tests:
   - In planning.rs tests, a unit test `assertion_due_schema_plan_schema_describes_milestone` asserting the schema's validationContract items have a `milestone` property of type string with a non-empty description.
   - In crates/engine/tests/mission_test.rs, integration tests with prefix `assertion_due_approval_`: (a) `assertion_due_approval_refuses_unknown_milestone`: approving a plan whose assertion id `a-scope` names milestone 'Nope' errors and the error string contains `a-scope`, and the mission stays in planning (no plan.approved event); (b) `assertion_due_approval_accepts_known_milestone`: a two-milestone plan with an assertion naming the second milestone's title in different case is approved and its persisted contract keeps the field; (c) `assertion_due_approval_revision_refuses_milestone_change`, or an unknown milestone in a revision, if a revised-plan entry point is reachable from tests (see existing test `approve_revised_plan_rejects_dropping_a_completed_milestone` around mission_test.rs:9500 for the pattern). Use existing helpers in mission_test.rs: setup() ~121, test_cfg() ~207, make_engine ~221, simple_plan ~237, assertion() ~549 (set `.milestone` on the returned value).

Before finishing run, bare: cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace. No new dependencies.

Done when:
- `cargo test -p kranz-engine --lib assertion_due_rule_` runs >=1 test, all pass, covering none/before/at/after/case-insensitive/unknown/ambiguous
- `cargo test -p kranz-engine --lib assertion_due_schema_` passes including the plan_schema description test
- `cargo test -p kranz-engine --test mission_test assertion_due_approval_` runs >=2 tests, all pass; the refusal error contains the assertion id
- A revised plan that changes an existing assertion's milestone or names an unknown milestone is refused, by both planning.rs and the reducer
- prompts.rs and crates/engine/prompts/ unchanged

### 1.3 Scope validation rounds to due assertions, record not-yet-due results, divert not-due findings

Context: kranz-engine (crates/engine). `Assertion.milestone: Option<String>` exists. crates/engine/src/contract_scope.rs provides `is_due(assertion, milestone_titles, round_index)` and `due_index(...)`: due when the field is None, the title is unresolvable, or the named milestone's position <= round_index. Today `MissionEngine::validation_round(mi)` (crates/engine/src/orchestrator.rs ~6021) clones the WHOLE `self.state.mission.validation_contract` (~6071). It runs every command assertion through `run_contract_commands_for_validation` (~5949, renders `- [id] `cmd` → PASS/FAIL (shell: ..)` lines) and every pty-script assertion through `crate::pty_harness::run_pty_assertions` (~6096). It joins them into `contract_results` (~6154-6163), which goes into the functional validator's task as authoritative evidence (crates/engine/src/runner.rs ~2034). Both validators also receive the contract list (`contract_rendered`, runner.rs ~1887) and runtime evidence for agent-judgement assertions. The validator writes findings. Findings are collected (~6519-6521) and emitted as `EventKind::ValidationFinding` (~6542-6548). Any non-empty findings go to `convert_findings` (crates/engine/src/findings.rs ~141), which creates fix features (`fixfeature.created`) and bumps fix_cycles. The same contract_results are reused for the retry (~6386) and the frontier confirmation (~6501).

Problem: at milestone i, assertions planned for a later milestone fail (their test filters match nothing yet) and become critical findings and fix features.

Do (in the round for milestone index mi; titles = self.state.mission.milestones titles):
1. Partition the contract into `due` (is_due true) and `not_due` (false). The due milestone id is `self.state.mission.milestones[due_index].id`.
2. Run due command and pty assertions exactly as today. ONLY their rendered lines form `contract_results`. Run the not_due command assertions with the same env, sandbox, shell and timeout, and the not_due pty assertions through the same harness. Their transcripts are recorded as today by the harness, but their rendered text is never added to contract_results.
3. Add two events in crates/engine/src/events.rs, following the style of `ValidationPtyTranscript` (~472) and adding the type_name arms (~1230):
   - `#[serde(rename = "validation.assertion.notdue")] ValidationAssertionNotDue { milestone_id: String, assertion_id: String, due_milestone_id: String, verdict: GateVerdict, detail: String }`, emitted once per not-yet-due command or pty assertion per round, pass or fail. `detail` is a bounded output tail, the same bound used for round evidence lines; reuse the existing tail helper.
   - `#[serde(rename = "validation.finding.notdue")] ValidationFindingNotDue { milestone_id: String, run_id: String, due_milestone_id: String, finding: Finding }`.
   Both fold as no-ops in the reducer (crates/engine/src/reducer.rs). Keep any exhaustive matches compiling (report_render, outcomes, trace_export, corpus_export, slack and cli formatters, etc.); a short rendering is welcome where similar events render.
4. Pass validators only the `due` assertions: the contract list rendered into both the scrutiny and functional tasks, and any runtime evidence for agent-judgement assertions, filtered to due ones. Find where validation_round builds the runner call and filter the contract slice there. Do not change runner.rs task wording, and do not edit crates/engine/src/prompts.rs or crates/engine/prompts/** (frozen).
5. Deterministic backstop: before emitting ValidationFinding, take every validator finding whose `subject.trim()` equals the id of a not_due assertion out of the findings set. Emit it as `ValidationFindingNotDue` instead. Such findings never reach convert_findings, never create fix features and never block the milestone. Apply this to the final findings set the round acts on, covering primary, retry and confirmation.
6. Unscoped plans (no assertion has a milestone) must be byte-for-byte unchanged in behaviour: same commands, same validator task text, no new events.
7. Do not change final_gate (crates/engine/src/orchestrator/finalization.rs). It must still run every command assertion and turn failures into critical `command-assertion` findings regardless of milestone.
8. Integration tests in crates/engine/tests/mission_test.rs:
   - `assertion_due_round_defers_failure_until_its_milestone`: two milestones 'M1' and 'M2', each with one feature; the functional validator is enabled (skip_functional false; see test_cfg ~207); max_fix_cycles_per_milestone 1. Contract: an unscoped passing command (use the portable helper file_exists_contract ~76 or a command that always succeeds), plus `a-late` with a failing command (`cd kranz-no-such-dir`, as at ~4137) and `milestone: Some("M2")`. Script the M1 functional validator to return a critical finding with subject `a-late` (use `validator_with(json!([...]))` ~335). This simulates the old behaviour. Assert:
     - the log has a `validation.assertion.notdue` event for ms-1 / a-late with a failing verdict and due_milestone_id ms-2;
     - a `validation.finding.notdue` event exists for ms-1;
     - there is no `validation.finding` and no `fixfeature.created` for ms-1, and ms-1 completes;
     - the ms-1 functional validator task (MockBackend::started_specs ~467) does not contain `a-late`;
     - the ms-2 functional validator task contains `[a-late]` and `FAIL`;
     - when the M2 validator is scripted with a critical a-late finding, it is emitted as `validation.finding` for ms-2 and a fix feature is created for ms-2 (orchestrator reply `fix_features(1)` ~488).
     Model the script ordering on `approve_revised_plan_rejects_dropping_a_completed_milestone` (~9500, a two-milestone mission with scripted validators, order commented ~9518-9536) and `contract_commands_run_engine_side_and_reach_validator_task` (~3858). Helpers: worker_pass ~279, orch_script ~464, dirty_tree_commit_as_is ~475, judgement ~480, no_lesson ~503, read_log ~568, event_types ~572.
   - `assertion_due_unscoped_plan_behaves_as_before`: the same two-milestone shape, but no assertion names a milestone. Assert both rounds' functional validator tasks list and show results for every assertion, the log contains no `validation.assertion.notdue` / `validation.finding.notdue`, and plan.json on disk has no `"milestone"` key.
   - `assertion_due_final_gate_fails_scoped_assertion`: a mission whose failing command assertion names a milestone (e.g. the first of two, or the only milestone) reaches the final gate. Assert a critical finding with class `command-assertion` and that assertion id as subject is emitted. Model it on `command_assertion_at_final_gate_is_non_waivable` (~4125).
   Every new test name must start with `assertion_due_round_`, `assertion_due_unscoped_` or `assertion_due_final_gate_` exactly as named above.

Before finishing run, bare: cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace. If the workspace suite fails, rerun it with full output captured to find the failing test; the sandbox containment tests print 'Operation not permitted' on passing runs, so those lines are not evidence of a fault. No new dependencies.

Done when:
- `cargo test -p kranz-engine --test mission_test assertion_due_round_` passes: ms-1 records a failing notdue result, emits no validation.finding or fixfeature.created for ms-1, and completes; ms-2 shows the FAIL to the validator and creates a fix feature
- `cargo test -p kranz-engine --test mission_test assertion_due_unscoped_` passes: no new events, every assertion shown in every round, no milestone key in plan.json
- `cargo test -p kranz-engine --test mission_test assertion_due_final_gate_` passes: the scoped failing assertion yields a critical command-assertion finding at the final gate
- finalization.rs final-gate assertion handling is unchanged
- prompts.rs and crates/engine/prompts/ unchanged; cargo test --workspace passes

### 1.4 Document milestone-scoped assertions and recheck stale knowledge notes

Context: kranz keeps agent-maintained knowledge notes under docs/knowledge/ with YAML frontmatter (`owner`, `freshness`, `last_verified`, `verified_against`). `cargo run --locked --package kranz -- knowledge-refresh` fails when a path a note cites in `verified_against` changed after the note's `last_verified` date. Previous features in this mission changed crates/engine/src/types.rs, planning.rs, orchestrator.rs, reducer.rs, events.rs, a new contract_scope.rs, and assertion literals in the cli, server and slack crates. The behaviour they added:
- a contract assertion may carry `milestone` (a plan milestone title, matched after trimming and ignoring case);
- approval refuses an unknown or ambiguous title, naming the assertion id, and revisions may not change the field;
- a milestone round treats an assertion as due when it names no milestone or names that milestone or an earlier one;
- not-yet-due assertions are still run each round and recorded as `validation.assertion.notdue` events, kept out of the validators' tasks, and any validator finding citing one is diverted to `validation.finding.notdue` (no fix feature, no block);
- the final gate still runs every assertion.

Do:
1. In docs/knowledge/validation/gates.md, add a concise section "Milestone-scoped assertions" describing the above, near the final-gate / validation-round material. Add a dated `Rechecked YYYY-MM-DD (UTC)` line at the top of the recheck list in the file's existing style, and update `last_verified` and, if needed, `verified_against` (add crates/engine/src/contract_scope.rs).
2. In docs/knowledge/architecture/mission-pipeline.md, add one or two sentences where validation rounds are described, plus a recheck line and a `last_verified` bump.
3. Run `cargo run --locked --package kranz -- knowledge-refresh`. For every note it flags as stale (likely glossary.md, decisions/inviolable-invariants.md, CONVENTIONS.md and surfaces/slack-commands.md), read the note against the change. Add a short dated recheck line stating what changed and whether the note's claims still hold, fixing any claim that is now wrong, and bump `last_verified`. Agents may recheck human-owned notes; follow docs/knowledge/CONVENTIONS.md for how. Optionally add a glossary entry for 'not-yet-due assertion'.
4. Use the current UTC date for recheck lines and last_verified, and make sure it is on or after the commit dates of the changed code.
5. Touch only docs/knowledge/**. Do not edit code or the prompt files.

Finish when `cargo run --locked --package kranz -- knowledge-refresh` exits 0. Also run `cargo fmt --all --check` to confirm nothing else changed.

Done when:
- `cargo run --locked --package kranz -- knowledge-refresh` exits 0
- docs/knowledge/validation/gates.md has a section describing milestone-scoped assertions, not-yet-due recording, the finding diversion, the approval refusal, and that the final gate checks every assertion
- Only files under docs/knowledge/ are changed by this feature
