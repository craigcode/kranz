# Mission plan — m-59089f

**Goal:** Every engine-run contract command (validation round, not-yet-due run, final gate) leaves a machine-readable contract.command.result event in the mission's event log. The event carries the assertion id, stage, exit code or signal, a could-not-start or timed-out marker, the duration, scrubbed byte-bounded stdout and stderr tails, and a stable failure digest, so two runs of the same failing check can be compared as strings.

Branch `kranz/mission-m-59089f` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Authority granted on approval

Approving this plan grants the following to the mission's sessions with no further prompt.

**Commands** (5; each is a prefix, so `x` also allows `x <anything>`):
- `git diff $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs`
- `git diff $KRANZ_BASE_SHA --stat`
- `cargo test -p kranz-engine --lib command_result_`
- `cargo test -p kranz-engine --test mission_test command_result_`
- `cargo test -p kranz-engine --test reducer_test command_result_`

**Write paths** (13; glob patterns the mission may change):
- `crates/engine/src/command_exec.rs`
- `crates/engine/src/command_exec/**`
- `crates/engine/src/command_result.rs`
- `crates/engine/src/lib.rs`
- `crates/engine/src/events.rs`
- `crates/engine/src/reducer.rs`
- `crates/engine/src/orchestrator.rs`
- `crates/engine/src/orchestrator/finalization.rs`
- `crates/cli/src/tail.rs`
- `crates/engine/tests/mission_test.rs`
- `crates/engine/tests/reducer_test.rs`
- `docs/knowledge/validation/gates.md`
- `CHANGELOG.md`

## Cost estimate

Estimated **$6.05 – $40.26** (expected ~$18.59). Rough estimate — live usage is authoritative; based on 59 completed mission(s).

Context-fit check (corpus p90 over 70 plan(s): 5008 chars / 25 files):
- **f-1-2** (Pure command-result record builder: scrubbed byte-bounded tails and failure digest) — spec is 5764 chars (corpus p90: 5008)
- **f-2-1** (Emit contract.command.result from the validation round, not-due run and final gate) — spec is 5491 chars (corpus p90: 5008)

## Considered alternatives

**Chosen approach:** A dedicated audit-only contract.command.result event backed by a small public command_result module. Milestone 1 builds the structured runner output and a pure, unit-tested record builder. Milestone 2 adds the event, wires the three contract-command call sites, and puts the end-to-end mission tests and the fold and doc work in their own features, per lessons m-e7b464 and m-4ad3ae. This captures exactly what the engine already reads (per-stream tails before the join), leaves the validator's text byte-identical, and gives the follow-up harness-fault ticket a deterministic digest to compare.

