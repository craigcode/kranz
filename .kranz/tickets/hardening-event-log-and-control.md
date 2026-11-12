---
title: Close remaining event-log and control-inbox integrity gaps
priority: 3
schedule: once
---

## Goal

Finish the integrity work on mission layout resolution, seals, locks and the control inbox.

## Context

Reported by the 2026-11-10 code review as low-severity items and not yet addressed. Each item below is a report, not a confirmed defect: confirm it against current code before changing anything, and drop any that no longer apply.

- Mission layout resolution anchors on the last `.kranz/` component of a path, which is the wrong one for a path that contains more than one.
- Mission ids `hwm` and `ctl` collide with directory names the seal uses.
- A lock's generation counter restarts at zero, so an old holder record can look current.
- Of two control commands enqueued at the same moment, the slower one is quarantined as a replay.
- The rollback check on the sealed log is not applied when the merge and status paths read it.

## Scoping answers

- Fix each item on its own evidence; an item that cannot be reproduced is closed with a note, not patched speculatively.
- Contract files change additively only.
- Each fix carries a test that fails without it.

## Acceptance hints

- Layout resolution uses the repository's own `.kranz` directory.
- The reserved names are refused as mission ids.
- Lock generations never repeat for one lock.
- Two concurrent control commands are both applied, once each.
- Merge and status reads refuse a rolled-back log the same way the engine does.
