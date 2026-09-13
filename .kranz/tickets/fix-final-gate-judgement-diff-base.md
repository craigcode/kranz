---
state: done
state-note: code fix landed by hand (09bebcb); deferred regression test landed in b287651 (judge_gate_diff_uses_pinned_base_sha). m-e96644, approved to deliver exactly that test, verified superseded on main 2026-07-23 and abandoned unrun; branch deleted.
title: Final-gate judgement diff must use pinned base_sha, not the moving base branch
priority: 3
schedule: once
---

## Goal

judge_contract_assertions diffs the MOVING base branch (`let base =
self.state.mission.base_branch.clone(); ... diff_stat(&base, "HEAD")`,
orchestrator.rs ~2632-2633) while every other contract consumer uses
the base_sha pinned once at approval (KRANZ_BASE_SHA in command env,
validator rounds). A base branch that moved during the mission silently
changes the final judgement's diff. Use the pinned base_sha (falling
back to base_branch only when base_sha is None for legacy missions),
matching the never-re-resolve rule documented at approve_plan.

## Context

Found during M2 re-planning scoping (docs/scoping/mid-mission-
replanning.md D-E, 2026-07-06): the doc's own consistency argument
exposed the one remaining moving-base consumer. Same class as the
fix-base-sha-final-gate-env ticket that pinned the command env.

## Acceptance hints

- Judgement diff uses base_sha when present; test with a temp repo
  whose base branch advances mid-mission — the judgement diff must not
  change.
- cargo test --workspace green.
