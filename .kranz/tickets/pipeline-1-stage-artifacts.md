---
title: Persist estimates; serve plan, report, diff, and merged-state over REST
priority: 2
schedule: once
---

## Goal
The pipeline view's data layer: persist the cost estimate into plan.md's header at park time (today it dies in draft stdout — the human gate is uninformed); add GET endpoints rendering a mission's plan.md, report.md, and diff stat; add a merged/unmerged bit per mission (server-side probe: is the mission branch tip an ancestor of its base). Pure engine/REST — no UI in this ticket.

## Context
Design of record: docs/scoping/pipeline-view.md (stage model, decided
D-B/D-C/D-D, design principles: simple lists / easy buttons / iterate
always on offer). Read it in full before planning; do not contradict a
decided section. D-A verb copy: Approve reserved for plan approval,
Queue for ticket-queueing, pending final operator confirmation.

## Scoping answers

## Acceptance hints
