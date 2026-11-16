---
title: Block the milestone when a contract command cannot run, not spend fix cycles on it
priority: 2
schedule: once
blocked-by: [contract-check-results-recorded]
---

## Goal

When a contract command fails for a reason no worker can fix, the milestone blocks for the operator with the evidence attached. No fix feature is created for it.

## Context

Reported from a mission run on 2026-11-15, abandoned at $7.80 with the product code already correct. Every failing check was a command the host shell had mangled. The functional validator said so in its first finding, a fix worker escalated, and the orchestrator failed a fix feature with the reason that all findings were harness faults. The engine still created 17 fix features over two cycles. Workers added helper scripts to work around the check; a later fix feature removed them.

The ticket `escalate-contract-authoring-bugs` (done) added a `commandBroken` answer to the findings-conversion turn. It is honoured only for findings whose class is `command-assertion`, and only the final gate produces those. Findings from a milestone validation round carry an empty class, so the escape hatch in `convert_findings` (`crates/engine/src/findings.rs`) ignores them. The comment in `orchestrator.rs` beside that call says as much.

## Scoping answers

- Extend the existing escalation to milestone-round findings whose subject is a contract command assertion. Do not add a second mechanism.
- Ground the decision in the recorded command result from `contract-check-results-recorded`, not in the validator's prose. Two deterministic triggers:
  - the command could not start, or the shell rejected it before the program ran;
  - the same assertion fails with an equal failure digest after a fix attempt that was meant to address it.
- The orchestrator's `commandBroken` judgement stays available for the cases the triggers do not cover, and stays limited to command assertions.
- A blocked milestone names each assertion, its exit code and the output tail, and says what the operator can do: fix the contract through a revision, or abandon.
- Fix-cycle accounting for product findings does not change. A finding about product behaviour still becomes a fix feature.
- This is a gate-outcome rule. It adds no worker capability and no prompt routing; check it against the positioning ADR before starting.

## Acceptance hints

- A mission test where a command assertion cannot start in the milestone round ends with the milestone blocked, the assertion named, and zero fix features created.
- A mission test where a command assertion fails, one fix feature runs, and the assertion fails again with the same digest ends blocked after that one cycle, not at the cap.
- A mission test with an ordinary product finding still creates a fix feature.
- The escalation is refused for a finding whose subject is not a command assertion.
