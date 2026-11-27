# Research — m-59089f

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- .kranz/tickets/contract-check-results-recorded.md
- .kranz/tickets/harness-fault-findings-block-milestone.md
- AGENTS.md
- docs/knowledge/validation/gates.md
- crates/engine/src/command_exec.rs
- crates/engine/src/orchestrator.rs
- crates/engine/src/orchestrator/finalization.rs
- crates/engine/src/orchestrator/external_gates.rs
- crates/engine/src/events.rs
- crates/engine/src/reducer.rs
- crates/engine/src/scrub.rs
- crates/engine/src/contract_shell.rs
- crates/cli/src/tail.rs
- crates/engine/tests/mission_test.rs
- crates/engine/tests/reducer_test.rs
- .kranz/lessons/*.md

## External sources

- crates/engine/src/command_exec.rs:1527-1597
- crates/engine/src/command_exec.rs:1947-2084
- crates/engine/src/orchestrator.rs:6020-6134
- crates/engine/src/orchestrator.rs:6240-6424
- crates/engine/src/orchestrator/finalization.rs:231-291
- crates/engine/src/event_log.rs:749

## Facts

- Contract commands run through run_contract_command_timed, which returns only {code, output, timed_out}. — `crates/engine/src/command_exec.rs:1527-1597`
- stdout and stderr are drained into separate 1500-char tails, then joined with a '--- stderr ---' marker and cut to the last 1500 chars, so a long stderr pushes out stdout's failing test names. — `crates/engine/src/command_exec.rs:2040-2050, 2055-2084`
- Spawn failure, read or wait failure and signal death all return code None. Sandbox-wrap failure also returns code None, with a message in output. — `crates/engine/src/command_exec.rs:1554-1563, 1954, 2017-2050`
- On timeout, all command output is dropped; only 'timed out after Ns' remains. — `crates/engine/src/command_exec.rs:2000-2016`
- There are three engine-run contract-command call sites: the validation round's due commands, the not-yet-due commands and the final gate. — `orchestrator.rs:6042, orchestrator.rs:6104, orchestrator/finalization.rs:248`
- Validation-round contract commands run only when the functional validator role is enabled. — `crates/engine/src/orchestrator.rs:6244`
- Every appended event is passed through scrub_json_value. — `crates/engine/src/event_log.rs:749`
- The CLI tail renderer matches EventKind exhaustively, so a new variant needs an arm. — `crates/cli/src/tail.rs:463-500 (no catch-all arm)`
- External-gate checks run contract commands through a different runner (run_shell_command_sandboxed_with_code, 300 s) and already record the exit code and scrubbed output in an orchestrator.decision. — `crates/engine/src/orchestrator/external_gates.rs:93-122`
- No existing test name contains 'command_result', so the contract filters cannot collide with an old test. — `grep for 'fn [a-z_]*command_result' across crates returned nothing`

## Ambiguities & stale docs

- The operator did not answer the planning question on digest normalisation. The plan uses the proposed default: line endings, trailing whitespace and duration tokens are normalised; hashes and paths are not masked.
- The operator did not answer whether to include the external-gate check path. The plan leaves it and the approval lint out of scope, and records that in gates.md.
- 'One result event' in the ticket's first acceptance hint is read as one event per engine run of the assertion. The final-gate test pairs records 1:1 with command-assertion findings.

## Candidate knowledge updates

- docs/knowledge/validation/gates.md: a 'Recorded contract command results' section covering the event fields, stages, bound, digest rules and limits.
- docs/knowledge/glossary.md: a 'failure digest' entry pointing at gates.md, for the follow-up harness-fault ticket.
