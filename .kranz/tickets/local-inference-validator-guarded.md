---
title: Local validator for mechanical checks only, frontier-confirmed on judgment
priority: 4
schedule: once
---

## Goal
Allow the validator to run local for DETERMINISTIC mechanical checks
(compile/test/lint exit codes, contract-command pass/fail) while keeping
scrutiny and any judgment verdict on the frontier tier. A local validator PASS
on a contract command must get a frontier confirmation regardless of the
frontier spot-check sampling rate. This is the guarded, separate half of tier
routing — split out of the executor-routing ticket on purpose.

## Context
From docs/scoping/local-inference-executor-tier.md (KRZ-206b), gated per the
review addendum §4. The "no silent green" promise rests entirely on the
validator. The executor-escalation valve (two fails → frontier) catches
executor FAILURES but not validator MISSES: a weak local validator that
wrongly PASSES bad work is not a failure, so escalation never fires, and the
10–20% frontier spot-check leaves 80–90% of local-validator verdicts trusted.
Acceptable for deterministic checks (exit codes barely need a model);
unacceptable for judgment. Study: the scrutiny vs functional validator split
(validator_scrutiny / validator_functional in config.rs), the contract-command
final gate (orchestrator.rs), how a validator verdict becomes a pass/finding,
the spot-check sampling open question (start 10–20%, tune against measured
miss rate). Do NOT start before the local-validator miss rate can be measured
against frontier ground truth (the flywheel's validation-outcome provenance
provides it).

## Acceptance hints
- Mechanical (exit-code) validation can run local; scrutiny stays frontier and
  cannot be configured local by this change.
- Every local-validator PASS on a contract-command assertion triggers a
  frontier confirm before the gate is allowed to go green; a disagreement is
  recorded and fails closed to frontier.
- Local-validator miss rate vs frontier is measurable from the event store.
- cargo test --workspace passes with pins for the confirm-on-pass path and the
  scrutiny-stays-frontier guard.
