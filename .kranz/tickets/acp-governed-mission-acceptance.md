---
state: open
state-note: Qualified profiles and synthetic failure/repair coverage are implemented. Bounded live Claude and Codex missions passed through one-call permission, checks, exact-tree local merge and export on macOS ARM64/Colima, with scripted controller/reviewers. The separately authorized Claude pass retains the earlier failed fixture attempt. Local workspace gates pass (3063 tests) and runner revision 0627086 passed CI; evidence-commit CI and independent review of the branch and combined S7 evidence remain. See docs/reviews/2026-09-20-acp-governed-live-preparation.md.
title: Governed ACP acceptance — prove consent, independent gates and portable evidence
priority: 2
schedule: once
blocked-by: [acp-live-permission-consent, gate-lifecycle-evidence-integration, acp-worker-containment-proof]
---

## Goal

Prove the integrated governance workflow on a small synthetic repository and
publish a reproducible, redacted acceptance record.

## Context

S7 of docs/scoping/acp-worker-gate-contract.md. This closes the new integration
scope, not the already-completed ACP/gate foundation tickets.

## Scoping answers

- Approve a scoped plan, dispatch a contained ACP worker, answer a one-call permission, and run a generic external gate from a synthetic pack.
- Seed a defect that deterministic/independent validation catches; repair it and judge the exact scratch integration tree before local merge.
- Exercise request/decision/send crash windows, expired/stale clicks, peer death, plugin failure and policy drift.
- Export a self-contained audit record showing authorization, actual changes, checks, actors, unavailable telemetry and remaining human obligations.
- Run fixtures first; prepare exact versions, workload and budget for an operator-authorized live pass with each supported adapter.

- Out of scope: Automatic default promotion, paid runs without explicit scope/budget, private consumer-pack implementation, remote hosting and public publishing.

## Acceptance hints

- Nonempty feature delivery is required; no push occurs and the primary checkout remains untouched throughout worker and validation execution.
- One-time permission cannot become a permanent/session/mission grant.
- No script/model can supply human consent or pass by omitting a verdict.
- Replay survives cleaned runtime artifacts with explicit unresolved evidence; it never re-executes an effect.
- Receipts distinguish mock/live, tested/unsupported platforms and version pins.
- Private-pack compatibility is shown with generic synthetic content; no consumer identifiers or knowledge enter core, tickets or fixtures.
- Full workspace gates pass; changed dashboard code runs all dashboard gates.
