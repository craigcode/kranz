---
title: Decompose complex goals into a ticket DAG (plan → multiple missions over blocked-by edges)
priority: 3
schedule: once
---

## Goal
Add a decomposition step that takes a complex goal and produces several
tickets linked by `blocked-by` edges — reusing the existing dependency
machinery (deps.rs satisfaction, cycle detection, work-time skip-on-failed-
blocker) instead of inventing new orchestration. The drain then executes the
DAG in dependency order under the existing claim protocol. This is the AMM
L5 shape — automatic decomposition into a directed acyclic graph of subtasks
— mapped to kranz's native units (tickets and missions, not fattening one
mission).

## Context
The whitepaper's L5 feature is auto-decomposition of complex requirements
into a subtask DAG. Kranz decomposes within one mission today (milestones/
features); across missions it has tickets with `blocked-by` but no producer
of those edges. The meta-review's framing is the sharp one: decompose across
missions, reusing blocked-by, so each node gets its own plan approval,
pinned base, contract, and report — the mission stays the atom, the DAG is
the molecule. Open design question to resolve in the plan: per-node plan
approval (each node drafts and parks for review) vs one up-front approval of
the whole decomposition (operator reviews the DAG once).

## Acceptance hints
- `kranz decompose <goal>` (or a draft-mode flag) emits N tickets with a
  valid blocked-by DAG, cycle-checked at write time (cycles refused loudly).
- Each node is an ordinary ticket: draftable, queueable, and gated by
  deps.rs so a node never runs before its blockers Complete; a failed
  blocker skips dependents with a warning (existing semantics, no changes).
- End-to-end dogfood: a real multi-part requirement decomposed, drained to
  completion in order, with the DAG visible in the pipeline view.
- cargo test --workspace green: DAG validity, cycle refusal, order-of-
  execution over the existing queue/drain harness.