Rejected shapes:
- **Add additive fields (exit code, tails, digest) to validation.finding, validation.assertion.notdue and the final-gate finding** — Findings exist only when a model or gate raises one, so passing runs and unflagged failures leave no trace. The validation round's due commands have no per-assertion event at all.
- **Write full raw command output to runs/ artifact files referenced from the event** — It goes beyond 'record what the engine already captures', since the capture is 1500 chars per stream. It also adds artifact pruning and path handling, and the files are runtime data that can disappear, which defeats durable comparison.
- **Also record the approval contract lint and the external-gate check path as further stages** — The lint runs before any milestone exists. The external-gate path uses a different runner and already logs the exit code and an 8 KiB summary in an orchestrator.decision. Both widen the scope without serving the immediate need: repeat milestone-round failures for harness-fault-findings-block-milestone.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** A contract command run reports its stdout and stderr tails separately. A command that prints a marker to each stream and exits 3 yields exit code 3, a stdout tail holding only the stdout marker, and a stderr tail holding the stderr marker.
  `out=$(cargo test -p kranz-engine --lib command_result_stdout_and_stderr 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a2]** The joined output string the functional validator and the final-gate findings are built from is byte-identical to the previous format: stdout, then '\n--- stderr ---\n' and stderr, cut to the last 1500 characters.
  `out=$(cargo test -p kranz-engine --lib command_result_output_unchanged 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a3]** A contract command whose shell cannot be spawned records the could-not-start marker and a start error, with no exit code and no signal.
  `out=$(cargo test -p kranz-engine --lib command_result_could_not_start 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a4]** The failure digest is 'sha256:' plus 64 lowercase hex characters. It is equal for two runs that differ only in duration tokens or trailing whitespace, differs when the exit code or tail text differs, and is absent for a passing run.
  `out=$(cargo test -p kranz-engine --lib command_result_digest 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a5]** Recorded output tails are scrubbed for secrets before they are bounded. Each is at most 4096 bytes, keeps the end of the output, is cut on a UTF-8 character boundary, and sets a truncated flag when it was cut.
  `out=$(cargo test -p kranz-engine --lib command_result_tail 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a6]** The contract.command.result event round-trips through serde. A JSON event carrying only its required fields (milestoneId, result.assertionId, result.stage) deserializes, with every other field at its default.
  `out=$(cargo test -p kranz-engine --lib command_result_event 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a7]** The validation-round and not-yet-due command helpers return one result record per command assertion, with the matching stage. The rendered validator evidence text is unchanged.
  `out=$(cargo test -p kranz-engine --lib command_result_validation_helper 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a8]** A mission whose failing command assertion is validated in two rounds records exactly two validation-stage result events for it. Each carries exit code 3 and both output markers, and the two failure digests are equal.
  `out=$(cargo test -p kranz-engine --test mission_test command_result_two_validation_rounds 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a9]** A mission whose failing command assertion reaches the final gate records one final-gate result event per final-gate command-assertion finding. Each carries the assertion id, a non-zero exit code, the stderr tail and a failure digest.
  `out=$(cargo test -p kranz-engine --test mission_test command_result_final_gate 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a10]** An existing event log without contract.command.result events still folds. A minimal result event folds without changing mission state, and a result event naming an unknown milestone is rejected.
  `out=$(cargo test -p kranz-engine --test reducer_test command_result_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a11]** The existing contract-command, contract-timeout and due-at mission tests still pass, so the evidence shown to the validator and the gate verdicts are unchanged.
  `out=$(cargo test -p kranz-engine --test mission_test contract_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && out2=$(cargo test -p kranz-engine --test mission_test assertion_due_ 2>&1) && printf '%s\n' "$out2" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a12]** events.rs and types.rs change additively only: the mission removes no line from either file.
  `git rev-parse --verify $KRANZ_BASE_SHA >/dev/null && ! git diff $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs | grep -qE '^-([^-]|$)'`
- **[a13]** The workspace is formatted.
  `cargo fmt --all --check`
- **[a14]** The whole workspace, including the CLI's event tail renderer, builds with no clippy warnings.
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[a15]** Judged against the full mission diff, the mission only adds a record. The validator's rendered contract-results text, the not-due detail strings, the final-gate finding evidence and every verdict are built from exactly the same inputs as before. Result events are emitted only for the validation, validation-notdue and final-gate stages, and none of them drives a state transition in the reducer. *(agent judgement)*
- **[a16]** docs/knowledge/validation/gates.md accurately describes the contract.command.result event:
- its fields and the three stages;
- the 4096-byte tail bound and scrub-before-bound order;
- that timed-out runs have empty tails;
- that validation-round results exist only when the functional validator runs;
- the digest normalisation rules and their known limit (cargo compile lines differ after a code change).
CHANGELOG.md's Unreleased section has a user-facing entry for the event. *(agent judgement)*
## Contract lint

Each `check: command` assertion above was run once against the untouched base tree at approval time. Suspects are assertions that already pass (or could not reach a verdict) before this plan's work lands — a possible polarity/vacuity bug in the assertion itself. This never blocks approval.

author-bug suspects (already pass / no verdict on the untouched base): [a11] out=$(cargo test -p kranz-engine --test mission_test contract_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && out2=$(cargo test -p kranz-engine --test mission_test assertion_due_ 2>&1) && printf '%s\n' "$out2" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a12] git rev-parse --verify $KRANZ_BASE_SHA >/dev/null && ! git diff $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs | grep -qE '^-([^-]|$)'; [a13] cargo fmt --all --check; [a14] cargo clippy --workspace --all-targets -- -D warnings
base-expected-to-fail (benign): [a1] out=$(cargo test -p kranz-engine --lib command_result_stdout_and_stderr 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a2] out=$(cargo test -p kranz-engine --lib command_result_output_unchanged 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a3] out=$(cargo test -p kranz-engine --lib command_result_could_not_start 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a4] out=$(cargo test -p kranz-engine --lib command_result_digest 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a5] out=$(cargo test -p kranz-engine --lib command_result_tail 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a6] out=$(cargo test -p kranz-engine --lib command_result_event 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a7] out=$(cargo test -p kranz-engine --lib command_result_validation_helper 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a8] out=$(cargo test -p kranz-engine --test mission_test command_result_two_validation_rounds 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a9] out=$(cargo test -p kranz-engine --test mission_test command_result_final_gate 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a10] out=$(cargo test -p kranz-engine --test reducer_test command_result_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'

Lint commands ran under shell: sh

named contract gates (defect classes):
- vacuous-filter: PASS
- wrong-polarity: PASS
- host-shell-syntax: PASS
- passes-on-base: FAIL
  [a11] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `out=$(cargo test -p kranz-engine --test mission_test contract_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && out2=$(cargo test -p kranz-engine --test mission_test assertion_due_ 2>&1) && printf '%s\n' "$out2" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
  [a12] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `git rev-parse --verify $KRANZ_BASE_SHA >/dev/null && ! git diff $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs | grep -qE '^-([^-]|$)'`
  [a13] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo fmt --all --check`
  [a14] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo clippy --workspace --all-targets -- -D warnings`
- env-sensitive: PASS


## Milestone 1 — Contract command runs report a structured per-stream result

### 1.1 Contract command runner reports separate tails, signal, could-not-start and duration

Repository: kranz, a Rust workspace. The engine crate is `kranz-engine` at crates/engine.

Goal: the engine's contract-command runner returns enough structure to record each run machine-readably. Do not change how anything is rendered or judged.

File: crates/engine/src/command_exec.rs. Read these parts first:
- `ContractCommandRun` (around line 1527): currently `{ code: Option<i32>, output: String, timed_out: bool }`.
- `run_contract_command_timed` (around line 1538).
- `run_contract_command_timed_blocking` (around line 1643).
- `run_bounded_argv_core` (around line 474).
- `BoundedOutcome` (around line 1920).
- `run_command_bounded_core` (around line 1947).
- `read_stream_tail` (around line 2055).

How it works today: `run_command_bounded_core` drains stdout and stderr separately. Each stream is cut to the last `COMMAND_OUTPUT_TAIL` (1500) chars. It then JOINS them as `stdout + "\n--- stderr ---\n" + stderr.trim_end()` (only when stderr.trim() is non-empty) and cuts the joined text again to the last 1500 chars. That loses stdout whenever stderr is long. The separate tails are discarded. Several failure causes all come back as `Completed(None, msg)`: spawn failure ("failed to spawn shell: {e}"), read or wait failure, and signal death.

Changes:
1. Make `BoundedOutcome` carry the detail. Suggested shape: `Completed { code, signal: Option<i32>, stdout: String, stderr: String, output: String }`, `CouldNotStart(String)` for a `cmd.spawn()` failure, `Failed(String)` for a read or wait failure after spawn, and `TimedOut(String)`. `stdout` and `stderr` are the per-stream tails exactly as `read_stream_tail` returns them. Keep `into_code_output()` returning exactly what it returns today for every case, so `run_bounded_argv`, `run_command_bounded` and every other caller behave byte-identically. Spawn failure is still `(None, "failed to spawn shell: {e}")`.
2. Signal: on unix use `std::os::unix::process::ExitStatusExt::signal()` on the exit status. Use None elsewhere.
3. Add these fields to `ContractCommandRun`:
   - `signal: Option<i32>`;
   - `could_not_start: Option<String>` (the start error);
   - `stdout_tail: String`;
   - `stderr_tail: String`;
   - `duration: std::time::Duration`.
   Keep `code`, `output` and `timed_out` with exactly today's values. `output` must stay the joined string, byte-identical.
4. In `run_contract_command_timed`:
   - Measure the duration with `std::time::Instant` around the whole run.
   - A `wrap_contract_shell` error sets `could_not_start: Some(<the same message used in output>)`.
   - A spawn failure sets `could_not_start: Some(<the spawn error message>)`.
   - On timeout, the tails are empty strings (the engine keeps no output on timeout) and `could_not_start` is None.
5. `run_contract_command_timed_blocking`: a runtime-creation failure or a panic sets `could_not_start: Some(<message>)`. Fill the other new fields with defaults.
6. Fix every other construction of `ContractCommandRun` so the crate compiles.

Nothing outside the engine reads these fields yet; a later feature wires them into an event. If clippy (`-D warnings`) flags the new fields as dead code in non-test builds, put `#[allow(dead_code)] // read by contract.command.result emission` on the field or struct. Do not remove any existing behaviour.

Tests go in command_exec.rs's existing `#[cfg(test)]` module. Mirror the scratch and env setup of the existing tests that call `run_contract_command_timed` (around lines 2535-2600; they use `crate::agent_env::contract_command_env` and `GateSandbox::Disabled`). EVERY new test name must start with `command_result_`:
- `command_result_stdout_and_stderr_kept_separately`
- `command_result_output_unchanged_for_validator`
- `command_result_could_not_start_records_marker_not_exit_code`. Use a `crate::contract_shell::HostShell { kind: ShellKind::Sh, program: PathBuf::from("/nonexistent/kranz-no-such-shell") }` with `GateSandbox::Disabled`. First check that the Disabled wrap really uses `shell.program` as the program. If it does not, make spawn fail with a cwd that does not exist.
- `command_result_signal_recorded` (`#[cfg(unix)]`; run `kill -9 $$`)
- `command_result_timeout_has_no_start_marker` (use the existing timeout-test pattern)

Verification, in priority order. Run only filtered tests, NEVER the unfiltered `cargo test -p kranz-engine --lib` (it can exceed the command time limit):
1. `cargo test -p kranz-engine --lib command_result_`
2. `cargo test -p kranz-engine --lib command_exec::` (existing runner tests must still pass; if this is slow, filter to the run_contract or bounded tests you touched)
3. `cargo fmt --all`
4. `cargo clippy -p kranz-engine --all-targets -- -D warnings`

Commit as soon as the command_result_ tests pass and fmt is clean. Do not spend turns on extra checks first. Never push.

Done when:
- A command that echoes 'kranz-out-marker' to stdout and 'kranz-err-marker' to stderr and exits 3 yields code Some(3). Its stdout_tail contains kranz-out-marker and not kranz-err-marker. Its stderr_tail contains kranz-err-marker. could_not_start is None and timed_out is false.
- For the same command, ContractCommandRun.output equals the legacy join: the stdout tail, then '\n--- stderr ---\n', then the trimmed stderr, with the whole trim_end'd and cut to the last 1500 chars (test command_result_output_unchanged_for_validator).
- A shell program that cannot be spawned yields could_not_start Some(non-empty), code None, signal None and timed_out false.
- On unix, a command that SIGKILLs itself yields signal Some(9) and code None.
- A timed-out run has timed_out true, could_not_start None and empty tails.
- duration is greater than zero for a completed run.
- run_bounded_argv and run_command_bounded return exactly the same (code, output) as before for success, failure, spawn failure and timeout.

### 1.2 Pure command-result record builder: scrubbed byte-bounded tails and failure digest

Repository: kranz, a Rust workspace; engine crate `kranz-engine` at crates/engine.

Prerequisite, already merged: `crate::command_exec::ContractCommandRun` (crates/engine/src/command_exec.rs) has these fields:
- `code: Option<i32>`;
- `output: String`;
- `timed_out: bool`;
- `signal: Option<i32>`;
- `could_not_start: Option<String>`;
- `stdout_tail: String`;
- `stderr_tail: String`;
- `duration: Duration`.

Create a new PUBLIC module `crates/engine/src/command_result.rs`. Register it in crates/engine/src/lib.rs as `pub mod command_result;`. It must be pub because a later feature embeds its type in the public `EventKind` enum and integration tests read it. The module turns a run into a machine-readable record. Give it a module doc comment in the style of the repo's other modules.

Contents:
1. `pub const TAIL_MAX_BYTES: usize = 4096;`
2. `#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] #[serde(rename_all = "kebab-case")] pub enum CommandStage { Validation, ValidationNotdue, FinalGate }`. Check that the serialized names are exactly "validation", "validation-notdue" and "final-gate"; use explicit `#[serde(rename)]` if needed.
3. `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(rename_all = "camelCase")] pub struct ContractCommandResult`.
   - REQUIRED fields: `assertion_id: String` and `stage: CommandStage`.
   - Every other field carries `#[serde(default)]`, and the Option fields also carry `skip_serializing_if = "Option::is_none"`.
   - The other fields: `passed: bool`, `exit_code: Option<i32>`, `signal: Option<i32>`, `timed_out: bool`, `could_not_start: bool`, `start_error: Option<String>`, `duration_ms: u64`, `stdout_tail: String`, `stderr_tail: String`, `stdout_truncated: bool`, `stderr_truncated: bool`, `failure_digest: Option<String>`.
4. `pub(crate) fn bound_tail(text: &str) -> (String, bool)`. First run `crate::scrub::scrub(text)`, so a secret is redacted before it can be cut. Then keep at most TAIL_MAX_BYTES bytes from the END, cut on a UTF-8 char boundary. Return whether anything was cut.
5. `pub(crate) fn normalise_for_digest(text: &str) -> String`:
   - CRLF and lone CR become LF;
   - trailing whitespace is stripped from each line;
   - trailing empty lines are dropped;
   - duration tokens matching `\b\d+(?:\.\d+)?(?:ns|us|µs|ms|s)\b` are replaced with `<dur>`. Use the `regex` crate, which is already a dependency, compiled once in a `OnceLock`, as scrub.rs does.
   Do not mask hashes, paths or anything else.
6. `pub(crate) fn failure_digest(outcome: &str, exit_code: Option<i32>, signal: Option<i32>, start_error: &str, stdout_tail: &str, stderr_tail: &str) -> String`. It returns `sha256:` plus 64 lowercase hex characters. Use `sha2::Sha256`, already a dependency (see `use sha2` in scrub.rs). Hash a canonical, LENGTH-PREFIXED encoding, so no tail's content can forge another field:
   - the outcome label;
   - the exit code or `-`;
   - the signal or `-`;
   - then normalise_for_digest of start_error, stdout_tail and stderr_tail, in that order.
   The digest is computed from the STORED (scrubbed, bounded) tails, so anyone can recompute it from the event.
7. Outcome labels: `exited` (an exit code is present), `signalled`, `timed-out`, `could-not-start`, and `lost` (no code, no signal, not timed out, not could-not-start: a read or wait failure after spawn).
8. `pub(crate) fn from_run(assertion_id: &str, stage: CommandStage, run: &crate::command_exec::ContractCommandRun) -> ContractCommandResult`:
   - `passed` = `!run.timed_out && run.code == Some(0)`;
   - the tails go through bound_tail;
   - `start_error` = `run.could_not_start` passed through bound_tail;
   - `could_not_start` = `run.could_not_start.is_some()`;
   - when could_not_start is true, `exit_code` and `signal` are None;
   - `duration_ms` = the duration in milliseconds;
   - `failure_digest` = Some(failure_digest(..)) only when `!passed`.
9. `pub(crate) fn missing_command(assertion_id: &str, stage: CommandStage) -> ContractCommandResult`, for an assertion with check=command but no command text. It sets could_not_start true, start_error Some("assertion has check=command but no command") and a failure digest.

Nothing calls from_run or missing_command in production yet; a later feature wires them in. If clippy `-D warnings` flags dead code in non-test builds, add `#[allow(dead_code)] // wired by contract.command.result emission` on the affected items.

Tests go in a `#[cfg(test)] mod tests` in command_result.rs. Build ContractCommandRun values directly; its fields are pub. EVERY test name must start with `command_result_`:
- `command_result_digest_equal_modulo_durations_and_trailing_whitespace`
- `command_result_digest_differs_on_exit_code_or_tail`
- `command_result_digest_absent_on_pass`
- `command_result_digest_format_is_sha256_hex`
- `command_result_tail_bounded_to_4096_bytes_keeps_end_on_char_boundary` (use multibyte text that straddles the cut)
- `command_result_tail_redacts_secret_before_bounding`. Use a credential shape that crates/engine/tests/scrub_test.rs or scrub.rs tests already prove gets `[REDACTED]`, placed so it straddles the 4096-byte cut point.
- `command_result_could_not_start_record_has_no_exit_code`
- `command_result_minimal_json_deserializes`: `{"assertionId":"a-1","stage":"final-gate"}` deserializes with all defaults.
- `command_result_stage_serializes_kebab_case`

Verification, in priority order. Filtered tests only; never run the unfiltered `cargo test -p kranz-engine --lib`:
1. `cargo test -p kranz-engine --lib command_result_`
2. `cargo fmt --all`
3. `cargo clippy -p kranz-engine --all-targets -- -D warnings`

Commit as soon as the tests pass and fmt is clean. Never push. Do not edit events.rs or types.rs in this feature.

Done when:
- Two runs that both exit 3 with tails differing only in 'finished in 0.53s' vs 'finished in 1.20s' and trailing spaces or CRLF give equal failure digests.
- Changing the exit code, signal or any non-duration tail text changes the digest.
- A passing run (code 0, not timed out) has passed true and failure_digest None.
- The digest matches ^sha256:[0-9a-f]{64}$.
- A stdout or stderr tail longer than 4096 bytes is stored as at most 4096 bytes, keeps the final bytes, is valid UTF-8 on a char boundary, and has its truncated flag set. A short tail is unchanged, with the flag false.
- A secret that straddles the cut point is redacted rather than partly leaked.
- A run with could_not_start Some(..) yields could_not_start true, a start_error, exit_code None, signal None and a failure digest.
- JSON with only assertionId and stage deserializes into ContractCommandResult with all other fields defaulted, and stages serialize as validation, validation-notdue and final-gate.


## Milestone 2 — Every engine-run contract command leaves a contract.command.result event

### 2.1 Emit contract.command.result from the validation round, not-due run and final gate

Repository: kranz, a Rust workspace; engine crate `kranz-engine` at crates/engine, CLI crate at crates/cli.

Already merged: the public module `kranz_engine::command_result` (crates/engine/src/command_result.rs) provides:
- `ContractCommandResult` (serde camelCase; required `assertionId` and `stage`);
- `CommandStage { Validation, ValidationNotdue, FinalGate }`;
- `pub(crate) fn from_run(assertion_id, stage, &ContractCommandRun)`;
- `pub(crate) fn missing_command(assertion_id, stage)`.
`crate::command_exec::ContractCommandRun` now carries separate tails, a signal, a could-not-start marker and a duration.

This feature adds the event and emits it wherever the engine runs a contract command assertion.

Hard rule: crates/engine/src/events.rs and types.rs change ADDITIVELY ONLY. Remove or modify no existing line; only insert new lines. Optional payload fields carry `#[serde(default)]`. Do not touch types.rs.

1. events.rs: add a variant next to `ValidationAssertionNotDue` (around line 501), with a doc comment in the same style: an audit-only record of one engine-run contract command, absent in pre-field logs. The variant:
   ```
   #[serde(rename = "contract.command.result")]
   ContractCommandResult {
       #[serde(rename = "milestoneId")] milestone_id: String,
       result: crate::command_result::ContractCommandResult,
   }
   ```
   Add an arm to `type_name` (around line 1253) returning "contract.command.result". In the events.rs test module, add `command_result_event_roundtrips_and_minimal_json_deserializes`, mirroring the existing ValidationPtyTranscript serde tests (around line 2012). It round-trips a full event, and parses a minimal JSON event with only milestoneId and result {assertionId, stage}.
2. crates/engine/src/reducer.rs: add an audit-only arm next to `ValidationAssertionNotDue` (around line 465). It only validates the milestone reference with `milestone_mut(state, milestone_id)?;` and drives no state transition. Give it a comment in the same style.
3. crates/cli/src/tail.rs: the EventKind match has no catch-all, so add an arm next to `ValidationAssertionNotDue` (around line 482). It returns `(format!("milestone {milestone_id}"), <ansi::DIM if passed else ansi::YELLOW>, format!("contract command [{assertion_id}] {stage}: {outcome}"))`. outcome is PASS, `exit N`, `signal N`, TIMEOUT or `could not start`.
4. Wiring in crates/engine/src/orchestrator.rs:
   - `run_contract_commands_for_validation` (around line 6020) is an associated fn with NO self receiver; keep it that way, see its doc comment. Change it to also return a `Vec<ContractCommandResult>`, built with `from_run(id, CommandStage::Validation, &run)`, or `missing_command` for the no-command branch. The rendered String must be built exactly as today.
   - `run_contract_commands_notdue` (around line 6085): also return a record per assertion, with stage ValidationNotdue. Its existing `(id, ok, detail)` values must be unchanged.
   - In `validation_round` (around lines 6244-6420): after each helper returns, emit `EventKind::ContractCommandResult { milestone_id: milestone_id.clone(), result }` for each record, in contract order. Use the existing error pattern `if let Err(error) = self.emit(..) { gate_sandbox.cleanup()?; return Err(error); }`. Emit the due records right after `run_contract_commands_for_validation` returns, and the not-due records before the existing `ValidationAssertionNotDue` emits.
   - Update the existing test caller of `run_contract_commands_for_validation` (around line 17480).
5. crates/engine/src/orchestrator/finalization.rs, the final gate (around lines 231-291): for each command assertion, emit a result with stage FinalGate right after `run_contract_command_timed` returns, before pushing any finding. The no-command branch emits `missing_command(..)`. The milestone id is the LAST milestone's id, as the final gate already uses for MilestoneBlocked around line 188. On an emit error, call `gate_sandbox.cleanup()?` before returning, as nearby code does.
6. Remove every temporary `#[allow(dead_code)]` that earlier features added in command_result.rs and on ContractCommandRun fields that are now used.

NOT in scope: the approval contract lint (contract_lint.rs), the external-gate checks (orchestrator/external_gates.rs), the review packet and the dashboard. Do not change any rendered text, finding evidence, verdict or detail string.

Tests in orchestrator.rs's test module, next to the existing caller near line 17480; mirror its setup. Names start with `command_result_validation_helper`:
- `command_result_validation_helper_returns_one_record_per_command_assertion`: two command assertions, one passing and one running `echo kranz-out-marker && echo kranz-err-marker 1>&2 && exit 3`. It asserts two records with the right ids, stage Validation and exit codes (0 and 3); the failing one has a digest and the passing one has none. The rendered text still contains `→ PASS` and `→ FAIL` lines.
- `command_result_validation_helper_notdue_records_stage`: the not-due helper's records carry stage ValidationNotdue.

Verification, in priority order. Filtered tests only; never run the unfiltered `cargo test -p kranz-engine --lib`:
1. `cargo test -p kranz-engine --lib command_result_`
2. `cargo test -p kranz-engine --test mission_test contract_commands_run_engine_side`
3. `cargo fmt --all`
4. `cargo clippy --workspace --all-targets -- -D warnings` (the CLI must compile)

Commit as soon as 1 and 2 pass and fmt is clean. Never push.

Done when:
- events.rs has a contract.command.result variant whose result payload round-trips, and a minimal JSON event (milestoneId plus result.assertionId and result.stage) deserializes.
- git diff of events.rs against the base shows only added lines, and types.rs is untouched.
- The reducer accepts a contract.command.result for an existing milestone without changing state, and rejects one naming an unknown milestone.
- The validation helper returns one record per command assertion with stage Validation, correct exit codes, and a digest only on failure. Its rendered text is unchanged.
- The not-due helper's records carry stage ValidationNotdue.
- The final gate emits one FinalGate record per command assertion, using the last milestone's id, including a could-not-start record for a check=command assertion with no command.
- crates/cli/src/tail.rs renders the new event, and the workspace passes clippy with -D warnings.

### 2.2 End-to-end mission tests for contract.command.result

Repository: kranz; engine crate `kranz-engine`. Add two end-to-end tests to crates/engine/tests/mission_test.rs, a large file of MockBackend-scripted mission tests. The engine now emits `EventKind::ContractCommandResult { milestone_id, result }` (type "contract.command.result"). `result` is a `kranz_engine::command_result::ContractCommandResult` with pub fields:
- `assertion_id`;
- `stage` (`kranz_engine::command_result::CommandStage::{Validation, ValidationNotdue, FinalGate}`);
- `passed`, `exit_code: Option<i32>`, `signal`, `timed_out`, `could_not_start`, `start_error`, `duration_ms`;
- `stdout_tail`, `stderr_tail`, `stdout_truncated`, `stderr_truncated`;
- `failure_digest: Option<String>` ("sha256:<hex>").

The engine emits one per command assertion each time it runs contract commands: in a milestone validation round (only when the functional validator is enabled), and once per final-gate run.

Use the file's existing helpers: `setup()`, `init_repo()`, `assertion(id, statement, Some(cmd))`, `simple_plan(n, contract)`, `MockBackend::with_scripts`, `worker_pass()`, `orch_script(..)`, `dirty_tree_commit_as_is()`, `judgement(..)`, `fix_features(n)`, `waive_reply(..)`, `validator_with(json!(..))`, `make_engine`, `test_cfg()`, `read_log`, `event_types`, `TEST_TIMEOUT`.

Use this failing command in both tests: `const COMMAND_RESULT_FAIL: &str = "echo kranz-out-marker && echo kranz-err-marker 1>&2 && exit 3";`

Test 1, `command_result_two_validation_rounds_record_equal_digests`. COPY the scripting of `loop_guard_blocks_milestone_after_max_fix_cycles` (around line 4448) exactly, changing only the plan to `simple_plan(1, vec![assertion("a-fail", "the check passes", Some(COMMAND_RESULT_FAIL))])`. The script, in order:
1. `worker_pass()`;
2. `orch_script(vec![dirty_tree_commit_as_is(), judgement("complete", ""), fix_features(1), dirty_tree_commit_as_is(), judgement("complete", ""), fix_features(1)])`;
3. `validator_with(finding.clone())`;
4. `worker_pass()`;
5. `validator_with(finding)`.

Use the same `finding` JSON as that test, and cfg `MissionConfig { skip_functional: false, max_fix_cycles_per_milestone: 1, ..test_cfg() }`. Expect MissionStatus::Blocked; the cap is spent and no final gate runs. Then assert:
- exactly two ContractCommandResult events with assertion_id "a-fail" and stage Validation, both with milestone_id "ms-1";
- each has exit_code Some(3), passed false, could_not_start false and timed_out false;
- stdout_tail contains kranz-out-marker and stderr_tail contains kranz-err-marker;
- failure_digest is Some, starts with "sha256:", and is EQUAL across the two events;
- a `milestone.validating` event lies between them by seq, so they come from distinct rounds.

Test 2, `command_result_final_gate_records_failing_assertion`. COPY the scripting of `assertion_due_final_gate_fails_scoped_assertion` (around line 1068), but use an UNscoped assertion `assertion("a-1", "the build succeeds", Some(COMMAND_RESULT_FAIL))`. The script, in order:
1. `worker_pass()`;
2. `orch_script(vec![dirty_tree_commit_as_is(), judgement("complete", ""), waive_reply("a-1", "command not runnable in this environment"), dirty_tree_commit_as_is(), judgement("complete", ""), waive_reply("a-1", "still not runnable")])`;
3. `worker_pass()`.

Use cfg `MissionConfig { max_fix_cycles_per_milestone: 1, ..test_cfg() }` and expect Blocked. Then assert:
- the FinalGate-stage records for a-1 are non-empty;
- their count EQUALS the number of ValidationFinding events whose finding.subject == "a-1" and finding.class == "command-assertion" (each final-gate run yields exactly one record and one such finding);
- each record has milestone_id "ms-1", exit_code Some(3), passed false, stderr_tail containing kranz-err-marker, stdout_tail containing kranz-out-marker, and failure_digest Some;
- if there is more than one record, all digests are equal.

Also check whether test_cfg() skips the functional validator. If it does, assert there are no Validation-stage records in this test.

If a script reply order does not match the engine (the mock panics or the run hangs), read the copied test's surrounding comments rather than re-reading orchestrator.rs or finalization.rs at length. These two flows already work with exactly these scripts.

Verification, in priority order:
1. `cargo test -p kranz-engine --test mission_test command_result_`
2. `cargo fmt --all`

Commit as soon as both tests pass. Then optionally run `cargo clippy -p kranz-engine --all-targets -- -D warnings`. Never push. Do not modify engine source in this feature; if a test exposes an engine bug, record it in knownGaps with evidence.

Done when:
- command_result_two_validation_rounds_record_equal_digests passes: two validation-stage records for a-fail on ms-1, in distinct rounds, each with exit 3, both markers and equal sha256 digests.
- command_result_final_gate_records_failing_assertion passes: each final-gate run yields exactly one final-gate record for a-1 (count equals the command-assertion findings for a-1), with exit 3, the stderr marker and a digest.
- No engine source file is modified by this feature.

### 2.3 Fold-compatibility tests and documentation for contract.command.result

Repository: kranz; engine crate `kranz-engine`. The engine now has an audit-only event `EventKind::ContractCommandResult { milestone_id: String, result: kranz_engine::command_result::ContractCommandResult }`, serialized with type "contract.command.result". The reducer validates only that the milestone exists.

Part A: tests in crates/engine/tests/reducer_test.rs. Use the file's helpers: `ev`, `created`, `plan`, `fold_kinds`, `milestone`. Look at how the test around line 4231 loads `tests/fixtures/gate-results-ladder.events.jsonl`. Names start with `command_result_`:
- `command_result_old_log_without_result_events_folds`: load and fold that fixture log the same way the existing test does. Assert the fixture text contains no "contract.command.result" and that the fold succeeds.
- `command_result_minimal_event_folds_without_state_change`: fold a created plus plan-approved sequence the way other tests in the file do. Then also fold one extra event parsed from JSON carrying only the required fields (type, milestoneId of an existing milestone, result {assertionId, stage: "validation"}). Find the event envelope's exact shape (seq, ts, type) by mirroring how the file builds events, or serialize an `ev(..)` and inspect it. Assert the mission state serializes identically with and without the extra event, apart from any seq or last-event bookkeeping the reducer keeps; compare with care.
- `command_result_event_for_unknown_milestone_is_rejected`: the fold returns an error.

Part B: documentation.
1. In docs/knowledge/validation/gates.md, add `crates/engine/src/command_result.rs` to the front-matter `verified_against` list. Add a new dated paragraph at the TOP of the 'Rechecked …' list, in the existing style: "Rechecked 2026-11-27 (UTC) against …". Then add a short section "Recorded contract command results". The section covers:
   - Every validation-round, not-yet-due and final-gate contract command emits an audit-only `contract.command.result`, with these fields: milestoneId, assertionId, stage, passed, exitCode, signal, timedOut, couldNotStart, startError, durationMs, stdoutTail, stderrTail, the truncated flags and failureDigest.
   - The tails are the engine's existing per-stream captures (up to 1500 chars each), scrubbed and then bounded to 4096 bytes, keeping the end.
   - Timed-out runs have empty tails.
   - Validation-round results exist only when the functional validator runs.
   - The approval lint and external-gate checks are not recorded.
   - failureDigest appears only on a non-pass. It is sha256 over the outcome, exit code, signal, start error and normalised tails. Normalisation means line endings, trailing whitespace and duration tokens masked as <dur>.
   - Known limit: after a code change, cargo's 'Compiling …' lines differ, so a real repeat failure may not match. Digest equality is evidence of the same failure; inequality is not proof of a different one.
   - The event changes no verdict and nothing the validator is shown.
   Keep the prose plain and concise like the rest of the file.
2. In CHANGELOG.md, under '## Unreleased', add an 'Added:' bullet at the top in the same voice as the existing entries. It says each contract command the engine runs now leaves a contract.command.result event in the mission's event log, with the exit code or signal, timed-out or could-not-start marker, duration, bounded stdout and stderr tails, and a failure digest for comparing repeat failures.

Verification, in priority order:
1. `cargo test -p kranz-engine --test reducer_test command_result_`
2. `cargo fmt --all`

Commit as soon as the tests pass. Never push. Change no engine source in this feature.

Done when:
- command_result_old_log_without_result_events_folds passes on the existing fixture log.
- command_result_minimal_event_folds_without_state_change passes: a minimal contract.command.result folds and leaves mission state unchanged.
- command_result_event_for_unknown_milestone_is_rejected passes.
- gates.md documents the event's fields, stages, byte bound, scrub order, timeout tails, the functional-validator condition, the digest normalisation and its limit, and lists command_result.rs in verified_against.
- CHANGELOG.md Unreleased has an Added entry for contract.command.result.
