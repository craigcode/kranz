---
state: done
state-note: Done at 4dd5d92: Seatbelt read-deny for serve.token/serve.read.token/config.json (raw+canonical, deny precedence verified); bwrap /dev/null ro-bind masks; container /dev/null:ro mounts. kranz serve mints a read-only token (gated GETs/WS only, never mutations). Flagged gaps: operator-global ~/.kranz tokens uncovered; bwrap/container masks existence-guarded at spawn. Tests 2>&1 | grep -qE 'test result: ok. [1-9]'
title: Deny sandbox reads of serve.token + split mutation authority (P1)
priority: 1
schedule: once
---

## Goal
A sandboxed worker must not read `.kranz/serve.token` and turn loopback +
broad reads into mutation authority (self-approving grants, starts,
merges). Sandbox profiles (Seatbelt + container) deny reads of authority
material (serve.token, .kranz/config.json); the server splits mutation
authority from read authority; per-run capability tokens where cheap.

## Context
From the review (P1 #4): Seatbelt allows all file reads and loopback;
`x-kranz-token` grants mutation on every /api verb. v1: deny-by-path in
the sandbox profile + a distinct mutation token file readable only by the
operator/serve process, never by sandboxed sessions.

## Acceptance hints
- Sandboxed session cannot read serve.token/config.json (assertion via the
  existing sandbox_wrap probe shape).
- Read-only GETs keep working with the read token; mutations need the
  mutation authority that agents cannot see.
- cargo test --workspace green.
