---
title: Workers and validators always run in dedicated worktrees
priority: 2
schedule: once
---

## Goal
M7 tier 1 (docs/scoping/worker-sandboxing.md): every worker and validator session runs in a dedicated git worktree — never the primary checkout — even for sequential single-worker runs (M3's worktree machinery exists: GitRepo::add_worktree). The primary checkout stops changing branches entirely: run() no longer checks out the mission branch in the primary tree; workers get worktree cwds; the run loop merges worker results within the mission branch exactly as the M3 parallel path already does. Config workerIsolation: worktree|checkout, default worktree after one soak mission. The primary checkout must be byte-untouched across an entire mission (assert in tests).

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
