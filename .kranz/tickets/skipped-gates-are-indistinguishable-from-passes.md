---
state: done
state-note: "Done: test_capability::skip replaces the bare eprintln+return at ~50 gate sites. Two mechanisms, because printing alone does not work - libtest swallows a passing test's output and a skipping test passes. KRANZ_REQUIRED_CAPABILITIES makes skip() PANIC when a required capability is missing (ubuntu git,bwrap,container,grep; macOS git,sandbox-exec; Windows git), so a lane that starts skipping goes red instead of quietly green. The container sandbox provider is release-supported only on Linux and fails closed elsewhere. KRANZ_SKIP_LOG writes a ledger that survives libtest capture, and each lane prints it after the suite, so a green run still shows what it did not exercise. Verified both directions on Windows: skip-and-pass when unrequired, loud panic when required-but-absent, and the ledger captured a skip that produced no console output at all. Rule documented beside the existing anti-vacuity contract in docs/knowledge/validation/gates.md. Windows suite 2424 passed / 0 failed."
title: A runtime-gated test that skips is indistinguishable from one that passes
priority: 2
schedule: once
---

## Goal

Make a skipped runtime-gated test visible in CI output, so a capability that
silently stopped being exercised cannot keep reporting `ok` indefinitely.

## Why this exists

The repo already has an anti-vacuity rule for test FILTERS
([AGENTS.md](../../AGENTS.md) rule 5, `docs/knowledge/validation/gates.md`):

```bash
cargo test --workspace <filter> 2>&1 | grep -qE 'test result: ok\. [1-9]'
```

That catches a filter matching zero tests. It does not catch the other shape:
a test that RUNS, takes an early `return` because a runtime is missing, and
prints `ok`. libtest reports it identically to a test that did the work, and
the `eprintln!` explaining the skip is captured and never shown unless the
test fails or someone passes `--nocapture`.

Three instances surfaced in a single session, none caught by review:

- **Windows container tests.** `command_available` did not consult `PATHEXT`,
  so `detect()` never found `docker.exe` and four live container tests skipped.
  Fixing the lookup turned them on and they immediately failed on two real
  bugs. They had been green-but-vacuous for the life of the Windows lane.
- **macOS container tests.** GitHub macOS runners ship no usable VM-backed
  container runtime. A local operator receipt exists, but the same path cannot
  be renewed continuously in hosted CI. The public support matrix therefore
  narrows the container sandbox provider to Linux; tracked separately as
  `macos-container-path-unexercised`.
- **The inverted-grep lint test.** Deliberately gated on `grep` being present,
  because it is about POSIX grep's exit-status semantics. Honest, but the skip
  is equally invisible.

The failure mode is not a flaky test. It is a capability quietly ceasing to be
exercised while CI keeps reporting success — the same class of problem the
anti-vacuity rule already exists to prevent, in a shape the rule does not cover.

## Done when

- [x] Skipped runtime gates are visible in CI output. `test_capability::skip`
  writes a ledger to `KRANZ_SKIP_LOG`, which survives libtest capture — the
  printed marker alone does NOT, because a skipping test passes and libtest
  swallows a passing test's output. Each lane prints the ledger afterwards.
- [x] CI declares the expected capabilities per platform rather than asserting
  a skip list after the fact. `KRANZ_REQUIRED_CAPABILITIES` makes
  `test_capability::skip` PANIC when a required capability is missing, so a
  lane that starts skipping goes red. ubuntu requires git,bwrap,container,grep;
  macOS git,sandbox-exec; Windows only git. macOS and Windows legitimately skip
  the Linux-only container path.
- [x] The rule is written down beside the existing anti-vacuity contract in
  `docs/knowledge/validation/gates.md`, so the next runtime-gated test is
  written with it in mind.

## Notes

Deliberately not solved inside the branch that found it: that branch was
already carrying Windows defects, test portability, CI hardening, M7 closure
and container argv fixes. This wants its own change and its own receipt.

Worth considering `--format json` or a nextest profile rather than hand-rolled
reporting, if either can express "these tests must not skip on this platform"
directly.
