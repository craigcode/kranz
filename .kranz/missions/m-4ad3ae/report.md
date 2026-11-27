# Mission report — m-4ad3ae

**Goal:** Park a non-passing worker run for a WorkerDeny grant only when the denial that stopped the session matches a liftable Bash deny rule, and otherwise record one orchestrator decision saying why no grant was offered.

Branch `kranz/mission-m-4ad3ae` (from `main`). Plan of record: [plan.md](plan.md).

## The plan

1 milestone, 1 feature, gated by 7 contract assertions (6 command, 1 judgement).

1. **Worker deny-grant offered only for the denial that stopped the session**
   - Key the worker deny-grant park on the stopping denial, with mission tests and the scoping note — ## Background

**Chosen approach:** Do it all in one feature.
**Elapsed:** 1h 52m 59s
**Tokens:** 150 in / 53160 out / 4071545 cache read / 338016 cache write
**Cost:** $3.76 actual vs $1.96–$11.71 estimated (expected $5.37)
**Interventions:** 0 (0 steers, 0 grant decisions, 0 revision rulings, 0 operator unblocks)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-4ad3ae-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Provider:** local-worktree (source isolation) · template: worktree · no workspace contract
- **Workspace contract:** no workspace contract (source isolation only)
- **Preflight:** preflight: clear — no advisory issues recorded

## What shipped

### Milestone 1 — Worker deny-grant offered only for the denial that stopped the session ✅

- ✅ **Key the worker deny-grant park on the stopping denial, with mission tests and the scoping note** — 2 runs, 1 respawn
  ## Background
  - `4ffa53e` [f-1-1] checkpoint (engine commit)
  - ✓ `cargo test -p kranz-engine --test mission_test worker_deny_stop_rule` runs exactly the two new tests and both pass
  - ✓ Test A: a worker denied `git push origin main` that then makes Read and Bash tool calls and fails raises no grant.requested; f-1-1 ends Failed with respawns == 1, two worker runs and reason containing `respawn budget exhausted` under max_respawns = 1; an orchestrator decision contains ``not offering a grant for `git push origin main`: the denial did not stop the worker session``
  - ✓ Test B: a worker denied `git stash`, then making a Read, then denied `git push origin main` as its last tool call, parks a WorkerDeny grant with command `Bash(git push*)` (never `Bash(git stash*)`); approving it completes the mission and deny_exceptions contains only the push rule
  - ✓ A stopping denied command that matches no liftable worker deny rule emits exactly one decision ``not offering a grant for `<cmd>`: it matches no liftable worker deny rule`` and does not park
  - ✓ Both new tests fail when run against the pre-change maybe_park_for_worker_deny_grant
  - ✓ `cargo test -p kranz-engine --test mission_test worker_deny_grant` still shows 2 passed, and the validator_denial / validator_stopping_denial tests still pass with unchanged validator summary wording
  - ✓ runner.rs and prompts.rs are unchanged; cargo fmt --all --check and cargo clippy -p kranz-engine --all-targets -- -D warnings are clean

## Validation history

### ms-1 round 1 — Worker deny-grant offered only for the denial that stopped the session

No findings.

## Contract outcomes

- ✅ **[a1]** The new mission tests (filter prefix worker_deny_stop_rule, which matches no pre-existing test) exist, at least two of them run, and all pass: a worker denied a command that then made further tool calls and failed raises no grant.requested and is charged to the normal respawn budget, and a worker whose last tool call was denied parks a WorkerDeny grant for the rule matching that stopping command, not for an earlier denied command. *(command: `out=$(cargo test -p kranz-engine --test mission_test worker_deny_stop_rule 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. ([2-9]|[1-9][0-9]+) passed'`)*
- ✅ **[a2]** The two existing worker deny-grant tests (worker_deny_grant_lifts_the_rule_and_respawn_completes and worker_deny_grant_respawn_does_not_eat_the_respawn_budget) still pass, and both run. *(command: `out=$(cargo test -p kranz-engine --test mission_test worker_deny_grant 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. 2 passed'`)*
- ✅ **[a3]** The whole mission_test suite passes, including the validator stopping-denial tests that rely on the existing session-end summary wording and worker_denial_on_a_passing_run_does_not_park. *(command: `out=$(cargo test -p kranz-engine --test mission_test 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a4]** The kranz-engine library unit test suite passes. *(command: `out=$(cargo test -p kranz-engine --lib 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a5]** No second way of deciding which denial stopped a session is added and prompts are untouched: runner.rs and prompts.rs are byte-identical to the base commit. *(command: `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/runner.rs crates/engine/src/prompts.rs`)*
- ✅ **[a6]** The workspace is rustfmt-clean and kranz-engine is clippy-clean with warnings denied. *(command: `cargo fmt --all --check && cargo clippy -p kranz-engine --all-targets -- -D warnings`)*
- ✅ **[a7]** The new tests are not vacuous. Against the base commit's orchestrator.rs, the continued-past test would fail, because the old code parks a grant.requested for Bash(git push*). The last-call test would fail too, because the old code offers Bash(git stash*), the first denied command, instead of Bash(git push*). The worker path decides the stopping denial only from the existing RunOutcome fields stopping_denied_command / denied_commands, and each non-offer emits exactly one orchestrator decision in the validator path's 'not offering a grant for `...`: ...' wording. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
