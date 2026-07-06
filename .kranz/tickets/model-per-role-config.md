---
title: Generalize role -> (backend, model, effort) selection in config
priority: 3
schedule: once
blocked-by: droid-backend-glm52
---

## Goal

Replace role-hardwired backend selection with one config mapping: each
engine role — worker, functional validator, scrutiny validator (and
eventually the planning orchestrator) — selects (backend, model,
reasoning effort) from config, validated by config::validate rules
that encode the safety floors. Per-mission config overrides work as
they do for every other key. End state (operator direction,
2026-07-06): any task can run on any supported model, with the rails
making the safe combinations the easy ones.

## Context

Step 2 of the multi-vendor lane. Step 1 (droid-backend-glm52) adds the
third backend under the existing scrutiny-only seam; this ticket
generalizes selection without changing any default. The kranz design
premise makes this safe: validation contracts, independent audit,
command allowlists, and sandboxing (M7) treat every agent as an
unreliable contractor — the contractor is therefore swappable; the
rails are not.

Safety floors to encode in config::validate (not new policy — existing
posture made explicit): scrutiny validator may be any supported
backend (cross-vendor is the point); workers below the default tier
require an explicit per-mission opt-in (soak-gated promotion, not
silent default); the planning orchestrator keeps a frontier-model
floor — plan quality gates everything downstream and thin contracts
fail silently. Estimates: the calibration corpus is claude-shaped;
per-model pricing tables and a calibration-confidence penalty for
uncalibrated models must ship with this, or estimates lie.

Verified lanes awaiting this ticket: claude-fable-5 via DroidBackend
(Factory Max billing — a second frontier pool independent of the
Anthropic subscription; probe 2026-07-06) for worker/planner roles.
Scrutiny selection should weight model-family diversity, not just
backend diversity.

Seams: select_scrutiny_backend (generalize to select_backend(role)),
SessionSpec (model/cwd/tools already per-session; claude-isms tagged
during m-5d2c79), cost.rs pricing tables, config.rs layers +
validate.

## Scoping answers

- No default changes this mission: claude remains default for every
  role; this is plumbing + rails only.
- Per-milestone model hints (planner tags a milestone mechanical ->
  cheap tier) are explicitly OUT of scope — separate ticket once
  shape-aware calibration has per-model data.

## Acceptance hints

- Config maps each role to (backend, model, effort); omitted = today's
  defaults byte-for-byte.
- validate rejects: sub-tier worker without per-mission opt-in flag;
  non-frontier orchestrator; unknown backend/model combos. Loud
  fallback decisions when a selected backend is unavailable at spawn.
- cargo test --workspace green; stub-backend tests cover selection per
  role.
