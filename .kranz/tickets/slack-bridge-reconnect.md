---
state: done
title: Slack socket-mode bridge must detect a dead connection and reconnect
priority: 2
schedule: once
---

## Goal

The `kranz serve --slack` bridge can have its Slack socket-mode
WebSocket drop while the serve process stays alive, after which Slack
commands silently go nowhere (no response, no error). Add connection-
liveness detection and automatic reconnect (with backoff), plus a
visible health signal so a dead bridge is observable rather than silent.

## Context

Hit live 2026-07-06: operator messaged Slack and got no response; serve
(pid alive, --slack present) had not written slack-threads.json in ~5h.
Leading cause: a stale socket-mode connection after a long-running
sibling drain and idle time. A manual serve restart is the current
workaround. Socket-mode clients should heartbeat/detect disconnect and
reconnect; verify what the current bridge client does
(crates/slack socket loop) and add reconnect + backoff if missing.
Health signal options: a GET /api/slack/health the dashboard shows, a
periodic bridge log line, or a startup/ready ping to the channel. The
silent-failure mode is the real defect — even without auto-reconnect, a
dead bridge must be loud.

## Acceptance hints

- A dropped socket-mode connection triggers an automatic reconnect with
  backoff; commands work again without a manual restart.
- A dead/disconnected bridge is observable (health endpoint or log/
  channel signal), never silent.
- Verify against the existing socket loop; test the reconnect path with
  a simulated disconnect where feasible.
