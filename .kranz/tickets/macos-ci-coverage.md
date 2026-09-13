---
state: done
title: Add a macOS CI job covering the Seatbelt and process-group code paths
priority: 3
schedule: once
---

## Goal
Add a `macos-latest` job to `.github/workflows/ci.yml` so the platform kranz
ships a binary for (and the primary dev platform) is actually tested in CI:
`cargo test --workspace` (clippy/fmt stay ubuntu-only to save runner
minutes), which exercises the macOS Seatbelt sandbox tests, lock-liveness,
and unix process-group kill paths that today only run on the developer's
machine.

## Context
Repo review 2026-07-18 (finding: platform coverage gaps). CI covers
ubuntu+windows; release.yml ships a macos-aarch64 binary that is never built
or tested on macOS in CI — "informal coverage via the dev host" at best. The
sandbox.rs Seatbelt tests and backend_claude process-group tests are
mac-specific and silent in CI today. macOS runners cost more; scope the job
to the test suite and let ubuntu keep fmt/clippy.

## Acceptance hints
- `macos-latest` job runs `cargo test --workspace --no-fail-fast` on every
  push/PR; green on main.
- macOS-gated sandbox tests (Seatbelt profile generation, fail-closed fs+net
  refusal) execute in the job log — verify they are not silently skipped.
- rust-cache configured for the job; SHA-pinned actions per repo convention.
