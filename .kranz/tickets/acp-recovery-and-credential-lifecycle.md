---
state: open
title: Qualify ACP crash recovery and credential renewal
priority: 2
schedule: once
---

## Goal

Close the remaining ACP operational recovery gaps without broad cleanup or trusting worker-authored credentials.

## Context

Follow-up to the September 26 review of public a5442c55. Normal cleanup is owned and bounded, but SIGKILL between create and start can leave a never-started container. Codex refresh-token rotation in its private copied home can supersede the operator-selected source login. Current behavior retains recovery evidence and requires operator reconciliation or renewed login. See [containment](../../docs/acp-containment.md) and [review dispositions](../../docs/reviews/2026-09-26-acp-gate-remediation.md).

## Scoping answers

- Design an explicit recovery operation against the recorded Docker endpoint, immutable owner label and inspected full IDs; require proof that the owner is dead and recheck inventory after cleanup, including delayed creates. Never prune broadly or remove by name.
- Design credential renewal or a trusted broker without copying worker-writable auth.json back to the operator home. Preserve file-only authentication and prohibit automatic Keychain, browser or login fallback.
- Treat concurrent sessions, refresh rotation, stale ledgers, changed daemon contexts, engine death before start and unavailable inventory as adversarial cases.
- Keep existing profiles and their documented recovery limits until a replacement is reviewed and qualified. Any live-provider check needs its own bounded authorization.

## Acceptance hints

- An operator-reviewed design identifies ownership, concurrency and migration rules before implementation.
- Synthetic crash and token-rotation fixtures demonstrate recovery, honest uncertainty and noninterference with unrelated containers or host credentials.
