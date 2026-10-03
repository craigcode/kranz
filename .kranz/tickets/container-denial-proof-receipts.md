---
state-note: Implemented and reviewed in PR #101; local workspace gates and live Linux file/network denial proofs passed. Production containment and deadlines are unchanged.
state: done
title: Require conclusive execution receipts for container denial tests
priority: 2
schedule: once
---

## Goal

Distinguish a proved container access denial from runtime, startup and supervision failures in legacy negative tests.

## Context

During September 26 release verification, the unchanged container_gate_wrap_runs_contract_command_inside_the_container test encountered a local guest-start anomaly. Its nonzero result satisfied the denial assertion without establishing guest execution. That subprobe is recorded as inconclusive; see [review evidence](../../docs/reviews/2026-09-26-acp-gate-remediation.md).

## Scoping answers

- Audit negative container fixtures for assertions that accept any unsuccessful execution as proof of denied access.
- Require positive guest-start evidence and a specific denied-access receipt; a daemon, launch, command or cleanup timeout must remain a failure or explicit inconclusive result, never a containment pass.
- Preserve production deadlines and immutable-ID owned cleanup; do not solve flaky tests by accepting unavailable runtimes as denial.

## Acceptance hints

- Synthetic launch failure and supervision timeout cases cannot satisfy an access-denial assertion.
- A real denied read has an independently observed guest execution marker, the expected denial result and confirmed owned cleanup; allowed reads still prove the positive control.

## Implementation review

The legacy worker and command-gate file-access probes and the direct-socket bypass fixture now require conclusive guest receipts and confirmed cleanup. Synthetic controls cover launch failure, supervision timeout, unrelated socket errors, unexpected access, missing witnesses, unowned IDs and unavailable or persistent inventory. The local workspace gates, live Linux file and network proofs, and five-axis self-review passed; PR #101 records the implementation and required merge checks. See [fixture review](../../docs/reviews/2026-09-30-container-denial-receipts.md).
