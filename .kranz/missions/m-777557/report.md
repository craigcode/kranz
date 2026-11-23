# Mission report — m-777557

**Goal:** Make the limit on engine-run contract `command` assertions configurable: a per-assertion `timeoutSecs` in the plan wins over an operator-only `contractCommandTimeoutSecs` in `~/.kranz/config.json`, which wins over the 600s default, all capped at 14,400s. Validators and the final gate use the same resolved limit, a timed-out command stays a failed assertion but is reported as a timeout, and the approval-time contract lint names timed-out commands separately from commands that pass on the base.

Branch `kranz/mission-m-777557` (from `main`). Plan of record: [plan.md](plan.md).

## The plan

2 milestones, 5 features, gated by 16 contract assertions (15 command, 1 judgement).

1. **Every contract-command run honours one resolved, bounded limit**
   - Per-assertion timeoutSecs in the contract schema with a hard ceiling — CONTEXT: kranz (Rust workspace) runs each contract `check: command` assertion in a mission plan under a hard 600-second cap, `COMMAND_TIMEO…
   - Operator-only contractCommandTimeoutSecs config key and the limit resolver — CONTEXT: kranz runs contract `check: command` assertions under a fixed 600s cap (`COMMAND_TIMEOUT`, crates/engine/src/command_exec.rs:149).
   - Final gate and validation rounds run under the resolved limit and report timeouts as timeouts — CONTEXT: kranz runs contract `check: command` assertions engine-side at the final gate (crates/engine/src/orchestrator/finalization.rs ~lin…
2. **The approval lint names timed-out commands before the operator approves**
   - Approval contract lint reports timed-out commands as timeouts — CONTEXT: when a plan is approved, kranz runs each contract `check: command` assertion once against the untouched base tree (crates/engine/s…
   - Document the configurable limit, teach planners timeoutSecs, recheck knowledge notes — CONTEXT: this mission made the limit on engine-run contract `check: command` assertions configurable.

**Chosen approach:** Per-assertion timeoutSecs (frozen with the contract) over an operator-only global config default over 600s, with a fixed 14,400s engine cei…
**Elapsed:** 3h 05m 20s
**Tokens:** 494 in / 137310 out / 17987735 cache read / 825884 cache write
**Cost:** $10.13 actual vs $8.97–$44.86 estimated (expected $20.55)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-777557-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Provider:** local-worktree (source isolation) · template: worktree · no workspace contract
- **Workspace contract:** no workspace contract (source isolation only)
- **Preflight:** preflight: 1 issue(s): [warn] command assertion [a10] uses '!', which was not found on PATH

## What shipped

### Milestone 1 — Every contract-command run honours one resolved, bounded limit ✅

- ✅ **Per-assertion timeoutSecs in the contract schema with a hard ceiling** — 2 runs, 1 respawn
  CONTEXT: kranz (Rust workspace) runs each contract `check: command` assertion in a mission plan under a hard 600-second cap, `COMMAND_TIMEO…
  - `a69d84f` [f-1-1] checkpoint (engine commit)
  - ✓ contract_timeout_schema_: a plan JSON assertion with "timeoutSecs": 5400 deserialises to Some(5400) and re-serialises with timeoutSecs: 5400
  - ✓ contract_timeout_schema_: validate refuses timeoutSecs 14401 with a message containing the assertion id and 14400
  - ✓ contract_timeout_schema_: validate refuses timeoutSecs 0, and refuses timeoutSecs on an agent-judgement assertion, each message containing the assertion id
  - ✓ contract_timeout_schema_: validate_contract_extends (planning) and the reducer's revised-contract check both refuse a revision that changes only timeout_secs of an existing assertion
  - ✓ contract_timeout_default_: a plan JSON assertion without timeoutSecs deserialises to None and re-serialises with no timeoutSecs key (byte-identical to the input JSON value)
  - ✓ The plan JSON schema in planning.rs lists timeoutSecs with minimum 1 and maximum 14400
  - ✓ cargo clippy --workspace --all-targets -- -D warnings passes
- ✅ **Operator-only contractCommandTimeoutSecs config key and the limit resolver** — 2 runs, 1 respawn
  CONTEXT: kranz runs contract `check: command` assertions under a fixed 600s cap (`COMMAND_TIMEOUT`, crates/engine/src/command_exec.rs:149).
  - `547eb1e` [f-1-2] checkpoint (engine commit)
  - `427ce34` [f-1-2] silence dead_code on not-yet-wired contract timeout resolver and fmt
  - ✓ contract_timeout_config_: a global-layer config JSON with contractCommandTimeoutSecs 5400 parses to Some(5400)
  - ✓ contract_timeout_config_: a project-layer config setting contractCommandTimeoutSecs is refused with the operator-only message naming the key
  - ✓ contract_timeout_config_: config validation refuses contractCommandTimeoutSecs 0 and 14401 with messages naming contractCommandTimeoutSecs
  - ✓ contract_timeout_resolve_: assertion timeoutSecs 1 with config 3 resolves to 1s with source Assertion
  - ✓ contract_timeout_resolve_: assertion without timeoutSecs and config 3 resolves to 3s with source Config
  - ✓ contract_timeout_resolve_: an assertion with an out-of-range value constructed directly (bypassing validate) is clamped to 14400
  - ✓ contract_timeout_default_: an assertion with no timeoutSecs and a default MissionConfig resolves to exactly 600s with source Default, and a default MissionConfig serialises without a contractCommandTimeoutSecs key
