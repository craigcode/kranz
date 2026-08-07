---
state: done
state-note: d60ca89: workspace.teardownMode config (keep|hibernate|destroy, default keep) drives provider teardown at terminal states; workspace.teardown event gains state (kept|stopped|destroyed|failed, additive); MissionState.workspace_lifecycle folds latest-wins with ts; remote idleAfterHours passes through to the substrate at provision (kranz never schedules); local-worktree always Keep; teardown failure logs a scrubbed decision and never masks the terminal outcome; endpoint gains workspaceLifecycle. 13 … (truncated)
title: Workspace idle hibernate / destroy lifecycle events
priority: 3
schedule: once
blocked-by: [workspace-remote-coder-provider]
---

## Goal
Record and drive provider-owned idle hibernation and destroy for remote
workspaces (after inactivity or mission terminal states), with append-only
lifecycle events and cost-relevant timestamps — without kranz scheduling
VMs itself.

## Context
Monaco: Coder scheduled hibernation after idle hours; replaceable VMs with
persistent volumes. Design D-B teardown modes. Local providers may no-op
hibernate.

## Acceptance hints
- Teardown modes keep | hibernate | destroy are explicit in the seam.
- Events record transitions; dashboard shows workspace lifecycle state.
- Idle policy is provider/config-owned; engine does not invent a cloud
  scheduler.
- Anti-vacuity grep on a named filter unique to this work.
