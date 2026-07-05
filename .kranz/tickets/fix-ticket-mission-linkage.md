---
title: Record the ticket slug on drafted missions; stop resolving by goal equality
priority: 2
schedule: once
---

## Goal
kranz ticket approve auto-resolution failed for all three drafted tickets on first real use: find_mission_for_ticket (crates/cli/src/backlog.rs:579) matches state.mission.goal against Ticket::mission_goal() by string equality, but the draft pipeline itself rewrites mission.goal to the orchestrator's refined plan goal at plan.approved — the linkage key mutates before lookup. Record the ticket slug durably on the mission instead (e.g. in the mission.created payload and folded state) and resolve by slug; keep goal-matching only as a legacy fallback for pre-existing missions.

## Context
Observed live 2026-07-05: three tickets drafted (m-468277, m-642a1a,
m-84e7ea), all parked in REVIEW, and `kranz ticket approve <slug>` failed
for every one with "could not find the drafted mission automatically".
Root cause chain: cmd_draft creates the mission with goal =
Ticket::mission_goal() (goal + folded sections); the parked plan is
committed via plan.approved; the reducer updates mission.goal to the
PLAN's refined goal; find_mission_for_ticket then compares the new goal
to the ticket's — never equal unless the orchestrator echoes the ticket
verbatim. The event schema is append-only and versioned-by-addition:
adding an optional ticketSlug field to mission.created's payload is
backward-compatible (old logs fold with None). Update cmd_draft to set
it, the reducer/state to carry it, find_mission_for_ticket to prefer it,
and kranz missions to display it. Consider the same slug surfacing in
GET /api/missions for the upcoming backlog REST surface
(backlog-host-draft-deps).

## Scoping answers

## Acceptance hints
- Draft-then-approve round-trips by slug alone on a fresh ticket (integration test: new ticket -> cmd_draft with mock backend -> cmd_ticket_approve without --mission succeeds).
- Old missions without ticketSlug still fold and list correctly (regression test on an existing fixture log).
- kranz missions output shows the ticket slug for drafted missions.
- cargo test --workspace passes, piped through grep -qE 'result: ok\. [1-9][0-9]* passed'; clippy and fmt clean.
