---
state: done
title: Dashboard approves via the host-parked pending plan
priority: 2
schedule: once
---

## Goal
The web UI posts its client-side plan copy to /api/missions/:id/approve, bypassing the unified pending-plan cache: the host-parked plan goes stale (a later Slack /kranz approve errors), and two surfaces can hold divergent plan copies. Switch the dashboard to POST approve-pending, and make a direct /approve clear the parked plan.

## Context
Unified cache (380b39b): request_plan parks the reviewed plan host-side; Slack and glasses approve via approve-pending, consuming it. The dashboard still posts its client-side copy to /api/missions/:id/approve (apps/dashboard/src/lib/api.ts:166), so the parked copy survives — a later Slack /kranz approve retries the stale parked plan against an already-approved mission and errors (observed conceptually in m-d341a7's web approve). Fix both ends: dashboard calls POST approve-pending{start:false} (falling back to /approve only when it holds an edited plan, if that's ever a feature), and MissionHost::approve clears the parked plan on success so no path can leave a stale copy.

## Scoping answers

## Acceptance hints
- After a web approve, GET pending-plan returns none and a Slack /kranz approve reports the honest state (no stale-plan error) — integration test at the host level.
- Dashboard approve path exercises approve-pending (network assertion in a UI test or handler unit test).
- cargo test --workspace passes with the passed-count guard; npx tsc --noEmit passes in apps/dashboard.
