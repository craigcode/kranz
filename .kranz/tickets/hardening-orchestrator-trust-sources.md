---
title: Read orchestrator inputs from approved or sealed sources
priority: 3
schedule: once
---

## Goal

Make four orchestrator decisions depend on what was approved or sealed, not on mutable working-tree state.

## Context

Reported by the 2026-11-10 code review as low-severity items and not yet addressed. Each item below is a report, not a confirmed defect: confirm it against current code before changing anything, and drop any that no longer apply.

- Packs are read from the working tree, so a mission can change the pack that judges it. They should be read from the pinned base commit.
- An engine-written decision clears queued operator messages before the orchestrator has consulted them.
- The "a mission must deliver" gate counts commits; a commit followed by its revert passes with no net change.
- `plan.json` on disk is preferred over the plan recorded in the sealed event log.

## Scoping answers

- The delivery gate is an inviolable invariant: tighten it, never loosen it.
- Changing where packs are read from may be a contract change request; check before implementing.
- Each fix carries a test that fails without it.

## Acceptance hints

- A pack edited on the mission branch does not affect that mission's gates.
- A queued operator message survives until an orchestrator turn has seen it.
- A mission whose net diff against `base_sha` is empty fails the delivery gate.
- The approved plan is taken from the event log; a differing `plan.json` is reported, not used.
