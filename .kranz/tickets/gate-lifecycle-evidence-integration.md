---
state: open
title: Gate lifecycle — bind every stage to evidence, authority and replay
priority: 1
schedule: once
blocked-by: [gate-evaluation-contract-v1, gate-subprocess-evaluator]
---

## Goal

Connect the shared evaluation envelope to approval, milestone validation,
final checks and merge, with permission-stage joins supplied by S4, without
changing existing consent or enforcement behavior.

## Context

S5 of docs/scoping/acp-worker-gate-contract.md. gate.result currently records
Approval/FinalGate; other stages already have their own events. Reuse them
through additive joins instead of replacing the state machine.

## Scope

- Build restricted immutable evaluator inputs from scope, criteria, diff/tree,
  environment-labelled test receipts and prior independent findings.
- Pin policy and exact judged content; merge binds the scratch integration
  tree and live base. Reject stale approvals and checker/policy drift.
- Adapt existing stage drivers to the approved lifecycle and keep deterministic
  checks before model judgement. Preserve blocking/advisory and waiver rules.
- Record request, attempt, result, engine/human resolution and consumption;
  join legacy records without changing their meanings.
- Extend provenance, evidence export and pending-decision views; replay never
  runs checks or sends a permission response.

## Acceptance hints

- All stage subjects are distinguishable; final is not relabelled as merge.
- An old fixture log folds unchanged; a new log reconstructs exact evidence,
  evaluator identity, policy, decisions and remaining human obligations.
- A model validator cannot read worker reasoning/transcripts via inputs,
  snapshot files, native session state or shared repository metadata.
- Dirty/untracked inputs where permitted are fingerprinted; a changed candidate
  or live integration tree invalidates earlier consumption.
- Gate error/escalation never becomes a fabricated pass; no plugin can
  replace required human consent or waive nonwaivable floors.
- Missing retained artifacts are unresolved, not silently lost; export hashes
  match retained redacted bytes. Full workspace gates pass.

## Out of scope

A second evidence exporter, automatic policy weakening, domain knowledge and
mandatory conversion of every internal Gate implementation.
