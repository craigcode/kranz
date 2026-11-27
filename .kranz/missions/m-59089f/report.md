# Mission report — m-59089f

**Goal:** Every engine-run contract command (validation round, not-yet-due run, final gate) leaves a machine-readable contract.command.result event in the mission's event log. The event carries the assertion id, stage, exit code or signal, a could-not-start or timed-out marker, the duration, scrubbed byte-bounded stdout and stderr tails, and a stable failure digest, so two runs of the same failing check can be compared as strings.

Branch `kranz/mission-m-59089f` (from `main`). Plan of record: [plan.md](plan.md).

## The plan

2 milestones, 5 features, gated by 16 contract assertions (14 command, 2 judgement).

1. **Contract command runs report a structured per-stream result**
   - Contract command runner reports separate tails, signal, could-not-start and duration — Repository: kranz, a Rust workspace.
   - Pure command-result record builder: scrubbed byte-bounded tails and failure digest — Repository: kranz, a Rust workspace; engine crate `kranz-engine` at crates/engine.
2. **Every engine-run contract command leaves a contract.command.result event**
   - Emit contract.command.result from the validation round, not-due run and final gate — Repository: kranz, a Rust workspace; engine crate `kranz-engine` at crates/engine, CLI crate at crates/cli.
   - End-to-end mission tests for contract.command.result — Repository: kranz; engine crate `kranz-engine`.
   - Fold-compatibility tests and documentation for contract.command.result — Repository: kranz; engine crate `kranz-engine`.

**Chosen approach:** A dedicated audit-only contract.command.result event backed by a small public command_result module.
**Elapsed:** 2h 10m 10s
**Tokens:** 564 in / 174688 out / 18972512 cache read / 914111 cache write
**Cost:** $10.83 actual vs $6.05–$40.26 estimated (expected $18.59)
**Interventions:** 0 (0 steers, 0 grant decisions, 0 revision rulings, 0 operator unblocks)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-59089f-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Provider:** local-worktree (source isolation) · template: worktree · no workspace contract
- **Workspace contract:** no workspace contract (source isolation only)
- **Preflight:** preflight: clear — no advisory issues recorded

## What shipped

### Milestone 1 — Contract command runs report a structured per-stream result ✅

- ✅ **Contract command runner reports separate tails, signal, could-not-start and duration** — 1 run
  Repository: kranz, a Rust workspace.
  - `1031793` [f-1-1] report signal, could-not-start, duration and separate tails for contract commands
  - ✓ A command that echoes 'kranz-out-marker' to stdout and 'kranz-err-marker' to stderr and exits 3 yields code Some(3). Its stdout_tail contains kranz-out-marker and not kranz-err-marker. Its stderr_tail contains kranz-err-marker. could_not_start is None and timed_out is false.
  - ✓ For the same command, ContractCommandRun.output equals the legacy join: the stdout tail, then '\n--- stderr ---\n', then the trimmed stderr, with the whole trim_end'd and cut to the last 1500 chars (test command_result_output_unchanged_for_validator).
  - ✓ A shell program that cannot be spawned yields could_not_start Some(non-empty), code None, signal None and timed_out false.
  - ✓ On unix, a command that SIGKILLs itself yields signal Some(9) and code None.
  - ✓ A timed-out run has timed_out true, could_not_start None and empty tails.
  - ✓ duration is greater than zero for a completed run.
  - ✓ run_bounded_argv and run_command_bounded return exactly the same (code, output) as before for success, failure, spawn failure and timeout.
