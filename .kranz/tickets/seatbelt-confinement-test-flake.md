---
state: done
state-note: investigated 2026-07-25: sandbox-exec itself confines reliably (60/60 in a controlled repro, 30 idle + 30 under cargo-build load) and the instrumented test passed 50/50 consecutive runs — no systemic fail-open, and the flake did not reproduce. Root cause assessment: environment-sensitive test-harness timing (session lifecycle under heavy load), NOT a confinement gap; the tier-3 diff was provably orthogonal. Assertions now carry diagnostic context (inside_exists/outside_exists) so any recurrence  … (truncated)
title: sandbox_wrap_macos_start_confines_spawned_process flakes under load (Seatbelt/sandbox-exec)
priority: 3
schedule: once
---

## Goal
The macOS Seatbelt confinement test
(backend_claude_test::sandbox_wrap::sandbox_wrap_macos_start_confines_spawned_process)
flaked ~2-in-3 locally on 2026-07-24 under a heavy cargo build (runs
hitting its 10s session timeout fail; ~9.9s runs pass) while passing on
CI rust-macos. Determine the real failure mode — timing (the probe write
racing the next_event drain), sandbox-exec deprecation flakiness under
load, or a genuine confinement gap — and pin it: either stabilize the
test (bounded wait for the probe file with a deadline instead of a fixed
session drain) or move the confinement proof to the tier-3 container
boundary and mark the Seatbelt one #[ignore] on macOS with a pointer.

## Context
Verified NOT a tier-3 regression: the tier-3 diff touched only
sandbox_container.rs and its tests; the test passed at 7f0ef53's gate and
intermittently after. sandbox-exec is Apple-deprecated — the same
fragility the tier-3 provider exists to escape. If it turns out to be a
genuine confinement failure (not test timing), it becomes a pri-2:
process sandboxing on macOS would be failing open.

## Acceptance hints
- Root cause named (timing vs sandbox-exec vs real gap) with evidence.
- The test is stable across 50 consecutive runs locally, or deliberately
  retired/replaced with the reason recorded.
