---
state: done
title: A mission that produces zero deliverable commits must FAIL, not COMPLETE
priority: 1
schedule: once
---

## Goal

Mission m-66aff8 reached COMPLETE with its contract reported all-green
while its branch carried ZERO feature commits — the workers produced "no
report, no commits, empty diff" (an auth outage) yet the mission
succeeded. A mission whose deliverable diff against the pinned base is
empty (no feature commits landed on the mission branch) must terminate
as Failed with an honest note, never COMPLETE — regardless of what the
contract gate reports. This is the safety net that would have caught the
env-hygiene work-loss immediately instead of after a merge review.

## Context

Discovered 2026-07-06 investigating the env-hygiene auth outage
(fix-worker-env-hygiene-starves-auth). Two failures compounded:
(1) workers produced nothing; (2) the completion path did not notice the
branch had no feature commits and declared success. The engine's own
per-feature judge DID log "no report, no commits, empty diff — nothing
landed," so the signal exists — it just did not gate mission completion.
Open questions for the plan: WHERE to assert non-emptiness (final gate,
before write_mission_report, at milestone close); how it interacts with
legitimately doc-only or no-op missions (should be rare — a mission with
a validation contract that passes on an empty diff is itself suspect);
and how this relates to the anti-vacuity contract-grep (the contract
commands passed here despite an empty tree — investigate whether the gate
ran against stale/uncommitted worktree state, which is its own concern).
Reuses the base_sha diff primitives already in the engine.

## Acceptance hints

- A mock mission whose worker produces no commits terminates Failed
  (with a clear "no deliverables landed" note), not Complete.
- A genuinely-delivering mission is unaffected.
- Investigate and note how the contract gate reported green on an empty
  tree (stale build / uncommitted worktree state) — file a follow-up if
  it is a distinct defect.
- cargo test --workspace green.
