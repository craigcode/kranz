---
state: done
state-note: complete: autonomy ratio + grant latency + escalation ledger shipped earlier via m-d1e3c3 (flight surgeon); f5fe09d adds the remaining two lagging metrics — cost per change (Σ costUsd / non-meta commits, meta classified by subject from events only, local tier $0) and cycle time (created→terminal minus paused spans) — folded single-source into outcomes.rs and projected to CLI, Slack card, dashboard panel, and REST. Byte-identical regeneration holds (pure fold + memoization). Per-mission --mission … (truncated)
title: Outcomes view — autonomy ratio, cost per change, and cycle time computed from event logs
priority: 3
schedule: once
---

## Goal
Add an outcomes report (CLI first) that computes the whitepaper's lagging
metrics as pure, regenerable functions of mission event logs: **autonomy
ratio** (share of execution done without human intervention — worker/
validator turns vs operator interventions: msgs, grant decisions, revision
decisions, unblocks), **cost per change** (costUsd per merged non-meta
commit), and **cycle time** (mission.created → terminal, minus paused spans
— reuse the dashboard's pause-accounting rule, not a new one).

## Context
The AMM paper pairs its readiness leading indicator with business-outcome
lagging metrics; kranz's event logs already carry everything needed
(worker.completed costUsd, control commands, grant/revision events, pause
spans, commit trailers). trace_export.rs is the precedent for a pure
log→artifact function (regenerable, no second source of truth). This also
feeds the LoRA-corpus argument: validation-passed traces gain cost/outcome
provenance, making the export a measurable asset rather than a byproduct.

## Acceptance hints
- `kranz outcomes [--mission <id>] [--json]` prints per-mission and
  aggregate autonomy ratio, cost per change, cycle time; byte-identical on
  regeneration from the same log.
- Operator interventions are counted from events (control commands drained,
  grant/revision decisions), not inferred from gaps.
- Aggregates cover a mission set (all missions, or filtered by ticket);
  unreadable/corrupt logs degrade per-row with a reason.
- cargo test --workspace green with seeded-log fixtures for each metric
  (including a paused mission and a grant-heavy mission).
