---
state: done
state-note: Done: gate.rs (Gate trait, two-section GatePipeline — model-before-deterministic unrepresentable, verdict-authoritative GateOutcome with optional score), merge-gate suite adapted via MergeSuiteGate. gate_plugin filter: 7 tests green. Full gates green.
title: Gate plugin interface — ordered, typed, registrable gates
priority: 1
schedule: once
---

## Goal
A first-class gate abstraction: gates are ordered, typed, independently
registrable, and each returns pass/fail plus an artefact reference.
Deterministic gates precede model-judged gates by construction — encoded in
the types, not by convention.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-311). Today's gates are
bespoke and layered: contract command assertions (with contract_lint.rs
approval-time linting), scrutiny validators + judgement turns, the
empty-deliverable final gate, merge_gate.rs (the nearest precedent — a
declarative, ordered, base-branch-owned suite), workspace_gate.rs, and
preflight. This ticket defines the trait + registry and adapts ONE existing
gate (the merge-gate suite) through it as proof; wholesale migration is
follow-up work, not this brief. Subsumes the repo-owned-scrutiny-checks
draft (docs/reviews/ampcode.md §2): repo-declared model checks register
through this interface. The merge-gates ownership rule carries over — a
mission cannot weaken or reorder the gates that judge its own diff.
Result-type note (scored-gates addendum, KRZ-315): the result carries an
OPTIONAL gate-supplied confidence score + threshold from day one — verdict
stays authoritative and is never derived from the score; boolean-only
gates omit it with no special-casing. Reserving the field now means no
gate is ever written against a boolean-only shape and reworked later;
persistence/query is gate-confidence-score's slice.

## Acceptance hints
- Ordering is structural: a pipeline placing a model gate ahead of a
  deterministic gate is unrepresentable (type-level) or rejected at
  registration, with a test proving it.
- Every gate outcome carries an artefact reference (consumed by
  gate-results-first-class-events).
- The result type admits an optional score + threshold; a boolean-only
  gate compiles and runs without naming either (test).
- The merge-gate suite runs unchanged through the interface: same commands,
  same order, stop-at-first-failure, base branch untouched (regression).
- Anti-vacuity grep on a named filter unique to this work.
