---
state: done
state-note: Done in the M6 arc: WorkspaceProvider seam (workspace_provider.rs) with workspace.* events; local default provider.
title: WorkspaceProvider seam + local-worktree implementation
priority: 1
schedule: once
blocked-by: [workspace-contract-design]
---

## Goal
Introduce a WorkspaceProvider seam separate from AgentBackend, with a
local-worktree implementation that returns a handle (cwd, env, readiness,
optional preview placeholders) and teardown, and emit additive lifecycle
events. No remote/Coder provider in this ticket.

## Context
Design D-B / D-E in `docs/scoping/workspace-contract.md`. Unblocks
`workspace-provider-pin-at-approval` and `local-container-workspace`.
Sandbox (`sandbox.provider`) remains a different config surface even if
container code is later shared.

## Acceptance hints
- Engine selects provider from config/contract; default local-worktree.
- Provision → readiness → teardown are test-covered; primary checkout
  stays untouched in worktree mode.
- Additive events record provider kind + readiness outcome (no secrets).
- Bootstrap preflight can call through this seam (or migrate onto it).
- Anti-vacuity grep on a named filter unique to this work.
