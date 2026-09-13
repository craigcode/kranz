---
state: done
state-note: Done in the outcomes-fold trio commit: per-task-class rows + context-reuse split; cost-per-merged-change (CLI --all + GET /api/cost-per-merged-change, 30d default); rubberStampThresholdMs flag (default 10s, boundary-tested). outcomes_report_ filter: 15 green; full gates green.
title: Flag grants approved under a threshold as rubber-stamp signals
priority: 3
schedule: once
---

## Goal
Grants approved faster than a configurable threshold are flagged as
rubber-stamp signals in the outcomes report — a grant approved in under N
seconds was not reviewed.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-323). Mostly shipped:
the grant-latency distribution already exists in the outcomes.rs fold. This
adds the threshold classification and its surfacing (report row + per-grant
marker). A flag, never an enforcement — the operator judges what to do with
the signal. Threshold is config with a documented default.

## Acceptance hints
- Grants under the threshold are flagged; at/over are not; boundary case
  tested.
- Report shows rubber-stamp rate alongside the existing latency
  distribution.
- Derived at fold time — no new persisted state.
- Anti-vacuity grep on a named filter unique to this work.
