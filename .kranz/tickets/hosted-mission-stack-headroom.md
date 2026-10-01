---
state: open
title: Restore stack headroom in hosted mission futures
priority: 2
schedule: once
---

## Goal

Give the hosted REST mission lifecycle real headroom under its 1 MiB arm64 macOS stack budget, so an ordinary additive config field cannot overflow it.

## Context

`hosted_lifecycle_reaches_complete_without_a_terminal` (`crates/server/tests/host_test.rs`) runs the full REST lifecycle on a Tokio runtime with a 1 MiB worker stack on arm64 macOS (2 MiB elsewhere). That budget exists because an earlier overflow was fixed by heap-pinning `run_feature` at the mission loop (91ba68b0).

While building ACP resource ceilings (R1, PR #94), adding an 8-byte optional field to `AcpWorkerProfile` on a clean `origin/main` reproduced the overflow: `thread 'tokio-rt-worker' has overflowed its stack`. `MissionConfig` embeds an `Option<AcpWorkerProfile>` in each `RoleConfig`, so a few bytes per role were enough. PR #94 works around it by boxing `RoleConfig.acp_profile` (serialization-identical), which leaves `MissionConfig` at 1672 bytes, smaller than before. The margin is still unknown, and the next config field anyone adds may hit the same wall.

## Scoping answers

- Measure first: find the hosted-path futures whose state holds `MissionConfig` (or other large values) by value across awaits. A debugger needs macOS Developer Tools access; prefer compiler type-size output (`-Zprint-type-sizes` on nightly) or targeted `size_of_val` probes on the futures.
- Fix at the owning boundary the same way 91ba68b0 did: `Box::pin` the large future, or hold `Arc<MissionConfig>` instead of clones across awaits. Do not raise the test's stack budget; it is the regression check.
- Record the measured headroom (for example, the smallest stack that passes) in the test comment so the next change can see how close it is.

## Acceptance hints

- The hosted lifecycle test passes at 1 MiB with a documented margin, for example still passing at 768 KiB.
- A synthetic check that grows `MissionConfig` by a few hundred bytes no longer overflows.
- Full workspace gates pass on macOS, Linux and Windows.
