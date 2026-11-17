---
title: Record and bound how much disk a mission's build output takes while it runs
priority: 2
schedule: once
---

## Goal

A mission's build output cannot fill the disk unnoticed. The event log shows how large the mission's worktree has grown, the mission stops with a clear reason before the disk is full, and an abandoned mission's build output does not stay behind without the operator being told its size.

## Context

Ticket `mission-build-footprint` added a check before a mission starts: it refuses when free space is below an estimate. Nothing looks again after that.

Seen in a mission run on 2026-11-16 on this workspace. The integration worktree's build folder reached 46 GB over about six hours, and free space on the machine fell to about 5 GB. Two things made it larger than the estimate:

- Workers built for a second target triple to check Windows-only code, which adds a second full build tree.
- The mission ran many more worker sessions than planned, each rebuilding after its changes.

The mission ended Blocked, so its worktree stayed. `kranz abandon` now removes a mission's worktree only when it holds nothing but committed files. A build folder is an ignored directory, so the worktree is kept and its path printed. The 46 GB stayed until it was deleted by hand.

## Scoping answers

- Recording the size is evidence and is in scope. Record the worktree's size on disk, and the free space on its volume, at milestone boundaries. Use an existing event with an additive field, or a decision record. Contract files change additively only.
- Bounding is the operator decision. Flag as D-X: when free space falls below a floor during a mission, does the engine block the milestone with a disk cause, or only record it? A block must name the size and the path.
- When `kranz abandon` or `kranz clean` keeps a worktree because of ignored files, print the size of the ignored files beside the path. Flag as D-X whether those commands may remove ignored build output when the operator passes an explicit flag. They must not remove it by default: the existing rule keeps anything that is not committed.
- Measuring size walks a large tree. Bound the cost: measure at milestone boundaries, not per command, and put a time limit on the walk.
- Do not share one build folder across worktrees and do not change build profiles here. Ticket `mission-build-footprint` left those as later options and they are still out of scope.
- The primary checkout stays byte-untouched.

## Acceptance hints

- A mission test shows the size record at a milestone boundary, with a worktree that has grown between two milestones.
- If the block is approved: a test with the floor set above the available space ends with the milestone blocked and a reason naming the size.
- A test for `kranz abandon` on a worktree with ignored files shows the size in the printed line and the directory still present.
- Anti-vacuity: the test filter matches only the new tests.
