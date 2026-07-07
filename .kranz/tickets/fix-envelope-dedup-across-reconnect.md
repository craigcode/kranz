---
title: Slack envelope dedup is per-connection -> duplicate missions on reconnect
priority: 2
schedule: once
---

## Goal
SeenEnvelopes::default() is created fresh per (re)connect (bridge.rs:~391, connect_once). Socket Mode redelivers an unacked envelope with the same envelope_id after ~3s; the bridge acks-then-dispatches. If the socket drops before Slack registers the ack, the reconnect gets an empty seen-set and the redelivered envelope passes seen.insert() as new -> re-runs dispatch_action. For NewMission/Draft that calls host.create()/host.draft() twice -> duplicate money-spending mission. Fix: hoist SeenEnvelopes (or a time-bounded id cache) into run_socket so it persists across reconnects. Interacts with the slack-bridge-reconnect work just landed.

## Context
Found by a security code review 2026-07-07 (HEAD 0740072). Double-spend risk; timing-dependent (hence P2).

## Acceptance hints
- SeenEnvelopes persists across a simulated reconnect; a redelivered envelope after reconnect is deduped, not re-dispatched. cargo test -p kranz-slack green.
