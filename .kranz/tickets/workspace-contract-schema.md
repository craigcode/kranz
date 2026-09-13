---
state: done
state-note: Done in the M6 arc: .kranz/workspace.json schema (both absolute forms), validated at draft/approve (workspace_contract.rs); CI green.
title: Tracked workspace contract schema (.kranz/workspace.json)
priority: 1
schedule: once
blocked-by: [workspace-contract-design]
---

## Goal
Land an additive, versioned workspace contract schema on the base branch
(proposed `.kranz/workspace.json`) covering bootstrap, services, readiness,
optional data hooks, previews, and secret *names* — parseable and
validatable at draft/approve without requiring a non-local provider.

## Context
Design: `docs/scoping/workspace-contract.md` D-A / D-H. Missing contract
must preserve today's worktree-only behavior. Invalid contract fails
closed. Mission branches must not weaken the base-branch contract (mirror
merge-gates ownership). Command hooks (bootstrap/readiness/data) carry
bounded timeouts, explicit or schema-defaulted, so a hung hook fails loud
at preflight rather than stalling provision (Amp orb resume-hook precedent,
docs/reviews/ampcode.md §8).

## Acceptance hints
- Schema + parse/validate in engine; `#[serde(default)]` additive fields.
- Hook entries accept an optional timeout; absent → documented default; a
  hook exceeding it fails preflight naming the hook, never hangs provision.
- Approve/draft path refuses invalid contracts with a clear repo-setup error.
- Missions with no contract file behave as today.
- Dashboard/report can show "no workspace contract" vs "contract present"
  (full readiness UI may wait for bootstrap ticket).
- Anti-vacuity grep on a named filter unique to this work.
