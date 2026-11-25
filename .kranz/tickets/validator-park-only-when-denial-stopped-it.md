---
state: done
state-note: "Mission m-e36de6. A validator is offered a grant only for the denial that stopped its session, meaning no tool call followed the last denied tool result. Otherwise the engine retries once and blocks with a reason that says how the session ended. The worker grant path and egress grants are unchanged and recorded as open in the scoping note."
title: Park a validator for a grant only when the denial stopped it
priority: 2
schedule: once
---

## Goal

A validator session that ends without a report is parked for a command grant if any command in the session was denied, even when the validator carried on after that denial and stopped for an unrelated reason. Offer a grant only when the denial is what stopped the session, and say what actually ended it otherwise.

## Context

Found on 2026-11-14 in missions `m-373b57` and `m-7886d9`.

In `m-7886d9` the scrutiny validator ran `grep ...; echo "exit:$?"`. The agent CLI refused the `echo` part of the compound command. The validator continued for about a hundred more events, then launched a background sub-agent and ended its turn while waiting for it. The session result was partial. The engine found a denied command on record and parked the milestone for a grant on that `grep` line. The grant timed out after an hour and the mission blocked.

Approving the grant would have re-run validation, but the grant was not what the validator needed. After the operator sent guidance telling validators to run one command per call and not to start background agents, the same validation passed with no grant.

`maybe_park_for_grant` in `crates/engine/src/orchestrator.rs` offers the first entry of `denied_commands` whenever the outcome is untrusted. `crates/engine/src/runner.rs` collects those commands across the whole session.

## Scoping answers

- Decide what "the denial stopped it" means in terms the runner can observe, for example the denial being the last tool result before the session ended, or the session ending within a small number of events of it. Flag the rule as a D-X decision.
- When an untrusted validator session has an earlier denial that did not stop it, do not offer a grant. Retry the validator within the existing retry budget, or block with a reason that names how the session ended.
- The validator role prompt lives in a frozen module. Changing prompt text is out of scope here unless the operator says otherwise (see `docs/knowledge/decisions/positioning-governance-evidence-layer.md`). The operator guidance that worked is recorded above so that decision can be made separately.
- A grant request that times out currently waits a full hour with the mission idle. Whether the wait should be shorter, or should notify, is a separate question; note it and leave it.
- Contract files change additively only.

## Acceptance hints

- A test runs a validator session with an early denied command followed by further successful tool calls and a partial end, and asserts no grant request is raised for that command.
- A test where the denied command is the last thing before the session ends still parks for a grant.
- The block reason for the first case names how the session ended.
