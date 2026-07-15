---
title: Optional PR handoff for completed missions, without auto-push
priority: 2
schedule: once
---

## Goal
Add an optional GitHub PR handoff for completed missions that never pushes
automatically. When a mission branch is complete and reviewable, kranz should
help the operator create a pull request only if the branch already exists on a
remote, or else show the exact push command the human must run first. The
handoff can live in the dashboard, CLI, and Slack, but it must preserve the
local rule: kranz does not push refs from the operator's machine.

## Context
Mission Control has commit/push/PR affordances. The useful kranz-sized slice is
not auto-push and not general git publishing; it is a post-completion review
bridge for teams that want GitHub PR review around a delivered mission branch.

This overlaps with M6's scoped push exception, but the local product rule stays
stricter. Cloud/CI execution may push `kranz/*` refs under explicit deploy-key
scope; local kranz should only detect remote branch presence and run or prepare
`gh pr create` when no push is required.

## Acceptance hints
- COMPLETE-but-unmerged missions show a PR handoff affordance when the repo has
  a GitHub remote and `gh` is available/authenticated.
- If the mission branch is not present on the chosen remote, kranz shows a
  copyable `git push origin <branch>` command and performs no push.
- If the branch is already present remotely, kranz can run `gh pr create` (or
  open a prefilled command) with title/body derived from plan/report artifacts.
- The generated PR body links mission id, plan/report paths, validation
  summary, and merge-gate status, with secrets scrubbed and bounded length.
- Tests prove no local PR handoff path invokes `git push`.
