---
title: macOS Seatbelt profiles: enforce=fs for worker sessions
priority: 2
schedule: once
blocked-by: [sandbox-1-worktree-always]
---

## Goal
M7 tier 2 (macOS): generate a Seatbelt profile per session (sandbox-exec): write allowlist = the session's worktree + mission control dir + TMPDIR + opt-in toolchain caches (~/.cargo, npm cache); read broad; no network restriction yet (that is the next ticket). Config surface sandbox: { enforce: off|fs, extraWrite: [] } per role, default off; backend_claude wraps the spawn when enforced. Preflight probe: run the contract's validation commands under the profile and surface failures as preflight issues, not mid-run mysteries. Windows explicitly documented as out of scope.

## Context
Design of record: docs/scoping/worker-sandboxing.md (threat model with
receipts, three tiers, sequencing, open questions) — read fully before
planning. Fresh incident receipts strengthening tier 1, all from
2026-07-05/06: an operator commit landed on a live mission's branch
(twice); sequential drafts stacked mission branches; a checkout
crossfire between branches tracking and main ignoring runtime files
deleted six ticket sidecars. Every one is impossible once workers live
in worktrees and the primary checkout never moves. Engine seams: M3's
GitRepo::add_worktree/remove_worktree/prune_worktrees + the parallel
path in orchestrator.rs run_parallel_batch_inner; permissions.rs;
backend_claude.rs spawn. Missions must gate --workspace, not -p
kranz-engine (repo meta-lesson).


## Scoping answers

## Acceptance hints
