---
title: Local container workspace provider (ports, network, compose)
priority: 2
schedule: once
blocked-by: [workspace-provider-seam, workspace-contract-schema]
---

## Goal
Add a local-container WorkspaceProvider that gives a mission (or feature)
an isolated runnable environment: network namespace / compose project,
dynamic ports, service health from the workspace contract — addressing
host port clashes and Docker contention that bare worktrees cannot solve.

## Context
Monaco's local failure mode #1 after tmux UX. Distinct from
`tier3-container-sandbox` (process blast radius for agent CLIs). Prefer
sharing container runtime helpers with Tier-3, but expose workspace
semantics (services, previews, readiness). Design D-B / D-G.

## Acceptance hints
- Config can select `workspace.provider: container` (name per accepted
  design); unsupported hosts fail closed.
- Two parallel missions/features do not collide on declared service ports.
- Contract readiness/health gates apply inside the container network.
- Teardown removes compose/network resources; disk prune hooks honored
  when declared.
- Primary checkout remains byte-untouched.
- Anti-vacuity grep on a named filter unique to this work.
