---
state: done
state-note: c2e7f3b: POST /api/hooks/github with HMAC-SHA256 verification (refuses closed without a configured secret), event allowlist (CI workflow_run failure + kanz:fix/kanz:fix-and-queue comments), slug-deterministic dedup, provenance-carrying ticket drafts through the EXISTING draft path — plan approval never skipped; the queue label pre-consents only the queue step. No push/merge paths anywhere (source-scan tests assert). 21 engine + 9 server tests, workspace 1743 green. Dashboard UI surfacing of the  … (truncated)
title: CI failure / PR comment triggers → audited fix missions
priority: 3
schedule: once
blocked-by: [workspace-provider-seam, post-complete-pr-handoff-no-push]
---

## Goal
Accept GitHub (and optionally Linear) webhook/label events for CI failures
or review comments and open an audited fix-feature or follow-up mission /
ticket draft — preserving spend gates and never silently merging or
pushing. Replaces skill-only "watch and address" loops as the product path.

## Context
Monaco Monacoder AFK quality loop; design D-F and roadmap automation-trigger
notes. Builds on `post-complete-pr-handoff-no-push` without adding push.

## Acceptance hints
- Documented webhook/label → ticket or mission path with event-log records
  (trigger source, actor, consent state).
- Cannot merge or push from the trigger path.
- Operator can approve/deny spend; unauthenticated webhooks refuse closed.
- Anti-vacuity grep on a named filter unique to this work.
