---
title: Grant-request decision flow — fail closed, then offer the narrowest consent
priority: 2
schedule: once
---

## FIRST ATTEMPT REVERTED 2026-07-13 — the premise below is WRONG; read this first

A B-core build (commit 79d3f5e) was reverted after adversarial review found it
DEAD IN PRODUCTION on two counts (both verified in code):

1. **The worker-denial trigger cannot fire.** The capture keyed off
   `ToolResult { tool: Some("bash") }`, but Claude ALWAYS sets `tool: None`
   (backend_claude.rs — the tool name is on the tool_use block, never
   correlated to the tool_result); Codex sets `tool: "command_execution"`
   with the command's OUTPUT (not the command) as summary; Droid emits no
   ToolResult. So `denied_commands` is always empty in prod and GrantRequested
   never fires. The green "end-to-end" tests only passed because
   `mock_denied("Bash", cmd)` fabricates a shape no backend produces.
2. **Even if it fired, an approved grant cannot unblock a WORKER denial.**
   `command_grants` adds an ALLOW; workers already have a blanket `Bash` allow,
   so their only denials are WORKER_DENY (curl/git push/sudo), `deny_patterns`,
   or hooks — all in `disallowed_tools`, and DENY WINS over allow in Claude
   Code (permissions.rs:4). So approving the command is a no-op unblock.

Corrected approach for the next attempt:
- **Trigger on VALIDATOR command-denials, not worker denials.** Validators get
  NO blanket Bash; a command outside their allow-set is a real allow-set miss
  that extending `command_grants` genuinely unblocks (permissions.rs: validator
  denials "surface as findings"). That is the case the grant flow can actually
  fix. (Worker deny-rule/hook denials would need the grant to SUBTRACT from the
  deny set, a bigger change — defer or scope out.)
- **Get the command from the preceding `ToolUse` block** (correlate by
  tool_use_id), never from `ToolResult.tool`/`summary`.
- **Decide approve-vs-deny from the EVENT** (GrantApproved vs GrantDenied), not
  by re-deriving "is the command in command_grants" — a concurrent plan
  revision that extends command_grants can otherwise flip a DENY into approve.
- **Add a per-feature grant-request CAP** (the reverted build removed the
  respawn-budget charge with no replacement → unbounded loop under any
  auto-approver).
- **Route the gate at the run-loop level, not deep inside run_feature.** The
  reverted build's park called the full `drain_control`, which applied plan
  revisions under stale mi/fi indices (OOB / wrong-feature) and bypassed the
  §4.4 dirty-tree gate and the pending_revision/pause gates.
- **Reducer must cross-check `pending_grant_request`** on GrantApproved/Denied
  (like PlanRevised does on `pending.revision`), or a forged/replayed
  grant.approved silently widens command_grants.
- **Test from a REAL Claude event fixture** (`backend_claude::parse_user` or a
  captured stream), NOT `mock_denied`, so a dead trigger can't pass green again.
- Editing `crates/engine/src/types.rs` (a CONTRACT FILE) was needed for the new
  state/config — additive, but flag/confirm before doing it again.

## Implementation blueprint (mapped 2026-07-12; scope confirmed with Craig — NOTE: superseded by the reverted-attempt findings above, esp. the worker-vs-validator trigger)

MVP scope (confirmed): trigger = COMMAND-outside-grants only (defer touch-set
and egress); grant is MISSION-WIDE, not per-role (command_grants live on
Mission/Plan, extend-only — a config-scoped per-role grant is only possible for
egress, which is unobservable today). Structural clone of the pending-REVISION
flow throughout (that is the existing human approve/deny gate; OrchestratorDecision
is display-only and NOT the gate).

Slices:
- **B-core (engine):** 3 events `GrantRequested{feature_id, command}` /
  `GrantApproved{command}` / `GrantDenied{command, reason}` (events.rs, clone
  PlanRevision*); `MissionState.pending_grant_request: Option<PendingGrantRequest>`
  (types.rs, clone pending_revision at :356) + reducer folding (set on Requested,
  clear on Approved/Denied, and on Approved EXTEND command_grants — reuse the
  extend-only path at reducer.rs:399-403); trigger in `run_feature` (orchestrator.rs
  ~2231, after `let outcome = outcome?`): when `RunOutcome.denied_count > 0`,
  derive the denied command and emit GrantRequested, then PARK the run loop
  (mirror the pending_revision park at orchestrator.rs:1914) with a
  timeout→auto-DENY (deny-default); `ControlCommand::ApproveGrant`/`DenyGrant`
  (types.rs:598, clone ApproveRevision) in `drain_control` (orchestrator.rs:2023);
  approve → GrantApproved + respawn the feature (respawn loop already at
  orchestrator.rs:2271); deny → GrantDenied + FeatureFailed.
  - NOTE (trigger fiddliness): the denied event gives `tool` + `summary` (e.g.
    `Bash: <summary>`), NOT a clean grantable prefix — `RunOutcome` only carries
    `denied_count` (runner.rs:187). Extend `RunOutcome` with
    `denied_commands: Vec<String>` captured at runner.rs:301 (RunSink::handle
    returns denied; grab the `tool`/`summary` there), and derive a minimal
    command prefix for the grant (or name tool+summary and let the operator
    confirm the prefix). This is the load-bearing design decision.
- **B-surfaces (next):** REST `POST /api/missions/:id/grant/approve|deny` →
  ApproveGrant/DenyGrant control commands (rest.rs, clone :349-377, guard the
  pending grant); dashboard GrantRequestPanel (clone RevisionPanel.tsx) with
  approve/deny; Slack card (clone format.rs build_revision_ready:562 +
  inbound.rs:405 action ids + bridge dispatch), all `is_authorized`-gated
  (config.rs:91). Minimal FUNCTIONAL vertical = B-core + the REST route (a
  parked grant needs at least one approve path); dashboard/Slack are ergonomics.

Deny-default + timeout-to-denied is the safety valve; every grant lands in the
event log with who/when. See the full architecture map in the session that
filed this (mission/audit/consent harness is the product).

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
