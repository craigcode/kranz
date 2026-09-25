---
state: open
title: ACP terminal provider contract and contained fake
priority: 2
schedule: once
blocked-by: [shared-acp-client-extraction]
---

## Goal

Define and prove the five ACP terminal operations in a contained fake provider before connecting a real terminal desk.

## Context

The operator approved this staged stream on 2026-09-24 after v0.4.1. See
[shared ACP scope and decisions D1–D4](../../docs/scoping/shared-acp-client.md).
The next implementation is specified in the
[terminal-provider contract](../../docs/scoping/acp-terminal-provider.md); its
authority, concurrency and contained-fixture proofs remain outstanding.
The review-effort pilot is independent. Follow the dependency-admission rules
linked by the scope; ticket completion alone is not a recorded Complete mission.

## Scoping answers

- Implement create, output, wait_for_exit, kill and release behind an admitted provider; terminal capability is false when no provider is configured.
- Bind opaque terminal handles to run, session and provider generation; reject stale and cross-session handles, path and symlink escapes, disallowed environment and oversized output.
- Require authority for the exact executable action at the provider; an agent may call terminal/create without first requesting permission.
- Keep the protocol reader live during pending terminal and human permission requests; bound cancellation, drain, release and cleanup with retained receipts.
- Execute inside the qualified worker namespace. Never expose a broad host socket or fall back to host RunProcess. A new execution profile requires explicit qualification.

## Acceptance hints

- Adversarial tests cover forged/stale/cross-session handles, capability omission, missing authority, output limits, dead peers, outstanding permissions and orphan cleanup.
- Separate cooperative cancel, process kill and release/invalidation. No PTY or keyboard-input guarantee is inferred from ACP terminals.
