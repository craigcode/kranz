---
title: Optional PR handoff for completed missions, without auto-push
priority: 2
schedule: once
---

## Goal
Add an optional GitHub PR handoff for COMPLETE-but-unmerged missions that
never pushes. If the mission branch already exists on the remote, kranz may
run or prepare `gh pr create`. If not, show a copyable `git push` for the
human. Preserve the local rule: kranz does not push refs from the operator
machine.

## Context
Mission Control's PR button is the UX reference; kranz adapts handoff only.
M6 cloud/CI may push `kranz/*` under explicit deploy-key scope — that path is
out of scope for this ticket. Local operator flow stays stricter.

## PR body rules
- Derive title/body from plan/report artifacts (scrubbed, length-capped).
- Include mission id, plan/report paths, and validation/contract summary from
  `report.md`.
- **Do not** claim merge-gate status as passed/failed before `/merge` has run.
  Say clearly: merge gates run at merge time and are still pending.
- Do not imply the mission is landed on the base branch.

## Acceptance hints
- COMPLETE-but-unmerged missions show the affordance when a GitHub remote
  exists and `gh` is available/authenticated (else a clear "unavailable"
  reason).
- Missing remote branch → copyable `git push origin <mission-branch>`; no
  push performed.
- Present remote branch → `gh pr create` (or prefilled command) only.
- Tests prove no local PR-handoff path invokes `git push` (string/command
  deny or process spy).
- Anti-vacuity grep on the named test filter.
