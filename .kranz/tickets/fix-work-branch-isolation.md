---
state: done
state-note: fixed directly (hand-built), commit ba98899, 2026-07-05 — sidecar restored after tracked-on-branch/ignored-on-main checkout crossfire deleted it
title: Queued missions must run on their mission branch, not the current checkout
priority: 2
schedule: once
---

## Goal
Missions drafted via kranz draft carry branch: None in mission state, and the kranz work dispatcher runs them directly on whatever branch is checked out — observed live: three missions committed every worker commit straight onto main (no isolation, no merge gate). The kranz/mission-<id> branch created for the parked plan commit must be the run branch: kranz work (and any run path with a missing branch) checks out the mission branch before spawning workers, and completion merges to the base branch exactly like serve-hosted runs.

## Context
Observed 2026-07-05 on the first kranz-work train (m-468277, m-642a1a,
m-84e7ea): linear worker commits directly on main, recorded branch: None
in each mission's state, while the kranz/mission-<id> branches held only
the parked-plan commits. Contrast serve-hosted missions (m-d341a7): branch
recorded, run isolated, merge on completion. Root cause to confirm: the
draft path parks the plan on a mission branch but never records it in
mission state, and the run loop treats branch: None as run-in-place.
Only safety that held: kranz never pushes locally, so origin was
untouched. Fix belongs at both ends: cmd_draft records the branch it
committed the plan on; the run path refuses to spawn workers with no
recorded branch unless explicitly overridden (a loud flag, not a silent
fallback). Interim severity is high: a failed queued mission leaves
unvalidated commits in main's local history. Related: the checkout-restore
ticket (fix-checkout-restore-on-completion) and M7 tier-1 worktree-always
(docs/scoping/worker-sandboxing.md) both reduce blast radius here.

More evidence from the same train: the engine's checkpoint commit
(af3787a, m-468277 f-1-3) swept the operator's uncommitted notes.txt
edits into mission history — the dirty-tree commit-as-is policy absorbs
operator working-tree changes when missions run in the primary checkout.
Branch/worktree isolation fixes this class too; until then checkpoint
commits should scope to worker-declared paths or record a refusal
decision instead of committing foreign changes.

## Scoping answers

## Acceptance hints
- A drafted-then-queued mission records its mission branch in state; kranz work checks it out before the first worker spawns and merges to the base branch on completion (integration test with the mock backend).
- A mission with no recorded branch is refused by the run path with an actionable message (test), not silently run in place.
- Existing serve-hosted run behavior unchanged (regression test).
- cargo test --workspace passes, piped through grep -qE 'result: ok\. [1-9][0-9]* passed'; clippy and fmt clean.
