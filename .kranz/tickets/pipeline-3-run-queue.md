---
state: done
title: Run the queue from web and Slack: serve-owned drain
priority: 2
schedule: once
---

## Goal
Hoist the dispatcher's drain/claim/skip loop to be host-callable (the draft-hoist pattern); token-gated POST /api/queue/drain (idempotent when a drain is live); dashboard 'Run queue' affordance; Slack '/kranz work run' that POSTs to serve — the bridge never runs missions on its socket loop; optional autoWork config that drains automatically when entries queue (default off).

## Context
Design of record: docs/scoping/pipeline-view.md (stage model, decided
D-B/D-C/D-D, design principles: simple lists / easy buttons / iterate
always on offer). Read it in full before planning; do not contradict a
decided section. D-A verb copy: Approve reserved for plan approval,
Queue for ticket-queueing, pending final operator confirmation.

## Scoping answers

## Acceptance hints
