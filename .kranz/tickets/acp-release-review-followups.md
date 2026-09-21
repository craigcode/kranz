---
state: open
title: ACP release follow-ups — cancellation scope and operational boundary diagnostics
priority: 2
schedule: once
blocked-by: [acp-worker-containment-proof, acp-live-permission-consent]
---

## Goal

Resolve the remaining low-severity observations from the v0.3.0 release review
with reproducible evidence and accurate operational guidance.

## Context

Schedule after the v0.3.0 release corrections described in
[the correction review](../../docs/reviews/2026-09-21-v030-release-corrections.md).
These are reported observations awaiting focused reproduction, not newly verified
release blockers. The release correction itself handles launch-file credential
retention, nested snapshot credentials, worker creation time and stage expiry.

## Scoping answers

- Reproduce whether an unanswered permission in one candidate cancels sibling candidates through shared notification, and which non-permission controls cancel a running ACP worker. Match cancellation to intended mission authority; document intentional mission-wide effects and isolate accidental cross-candidate effects.
- Reproduce qualified-profile refusal when a mission has an egress grant; preserve exact-profile admission and make the diagnostic identify the grant/profile mismatch without exposing sensitive data.
- Inventory read-only operator toolchain cache mounts and egress relay access from the default Docker bridge. Establish supported trust assumptions with negative probes; propose a separate scope if a boundary correction would change the qualified profile.
- Determine the minimum supported Docker daemon behavior for DNS isolation, including the reported pre-25.0.5 embedded-resolver gap. Refuse or clearly exclude unproven combinations rather than claiming a proof that was not run.
- Make optional Docker tests distinguish an installed CLI from an available daemon. Explicit KRANZ_ACP_CONTAINER_TESTS, KRANZ_GATE_CONTAINER_TESTS and KRANZ_MOUNT_CONTAINER_TESTS proof requests must still fail if the daemon is unavailable; never convert mandatory proof into a skip.
- Out of scope: new ACP filesystem/terminal services, provider credential acquisition, a sandbox rewrite, automatic permission grants or unapproved live-provider calls.

## Acceptance hints

- Each reported observation has a reproduction or refutation with the tested source, daemon/runtime and command recorded.
- Candidate cancellation and operator control behavior have focused regressions showing which sessions stop and which remain live.
- Profile refusals explain the relevant grant boundary while leaving existing admission restrictions intact.
- Cache, relay and DNS findings become tested boundaries or explicit unsupported cases; no unproven runtime is promoted by documentation alone.
- Optional daemon-unavailable tests emit a visible skip marker, and explicitly enabled containment proofs fail closed in the same situation.
- Full workspace gates pass for resulting code changes; material profile changes receive their own review and qualification scope before implementation.
