# Mission plan — m-4ad3ae

**Goal:** Park a non-passing worker run for a WorkerDeny grant only when the denial that stopped the session matches a liftable Bash deny rule, and otherwise record one orchestrator decision saying why no grant was offered.

Branch `kranz/mission-m-4ad3ae` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Authority granted on approval

Approving this plan grants the following to the mission's sessions with no further prompt.

**Commands** (3; each is a prefix, so `x` also allows `x <anything>`):
- `git diff $KRANZ_BASE_SHA -- crates/engine/src/orchestrator.rs`
- `git diff $KRANZ_BASE_SHA -- crates/engine/tests/mission_test.rs`
- `git show $KRANZ_BASE_SHA:crates/engine/src/orchestrator.rs`

**Write paths** (3; glob patterns the mission may change):
- `crates/engine/src/orchestrator.rs`
- `crates/engine/tests/mission_test.rs`
- `docs/scoping/validator-park-only-when-denial-stopped-it.md`

## Cost estimate

Estimated **$1.96 – $11.71** (expected ~$5.37). Rough estimate — live usage is authoritative; based on 58 completed mission(s).

Context-fit check (corpus p90 over 69 plan(s): 5008 chars / 25 files):
- **f-1-1** (Key the worker deny-grant park on the stopping denial, with mission tests and the scoping note) — spec is 10714 chars (corpus p90: 5008); spec points at 28 files (corpus p90: 25)

## Considered alternatives

**Chosen approach:** Do it all in one feature. Switch maybe_park_for_worker_deny_grant to the existing stopping_denied_command field. Mirror the validator path's not-offering decision, make the session-end summary read the right report field per role, and add two mission tests that copy existing templates with the FIFO order written out. The engine change is about 30 lines and the tests need no reopen or final-gate scripting, so one fresh session can finish it.

Rejected shapes:
- **Split the mission tests into their own feature, separate from the engine change (lesson m-e7b464)** — That lesson targets reopen and final-gate scripting. Here the tests are simple FIFO copies of existing ones, and splitting them out would leave the engine feature with no criteria-encoding tests.
- **Merge the validator and worker park paths into one shared helper** — The paths differ in grant kind, target (command vs rule) and rule lookup. A shared helper adds indirection and puts the validator behaviour at risk without removing much code.
- **Keep offering the first denied command and only add session-end detail to the grant description (D-X2)** — The operator rejected this in favour of D-X1. It still asks the operator to weaken a guardrail the worker did not need, and it still idles the mission for the grant timeout.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The new mission tests (filter prefix worker_deny_stop_rule, which matches no pre-existing test) exist, at least two of them run, and all pass: a worker denied a command that then made further tool calls and failed raises no grant.requested and is charged to the normal respawn budget, and a worker whose last tool call was denied parks a WorkerDeny grant for the rule matching that stopping command, not for an earlier denied command.
  `out=$(cargo test -p kranz-engine --test mission_test worker_deny_stop_rule 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. ([2-9]|[1-9][0-9]+) passed'`
- **[a2]** The two existing worker deny-grant tests (worker_deny_grant_lifts_the_rule_and_respawn_completes and worker_deny_grant_respawn_does_not_eat_the_respawn_budget) still pass, and both run.
  `out=$(cargo test -p kranz-engine --test mission_test worker_deny_grant 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. 2 passed'`
- **[a3]** The whole mission_test suite passes, including the validator stopping-denial tests that rely on the existing session-end summary wording and worker_denial_on_a_passing_run_does_not_park.
  `out=$(cargo test -p kranz-engine --test mission_test 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a4]** The kranz-engine library unit test suite passes.
  `out=$(cargo test -p kranz-engine --lib 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a5]** No second way of deciding which denial stopped a session is added and prompts are untouched: runner.rs and prompts.rs are byte-identical to the base commit.
  `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/runner.rs crates/engine/src/prompts.rs`
- **[a6]** The workspace is rustfmt-clean and kranz-engine is clippy-clean with warnings denied.
  `cargo fmt --all --check && cargo clippy -p kranz-engine --all-targets -- -D warnings`
- **[a7]** The new tests are not vacuous. Against the base commit's orchestrator.rs, the continued-past test would fail, because the old code parks a grant.requested for Bash(git push*). The last-call test would fail too, because the old code offers Bash(git stash*), the first denied command, instead of Bash(git push*). The worker path decides the stopping denial only from the existing RunOutcome fields stopping_denied_command / denied_commands, and each non-offer emits exactly one orchestrator decision in the validator path's 'not offering a grant for `...`: ...' wording. *(agent judgement)*
## Contract lint

