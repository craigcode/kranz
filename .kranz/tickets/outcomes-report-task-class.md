---
state: done
state-note: Done in the outcomes-fold trio commit: per-task-class rows + context-reuse split; cost-per-merged-change (CLI --all + GET /api/cost-per-merged-change, 30d default); rubberStampThresholdMs flag (default 10s, boundary-tested). outcomes_report_ filter: 15 green; full gates green.
title: Outcomes report — cost, cycle time, escalation rate per task class
priority: 2
schedule: once
---

## Goal
Extend the outcomes fold with cost per change, cycle time,
escalation/advisor-invocation rate broken down per task class, and the
context-reuse split — fresh vs cache-read vs cache-write input tokens per
mission/backend where the backend reports them — rendered as an operator
report (CLI + dashboard endpoint), with AMM-compatible output where that
is cheap.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-321). This is an
extension, not a build: outcomes.rs already folds autonomy ratio,
grant-latency distribution, and the escalation ledger from the event log —
pure-fold style, no second persisted source of truth (house pattern; keep
it). `task-class` exists in ticket frontmatter (executor-tier routing).
AMM projection precedent is crates/cli/src/amm.rs: mapped, never adopted —
kranz-native signals stay the source of truth. Context-reuse rationale
(Nate Jones token-burn analysis, Jul 2026): reuse shares above ~95% are a
real cost pattern that per-mission cost totals hide; the split is a signal
to investigate carried context, not a target to optimize. Claude
stream-json already reports cache token fields; other backends may not.

## Acceptance hints
- Fold-only: the same log always yields byte-identical report data.
- Per-task-class rows for cost/cycle/escalation; missions without a task
  class group under an explicit "unclassified" row.
- Missing data renders as absent, never zero-filled (house rule: no
  fabricated numbers); a backend that reports no cache fields yields an
  absent reuse split, never a 0% one.
- Anti-vacuity grep on a named filter unique to this work.
