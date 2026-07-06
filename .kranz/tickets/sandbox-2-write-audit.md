---
title: Out-of-contract write audit + worker env hygiene
priority: 2
schedule: once
blocked-by: [sandbox-1-worktree-always]
---

## Goal
M7 tier 1 second half: (1) post-run sweep comparing each worker worktree's diff against the plan's declared touch-set plus a cleanliness assertion on the primary checkout, surfacing violations as a new validator finding class out-of-contract-write; (2) probe and document the minimal file/dir set the claude CLI needs (scoping open question 1), then give worker sessions a scratch HOME/CLAUDE_CONFIG_DIR carrying only that set. Detection-layer honesty: this closes the visibility gap while tier 2 closes the capability gap.

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
