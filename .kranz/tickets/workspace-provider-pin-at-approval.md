---
state: done
state-note: Done in the M6 arc: WorkspacePin recorded at approval with workspace.provider.pinned event.
title: Pin workspace provider identity at mission approval (M6)
priority: 2
schedule: once
blocked-by: [workspace-sandbox-visibility, workspace-provider-seam]
---

## Goal
When a container/remote (or any non-default) workspace provider exists, pin
the effective provider kind + template/image identity (and version) at plan
approval, and surface readiness, preview links, and human-takeover
instructions as audited mission artifacts.

## Context
Split out of the original visibility ticket: approval-time provider pinning
is an M6 product seam (`docs/roadmap.md` M6,
`docs/scoping/workspace-contract.md` D-B / D-E), not a dashboard cosmetic.
Local visibility (`workspace-sandbox-visibility`) supplies UI chrome; the
provider seam supplies something real to pin.

## Acceptance hints
- Approval records provider id + template/image version in mission state /
  additive events.
- Dashboard/report show readiness, preview links, takeover details when
  present; secrets never logged.
- Workspace provisioning failures map to preflight or blocked with a clear
  owner (operator / provider / repo setup).
- Local worktree-only missions remain valid without a remote provider.
- Anti-vacuity grep on the named filter.
