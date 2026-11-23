---
title: A racing lock acquire on Windows can fail with "Access is denied" instead of LockHeld
priority: 2
schedule: once
---

## Goal

When several acquires race to steal a mission lock whose holder is dead, every loser gets `LockHeld` on Windows, as it does on Unix. The test that checks this stops failing intermittently in CI.

## Context

`racing_acquires_on_a_dead_lock_admit_exactly_one_winner` in `crates/engine/tests/event_log_test.rs` starts 16 threads that call `EventLog::acquire` on a lock file naming a dead holder. It asserts that exactly one wins and every loser returns `EngineError::LockHeld`.

On the `rust (windows-latest)` CI job it failed twice in two days, on two pull requests that did not touch the event log or the lock (a lockfile-only dependency bump on 2026-11-22, and the contract command timeout change on 2026-11-23). Both times the message was:

`losers must see LockHeld (the winner is alive), got: Io(Os { code: 5, kind: PermissionDenied, message: "Access is denied." })`

The same job passed when rerun on the same commit the first time. Each failure costs a rerun of a job that takes about 45 minutes.

This may be more than a flaky test. The assertion that failed is the one about what a loser sees, and the winner count was not reported as wrong. On Windows a file that another handle has open, or that is being deleted, can refuse an open, create or remove with `ERROR_ACCESS_DENIED`. If `acquire` passes that error through, a real second `kranz` process that loses the race on Windows would see an I/O error and not the "mission is locked" message. The cause is not confirmed; find it before choosing the fix.

## Scoping answers

- First reproduce and name the cause: which filesystem call in the steal path (probe, remove, create, or taking the guard) returns access denied, and why.
- If the product path can return it to a losing racer, fix the product: a loser whose failure is caused by a live winner gets `LockHeld`. Do not weaken the test to accept I/O errors in that case.
- If the cause is in how the test sets up or tears down, fix the test and say why the product path is not affected.
- Do not map every `PermissionDenied` to `LockHeld`. A lock file the user really cannot access must still report an I/O error.
- The exactly-one-winner guarantee and the dead-holder steal rules are unchanged. Read `docs/knowledge/decisions/inviolable-invariants.md` on the engine lock before changing `acquire`.
- Unix behaviour is unchanged.
- The fix can only be proven on Windows. The contract should include a check that runs on any host (for example a unit test of the error classification) and say that the racing test is confirmed by the Windows CI job.

## Acceptance hints

- The cause is stated in the pull request, with the call that returned access denied.
- A test of the classification: an access-denied result that comes from a live winner maps to `LockHeld`, and one that does not stays an I/O error.
- `racing_acquires_on_a_dead_lock_admit_exactly_one_winner` passes on the Windows CI job across repeated runs.
- Anti-vacuity: the test filter matches only the new tests.