- ✅ **Pure command-result record builder: scrubbed byte-bounded tails and failure digest** — 2 runs, 1 respawn
  Repository: kranz, a Rust workspace; engine crate `kranz-engine` at crates/engine.
  - `ed7f69d` [f-1-2] add pure command-result builder with scrubbed tails and failure digest
  - `5bb1f0b` [f-1-2] fix digest and tail-redaction tests to cover signal and cut straddle
  - ✓ Two runs that both exit 3 with tails differing only in 'finished in 0.53s' vs 'finished in 1.20s' and trailing spaces or CRLF give equal failure digests.
  - ✓ Changing the exit code, signal or any non-duration tail text changes the digest.
  - ✓ A passing run (code 0, not timed out) has passed true and failure_digest None.
  - ✓ The digest matches ^sha256:[0-9a-f]{64}$.
  - ✓ A stdout or stderr tail longer than 4096 bytes is stored as at most 4096 bytes, keeps the final bytes, is valid UTF-8 on a char boundary, and has its truncated flag set. A short tail is unchanged, with the flag false.
  - ✓ A secret that straddles the cut point is redacted rather than partly leaked.
  - ✓ A run with could_not_start Some(..) yields could_not_start true, a start_error, exit_code None, signal None and a failure digest.
  - ✓ JSON with only assertionId and stage deserializes into ContractCommandResult with all other fields defaulted, and stages serialize as validation, validation-notdue and final-gate.

### Milestone 2 — Every engine-run contract command leaves a contract.command.result event ✅

- ✅ **Emit contract.command.result from the validation round, not-due run and final gate** — 1 run
  Repository: kranz, a Rust workspace; engine crate `kranz-engine` at crates/engine, CLI crate at crates/cli.
  - `1a72679` [f-2-1] emit contract.command.result from validation, not-due run and final gate
  - ✓ events.rs has a contract.command.result variant whose result payload round-trips, and a minimal JSON event (milestoneId plus result.assertionId and result.stage) deserializes.
  - ✓ git diff of events.rs against the base shows only added lines, and types.rs is untouched.
  - ✓ The reducer accepts a contract.command.result for an existing milestone without changing state, and rejects one naming an unknown milestone.
  - ✓ The validation helper returns one record per command assertion with stage Validation, correct exit codes, and a digest only on failure. Its rendered text is unchanged.
  - ✓ The not-due helper's records carry stage ValidationNotdue.
  - ✓ The final gate emits one FinalGate record per command assertion, using the last milestone's id, including a could-not-start record for a check=command assertion with no command.
  - ✓ crates/cli/src/tail.rs renders the new event, and the workspace passes clippy with -D warnings.
- ✅ **End-to-end mission tests for contract.command.result** — 1 run
  Repository: kranz; engine crate `kranz-engine`.
  - `ccf9f5a` [f-2-2] add e2e tests for contract.command.result across validation and final gate
  - ✓ command_result_two_validation_rounds_record_equal_digests passes: two validation-stage records for a-fail on ms-1, in distinct rounds, each with exit 3, both markers and equal sha256 digests.
  - ✓ command_result_final_gate_records_failing_assertion passes: each final-gate run yields exactly one final-gate record for a-1 (count equals the command-assertion findings for a-1), with exit 3, the stderr marker and a digest.
  - ✓ No engine source file is modified by this feature.
- ✅ **Fold-compatibility tests and documentation for contract.command.result** — 2 runs, 1 respawn
  Repository: kranz; engine crate `kranz-engine`.
  - `d7cee58` [f-2-3] add fold-compatibility tests and docs for contract.command.result Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>
  - `0427b4d` Clarify stage values and digest normalisation in gates.md
  - ✓ command_result_old_log_without_result_events_folds passes on the existing fixture log.
  - ✓ command_result_minimal_event_folds_without_state_change passes: a minimal contract.command.result folds and leaves mission state unchanged.
  - ✓ command_result_event_for_unknown_milestone_is_rejected passes.
  - ✓ gates.md documents the event's fields, stages, byte bound, scrub order, timeout tails, the functional-validator condition, the digest normalisation and its limit, and lists command_result.rs in verified_against.
  - ✓ CHANGELOG.md Unreleased has an Added entry for contract.command.result.

## Validation history

### ms-1 round 1 — Contract command runs report a structured per-stream result

No findings.

### ms-2 round 1 — Every engine-run contract command leaves a contract.command.result event

No findings.

## Contract outcomes

