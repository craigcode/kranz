---
state: open
title: Signal intake 8: defect linkage confirmation and latency
priority: 2
schedule: once
blocked-by: [signal-intake-5-proposal]
---

## Goal

Let the human who drafts a signal ticket confirm one of triage's `traced-from-mission` candidates, and report signal receipt to consent latency in the outcomes report.

## Context

Design of record: `docs/scoping/signal-intake.md`. Read it in full before planning and do not contradict a decided or proposed section. Slice 8, decision D-F.

False greens already count missions linked by `traced-from-mission` (`escalation_metrics.rs`, `docs/metrics.md` §3); `Ticket::seed_traced_from_mission` writes the key. Nothing writes the link automatically.

## Scoping answers

- Confirmation happens at draft time in the CLI and dashboard; no candidate is written without it.
- Latency comes from ledger timestamps (received to consented), shown beside grant latency in `outcomes.rs`.

## Acceptance hints

- A seeded signal whose confirmed candidate is a completed mission shows up as a false green in `kranz escalation-metrics --json`.
- An unconfirmed candidate never appears in the metric.
- The outcomes report shows receipt-to-consent latency for seeded signals.
