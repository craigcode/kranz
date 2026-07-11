---
title: backend_local — HTTP-in-engine local-inference backend + per-role config
priority: 3
schedule: once
---

## Goal
Add `BackendKind::Local`: an HTTP-in-engine completion backend (OpenAI-
compatible endpoint) plus base-url / context-budget / temperature fields on
the existing per-role config, so any worker/validator role is retargetable to
a local endpoint by config alone. Reuse the shipped per-role backend
machinery — do NOT rebuild the abstraction.

## Context
From docs/scoping/local-inference-executor-tier.md (KRZ-205), reframed per the
review addendum §1–2. This is NOT greenfield: `AgentBackend`
(crates/engine/src/backend.rs) already has four impls and a `BackendKind` enum
with per-role `parse_backend` / `role_default_model` / `model_tier` floors;
per-role backend selection shipped in 7c1b130 through dashboard/Slack/REST +
drain-time re-validation. Local slots in as a new kind + impl + RoleConfig
fields, plus catalog placement (a `model_tier` entry for local models, or an
explicit floor exemption gated by the existing allowBelowDefaultWorkerModel
opt-in).

KEY SHAPE (addendum §2): every existing backend spawns a CLI and streams
stream-json; mistral.rs serves HTTP, so `backend_local` is the FIRST
HTTP-in-engine backend — the engine holds the completion loop instead of
shelling out. This (a) sidesteps the worker sandbox (an HTTP call by the
engine is not the sandboxed subprocess, so no localhost egress-allowlist entry
is needed), and (b) shares machinery with the parked api-only-harness-tier
idea (un-park it as a byproduct: an HTTP, no-CLI, event-log-recorded backend).
Study: backend.rs trait surface (does an HTTP impl fit AgentSession/AgentEvent
cleanly, or does it need a small adapter?), config.rs validate()/model_tier,
sandbox.rs (confirm the engine-side call is outside the sandboxed subprocess),
backend_mock.rs as the simplest impl reference.

Depends on: a proven local endpoint (KRZ-201/203 in the scoping doc). Config
knobs (base_url, contextBudget, temperature) must be honored and validated;
per-role context budgets guard the KV-cache-blowout risk.

## Acceptance hints
- A worker role configured backend=local, base_url=<endpoint> completes a real
  turn via HTTP with no spawned CLI and no sandbox egress change, recorded in
  the event log with model identity (feeds the flywheel ticket).
- config validate() enforces the context budget and floor rules; an
  unreachable endpoint fails the role cleanly, not silently.
- cargo test --workspace passes; a stub/mock endpoint pins the HTTP backend
  path without needing real weights in CI.
