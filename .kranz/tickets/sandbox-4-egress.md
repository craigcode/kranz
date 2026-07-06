---
title: Egress allowlist: enforce=fs+net (macOS)
priority: 2
schedule: once
blocked-by: [sandbox-3-seatbelt-fs]
---

## Goal
M7 tier 2 completion on macOS: extend the generated Seatbelt profile with a network allowlist — Anthropic API endpoints by default plus a mission-config egress list (package registries when the contract needs builds). Config enforce gains fs+net; scrutiny-floor posture per the scoping doc: consider a floor of fs for AUTONOMOUS dispatch (kranz exec, Gas City beads) while attended runs stay free — implement the floor as config validation, mirroring skipScrutiny/--allow-unvalidated.

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
