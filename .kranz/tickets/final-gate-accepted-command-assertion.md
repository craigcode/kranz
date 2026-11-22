---
title: Tell the operator that skipping a command finding does not carry to the final gate
priority: 2
schedule: once
---

## Goal

An operator who unblocks a milestone by accepting a failed `command` assertion is told, before the mission spends another round, that the final gate will run that command again and cannot waive it. A mission whose command cannot pass for reasons outside the product has a defined human route to an ending, decided by the operator.

## Context

Seen in mission m-caf631 on 2026-11-20. Its `cargo test --workspace` assertion timed out at the fixed 600-second contract-command limit (ticket `contract-command-timeout`). The operator ran the suite by hand (3,534 passed, 0 failed) and sent guidance to accept it. The orchestrator chose `unblock-skip-findings`, the milestone was re-validated and completed, and the mission went to its final gate. The final gate ran the same command, it timed out again, and the mission blocked again with the same escalation. The re-validation round cost money and time and could not have ended differently.

This follows from a deliberate rule. `docs/knowledge/validation/gates.md` says a non-zero exit at the final gate produces a critical, non-waivable `command-assertion` finding, and model waivers are refused. That rule stays. The problem is that the unblock path accepted the finding without saying that it would come back, and the only way out left was to abandon the mission and finish its work by hand.

## Scoping answers

- Do not let a model waive a `command-assertion` finding at the final gate. That rule is unchanged.
- When an unblock decision skips findings that include a `command-assertion` failure, the engine records, and shows the operator, that the final gate will rerun the command and fail closed if it still fails.
- Flag as a D-X operator decision whether a separately authenticated human attestation may settle a final-gate command failure, in the way an enforced Flight Rules finding takes a human waiver that matches its exact evidence. If yes, the attestation names the assertion, the commit, the command and the evidence, and is recorded in the event log. If no, the mission is told plainly that abandoning is the way out.
- Read `docs/knowledge/validation/gates.md` and `docs/knowledge/decisions/inviolable-invariants.md` before changing the final gate.
- The timeout itself is ticket `contract-command-timeout`. This ticket covers what the operator is told and offered.

## Acceptance hints

- A mission test: a command assertion fails, the operator's guidance leads to `unblock-skip-findings`, and the event log records that the final gate will rerun that command.
- The final gate still turns the failure into a critical `command-assertion` finding when no human attestation exists.
- If the D-X decision allows attestation: a test that a matching attestation settles the finding and a mismatched one (other commit or other command) does not.
- Anti-vacuity: the test filter matches only the new tests.
