---
title: Stop worker sessions ending while a long gate command is still running
priority: 2
schedule: once
---

## Goal

A worker that runs a long command, such as the workspace test suite, can end its session before the command returns. The run is recorded as partial, the feature is respawned, and the same thing happens again until the respawn budget is spent. Find where the session is lost and close it, or record plainly that kranz leaves it to the agent CLI.

## Context

Found on 2026-11-13. In one mission a feature ran three times and failed with "respawn budget exhausted" although its implementation and tests were committed and matched the spec each time. The orchestrator's notes for the three runs give the same cause:

- The worker ran `cargo test --workspace` as a foreground command. It outlasted the agent CLI's default foreground limit and was moved to the background.
- The worker then tried to wait for it. The wait and monitor tool calls were denied by the session's tool policy.
- The session ended with no result for the command and no report.

Each respawn cost a full worker session and produced no new code. A failed feature then blocks the milestone on procedure, not on the change.

## Scoping answers

- This sits close to the positioning freeze on execution primitives (docs/knowledge/decisions/positioning-governance-evidence-layer.md). Flag as a D-X operator decision whether any remedy is in scope. Recording the cause in the event log is evidence and is in scope either way.
- Candidate remedies to weigh in a short design note, not to build by default: the engine sets the backend's foreground command limit from the existing worker timeout config; the session tool policy allows the wait call for a command the worker itself started; a run that ends with a backgrounded command still running is classified separately from an ordinary partial run and does not spend a respawn.
- Do not widen the session's tool policy without reading docs/knowledge/validation/child-containment.md and docs/acp-containment.md.
- Confirm the cause from a mission's event log before changing anything. The account above comes from orchestrator notes, not from a reproduced case.
- Contract files change additively only.

## Acceptance hints

- A design note states the cause as confirmed from an event log and records the operator's decision.
- The event log distinguishes a run that ended while its own command was still running from other partial runs.
- If a remedy is approved, a test shows a worker command longer than the default foreground limit completes and its result reaches the worker.