Each `check: command` assertion above was run once against the untouched base tree at approval time. Suspects are assertions that already pass (or could not reach a verdict) before this plan's work lands — a possible polarity/vacuity bug in the assertion itself. This never blocks approval.

author-bug suspects (already pass / no verdict on the untouched base): [a5] git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/runner.rs crates/engine/src/prompts.rs
base-expected-to-fail (benign): [a1] out=$(cargo test -p kranz-engine --test mission_test worker_deny_stop_rule 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. ([2-9]|[1-9][0-9]+) passed'; [a2] out=$(cargo test -p kranz-engine --test mission_test worker_deny_grant 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. 2 passed'; [a3] out=$(cargo test -p kranz-engine --test mission_test 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a4] out=$(cargo test -p kranz-engine --lib 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a6] cargo fmt --all --check && cargo clippy -p kranz-engine --all-targets -- -D warnings

Lint commands ran under shell: sh

named contract gates (defect classes):
- vacuous-filter: PASS
- wrong-polarity: PASS
- host-shell-syntax: PASS
- passes-on-base: FAIL
  [a5] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/runner.rs crates/engine/src/prompts.rs`
- env-sensitive: PASS


## Milestone 1 — Worker deny-grant offered only for the denial that stopped the session

### 1.1 Key the worker deny-grant park on the stopping denial, with mission tests and the scoping note

## Background
Repo: Rust workspace. Engine crate `kranz-engine` lives at `crates/engine`. Kranz parks a mission milestone for an operator 'grant' when a run is blocked. For WORKER runs, `maybe_park_for_worker_deny_grant` in `crates/engine/src/orchestrator.rs` (around line 2483) offers a `GrantKind::WorkerDeny` grant whose target is the worker deny RULE (for example `Bash(git push*)`) that matched the denied command. Approving it lifts that rule for the rest of the mission. Its only call site is around line 3984 and is gated on `outcome.result != RunResult::Pass`. When it parks, `grant_respawns` is credited so the re-run does not use the `max_respawns` budget.

BUG: it uses `outcome.denied_commands.first()`, the FIRST command denied anywhere in the session. A worker that was denied a command early, worked around it, made many more tool calls and then failed for another reason is still parked on that early command.

The validator path was already fixed (mission m-e36de6). `maybe_park_for_grant` (around line 2387) uses `outcome.stopping_denied_command`. That field is `Some(cmd)` only when no tool call followed the session's last denied tool result and that denial was a shell command. When it is `None` but `denied_commands` is non-empty, it emits `not offering a grant for `<first>`: the denial did not stop the <role> session; <summary>`, where summary comes from `validator_session_end_summary(outcome)` (around line 8329). The rules are D-A and D-B in `docs/scoping/validator-park-only-when-denial-stopped-it.md`; read that note first. `RunOutcome` in `crates/engine/src/runner.rs` already carries `tool_calls_after_last_denial`, `stopping_denied_command` and `last_tool_call`.

## Hard constraints
- Do NOT modify `crates/engine/src/runner.rs` or `crates/engine/src/prompts.rs` at all. Use the existing RunOutcome fields; add no new way of deciding which denial stopped a session.
- Do NOT change the call-site gating (~line 3984), the `grant_respawns` credit, `respawn_credit`, or egress grants.
- Keep: only a command matching a liftable `Bash(...)` worker deny rule (via `permissions::matching_deny_rule` against the worker profile with current `deny_exceptions`) is offered. A hook denial or non-Bash denial stays ungrantable.
- Only edit: `crates/engine/src/orchestrator.rs`, `crates/engine/tests/mission_test.rs`, `docs/scoping/validator-park-only-when-denial-stopped-it.md`.

## Engine change (orchestrator.rs)
1. Rewrite the body of `maybe_park_for_worker_deny_grant`:
   - `let Some(command) = outcome.stopping_denied_command.clone() else { ... }`. In the else branch: if `outcome.denied_commands.first()` is Some(first), emit exactly one decision via `self.emit_decision(&msg, None)?` with `msg = format!("not offering a grant for `{}`: the denial did not stop the worker session; {}", crate::presentation::visible(first), <worker session-end summary>)`. Then return `Ok(false)`. If `denied_commands` is empty (only non-shell denials or none), return `Ok(false)` with no decision.
   - Keep the existing profile computation and `matching_deny_rule(&command, &profile.disallowed_tools)`. If it returns None, emit exactly one decision `format!("not offering a grant for `{}`: it matches no liftable worker deny rule", crate::presentation::visible(&command))` and return `Ok(false)`.
   - If it returns Some(rule), keep the existing `desc` and `self.park_for_grant(milestone_id, GrantKind::WorkerDeny, &rule, &desc)` call unchanged.
   - Update the doc comment to say it offers only the rule matching the denial that STOPPED the session (D-A/D-B), and to describe the two not-offering decisions.
2. Session-end summary: `validator_session_end_summary` decides 'without a report' from `outcome.validator_report.is_none()`, which is always None for a worker. For a worker the parsed report is `outcome.report` (check its type on `RunOutcome` in runner.rs, read-only). Generalise the helper minimally, e.g. add a `has_report: bool` parameter or a `Role` parameter, and optionally rename it, e.g. `session_end_summary`. Update every existing caller (grep for `validator_session_end_summary`) so validator callers pass the validator-report presence. The validator output text must stay BYTE-IDENTICAL. Existing tests assert the phrases `ended its turn without a report`, `last tool call: `, `tool call(s) ran after the last denied tool result`. Keep the doc comment accurate.

## Tests (crates/engine/tests/mission_test.rs)
Add exactly two new tests directly after `worker_deny_grant_respawn_does_not_eat_the_respawn_budget` (~line 7558). Both names MUST start with `worker_deny_stop_rule` (no existing test matches that prefix). Reuse the helpers already in this file and the ones it imports from the engine's backend_mock: `setup()`, `init_repo()`, `MockBackend::with_scripts`, `MockScript { events, ..Default::default() }`, `mock_init`, `mock_tool_use(tool, summary)`, `mock_tool_result(tool, summary)`, `mock_denied(tool, summary)`, `mock_text`, `mock_result_json`, `worker_fail()`, `worker_pass()`, `orch_script`, `dirty_tree_commit_as_is()`, `judgement`, `no_lesson()`, `make_engine`, `test_cfg()`, `simple_plan`, `read_log`, `event_types`, `wait_for_pending_grant`, `control::enqueue`, `ControlCommand::ApproveGrant`, `reducer::fold`, `reducer::read_snapshot`, `TEST_TIMEOUT`. Look at `validator_denied_then_continued` (~line 418), `respawn_bounded_fails_feature_then_mission_continues` (~line 6121) and `worker_deny_grant_lifts_the_rule_and_respawn_completes` (~line 7378) as templates. MockBackend scripts are consumed FIFO in spawn order. Worker scripts that write no files leave a clean tree, so there is no dirty-tree orch turn for them. A failing worker run gets a deterministic runner-verdict respawn with NO orchestrator turn.

Test A: `worker_deny_stop_rule_continued_past_is_charged_to_the_respawn_budget`, `#[tokio::test(flavor = "multi_thread")]`.
- Worker script events in order: `mock_init("w1")`, `mock_tool_use("Bash", "git push origin main")`, `mock_denied("Bash", "Permission denied: Bash(git push origin main)")`, `mock_tool_use("Read", "src/lib.rs")`, `mock_tool_result("Read", "fn main() {}")`, `mock_tool_use("Bash", "cargo test")`, `mock_tool_result("Bash", "test result: FAILED")`, then `mock_text(&report.to_string())` and `mock_result_json(&report)` with `report = json!({"result": "fail", "summary": "tests still failing"})`.
- FIFO: `[that worker (f-1-1 attempt 1), worker_fail() (f-1-1 attempt 2), worker_pass() (f-1-2), orch_script(vec![dirty_tree_commit_as_is(), judgement("complete", ""), no_lesson()])]`.
- Config: `MissionConfig { max_respawns: 1, ..test_cfg() }`, `simple_plan(2, vec![])`. Call `engine.set_grant_request_timeout(Duration::from_millis(0))` after `approve_plan` so the old code could never hang the test.
- Assert: status `MissionStatus::Complete`; f-1-1 status Failed with `respawns == 1` and `worker_runs.len() == 2`; a `FeatureFailed` for f-1-1 whose reason contains `respawn budget exhausted`; NO `grant.requested` event; an `EventKind::OrchestratorDecision` whose summary contains ``not offering a grant for `git push origin main`: the denial did not stop the worker session``. Also assert that summary contains `2 tool call(s) ran after the last denied tool result`. Read `emit_decision` first: if it truncates the summary below that length, assert only the prefix.

Test B: `worker_deny_stop_rule_last_call_parks_for_that_rule`, `#[tokio::test]`.
- Worker script events in order: `mock_init("w1")`, `mock_tool_use("Bash", "git stash")`, `mock_denied("Bash", "Permission denied: Bash(git stash)")`, `mock_tool_use("Read", "src/lib.rs")`, `mock_tool_result("Read", "fn main() {}")`, `mock_tool_use("Bash", "git push origin main")`, `mock_denied("Bash", "Permission denied: Bash(git push origin main)")`, then `mock_text`/`mock_result_json` of `json!({"result": "fail", "summary": "blocked from pushing"})`. Both `Bash(git stash*)` and `Bash(git push*)` are built-in worker deny rules (permissions.rs ~lines 102, 104).
- FIFO: `[that worker, worker_pass(), orch_script(vec![dirty_tree_commit_as_is(), judgement("complete", ""), no_lesson()])]`, with `test_cfg()` and `simple_plan(1, vec![])`.
- Run the engine in a spawned task exactly like `worker_deny_grant_lifts_the_rule_and_respawn_completes`. `wait_for_pending_grant(&paths)`, read the snapshot, assert `pending.kind == GrantKind::WorkerDeny` and `pending.command == "Bash(git push*)"`. Enqueue `ApproveGrant { command: "Bash(git push*)", grant_kind: None, request_seq: None }` and await with TEST_TIMEOUT.
- Assert: status Complete; exactly one `GrantRequested` event, and its command is `Bash(git push*)`; no `GrantRequested` with command `Bash(git stash*)`; the folded `deny_exceptions` contains `Bash(git push*)` and does NOT contain `Bash(git stash*)`.
- Give each test a doc comment in the file's style.

## Docs
In `docs/scoping/validator-park-only-when-denial-stopped-it.md`, replace the 'Not changed here' bullet that says the worker deny-grant path still offers the first denied command. The new bullet says the worker path now follows D-A/D-B (ticket `worker-deny-grant-only-when-denial-stopped-it`, operator decision D-X1). Then add a short section `## Worker path (D-X1)` stating:
- the WorkerDeny grant is offered only for the deny rule matching `stopping_denied_command`;
- the two not-offering decisions and their wording;
- a run that is not parked goes to the normal judgement and is charged to `max_respawns`, and only parked re-runs are credited via `grant_respawns`;
- the accepted caveat: a worker that retries a variant of a denied command makes a tool call after the first denial, so the grant offered is for the LAST denied command, whose matching rule may differ from the first.
Plain, factual prose, matching the note's style.

## How to run checks (IMPORTANT)
Run ONLY filtered tests. Never run the unfiltered `cargo test -p kranz-engine --lib` or `--workspace`, which can exceed your command time limit. Use:
- `cargo test -p kranz-engine --test mission_test worker_deny_stop_rule`
- `cargo test -p kranz-engine --test mission_test worker_deny_grant` (must show 2 passed)
- `cargo test -p kranz-engine --test mission_test validator_denial`
- `cargo test -p kranz-engine --test mission_test validator_stopping_denial`
- `cargo test -p kranz-engine --test mission_test worker_denial_on_a_passing_run`
- `cargo test -p kranz-engine --test mission_test respawn_bounded`
- `cargo test -p kranz-engine --lib orchestrator` (if you touch unit tests)
- `cargo fmt --all`
- `cargo clippy -p kranz-engine --all-targets -- -D warnings`
Write the two tests first and confirm Test A and Test B fail before the engine change. Then implement, re-run, and commit.

Done when:
- `cargo test -p kranz-engine --test mission_test worker_deny_stop_rule` runs exactly the two new tests and both pass
- Test A: a worker denied `git push origin main` that then makes Read and Bash tool calls and fails raises no grant.requested; f-1-1 ends Failed with respawns == 1, two worker runs and reason containing `respawn budget exhausted` under max_respawns = 1; an orchestrator decision contains ``not offering a grant for `git push origin main`: the denial did not stop the worker session``
- Test B: a worker denied `git stash`, then making a Read, then denied `git push origin main` as its last tool call, parks a WorkerDeny grant with command `Bash(git push*)` (never `Bash(git stash*)`); approving it completes the mission and deny_exceptions contains only the push rule
- A stopping denied command that matches no liftable worker deny rule emits exactly one decision ``not offering a grant for `<cmd>`: it matches no liftable worker deny rule`` and does not park
- Both new tests fail when run against the pre-change maybe_park_for_worker_deny_grant
- `cargo test -p kranz-engine --test mission_test worker_deny_grant` still shows 2 passed, and the validator_denial / validator_stopping_denial tests still pass with unchanged validator summary wording
- runner.rs and prompts.rs are unchanged; cargo fmt --all --check and cargo clippy -p kranz-engine --all-targets -- -D warnings are clean
