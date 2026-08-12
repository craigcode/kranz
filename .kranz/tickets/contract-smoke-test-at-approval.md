---
state: done
title: Smoke-test contract command assertions against the base tree at approve_plan time
priority: 2
schedule: once
---

## Goal
At `approve_plan`, execute each `check: command` assertion in the proposed
validation contract once against the mission's base tree (read-only,
bounded, hooks-disabled) and surface failures to the operator BEFORE the
branch exists: a contract whose commands are buggy at authoring time
(false-negatives on a clean base) must never gate a mission at its end.
Warnings on base-expected-to-fail assertions (the usual case — the feature
hasn't landed) must be distinguishable from author bugs; the gate is a
linter, not a verdict.

## Context
m-0c885b (2026-07-20): the port completed correctly but the mission was
abandoned — its a6 command grep'd for a lockfile dep-ref string that only
exists in the pre-unification split state (the command could ONLY pass
while the requirement was NOT met), and its a8 diffed `$KRANZ_BASE_SHA`
without excluding the harness's own plan-artifact commits. Both bugs were
statically detectable at approval; instead they were discovered at the
final gate, after the fix-cycle cap had been spent editing plan.md (which
the final gate does not read). Assertion-altering revisions are rejected by
design, so a bad contract is fatal to an otherwise-correct mission. A
cheap approval-time lint turns that class from mission-killer to a draft-time
warning.

## Acceptance hints
- approve_plan runs command assertions against the base (bounded, sandboxed
  like the final-gate runner) and reports per-assertion lint results in the
  approval summary and plan.md.
- A provably-buggy command (e.g. the a6 lockfile grep from m-0c885b) is
  flagged at approval; the operator can still approve (it may be
  base-expected-to-fail), but with the lint visible.
- cargo test --workspace covers: buggy-command flagged, clean contract
  passes lint, approval succeeds either way.
