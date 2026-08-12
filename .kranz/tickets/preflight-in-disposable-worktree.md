---
state: done
state-note: Done at 4dd5d92: probes run in disposable detached worktree under runs/ (RAII cleanup + stale sweep), under the cleared contract env, via consolidated run_command_bounded (concurrent pipe drain, unix pgroup/Windows Job Object kill); profile file moved to gitignored runs/. Primary checkout byte-untouched verified by test. Tests 2>&1 | grep -qE 'test result: ok. [1-9]'
title: Run sandbox preflight in a disposable worktree, never the primary checkout (P1)
priority: 2
schedule: once
---

## Goal
Sandbox command preflight substitutes the PRIMARY checkout for the future
session worktree and runs write-capable contract commands there
(preflight.rs:165), violating the primary-checkout byte-untouched
invariant and leaving generated/malicious modifications before workers
start. Run preflight in the integration worktree or a disposable detached
one, via command_exec's bounded process-group runner (the current helper
does not drain pipes concurrently or kill the full tree).

## Context
From the review (P1 #7). The integration worktree exists by the time
workers need it; creating it before probing aligns both.

## Acceptance hints
- Primary checkout is byte-identical before/after preflight (assert).
- Preflight probes run in a disposable/integration worktree (assert cwd).
- Timeout kill takes the whole process tree (existing exec tests cover).
- cargo test --workspace green.
