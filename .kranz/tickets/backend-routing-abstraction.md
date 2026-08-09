---
state: done
state-note: "Done: routing.rs table (taskClassRules -> ExecutorTier, first-match, fail-closed validation, no model ids) + worker.escalated events via the WorkerReport escalation field (record-only; validators/tier/budget untouched); hosted fine-tune = plain local config. routing_abstraction filter: 18 green; full gates green."
title: Backend routing abstraction — local, frontier, fine-tune as peers
priority: 2
schedule: once
---

## Goal
Local endpoints, hosted frontier models, and hosted fine-tunes are peers
behind one routing interface: deterministic task-class routing as the
floor, worker self-escalation to a frontier advisor layered on top — never
replacing the deterministic floor.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-331). Extension of the
existing seam, not a new one: backend.rs (CONTRACT FILE) + per-role model
config carry most of this. Reconciliation, recorded explicitly: the
local-inference tickets (local-inference-backend-local,
-router-escalation, -validator-guarded, -trace-provenance-flywheel,
-cost-accounting) remain live as implementation slices UNDER this
abstraction — their scoping doc's KRZ-20x series folds into this one; do
not duplicate their content here. A hosted fine-tune endpoint is
configuration of the OpenAI-compatible local/hosted backend (base URL +
model), not a new backend kind. Model resolution under a task class uses
capability classes, never hardcoded ids in core
(docs/reviews/local-llm-and-triumvirate.md §1). Every escalation is an
event. The tracked config surface for the routing rules is split out as
routing-rules-config (Warp scan 2026-08-04: their complexity-tier and
ordered-rule router forms are the reference shapes).

## Acceptance hints
- A routing table maps task class → backend role; the floor route is
  deterministic (same inputs → same route, tested).
- A worker self-escalation records an escalation event naming source and
  target routes; escalation cannot bypass the floor's validator
  requirements.
- No hardcoded model ids introduced in core routing (grep-style test).
- Anti-vacuity grep on a named filter unique to this work.
