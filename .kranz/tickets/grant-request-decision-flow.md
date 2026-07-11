---
title: Grant-request decision flow — fail closed, then offer the narrowest consent
priority: 2
schedule: once
---

## Goal
When a worker run is stopped by a capability boundary — a command outside
command_grants, a sandbox egress denial, a write outside the touch-set that
the worker asks about — kranz today surfaces a finding, a refusal, or a
failed run, and the operator has to reverse-engineer WHAT consent would
unblock it. Model the boundary hit as a first-class decision instead: emit
an operator-facing event that names the exact minimal grant (the specific
command, egress destination, or path glob), and let the operator approve or
deny it in one click from the dashboard and Slack, recorded in the event
log like every other consent. Approval applies the narrowest grant (this
mission, this role), never a blanket widening.

## Context
Influence: the Flu (Astro) agent framework registers a skill's description
but not its files, so first use fails ON PURPOSE and the fix is an explicit,
legible escalation — grant access, or wrap the capability as a validated
tool. Kranz already fails closed everywhere (command grants, sandbox tiers,
merge gates) and already has the decision/waive vocabulary for findings;
this extends the same UX to capability denials so the consent gate feels
like a doorbell, not a wall. Study: command_grants in plan/types,
sandbox.rs egress allowlist, the waive/fix decision flow in orchestrator.rs
convert_findings, Slack approve_flow, dashboard PlanReview decision cards.
Constraint: deny stays the default; an unanswered request times out to
denied; grants land in the event log with who/when.

## Acceptance hints
- A scripted mission whose worker needs an ungranted command produces a
  grant-request decision event naming the exact command, visible in the
  dashboard and Slack with approve/deny.
- Approving records the grant and the retried step succeeds; denying fails
  the feature with the existing refusal semantics; no path silently widens
  a grant beyond the requesting mission/role.
- cargo test --workspace and the dashboard suite pass with new pins for
  the request/approve/deny/timeout paths.
