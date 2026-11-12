---
title: Split the repo busy lock into a run lock and a merge lock
priority: 2
schedule: once
---

## Goal

Stop a merge waiting for a running mission, and a mission start waiting for a merge, by replacing the single repo-wide busy lock with two locks. One mission at a time per repository stays the rule.

## Context

Design and decisions: [Concurrent missions](../../docs/scoping/concurrent-missions.md), D-B and delivery slice 2. D-A (running more than one mission at once) is declined for now; this ticket is the part that does not depend on it.

Today `RepoBusyGuard` in `crates/engine/src/queue.rs` is taken by the queue claim (`claim_front_when_repo_free`), by a hosted start (`acquire_repo_busy`) and by the merge, and each holds it for its whole duration. The merge already works in a detached scratch worktree against pinned base and tip SHAs, so it does not need the run to be idle.

## Scoping answers

- Keep exactly one run slot. Do not add a configurable limit in this ticket.
- The merge lock is exclusive and held only for the merge transaction.
- `is_repo_busy` keeps its meaning for existing callers: it reports the running mission, if any.
- Stale-lock recovery (dead pid, pid reuse) must behave for both locks as it does for the one today.
- Contract files change additively only.

## Acceptance hints

- A merge of a delivered mission succeeds while another mission is running in the same repository, and the running mission is unaffected.
- Two merges in the same repository still serialize.
- A second mission still cannot start while one is running.
- A soak test exercises concurrent ref and object writes from a running mission worktree and a merge scratch worktree without corruption. Delivered as `soak_merge_during_run` in `crates/engine/tests/soak_test.rs`, run by `scripts/soak.sh`.
