---
title: Offer a worker a deny-rule grant only when the denial stopped it
priority: 3
schedule: once
---

## Goal

A worker run that does not pass is parked for a grant to lift a deny rule if any command in the session was denied, even when the worker carried on after that denial and failed for another reason. Offer the grant only when the denial is what stopped the session.

## Context

This is the worker side of `validator-park-only-when-denial-stopped-it`, which mission m-e36de6 fixed for validators on 2026-11-25. It was found by reading the code during that mission. No mission has been seen to park wrongly on this path yet.

`maybe_park_for_worker_deny_grant` in `crates/engine/src/orchestrator.rs` takes the first entry of `denied_commands`, finds the worker deny rule that matches it, and parks the milestone for a `WorkerDeny` grant. It runs whenever the worker's result is not a pass. `crates/engine/src/runner.rs` collects `denied_commands` across the whole session.

So a worker that was refused one command early, worked around it, made many more tool calls and then ended partial or failed for an unrelated reason is still parked on that early command.

The wrong offer costs more here than it did for validators:

- Approving lifts a deny rule for the rest of the mission. The grant target is the rule, for example `Bash(git push*)`, not the one command. An operator asked to lift a rule the worker did not need is asked to weaken a guardrail for nothing.
- Either decision re-runs the worker, and an unanswered request waits out `grantRequestTimeoutSecs` (one hour by default) with the mission idle.

The runner already records what is needed. Since m-e36de6, `RunOutcome` carries `tool_calls_after_last_denial`, `stopping_denied_command` and `last_tool_call`. The validator path uses them; the worker path does not.

The rule for validators is recorded in `docs/scoping/validator-park-only-when-denial-stopped-it.md` as D-A (a denial stopped the session when no tool call followed the last denied tool result) and D-B (the command offered is the one whose denial stopped the session).

## D-X operator decision

Whether the worker path takes the validator rule as it stands.

- D-X1: apply D-A and D-B unchanged. Offer a `WorkerDeny` grant only for `stopping_denied_command`. When the session had denials but none stopped it, offer nothing and let the run go to the normal judgement and respawn.
- D-X2: keep offering the first denied command, and only add the session-end detail to the grant description so the operator can judge.
- D-X3: leave the worker path alone until a mission is seen to park wrongly on it.

One difference from validators to weigh: a worker that is denied a command often tries a variant of it next. Under D-A that second attempt counts as a tool call after the denial, so the first denial did not stop the session. If the variant is denied too and the worker then stops, the grant offered is for the last denied command, and its matching rule may differ from the first.

## Scoping answers

- Reuse the fields `RunOutcome` already has. Do not add a second way of deciding which denial stopped a session.
- When no grant is offered for a denied command, record one orchestrator decision that says why, in the same wording style as the validator path.
- Keep the existing rule that only a command matching a liftable `Bash(...)` deny rule is offered. A hook denial or a non-Bash tool denial stays ungrantable.
- Keep the respawn accounting as it is: a grant-driven re-run is credited through `grant_respawns` and does not use the failure-retry budget. A run that is no longer parked must be charged to the normal budget.
- Egress grants are out of scope. A proxy denial has no position in the event stream.
- Attribution inside a parallel tool batch can name the wrong command. That gap is known and separate.
- Contract files change additively only. No change to `prompts.rs`.

## Acceptance hints

- A mission test where a worker is denied a command, makes further tool calls and ends without a pass. It asserts that no `grant.requested` event is raised for that command and that the feature goes to the normal judgement.
- A mission test where the denied command is the worker's last tool call. It still parks for a `WorkerDeny` grant on the matching rule.
- The existing tests `worker_deny_grant_lifts_the_rule_and_respawn_completes` and `worker_deny_grant_respawn_does_not_eat_the_respawn_budget` still pass.
- Anti-vacuity: the test filter matches only the new tests. `worker_deny_grant` already matches the two existing ones.
