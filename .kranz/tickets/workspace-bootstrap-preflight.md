---
state: done
state-note: Done in the M6 arc: readiness gate at block lift (workspace_gate.rs), relative-cd checks (eb12f61).
title: Run workspace bootstrap + readiness before first agent turn
priority: 1
schedule: once
blocked-by: [workspace-contract-schema]
---

## Goal
When a workspace contract exists, provision the local workspace cwd, run
bootstrap then readiness checks, and only then start workers/validators.
Failures map to preflight or Blocked with owner operator | provider |
repo-setup — never silent mid-run dependency discovery.

## Context
Highest local impact from the Monaco review: worktrees are "flimsy" without
hooks/venv/pnpm setup. This ticket delivers that without cloud. Design
D-C / D-H in `docs/scoping/workspace-contract.md`. Uses the local-worktree
path until `workspace-provider-seam` lands; may call a thin provisional
helper that the seam later owns.

## Acceptance hints
- With a valid contract, workers do not start until readiness passes.
- Bootstrap/readiness failures are loud, owned, and visible in
  WorkspacePanel / report.md / events.
- Without a contract, behavior unchanged.
- Operator copy distinguishes source isolation from workspace-ready.
- Anti-vacuity grep on a named filter unique to this work.
