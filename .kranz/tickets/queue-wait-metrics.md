---
title: Record how long each mission waited in the queue and how long it ran
priority: 3
schedule: once
---

## Goal

Make queue waiting measurable: for each mission, how long it sat queued before it started and how long it then ran.

## Context

There is no record today of a mission waiting behind another one, so whether the one-mission-per-repository rule costs anything cannot be answered from data. This is the evidence the D-A revisit trigger in [Concurrent missions](../../docs/scoping/concurrent-missions.md) asks for, and part of D-H there.

The queue entry is written at enqueue and claimed when the drain starts the mission; the event log records the first `milestone.started` or `worker.spawned` and the terminal event.

## Scoping answers

- Derive the numbers from records that already exist where possible. Any new field is additive.
- Report per mission and as a per-repository summary; no dashboard work is required in this ticket.
- A mission that was never queued (a hosted start) reports no queue time, not zero.
- Also report what was running while a mission waited, so waiting behind an unrelated mission can be told apart from waiting behind a dependency.

## Acceptance hints

- `kranz` can print, for a completed mission, its time queued and time running.
- A summary over recent missions gives total and median time queued.
- A test covers a mission that waited behind another and one that started immediately.
