---
title: Distinguish Approved-idle from Executing in mission status
priority: 2
schedule: once
---

## Goal
An approved mission with no live run loop must never display as RUNNING. Add an explicit status (e.g. Approved or Ready) folded by the reducer when a plan is approved but no run has started, and surface it in kranz status, the dashboard, and the Slack card copy.

## Context
Reducer: crates/engine/src/reducer.rs folds plan.approved straight to Running today — the ambiguity that stalled m-d341a7 for 32 minutes (every surface showed RUNNING while nothing executed). Consumers to update: kranz status (crates/cli), dashboard status chips (apps/dashboard), Slack card copy (crates/slack/src/format.rs), and the docs' status table. Detecting "a run loop exists" already has a canonical probe: a live events.jsonl.lock holder (see is_repo_busy in crates/engine/src/queue.rs) — but prefer an explicit run.started event fold over lock-sniffing if the event exists. Keep the REST/JSON status values backward-compatible or version them deliberately; the glasses app also reads status.

## Scoping answers

## Acceptance hints
- A mission with plan.approved but no run activity reports the new status (not Running) in: reducer fold unit test, kranz status output, GET /api/missions.
- Once a worker spawns, status is Running; after terminal events, terminal statuses unchanged.
- cargo test --workspace passes with output piped through grep -qE 'result: ok\. [1-9][0-9]* passed' for the new reducer tests (passed-count guard, never a bare name filter).
- Dashboard and Slack copy render the new state distinctly (screenshot or snapshot test).
