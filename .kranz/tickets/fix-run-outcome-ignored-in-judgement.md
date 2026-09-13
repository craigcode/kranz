---
state: done
state-note: landed in f4ba469 (Craig); reviewed + negative-tested + gates green
title: Worker/validator run failures ignored after a parseable report (gate hole)
priority: 1
schedule: once
---

## Goal
Feature judgement (judge_worker_run, called orchestrator.rs ~1900 sequential
/ ~2652 parallel) receives only the worker-authored report + commits + diff,
NOT outcome.result / exit / denied-count. The runner correctly downgrades
failed/aborted sessions to Fail/Partial (runner.rs ~352), but that verdict
is discarded: an interrupted or budget-aborted run that emitted a "pass"
report before dying can still Complete. Validators are accepted solely by
`if let Some(report)` (orchestrator.rs ~2788): a crashed validator with an
empty report contributes zero findings and GREEN-LIGHTS validation. Thread
outcome.result (and exit/denied count) into judgement; hard-fail or respawn
on a non-pass run outcome regardless of the self-reported report; treat a
failed/empty validator run as a validation failure (respawn), never as
"found nothing".

## Context
Found by a Codex code review 2026-07-07. Trust-critical — same class as
fix-partial-waiver-drops-findings: the gate must not accept a self-reported
"pass" from a run the engine knows failed. The validator half is the worse
half (silent crash = clean validation).

## Acceptance hints
- A worker run with outcome.result != Pass does not Complete on a "pass"
  report — it respawns/fails.
- A validator run that failed/aborted with no report is treated as a
  validation failure (respawn), not an empty (clean) result.
- Tests via the mock backend scripting an aborted/failed run with a stale
  pass report, and a failed validator with no report. cargo test --workspace
  green.
