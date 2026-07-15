---
title: Backend readiness and quota preflight before queue drain
priority: 2
schedule: once
---

## Goal
Before a queued mission spends real work cycles, run a lightweight readiness
preflight for the backends/models that mission will use. Surface
`ok` / `unauthenticated` / `rate_limited` / `missing` / `unsupported` /
`unknown` (or `meterless`) with a next action in CLI, dashboard, and Slack.

## Context
Reuse existing probes (config validation, worker auth preflight, model
floors, environment preflight). This is **not** `kranz ready` (repo
agent-readiness score — different ticket, already shipped as a command
shape). This ticket is per-backend/per-mission drain gating.

Mission Control's usage panel is broader than needed; stay honest about
providers that expose no quota API.

## Park policy (non-negotiable)
| Probe result | Drain behavior |
|---|---|
| `missing`, `unauthenticated`, `unsupported`, sandbox mismatch, invalid model | **Park** ticket/mission with actionable reason; do not claim |
| `rate_limited` | **Requeue/delay** with backoff (or park with retry-after); do not burn a doomed start |
| `unknown` / `meterless` quota | **Warn + proceed** — never treat unknown as failure |
| `ok` | Claim and run |

Fabricating a "0% quota" bar for metered-less providers is forbidden.

## Acceptance hints
- `kranz work` and hosted queue drain invoke the probe before claim.
- Dashboard/Slack show readiness for queued/approved work using the enum
  above.
- Probe is bounded (time + no secret leakage into child env beyond what
  spawn already needs).
- Tests: missing binary, expired auth, invalid model, sandbox mismatch,
  unknown-quota proceeds, passing backend.
- Anti-vacuity grep on the named filter.
