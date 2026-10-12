---
title: Stop copying mission state by value on the hosted stack
priority: 3
schedule: once
---

## Goal
Make hosted mission stack use independent of MissionConfig/MissionState size, so config growth stops costing ~200x its bytes in stack.

## Context

`hosted-mission-stack-headroom` moved 8 KiB stream read buffers to the heap and boxed the large stage futures in `run_loop`. `hosted_lifecycle_reaches_complete_without_a_terminal` (`crates/server/tests/host_test.rs`) now passes at 709 KiB of its 1 MiB arm64 macOS budget.

A synthetic 512-byte field on `MissionConfig` raised that floor to about 818 KiB: roughly 110 KiB of stack for 512 bytes of config. Future sizes from `-Zprint-type-sizes` no longer explain that, so the cost is most likely debug-build frames that hold `MissionConfig` or `MissionState` (which embeds it) by value: clones such as `self.state.config.clone()`, reducer folds that return a fresh `MissionState`, and temporaries passed by value. The exact sites have not been located.

## Scoping answers

- Locate first. Grow `MissionConfig` with a temporary padding field and bisect the worker stack with `KRANZ_HOSTED_STACK_KIB`; narrow the cost by boxing or borrowing candidate sites one at a time.
- Prefer borrowing or `Arc<MissionConfig>` at the owning boundary over boxing individual call sites. Keep `MissionConfig` and `MissionState` serialization identical; `types.rs` is a contract file (additive only).
- Do not raise the test's stack budget; it is the regression check.

## Acceptance hints

- With a temporary 512-byte `MissionConfig` padding field, the hosted lifecycle floor rises by well under 110 KiB; record the new numbers in the test comment.
- The unpadded floor does not regress from 709 KiB.
- Full workspace gates pass on macOS, Linux and Windows.
