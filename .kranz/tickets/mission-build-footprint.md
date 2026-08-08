---
title: Mission build footprint — one full target/ per worktree does not fit small disks
priority: 2
schedule: once
---

## Goal

A mission currently builds a complete `target/debug` (~43 GiB observed for
this workspace, m-eee81f) inside its integration worktree, then the final
validation snapshot copies a warmed subset again, and the merge gate may
build again in a scratch worktree. On a disk with <50 GiB free the mission
ENOSPCs mid-command, burning feature respawn budgets on Partial runs
(happened twice: m-a5a8fd, m-eee81f, both os error 28).

## Candidate shapes (decide in the plan)

- Share one warmed target across mission worktrees via `CARGO_TARGET_DIR`
  (watch the a3 CI lesson: feature-plan thrash between `-p` and
  `--workspace` invocations on a shared target causes rebuild storms;
  concurrent cargo locks serialize builds).
- Reuse the integration worktree's own target for the final gate instead of
  a fresh scratch build.
- Reduce debug info for mission builds (`profile.dev.debug = 0` or
  line-tables-only) via the contract command env, cutting target size by
  roughly half.
- Pre-flight disk check at drain: refuse to start a mission when free space
  < estimated build footprint, naming the number (cheap, honest, and would
  have prevented both observed ENOSPC failures).

## Acceptance hints

- A mission on a disk with ~10 GiB free either completes its full gate
  ladder or refuses at drain time with the estimate named — never dies
  mid-feature with os error 28.
- The primary-checkout byte-untouched and feature-plan invariants hold.
- Anti-vacuity: filter `mission_build_footprint_` matches only new tests.
