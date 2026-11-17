---
state: done
state-note: Mission m-2a80fd delivered milestones 1 and 2 and was abandoned while blocked; milestone 3 was finished by hand on the same branch. Still owed: a retest on the reporting Windows host with a build from main, and a check of its worker.sandbox.enforce. Windows CI showed Git sh cannot start inside the AppContainer sandbox, so a host with the worker sandbox on still runs contract commands under cmd.exe; if the reporting host is one, open a ticket for a shell that runs inside the sandbox.
title: Run contract commands on Windows the way the planner writes them
priority: 1
schedule: once
---

## Goal

A validation-contract command that passes under `sh -c` must not fail on Windows because of how the engine hands it to the shell. Make the Windows path run the planner's commands correctly, and reject at approval any command the host shell cannot run.

## Context

Reported from a Windows 11 host running release 0.5.0 on 2026-11-15. The planner wrote command assertions as `node -e "..."` one-liners with inner double quotes, and one check that reads `$KRANZ_BASE_SHA`. Eight of nine failed before any assertion executed. The same commands exit 0 through `sh -c`.

`shell_argv` in `crates/engine/src/command_exec.rs` returns `cmd /C <command>` on Windows and `sh -c <command>` elsewhere. Two separate faults follow:

- The command is passed as one argument through the standard argument encoder, which writes each inner `"` as `\"`. `cmd.exe` does not treat `\"` as an escape, so the program receives a script that starts with a literal quote.
- `cmd.exe` does not expand `$NAME`. A check that uses `$KRANZ_BASE_SHA` passes the literal text to git.

The planner prompt and every example in the docs use POSIX shell syntax. `prompts.rs` is frozen and is not the place to fix this.

## Scoping answers

- Operator decision 2026-11-15: prefer a POSIX `sh` when one is available on the host (Git for Windows ships one, and kranz already requires Git). Otherwise keep `cmd`, and add an approval-stage gate for commands it cannot run.
- Resolve `sh` to an absolute path outside the worktree and outside any path a worker can write. Never resolve it by bare name from the child's search path or working directory.
- On the `cmd` path, pass the command text unencoded (`raw_arg` or equivalent) so inner double quotes reach the program intact.
- Contract commands run inside the gate sandbox. Prove on Windows that the chosen `sh` starts and runs a quoted command inside the AppContainer wrap before making it the default. If it cannot run contained, keep `cmd` as the contained path and say so in the docs. Do not widen the sandbox to make it work.
- Record which shell ran each command, so a reader of the evidence can tell.
- The new approval gate sits beside `vacuous-filter` and `wrong-polarity` in `crates/engine/src/contract_gates.rs`. It flags syntax the resolved host shell will not honor, such as `$NAME` expansion or single-quoted arguments under `cmd`.
- Read `docs/knowledge/validation/child-containment.md` and `docs/knowledge/validation/gates.md` first. The gate signal policy and the fail-closed wrap do not change.
- Contract files change additively only.

## Acceptance hints

- On Windows CI, a command with inner double quotes, of the shape `prog -e "a 'b' c"`, reaches the program with its quotes intact on both the `sh` path and the `cmd` path.
- On Windows CI, a command that reads `$KRANZ_BASE_SHA` sees the pinned sha when `sh` is the resolved shell.
- With no `sh` available, approving a contract that contains `$NAME` expansion records a failing result from the new gate that names the assertion.
- A planted `sh.exe` in the worktree is never executed.
- Unix behaviour is byte-identical: `GateSandbox::Disabled` still reproduces `sh -c <command>`.
