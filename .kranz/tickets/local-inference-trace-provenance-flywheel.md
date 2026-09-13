---
state: done
title: Trace provenance + weight-hash pinning + validated-trace export (flywheel, frontier-first)
priority: 2
schedule: once
---

## Goal
Extend completion/run events so every model turn records model ID, quant (or
"n/a" for frontier), and its validation outcome, and add a derived export path
that yields a fine-tuning-ready dataset of validation-PASSED traces
(instruction-pair format) queried from the event store. Ship this on FRONTIER
traces first — it pays off with zero local-inference dependency: it sharpens
the audit/record pillar and produces a standalone dataset asset regardless of
whether local models or fine-tuning are ever adopted.

## Context
From docs/scoping/local-inference-executor-tier.md (KRZ-207), pulled to the
front per the review addendum §3: this is the highest-leverage, lowest-risk,
most mission-independent item in the workstream. Depends on none of the
GPU/mistral.rs stages. Study: the WorkerCompleted / run event shapes and
prompt_hash in events.rs; cost.rs (already records model/tokens/cost);
reducer fold; how report.md is derived-and-regenerable from the log (mirror
that — the export must be regenerable, never a second source of truth).
Constraint: for local models the provenance MUST record a content hash of the
weight file + quant, not just a model name — otherwise the "version-pinned by
definition" sovereignty claim is unauditable (qwen3-coder-30b-q4 can point at
different GGUFs). Frontier turns record the model id/alias as today.

## Acceptance hints
- A completed mission's events carry model identity + validation outcome on
  every completion turn; a query over the event store yields only
  validation-passed traces in instruction-pair form, regenerable from the log.
- Weight-hash field is present and populated for a local turn (stub/fixture is
  fine until backend_local lands) and absent-or-alias for frontier.
- cargo test --workspace passes with pins for the event shape and the export
  filter (passed-only, regenerable).
