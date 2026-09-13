---
state: done
state-note: fixed directly (hand-built), commit ba98899, 2026-07-05 — sidecar restored after tracked-on-branch/ignored-on-main checkout crossfire deleted it
title: Restore the primary checkout after a mission terminates
priority: 2
schedule: once
---

## Goal
A completed/failed/abandoned mission currently leaves the primary checkout on its mission branch. After reaching a terminal state (and after any merge step), the engine should switch the checkout back to the branch it found at approval time. Interim step toward M7 tier-1 worktree-always isolation.

## Context
Today a terminal mission (complete/failed/abandoned) leaves the primary checkout on kranz/mission-<id> — m-d341a7 left the repo on its branch after COMPLETE; the operator discovers it at the next git command. Record the checked-out branch at approval (or run start) in the event log, and after the terminal transition (post any merge step), switch back — refusing gracefully if the tree is dirty (log + event, never force). Interim measure until M7 tier-1 (workers always in dedicated worktrees, docs/scoping/worker-sandboxing.md) makes it moot; keep the implementation small.

Additional evidence (2026-07-05 draft train): `kranz draft` also leaves the
checkout on the drafted mission's branch, so SEQUENTIAL drafts stack — each
new mission branch forks from the previous mission's branch instead of main
(observed: kranz/mission-m-84e7ea contains m-642a1a's plan commit), and an
operator committing after a draft lands their commit on a mission branch by
surprise (happened live). Restore should apply after DRAFT parking too, not
just terminal states.

## Scoping answers

## Acceptance hints
- A mission run to Complete in a test repo restores the branch that was checked out at approval; same for Failed and Abandoned.
- A dirty tree at restore time is left alone with a recorded event and a warning, not a forced checkout.
- cargo test --workspace passes, piped through grep -qE 'result: ok\. [1-9][0-9]* passed'.
