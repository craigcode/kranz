---
title: Expose per-role backend selection in the config UI (dashboard modal + Slack)
priority: 2
schedule: once
---

## Goal
model-per-role-config (8892175) landed the engine config field + validate()
floors for per-role (backend, model, effort), but NO surface exposes the
backend choice — the /kranz config modal and Slack config only offer
role/model/effort. So choosing the "Codex lane" (or droid/Fable) requires
hand-editing .kranz/config.json + a binary rebuild. Wire backend selection
into the config UI so an operator can pick a role's lane from the dashboard
modal and Slack, with the validate() safety floors surfaced as inline
errors (below-default worker needs the opt-in; orchestrator frontier floor;
unknown combos rejected). Per-mission override, not just global.

## Context
Operator ask 2026-07-07: "how would I test a ticket on the Codex lane from
Slack or web UI?" — today you can't select the lane from a surface, only
the file. This is the UI half of the multi-lane unlock; the engine/config
half already shipped. Study: the existing config modal (role/model/effort
pickers), crates/slack config handlers, crates/engine/src/config.rs
parse_backend/model_tier/validate, types.rs BackendKind. Also needed for a
clean first live proof: a mission run with worker.backend=codex/droid has
never executed end-to-end — pair this with a live-verify runbook.

## Acceptance hints
- Dashboard config modal + Slack config let an operator set a role's
  backend (claude|codex|droid) and model; invalid selections are rejected
  with the validate() floor message inline.
- Per-mission override supported; omitted == today's all-claude default.
- cd apps/dashboard && npx tsc --noEmit && npm run test && npm run build,
  and cargo test --workspace, all pass.
