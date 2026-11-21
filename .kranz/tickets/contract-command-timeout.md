---
title: Let a contract command run longer than ten minutes
priority: 1
schedule: once
---

## Goal

A contract `command` assertion that needs longer than ten minutes, such as `cargo test --workspace` on this repo, can pass at the final gate. The limit can be raised by the operator, and a command that times out is reported as a timeout, not as a failed requirement.

## Context

Seen in two missions run on 2026-11-19 (m-882080 and m-caf631). Both plans had a `cargo test --workspace` assertion. Both missions delivered every feature and passed every other assertion, then blocked at the final gate because that command was killed with `timed out after 600s`. Run by hand in each mission's integration worktree, the same command passed: 3,547 tests in 32 minutes and 3,534 tests in 77 minutes, with no failures.

The limit is `COMMAND_TIMEOUT` (600 seconds) in `crates/engine/src/command_exec.rs`. No configuration reads or overrides it. Every contract command run under it, so any plan that includes the full workspace suite blocks the same way, and the operator can only accept the assertion by hand.

The approval-time contract lint also ran the command and hit the same limit, but listed it among assertions that "already pass or could not reach a verdict". The plan did not say the command could not finish in time, so the problem only showed after all the work was done.

Related: ticket `final-gate-accepted-command-assertion` covers what the operator is told when an accepted command finding comes back at the final gate.

## Scoping answers

- Flag as a D-X operator decision where the limit is set. Candidates: an optional per-assertion timeout in the plan, a workspace or mission config value, or both, with the per-assertion value winning. Any schema change is additive with `#[serde(default)]`, and an absent value keeps 600 seconds.
- Decide whether there is an upper bound an assertion or config may not exceed, so a plan cannot disable the limit.
- A timed-out command stays a failed assertion. Do not turn a timeout into a pass or a skip.
- The approval-time contract lint names a command that timed out as a timeout, separately from commands that passed on the base tree, so the operator sees it before approving.
- The process-tree kill on timeout is unchanged. Read `docs/knowledge/validation/child-containment.md` before changing how contract commands run.
- Validators and the final gate use the same limit for the same assertion.

## Acceptance hints

- A test runs a contract command with a short configured timeout and shows that the configured value, not 600 seconds, applies.
- A plan with no timeout set behaves exactly as today.
- A configured value above the upper bound is refused or clamped, with the assertion id in the message.
- A test shows the contract lint labels a timed-out command as a timeout.
- Anti-vacuity: the test filter matches only the new tests.
