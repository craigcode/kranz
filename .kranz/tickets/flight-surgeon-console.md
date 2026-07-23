---
title: Flight surgeon console — outcomes panel with autonomy ratio, grant-latency distribution, and the escalation ledger
priority: 3
schedule: once
---

## Goal
A shared outcomes fold over existing event logs, projected to all three
operator surfaces (engine-owned core, thin clients — the anti-drift rule):
(1) **autonomy ratio** — operator interventions per closed mission (control
msgs, grant decisions, revision decisions) plus the share of missions with
zero interventions; (2) **grant latency distribution** — time from
grant.requested to grant.approved/denied, bucketed (<10s / <60s / <10m /
>=10m), the rubber-stamp detector; (3) the **escalation ledger** — every
block/grant/revision with what was proposed and what the operator decided,
newest first. All pure folds of existing event logs; regenerable; no second
source of truth. Surfaces: full panel in the dashboard web UI (primary),
`kranz outcomes [--json]` text render on the CLI, and a `/kranz outcomes`
summary card in Slack (ratio + latency buckets + recent escalation count);
the full ledger is web-first — Slack already relays live grant events
(outbound GrantReady classification), so it needs the summary, not the table.

## Context
Born from the whitepaper review thread: the AMM measures foundations but not
the property autonomy depends on — whether the system escalates the right
things to humans. Grant latency is the closest computable proxy (sub-10s
approvals = rubber-stamping; slow evidence-attached decisions = judgment).
The ledger doubles as the consent corpus for the LoRA workstream — labeled
supervision on judgment, not just execution. outcomes-view (ticket) computes
the business-level metrics (cost/change, cycle time); this panel is the
operator-facing consent surface. Naming: flight surgeon's console — the
station watching the crew (agents), not the spacecraft (code). DEFERRED for
v1: false-green counting — it needs defect→mission linkage (a `defect-of:
<mission-id>` field on tickets); sketch the field in the types but do not
build the flow yet.

## Acceptance hints
- The outcomes fold lives in the engine (or a single shared module both
  server and CLI call), computed per request from the logs (no cache).
- `GET /api/missions/outcomes` (or repo-scoped equivalent) returns
  autonomyRatio, grantLatency buckets with counts, and the escalation ledger
  rows (ts, mission, kind, summary, decision, latencyMs).
- `kranz outcomes [--json]` prints the same data as text; empty history
  renders the ratio alone.
- `/kranz outcomes` in Slack returns a summary card (ratio, buckets, recent
  escalation count); it reuses the same fold — no Slack-specific math.
- Dashboard panel renders the three sections with empty states; TS types
  mirror the endpoint exactly (contract mirror discipline); panel tests
  cover buckets, ledger ordering, empty state.
- cargo test --workspace + dashboard gates green; no changes to events.rs/
  types.rs contracts.
