---
state: done
state-note: D-A…D-H accepted 2026-07-25 with two implementation notes (egress scoping: bootstrap local-worktree-first, local-container gated on the egress proxy or fs-tier-scoped; mount/cache-dir convention to be designed into the schema) and one delivery re-sequence (pin-at-approval ahead of local-container-workspace). Acceptance recorded in docs/scoping/workspace-contract.md status header.
title: Accept M6 workspace contract + provider seam design (D-A…D-H)
priority: 1
schedule: once
---

## Goal
Review and accept (or explicitly defer) the decisions in
`docs/scoping/workspace-contract.md`, then mark that doc status
**accepted** so the blocked-by implementation chain can draft honestly.

## Context
Monaco's agent-developer-workspaces post (2026-07-09) and kranz roadmap
product notes already agree: a worktree is not a workspace; buy substrate;
keep the policy plane. The scoping doc turns that into D-A…D-H plus an
impact-ordered ticket sequence. This ticket is design acceptance only —
no provider implementation.

## Decisions this ticket must close
- D-A contract artifact path + base-branch ownership
- D-B WorkspaceProvider ≠ AgentBackend; implementation order
- D-C bootstrap/readiness before first agent turn
- D-D golden data as validation infra (incl. skew → Blocked)
- D-E previews/takeover as mission artifacts
- D-F triggers → audited missions (no skill-only loops)
- D-G baked images over nested Devcontainers for v1 remote
- D-H operator language: isolation ≠ workspace
- Open questions 1–5 answered or deferred with owners

## Acceptance hints
- `docs/scoping/workspace-contract.md` status line is **accepted** (or
  lists deferred D-X with rationale).
- Roadmap M6 / product-notes backlog list matches the accepted sequence.
- No production provider code required in this ticket.
