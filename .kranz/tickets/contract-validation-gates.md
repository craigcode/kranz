---
title: Contract-validation gates — formalize the contract-defect class
priority: 1
schedule: once
---

## Goal
The contract-authoring defect class — vacuous filters, wrong polarity,
assertions that pass against the untouched base tree, environment-sensitive
checks — becomes a set of named, deterministic gates at approve and merge
time, each failing with the class named.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-327) — the wedge: an
evidenced differentiator (the positioning ADR names it) absent from
competing maturity framings. The escapes are documented in-repo: the
m-66aff8 a3 vacuous-filter incident (AGENTS.md anti-vacuity rule), the
SUSPECT classes contract_lint.rs already detects at approval, and
contract_health.rs tracking. Related tickets:
contract-smoke-test-at-approval, escalate-contract-authoring-bugs. This
ticket graduates advisory SUSPECT warnings into typed gate verdicts —
registering through the gate plugin interface once it lands (soft
dependency; do not block on it).

## Acceptance hints
- Each known escape class has a fixture that the corresponding named gate
  catches (vacuous filter, polarity, passes-on-base, env-sensitive).
- Well-formed contracts pass all gates unchanged (regression on existing
  fixtures).
- Gate verdicts carry the defect-class name into events/report.
- Anti-vacuity grep on a named filter unique to this work.
