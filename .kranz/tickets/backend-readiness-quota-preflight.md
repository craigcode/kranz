---
title: Backend readiness and quota preflight before queue drain
priority: 2
schedule: once
---

## Goal
Before a queued mission spends real work cycles, run a lightweight readiness
preflight for the selected backends and models: CLI installed, auth usable,
minimum version satisfied, model available, quota or rate-limit state known
when the provider exposes it, sandbox compatibility understood, and configured
model floors satisfied. Surface the result in CLI, dashboard, and Slack.

## Context
Mission Control's provider-usage panel is broad, but the kranz-sized lesson is
simple: operators need to know whether a backend can run before the queue
starts draining. Kranz already validates backend selection in config paths and
records cost after the fact, but readiness is still scattered across backend
spawn failures, auth preflight, and model floor checks.

This should reuse existing backend probes where possible and stay honest about
unknowns. Do not fabricate a "0%" usage bar for providers that do not expose
quota. Report `ok`, `unauthenticated`, `rate_limited`, `missing`, `unsupported`,
or `unknown` with clear next action.

## Acceptance hints
- `kranz work` and hosted queue drain run backend readiness before claiming a
  mission for execution; failures park the mission or ticket with an actionable
  reason instead of starting a doomed run.
- Dashboard and Slack show readiness state for queued or approved work.
- The probe is bounded and does not leak server environment secrets into child
  processes.
- Providers without quota APIs report honest `unknown` or `meterless` status.
- Tests cover missing binary, expired auth, invalid model, sandbox mismatch,
  and a passing backend.
