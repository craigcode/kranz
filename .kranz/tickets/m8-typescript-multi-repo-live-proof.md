---
state: open
title: Prove M8 end to end with a fresh TypeScript repository and live multi-repo host
priority: 1
schedule: once
---

## Goal

Close M8 with a reproducible live receipt: initialize a disposable TypeScript
Git repository using `kranz init`, take a real ticket through draft, approval,
queue, worktree execution, validation, and gated merge using that repository's
own Node gate, then host it beside another healthy repository in one Kranz
process with the configured Slack workspace and prove repository routing is
unambiguous.

## Constraints

- Do not copy this repository's `.kranz/config.json`, merge gates, tickets, or
  mission state into the proof repository.
- The proof repository must have no package-registry dependency; use the Node
  runtime's TypeScript support and test runner so the gate is deterministic.
- Preserve the operator's existing global host and Slack configuration. Add
  only the proof repository catalog row, and remove that row after collecting
  the receipt unless keeping it is required for an in-progress proof.
- Do not claim a live Slack command was routed unless a real inbound workspace
  event was observed. A live bridge connection plus the existing integration
  routing suite must be labelled as such if no operator event is sent.
- Keep the primary Kranz checkout on its original branch and byte-clean across
  the external mission, apart from this ticket and its eventual proof receipt.

## Acceptance hints

- `kranz init` creates a valid unconditional Node gate and is byte-idempotent.
- The mission reaches a terminal successful state with a non-empty feature
  commit merged through that repository's own gate.
- The worker uses a dedicated worktree and the proof repository's primary
  checkout remains on its original branch during execution.
- One `kranz serve` reports at least two healthy repository ids and its Slack
  bridge authenticates once for the catalog.
- Explicit `repo:<id>` routing, ambiguous-input refusal, channel routing, and
  thread affinity are exercised against the catalog without cross-repo state
  bleed.
- A committed receipt records commands, relevant SHAs, mission id, gate
  results, routing evidence, limitations, and cleanup.
