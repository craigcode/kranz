---
state: open
title: Qualify ACP resource budgets before Sgian terminal rollout
priority: 2
schedule: once
---

## Goal

Define and prove resource limits for released ACP workers and future terminal-provider sessions.

## Context

Current containers have no explicit memory or CPU ceilings. The contained terminal provider remains a synthetic fixture with bounded operation/request limits, not a released general-purpose session. See [September 26 review](../../docs/reviews/2026-09-26-acp-gate-remediation.md).
Scope, gaps and proposed D-R1…D-R7 decisions: [ACP resource budgets](../../docs/scoping/acp-resource-budgets.md).
R1 provides the fixture-only profile contract; [R1 review](../../docs/reviews/2026-10-04-acp-resource-contract.md). R2 adds test-only failure observations, completion receipts, tmpfs caps and a session ceiling; [R2 review](../../docs/reviews/2026-10-06-acp-resource-failures.md). R3 preparation adds protected usage accounting, descriptor/file-size proofs and a separately authorized measurement fixture; [R3 preparation review](../../docs/reviews/2026-10-06-acp-resource-qualification-preparation.md). This ticket stays open for live measurements, production qualification and terminal budgets.

## Scoping answers

- Design explicit memory, CPU and process ceilings that support the qualified adapters and declared toolchains; do not silently change a pinned profile's runtime contract.
- Separate RPC supervision from command lifetime and classify protocol requests separately from consent counts for a production terminal provider.
- Qualify long-running builds, delayed waits, output backpressure, limit exhaustion, cancellation and owned cleanup, retaining fail-closed behavior and actionable receipts.
- Do not enable terminal capability on a released adapter profile until the boundary and budgets have independent review and concrete proof.

## Acceptance hints

- An approved profile revision states defaults, override authority and operator-visible failure behavior.
- Positive and adversarial synthetic fixtures prove useful work within limits and containment after exhaustion; provider qualification remains separately authorized.
