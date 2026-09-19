---
state: open
state-note: Owned Docker preflight implementation and local proofs are on PR 69; keep open until the new Linux Docker CI proof passes. See docs/reviews/2026-09-19-mount-helper-cleanup.md.
title: Container mount proof — own and confirm helper cleanup on timeout
priority: 1
schedule: once
---

## Goal

Make container bind-mount preflight own its helper container through timeout, cancellation and failure, with bounded cleanup and explicit evidence when daemon absence cannot be confirmed.

## Context

The provider-free concurrent ACP qualification recorded in docs/reviews/2026-09-19-acp-concurrent-cleanup.md hit the existing 90-second mount-proof deadline before ACP initialization. The Docker helper remained running after the host command timed out; an explicit removal by its inspected ID succeeded. The helper runs only the trusted sentinel script, with no vendor credentials or model calls. This blocks closing S6 containment qualification in docs/scoping/acp-worker-gate-contract.md.

## Scoping answers

- Inspect sandbox_container.rs run_mount_proof and reuse existing bounded Docker control and ownership utilities where they apply; preserve supported-runtime behavior or refuse unsupported cleanup before spawn.
- Give each mount-proof helper an immutable ownership identity and remove only its inspected container ID. A name collision or replacement must never redirect removal.
- Treat host command timeout, interrupted creation, cancellation and engine death as distinct cases; retain recovery information whenever late creation or daemon absence is uncertain.
- Keep the existing sentinel round-trip, private environment, mount restrictions and fail-closed admission. Never convert a timed-out mount proof into a pass merely because cleanup succeeds.
- Out of scope: Raising the 90-second mount deadline, relaxing mount proof or sandbox admission, provider authentication, broad daemon pruning or changing ordinary mission defaults.

## Acceptance hints

- A deterministic blocked sentinel helper hits a bounded failure and is absent from a successful daemon inventory after cleanup; no provider call occurs.
- A failed or unavailable cleanup query remains an explicit unconfirmed outcome with actionable recovery evidence.
- Interrupted creation and engine death cannot silently leave a helper behind; any unsupported recovery guarantee is explicit and blocks the affected admission path.
- A normal sentinel round-trip still proves both directions and removes the helper; unrelated containers survive name reuse and parallel probes.
- Real Docker tests exercise success, failure and concurrency on the supported hosts; full workspace gates pass before the S6 dependency is cleared.
