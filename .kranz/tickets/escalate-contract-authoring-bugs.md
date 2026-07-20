---
title: Escalate contract-authoring bugs instead of spending fix-cycles on them
priority: 3
schedule: once
---

## Goal
When a validation finding's subject is a contract assertion whose COMMAND
is broken (false-negative: validators report the underlying requirement as
met while the command still fails), the orchestrator must escalate to the
operator (block with a precise message: "assertion appears buggy, evidence
attached") instead of converting it to a fix feature. Gate-repair is not
product work; burning the fix-cycle cap on it both wastes the budget and
masks authoring bugs that belong to a human.

## Context
m-0c885b: two fix-cycles were consumed editing plan.md for assertions whose
commands were author-broken (a6 lockfile grep that could only pass
pre-unification; a8 diff that counted the harness's own plan commits). The
mission then blocked at the cap with the product verified clean. The
orchestrator DID eventually reach the right diagnosis ("broken assertions
in my own contract commands, not defects in the port") — but only after the
cap was gone. The engine should route "command-broken" findings to an
operator decision immediately: fix the product, waive (judgement assertions
only), or abandon+take-2 — never spend fix cycles on the gate itself.
Pairs naturally with contract-smoke-test-at-approval (prevention) and
prompts-plan-json-is-the-contract (agent guidance).

## Acceptance hints
- A validation finding whose evidence indicates the assertion COMMAND is at
  fault (validator reports requirement met + command still red) routes to
  an operator-visible block, not convert_findings.
- fix_cycles accounting unchanged for genuine product findings.
- mission_test scenario: command-broken finding → blocked with named
  assertion, zero fix features created.
- cargo test --workspace green.
