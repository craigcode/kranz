---
state: done
title: Validator allowlists must cover brief-granted read-only commands
priority: 2
schedule: once
---

## Goal
In m-d341a7 the worker could run gc lint (brief-granted exception) but the scrutiny validator could not re-run it to verify the claim, and gc <cmd> --help was denied while gc help <cmd> was allowed — 22 wasted denials. Wire plan/brief-level command grants into BOTH worker and validator allowlists, and treat verification re-runs of worker-executed commands as first-class allowlist entries.

## Context
m-d341a7: the f-1-3 worker ran gc lint (brief-granted exception) but the scrutiny validator was DENIED the same command, so a true claim was flagged "not independently verifiable"; gc <cmd> --help was denied while gc help <cmd> was allowed, costing 22 denial round-trips in one session. Related prior lesson: m-4fe1d8 verbatim-prefix allowlists caused denial-findings that blocked a milestone. Allowlist construction lives in crates/engine/src/permissions.rs + SessionSpec assembly in orchestrator.rs. Design: commands granted to the worker (from the plan's contract + brief-level grants) must be granted to validators verifying them; consider deriving both from one plan-level command set.

## Scoping answers

## Acceptance hints
- A plan whose brief grants a read-only command yields worker AND validator sessions that can run it (unit test on SessionSpec assembly).
- Validator allowlists include the contract's validation commands and worker-executed commands cited in reports.
- cargo test --workspace passes, piped through grep -qE 'result: ok\. [1-9][0-9]* passed'.
