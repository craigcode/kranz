---
title: Finish container and ACP cleanup and rate-limit paths
priority: 3
schedule: once
---

## Goal

Close the remaining cleanup and limit gaps in the container and ACP worker paths.

## Context

Reported by the 2026-11-10 code review as low-severity items and not yet addressed. Each item below is a report, not a confirmed defect: confirm it against current code before changing anything, and drop any that no longer apply.

- A failed `docker create` is reported as an uncertain outcome and leaves its prepared inputs on disk.
- `recover_dead_owner_ledgers` is called only from tests, so ledgers of a crashed owner are never recovered in production.
- The gate container builder has no `--network none` fallback when no network is resolved.
- The ACP terminal supervisor remains dumpable until its `prctl` call. This is a fixture path and fails closed.
- A terminal peer's process group can be signalled after the peer was reaped.
- Only refused terminal calls count toward the terminal rate limit.

## Scoping answers

- Verification of the container items needs a running container daemon; say so in the change if one was not available.
- Fix each item on its own evidence; an item that cannot be reproduced is closed with a note.
- Each fix carries a test that fails without it.

## Acceptance hints

- A failed create is reported as failed and its inputs are removed.
- Dead-owner ledgers are recovered at start, with a test.
- A gate container with no resolved network runs with networking off.
- No signal is sent to a reaped peer's group.
- Accepted terminal calls count toward the rate limit.
