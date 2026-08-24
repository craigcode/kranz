---
state: open
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
- **macOS container tests.** GitHub macOS runners ship no container runtime,
  so the same tests skip there today while the module claims macOS support.
  Tracked separately as `macos-container-path-unexercised`.
- **The inverted-grep lint test.** Deliberately gated on `grep` being present,
  because it is about POSIX grep's exit-status semantics. Honest, but the skip
  is equally invisible.

The failure mode is not a flaky test. It is a capability quietly ceasing to be
exercised while CI keeps reporting success — the same class of problem the
anti-vacuity rule already exists to prevent, in a shape the rule does not cover.

## Done when

- Skipped runtime gates are visible in CI output rather than captured. The
  cheapest version is a shared helper the gates call instead of a bare
  `eprintln!` + `return`, which records the skip somewhere the job surfaces.
- CI asserts on the EXPECTED set of skips per platform, so a newly-skipping
  test fails the job. A Linux lane that starts skipping its container tests
  should go red, not quietly green. The expected set is platform-specific:
  Windows legitimately skips the container path, Linux must not.
- The rule is written down beside the existing anti-vacuity contract in
  `docs/knowledge/validation/gates.md` and AGENTS.md, so the next runtime-gated
  test is written with it in mind.

## Notes

Deliberately not solved inside the branch that found it: that branch was
already carrying Windows defects, test portability, CI hardening, M7 closure
and container argv fixes. This wants its own change and its own receipt.

Worth considering `--format json` or a nextest profile rather than hand-rolled
reporting, if either can express "these tests must not skip on this platform"
directly.
