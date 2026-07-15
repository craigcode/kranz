---
title: Workspace and sandbox visibility for local worktree missions
priority: 2
schedule: once
---

## Goal
Make the **effective local execution workspace** visible wherever an operator
reviews or steers a mission: isolation mode, active worktree/cwd, sandbox
enforce tier, and setup/preflight outcomes that already exist. Record a short
workspace summary in report.md / dashboard so operators stop inferring it from
branch names.

## Context
Mission Control's scope switcher inspired the operator need. This ticket is
**visibility of what kranz already has** (worktree isolation, sandbox tiers,
auth/preflight). It does **not** invent a remote workspace provider or pin
provider/image identity at approval — that is
`workspace-provider-pin-at-approval`.

## Out of scope
- Container / remote VM provider APIs
- Preview URLs / takeover SSH for cloud workspaces
- Approval-time provider version pinning

## Acceptance hints
- Dashboard mission view and `report.md` show isolation mode, mission worktree
  path (when worktree mode), and sandbox enforce setting actually used.
- Preflight/setup failures already in the event log are linked or summarized
  in that panel.
- Local checkout-mode missions still render an honest summary (not
  "worktree: n/a" as an error).
- No new secrets in the event log.
- Anti-vacuity grep on the named filter.
