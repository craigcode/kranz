---
title: Keep the output of a failed contract command
priority: 2
schedule: once
---

## Goal

When a command assertion fails in the contract gate, the evidence of why is lost. Record enough of the command's output that the orchestrator, a fix worker and the operator can see what failed without re-running it.

## Context

Found on 2026-11-14 in mission `m-7886d9`. Its final gate failed the assertion that runs `cargo test --workspace`, three times. The `validation.finding` event recorded only "command failed" and the command text. The assertion captured its own output in a shell variable, so nothing reached stdout, and the gate keeps no copy of stderr or of the exit status.

The effects, all visible in that mission's event log:

- The orchestrator wrote a fix feature from a list of guesses because it had no failing test name.
- Two fix cycles were spent finding out which test failed. The second found a candidate only by re-running the suite outside the gate, in a different environment.
- A functional validator described an output tail that belonged to a different assertion.
- The fix-cycle cap was reached and the mission blocked with the cause still unconfirmed.

The cause was found afterwards by running the same command by hand in the gate's environment with the output kept. Two engine unit tests failed every time because the gate's `TMPDIR` is inside a mission directory. The candidate the mission had settled on, a timing-sensitive test, was not the cause. One retained output tail would have shown this on the first failure.

The contract commands run in `run_contract_commands_for_validation` in `crates/engine/src/orchestrator.rs`, through `run_shell_command_sandboxed` in `crates/engine/src/command_exec.rs`.

## Scoping answers

- Keep the output of a FAILED command assertion as a mission artefact under the mission's `runs/` directory, scrubbed the same way as other captured output, and name it from the finding.
- Record the exit status, and whether the command hit its time limit, on the finding. A timeout and a failing test need different fixes.
- An assertion written in the anti-vacuity form (`out=$(...) && printf ... | grep -q ...`) swallows its own output. Decide whether the gate should capture the inner command's output for that shape, or whether the contract lint should steer authors to a form that leaves output visible. Flag it as a D-X decision.
- Bound the stored size. Keep the tail, since test failures print last.
- Contract files change additively only.
- The artefact is runtime state. It is not committed and must not be readable from inside a worker or validator sandbox beyond what the finding text already shows.

## Acceptance hints

- A test fails a command assertion on purpose and asserts that the finding carries the exit status and points at a stored output tail containing the failing line.
- A test covers a command that exceeds its time limit and asserts the finding says so.
- A secret-shaped string in the command's output is scrubbed in the stored artefact.
- `docs/knowledge/validation/gates.md` describes where failed command output is kept.
