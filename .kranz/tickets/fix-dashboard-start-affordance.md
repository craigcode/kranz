---
state: done
state-note: mission m-642a1a complete, merged 2026-07-05 — sidecar restored after tracked-on-branch/ignored-on-main checkout crossfire deleted it
title: Status-aware Start button on the dashboard mission page
priority: 2
schedule: once
---

## Goal
The only Start affordance today lives in the ephemeral plan-review panel; navigating away strands an approved mission with no way to start it from any UI (live incident: m-d341a7 stalled 32 minutes). Add a Start action to the mission page's management row whenever the mission is approved with no live run, wired to POST /api/missions/:id/start.

## Context
Live incident: m-d341a7 approved in the web UI, user navigated away, Start became unreachable (the only affordance is the ephemeral plan-review panel in apps/dashboard/src/components/PlanReview.tsx; management buttons row exists on the mission page from the earlier abandon/delete slice). api client already has startMission (apps/dashboard/src/lib/api.ts:170). Depends conceptually on the Approved-vs-Executing status ticket — render Start when approved-with-no-live-run; if that ticket hasn't landed, derive the condition server-side or from run events, do NOT render Start for genuinely-executing missions (double-start must stay refused by the host with a clear message either way).

## Scoping answers

## Acceptance hints
- An approved-idle mission's page shows Start in the management row; clicking it starts the run (worker.spawned within seconds) and the button disappears.
- A genuinely running mission never shows Start; clicking a stale Start (race) surfaces the host's conflict message, not a silent failure.
- npx tsc --noEmit and npm run build pass in apps/dashboard.