- ✅ **[a1]** A contract command run reports its stdout and stderr tails separately. A command that prints a marker to each stream and exits 3 yields exit code 3, a stdout tail holding only the stdout marker, and a stderr tail holding the stderr marker. *(command: `out=$(cargo test -p kranz-engine --lib command_result_stdout_and_stderr 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a2]** The joined output string the functional validator and the final-gate findings are built from is byte-identical to the previous format: stdout, then '\n--- stderr ---\n' and stderr, cut to the last 1500 characters. *(command: `out=$(cargo test -p kranz-engine --lib command_result_output_unchanged 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a3]** A contract command whose shell cannot be spawned records the could-not-start marker and a start error, with no exit code and no signal. *(command: `out=$(cargo test -p kranz-engine --lib command_result_could_not_start 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a4]** The failure digest is 'sha256:' plus 64 lowercase hex characters. It is equal for two runs that differ only in duration tokens or trailing whitespace, differs when the exit code or tail text differs, and is absent for a passing run. *(command: `out=$(cargo test -p kranz-engine --lib command_result_digest 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a5]** Recorded output tails are scrubbed for secrets before they are bounded. Each is at most 4096 bytes, keeps the end of the output, is cut on a UTF-8 character boundary, and sets a truncated flag when it was cut. *(command: `out=$(cargo test -p kranz-engine --lib command_result_tail 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a6]** The contract.command.result event round-trips through serde. A JSON event carrying only its required fields (milestoneId, result.assertionId, result.stage) deserializes, with every other field at its default. *(command: `out=$(cargo test -p kranz-engine --lib command_result_event 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a7]** The validation-round and not-yet-due command helpers return one result record per command assertion, with the matching stage. The rendered validator evidence text is unchanged. *(command: `out=$(cargo test -p kranz-engine --lib command_result_validation_helper 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a8]** A mission whose failing command assertion is validated in two rounds records exactly two validation-stage result events for it. Each carries exit code 3 and both output markers, and the two failure digests are equal. *(command: `out=$(cargo test -p kranz-engine --test mission_test command_result_two_validation_rounds 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a9]** A mission whose failing command assertion reaches the final gate records one final-gate result event per final-gate command-assertion finding. Each carries the assertion id, a non-zero exit code, the stderr tail and a failure digest. *(command: `out=$(cargo test -p kranz-engine --test mission_test command_result_final_gate 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a10]** An existing event log without contract.command.result events still folds. A minimal result event folds without changing mission state, and a result event naming an unknown milestone is rejected. *(command: `out=$(cargo test -p kranz-engine --test reducer_test command_result_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a11]** The existing contract-command, contract-timeout and due-at mission tests still pass, so the evidence shown to the validator and the gate verdicts are unchanged. *(command: `out=$(cargo test -p kranz-engine --test mission_test contract_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && out2=$(cargo test -p kranz-engine --test mission_test assertion_due_ 2>&1) && printf '%s\n' "$out2" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a12]** events.rs and types.rs change additively only: the mission removes no line from either file. *(command: `git rev-parse --verify $KRANZ_BASE_SHA >/dev/null && ! git diff $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs | grep -qE '^-([^-]|$)'`)*
- ✅ **[a13]** The workspace is formatted. *(command: `cargo fmt --all --check`)*
- ✅ **[a14]** The whole workspace, including the CLI's event tail renderer, builds with no clippy warnings. *(command: `cargo clippy --workspace --all-targets -- -D warnings`)*
- ✅ **[a15]** Judged against the full mission diff, the mission only adds a record. The validator's rendered contract-results text, the not-due detail strings, the final-gate finding evidence and every verdict are built from exactly the same inputs as before. Result events are emitted only for the validation, validation-notdue and final-gate stages, and none of them drives a state transition in the reducer. *(agent judgement)*
- ✅ **[a16]** docs/knowledge/validation/gates.md accurately describes the contract.command.result event:
- its fields and the three stages;
- the 4096-byte tail bound and scrub-before-bound order;
- that timed-out runs have empty tails;
- that validation-round results exist only when the functional validator runs;
- the digest normalisation rules and their known limit (cargo compile lines differ after a code change).
CHANGELOG.md's Unreleased section has a user-facing entry for the event. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
