---
title: Design and ship read-side authentication for kranz serve (M6 read-auth)
priority: 2
schedule: once
---

## Goal
Give `kranz serve` a read-auth model suitable for a non-loopback deployment:
today every `POST /api/...` requires `x-kranz-token`, but GETs and the WS
upgrade are tokenless on loopback binds, and the M6 cloud story (TLS
termination, token provisioning) is unbuilt. Define the model (who reads
what, how the dashboard and Tauri authenticate, the `?token=` WS story,
`/api/health` exemption) and implement it behind config so single-operator
localhost UX does not regress.

## Context
Repo review 2026-07-18 (`~/Desktop/kranz-repo-review-main.md`, finding 12/4).
`docs/deploy.md` says plainly: never expose a raw server until read-auth
ships — transcripts and mission state are sensitive. This is the last hard
blocker for the M6 live-deploy operator gate (`docs/operator-gates.md`). The
middleware stack (CORS → Host gate → JSON gate → token gate,
`crates/server/src/lib.rs`) already has the seam: `TokenGate.require_read_token`
exists for off-loopback binds; the work is making read-auth a first-class,
documented, deployment-ready mode rather than a bind-class side effect.

## Acceptance hints
- A config-gated read-auth mode where GETs/WS require the token on any bind
  class; loopback default behavior unchanged when the mode is off.
- Dashboard/Tauri token UX verified against the mode (token prompt, `?token=`
  WS upgrade, no mutation authority in URLs).
- Tests cover: reads rejected without a token when the mode is on, health
  exemption, WS origin policy unchanged.
- docs/deploy.md updated: the "never expose without read-auth" warning
  references the shipped mode instead of an unbuilt one.
- cargo test --workspace green.
