---
title: Human merge from web and Slack: gated, never pushes
priority: 2
schedule: once
blocked-by: [pipeline-1-stage-artifacts]
---

## Goal
A Merge action at the Delivered stage, human-triggered on both surfaces: POST /api/missions/:id/merge (token-gated) refuses on dirty tracked trees, runs the full gate suite (workspace tests, clippy -D warnings, fmt, dashboard tsc+build when touched), merges --no-ff on green, surfaces the failing gate verbatim on red, and NEVER pushes. Dashboard Merge button at Delivered; Slack Delivered card with allowlist-gated /kranz merge.

## Context
Design of record: docs/scoping/pipeline-view.md (stage model, decided
D-B/D-C/D-D, design principles: simple lists / easy buttons / iterate
always on offer). Read it in full before planning; do not contradict a
decided section. D-A verb copy: Approve reserved for plan approval,
Queue for ticket-queueing, CONFIRMED (D-A decided).

## Scoping answers

## Acceptance hints