- ✅ **Final gate and validation rounds run under the resolved limit and report timeouts as timeouts** — 3 runs, 2 respawns
  CONTEXT: kranz runs contract `check: command` assertions engine-side at the final gate (crates/engine/src/orchestrator/finalization.rs ~lin…
  - `5a19d21` [f-1-3] checkpoint (engine commit)
  - `1abe9a9` [f-1-3] checkpoint (engine commit)
  - `f772966` [f-1-3] fix validation-timeout fixture script queue and run cargo fmt
  - ✓ contract_timeout_run_: run_contract_command_timed with a 1s limit on a 5s sleep returns timed_out=true, code None, output containing 'timed out after 1s', and returns in well under 5s
  - ✓ contract_timeout_run_: a command that exits 1 returns timed_out=false with code Some(1); a wrap or spawn failure returns timed_out=false
  - ✓ contract_timeout_gate_: a mission whose command assertion sleeps longer than its timeoutSecs=1 reaches the final gate and gets a critical command-assertion finding whose evidence contains 'timed out after 1s' and 'plan timeoutSecs' and does not contain '600s'; the mission does not complete
  - ✓ contract_timeout_gate_: with config contractCommandTimeoutSecs=1 and no per-assertion value, the final-gate evidence contains 'timed out after 1s' and 'config contractCommandTimeoutSecs'
  - ✓ contract_timeout_validation_: a validation round's rendered contract evidence shows 'TIMEOUT after 1s' for the timed-out assertion and not 'FAIL'
  - ✓ contract_timeout_validation_: the validation round and the final gate report the same limit (seconds and source) for the same assertion
  - ✓ Existing final-gate failure wording ('command failed (shell: ...)') is unchanged for non-timeout failures

### Milestone 2 — The approval lint names timed-out commands before the operator approves ✅

- ✅ **Approval contract lint reports timed-out commands as timeouts** — 2 runs, 1 respawn
  CONTEXT: when a plan is approved, kranz runs each contract `check: command` assertion once against the untouched base tree (crates/engine/s…
  - `9cafdf0` [f-2-1] checkpoint (engine commit)
  - ✓ contract_timeout_lint_: a command sleeping 5s with a 1s resolved limit yields TimedOut, is not in suspects(), and summary() lists its id on a 'timed out on the base' line and not on the author-bug suspects line
  - ✓ contract_timeout_lint_: when the gate limit equals the lint limit, the summary says the command will time out at the final gate too unless its limit is raised; when the gate limit exceeds the lint cap, it says the gate allows that many seconds
  - ✓ contract_timeout_lint_: contract_health counts a lint decision whose headline names a timeout as linted but not clean, and PassesOnBaseGate passes for a TimedOut result
  - ✓ contract_timeout_approval_: approving a plan whose command assertion has timeoutSecs=1 and sleeps longer records a 'contract lint:' decision whose headline names the timed-out count and does not say 'already pass' or 'all command assertions correctly fail', and plan.md lists the id under the timed-out line
  - ✓ Existing contract_lint::, contract_gates:: and contract_health:: unit tests pass (with approval_lint_runner_times_out_slow_command updated to TimedOut)
- ✅ **Document the configurable limit, teach planners timeoutSecs, recheck knowledge notes** — 3 runs, 2 respawns
  CONTEXT: this mission made the limit on engine-run contract `check: command` assertions configurable.
  - `7a6d12b` [f-2-2] checkpoint (engine commit)
  - `446254b` [f-2-2] checkpoint (engine commit)
  - `e07709c` [f-2-2] recheck lessons.md against the contract timeout docs recheck
  - ✓ grep -q 'timeoutSecs' crates/engine/prompts/orchestrator.md succeeds and the prompt states the 600-second default and the 14400 maximum
  - ✓ docs/knowledge/validation/gates.md no longer says the timeout is a fixed 10 minutes and describes the resolution order plan timeoutSecs > config contractCommandTimeoutSecs > 600s with the 14400s ceiling
  - ✓ contractCommandTimeoutSecs is documented as operator-only alongside the other config keys
  - ✓ cargo run --locked --package kranz -- knowledge-refresh exits 0
  - ✓ child-containment.md body is unchanged apart from a dated Rechecked line, if one was added
  - ✓ Prompt tests (config_cost_prompts_test) pass

## Validation history

### ms-1 round 1 — Every contract-command run honours one resolved, bounded limit

No findings.

### ms-2 round 1 — The approval lint names timed-out commands before the operator approves

No findings.

## Contract outcomes

- ✅ **[a1]** A plan assertion may carry `timeoutSecs`. A plan without it parses and re-serialises with no `timeoutSecs` key. A value of 0, a value above 14400, or `timeoutSecs` on a non-command assertion is refused, and the message names the assertion id. A revised plan that changes an approved assertion's `timeoutSecs` is refused. *(command: `out=$(cargo test -p kranz-engine --lib contract_timeout_schema_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. ([4-9]|[1-9][0-9]+) passed'`)*
- ✅ **[a2]** `contractCommandTimeoutSecs` parses from the global config layer. The project layer (`<repo>/.kranz/config.json`) is refused when it sets it. A value of 0 or above 14400 is refused, and the message names the key. *(command: `out=$(cargo test -p kranz-engine --lib contract_timeout_config_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. ([3-9]|[1-9][0-9]+) passed'`)*
- ✅ **[a3]** An assertion's resolved limit is its own `timeoutSecs` when set, otherwise the config `contractCommandTimeoutSecs`, otherwise exactly 600 seconds, and it never exceeds 14400 seconds. *(command: `out=$(cargo test -p kranz-engine --lib contract_timeout_resolve_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. ([3-9]|[1-9][0-9]+) passed'`)*
- ✅ **[a4]** With no timeout set anywhere, behaviour is unchanged. The resolved limit is 600 seconds, plan JSON round-trips byte-identically, and config serialises with no new key. *(command: `out=$(cargo test -p kranz-engine --lib contract_timeout_default_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. ([2-9]|[1-9][0-9]+) passed'`)*
- ✅ **[a5]** The contract-command runner reports a timeout as a typed timeout. A command killed at a 1-second limit is flagged timed out, and its output names 1s. A command that exits non-zero, or that fails to spawn or wrap, is not flagged timed out. *(command: `out=$(cargo test -p kranz-engine --lib contract_timeout_run_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. ([2-9]|[1-9][0-9]+) passed'`)*
- ✅ **[a6]** At the final gate, a command assertion with a short configured limit is killed at that limit, not at 600 seconds. It produces a critical `command-assertion` finding whose evidence says it timed out after that many seconds and names where the limit came from. The mission does not complete. *(command: `out=$(cargo test -p kranz-engine --test mission_test contract_timeout_gate_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. ([2-9]|[1-9][0-9]+) passed'`)*
- ✅ **[a7]** Validation rounds run each command assertion under the same resolved limit as the final gate, and render a timed-out command as TIMEOUT with its limit, not as FAIL. *(command: `out=$(cargo test -p kranz-engine --test mission_test contract_timeout_validation_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. ([2-9]|[1-9][0-9]+) passed'`)*
- ✅ **[a8]** The approval-time contract lint labels a command that timed out on the base tree as a timeout. It is listed on its own line, separately from commands that passed on the base. It does not count as an author-bug suspect that already passes, and it does not count as a clean lint. *(command: `out=$(cargo test -p kranz-engine --lib contract_timeout_lint_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. ([3-9]|[1-9][0-9]+) passed'`)*
- ✅ **[a9]** Approving a plan whose command assertion cannot finish within its limit on the base tree records an approval decision whose headline names the timeout. plan.md lists that assertion id under the timed-out line, not under the already-pass suspects. *(command: `out=$(cargo test -p kranz-engine --test mission_test contract_timeout_approval_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a10]** Anti-vacuity: no test name or other identifier containing `contract_timeout_` existed in `crates/` at the pinned base, so the test filters above match only this mission's new tests. *(command: `! git grep -q 'contract_timeout_' $KRANZ_BASE_SHA -- crates`)*
- ✅ **[a11]** The existing contract-lint, contract-gate and contract-health unit tests still pass. *(command: `out=$(cargo test -p kranz-engine --lib contract_lint:: 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && out=$(cargo test -p kranz-engine --lib contract_gates:: 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && out=$(cargo test -p kranz-engine --lib contract_health:: 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a12]** The orchestrator planning prompt tells planners about `timeoutSecs` for command assertions that need longer than the default limit. *(command: `grep -q 'timeoutSecs' crates/engine/prompts/orchestrator.md`)*
- ✅ **[a13]** The workspace is formatted. *(command: `cargo fmt --all --check`)*
- ✅ **[a14]** Every crate and every test target in the workspace compiles without clippy warnings. *(command: `cargo clippy --workspace --all-targets -- -D warnings`)*
- ✅ **[a15]** The knowledge vault passes its freshness check after this mission's changes to the source files its notes cite. *(command: `cargo run --locked --package kranz -- knowledge-refresh`)*
- ✅ **[a16]** A contract command that exceeds its limit is still killed by the unchanged process-tree kill (unix process group SIGKILL, Windows Job Object, container timeout teardown). No code path turns a timed-out command into a pass or a skip. Merge-gate commands and workspace bootstrap, readiness and data hooks keep their 600-second limit. The body of the human-owned note docs/knowledge/validation/child-containment.md is unedited, apart from a dated Rechecked line at most. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
