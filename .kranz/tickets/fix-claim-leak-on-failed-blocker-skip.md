---
title: Claim leaks when the failed-blocker skip errors before run_mission
priority: 3
schedule: once
---

## Goal
In the work_skip_for_failed_blocker path (crates/engine/src/orchestrator.rs), if the skip step errors before run_mission runs, the acquired claim is not released -> stale claim (single-mission wedge, recoverable). Fix: release the claim on the early-error path.

## Context
Found by a security code review 2026-07-07 (HEAD 0740072). Low impact, recoverable.

## Acceptance hints
- An error in the failed-blocker skip releases the claim; a follow-up dispatch can re-claim. Test the early-error path.
