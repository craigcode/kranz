---
title: Golden data clone / migrate / reset / skew hooks for workspaces
priority: 2
schedule: once
blocked-by: [workspace-contract-schema, workspace-provider-seam]
---

## Goal
Implement optional workspace-contract `data` hooks (clone, migrate, reset,
skewCheck) so DB-backed repos can provision a de-identified golden dataset
into the workspace before agents run; migration/version skew fails to
Blocked with a clear migrate/reset owner, not validator flake.

## Context
Monaco's strongest efficacy claim; roadmap: "seeded data is validation
infrastructure." Design D-D. Repos without a `data` block are unaffected.
Secret values stay out of the contract and event log.

## Acceptance hints
- Contract `data` block parsed; hooks invoked in provision/readiness order.
- Skew detection parks/Blocks with actionable message; success path resets
  between validation rounds when configured.
- No golden data values committed by this ticket (fixture/stub OK in tests).
- Anti-vacuity grep on a named filter unique to this work.
