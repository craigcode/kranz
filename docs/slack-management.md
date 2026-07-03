# Design: Slack as a full control surface

Status: design. Target milestone: M2.9 (extends M2.75; rides on M2.5's host).

## Premise

Today Slack is a notification + light-steering layer (plan-ready/blocked/
complete, approve button, thread-reply→guidance, `/kranz ticket`). This makes
Slack a **full** management surface: create, plan, review, approve, start,
steer, configure, and run the backlog — without leaving Slack — while deep
forensic inspection hands off to the web UI via a deep link.

## Why it's a small lift: one registry, two clients

M2.5 turned the engine into a request/response API backed by `MissionHost`
(the hosted-engine registry). The web UI's create/plan/approve/start are HTTP
calls into it. **Slack management is a second client of the same registry** —
the Socket Mode handlers call `MissionHost::create / planning_turn /
request_plan / approve / start`, exactly like the axum handlers. No new engine
work; this is Slack-side routing + rendering.

```
              ┌───────── MissionHost (hosted-engine registry) ─────────┐
web UI  ──►   │  create · planning_turn · request_plan · approve · start │   ◄── Slack
(axum)        └────────────────────────────────────────────────────────┘   (socket mode)
```

## Interaction model (thread-centric)

- **New mission**: `/kranz new <goal>` (or an App Home button) → a modal
  (goal, optional model/effort/budget) → `MissionHost::create` → open a thread
  "Planning `m-xxxx`: <goal>"; the orchestrator's opening questions post
  in-thread.
- **Planning conversation**: every human message in a planning mission's thread
  → `planning_turn`; the reply posts threaded. (Requires `message.channels`
  for channels / `message.im` for DMs — already scoped for channels.) This is
  the web planning pane, in a thread.
- **Plan review**: a "Request plan" button / `/kranz plan` in-thread →
  `request_plan`. Ready → a Block Kit message with contract + milestones +
  calibrated estimate and **[Approve & start] [Approve & queue] [Back]**.
  NotReady → the orchestrator's prose posts in-thread (answer, request again).
- **Execution**: Start → `MissionHost::start`; the thread becomes the live
  feed (progress-log lines post threaded, throttled), plus blocked/complete.
- **Steering**: `/kranz pause|resume`, thread replies → guidance (built),
  blocked-milestone unblock (built), `/kranz msg --interrupt`.
- **Config**: `/kranz config` → a modal with per-role model text + effort
  select menus → `config-change`.
- **Backlog**: `/kranz tickets | show <slug> | draft <slug> | approve <slug> |
  queue | work`.
- **App Home tab**: a dashboard listing active missions (status pill), the
  queue, and tickets — each row links to its thread. Lightweight Mission
  Control inside Slack.

## What stays in the web UI (by design)

Deep forensic inspection — full worker/validator transcripts with tool calls,
the dense live four-pane view. These don't render in Slack and shouldn't be
forced. Every mission message carries an **"Open transcript / dashboard"**
deep link (`http://127.0.0.1:<port>/#/m/<id>` with the token fragment) that
jumps to the web UI at the exact run. Full lifecycle + backlog + steering in
Slack; forensics one tap away in the browser.

## Must-haves before it ships

1. **Spend authorization.** Web mutation is token-gated; Slack isn't. A
   `slack.allowUsers: [Uxxxx]` allowlist (Slack user ids) must gate every
   money-spending action (new/draft/start). Unlisted users get an ephemeral
   "not authorized" reply. Solo-workspace default can allow the installer.
2. **Always-on host.** Slack management needs `kranz serve --slack` running and
   hosting engines (the M2.5 registry). Same as web hosting; exactly what an
   M6 Railway deployment provides.
3. **Idempotent actions.** Slack retries envelopes; every handler must ack fast
   (<3s) and dedupe by envelope/action id so a retry can't double-create or
   double-start.

## Build slices (ship incrementally)

- **Slice 1 — lifecycle**: `/kranz new` modal → thread planning → request-plan
  block → approve/start buttons. The core "create a mission from Slack" loop.
  Reuses MissionHost + the existing plan-render + the built approve/queue paths.
- **Slice 2 — control & status**: `/kranz status|pause|resume|config`, deep-link
  buttons, spend allowlist.
- **Slice 3 — App Home**: the dashboard tab (missions + queue + tickets).

## Done when

A mission goes goal → planning conversation → reviewed plan → approve → start →
complete entirely in Slack; the backlog is fully operable via `/kranz`;
unauthorized users cannot spend; and any mission's transcripts are one tap from
its thread in the web UI.
