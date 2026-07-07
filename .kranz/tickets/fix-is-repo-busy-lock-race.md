---
title: is_repo_busy races the per-mission lock (two missions, one repo)
priority: 2
schedule: once
---

## Goal
In supervised/multi-dispatch mode the is_repo_busy check and the per-mission lock acquisition are not atomic (crates/engine/src/orchestrator.rs), so two dispatchers can both pass the check and start two missions against one repo. Fix: fold the busy check into the lock acquisition (single atomic guard) so the loser gets LockHeld instead of proceeding.

## Context
Found by a security code review 2026-07-07 (HEAD 0740072). TOCTOU; matters for the LAN-team-execution multi-dispatch future.

## Acceptance hints
- Two concurrent dispatchers against one busy repo: exactly one proceeds, the other gets LockHeld. Test the atomic guard.
