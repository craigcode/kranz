---
state: open
title: Qualify one pinned ACP adapter using client terminals
priority: 3
schedule: once
blocked-by: [acp-sgian-terminal-qualification]
---

## Goal

Prove the exact adapter actually uses the client terminal provider and preserve containment for operations it performs internally.

## Context

The operator approved this staged stream on 2026-09-24 after v0.4.1. See
[shared ACP scope and decisions D1–D4](../../docs/scoping/shared-acp-client.md).
The review-effort pilot is independent. Follow the dependency-admission rules
linked by the scope; ticket completion alone is not a recorded Complete mission.

## Scoping answers

- Start with one pinned adapter and profile; record protocol, command lifecycle, permission handling and output evidence before adding others.
- Run provider-free bypass probes first. Advertising terminal capability does not guarantee every agent command reaches the provider.
- Any live-provider run needs separate bounded approval specifying credentials, prompt/tool scope, timeouts, retries and spend limitations; this ticket does not authorize it.
- Keep alternate adapters opt-in until they pass the same contract; never silently promote a fallback or bypass required human gates.

## Acceptance hints

- Retain exact adapter/profile versions, observed terminal calls, bypass-test results and cleanup receipts with truthful capability limits.
- Additional adapters are separate proofs; missing telemetry or unobserved execution is not converted into a successful visibility claim.
