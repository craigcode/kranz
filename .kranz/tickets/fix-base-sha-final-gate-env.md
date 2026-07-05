---
title: Provide KRANZ_BASE_SHA to the final mission-gate environment
priority: 2
schedule: once
---

## Goal
The mission-level final gate ran contract command a4 without KRANZ_BASE_SHA in its environment, producing a false-critical the orchestrator had to manually re-verify and waive (m-d341a7 seq 1386). Every contract-command execution context — worker, validator, and the engine's own final gate — must carry the same pinned env.

## Context
m-d341a7 seq 1386: the engine's final mission-level gate ran contract command a4 (which references $KRANZ_BASE_SHA) without that env var, producing a false CRITICAL the orchestrator manually re-verified and waived (decision seq 1398). Worker and validator sessions already receive KRANZ_BASE_SHA (M1, pinned at approval, in crates/engine/src/orchestrator.rs); the engine's own direct contract-command execution path (runId "engine") does not. Audit every place contract commands execute and centralize the env construction so worker/validator/final-gate can never diverge.

## Scoping answers

## Acceptance hints
- A regression test runs a contract command referencing $KRANZ_BASE_SHA through the final-gate path and it resolves to the approval-pinned SHA.
- The env-construction is a single shared function used by worker, validator, and gate paths (no duplicated env maps).
- cargo test --workspace passes, piped through grep -qE 'result: ok\. [1-9][0-9]* passed'.
