---
title: Keep the contract scratch home out of the operator's checkout
priority: 2
schedule: once
---

## Goal

Contract commands run with `HOME`, `TMPDIR` and `CARGO_HOME` under the mission's `runs/contract-home`, which sits inside the operator's checkout. Decide whether that scratch should live outside any repository, and cover the contract-gate shape in CI either way.

## Context

Found on 2026-11-13 when a mission's final gate failed `cargo test --workspace` on a change that passes the same suite outside the gate. Five engine tests failed for reasons that had nothing to do with the change:

- A tempdir under the scratch is inside the operator's repository, so git discovery from a tempdir that is meant to be no repository walks up and finds one. A test that reads git config from a bare tempdir reads the operator's repository config.
- `HOME` is the scratch home, so a test that compares the OS account home with `$HOME` fails.
- The scratch path is long enough that a unix socket bound under `TMPDIR` exceeds the platform's socket path limit.

Those five tests were made independent of the scratch location. The underlying shape remains, and any project's own tests can trip on the same three properties. The planner cannot tell an operator's project to avoid them.

The `rust-macos-wrapped-suite` CI job did not catch this because its fixture mirrors the merge gate, whose scratch is a `kranz-gate-*` directory under the system temp root and whose `HOME` is the operator's.

## Scoping answers

- Where the contract scratch lives is an operator decision (flag it as a D-X decision in the design note): it is a writable root of the gate sandbox and is read by the session profile, the ACP worker profile and preflight.
- Read docs/knowledge/validation/child-containment.md and docs/acp-containment.md before changing any writable root. Moving the scratch must not widen what a gate or session can write.
- Contract files change additively only.
- Each fix carries a test that fails without it.

## Acceptance hints

- A design note states the options (keep the scratch in the checkout and inject a discovery ceiling, or move it under the system temp root with the same cleanup and deny rules) and records the decision.
- CI runs the workspace suite, or the engine library suite, in the contract-gate shape: scratch `HOME`, `TMPDIR` under a mission `runs/contract-home`.
- A contract command that runs `git rev-parse` from a fresh tempdir does not resolve to the operator's repository.
