---
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
