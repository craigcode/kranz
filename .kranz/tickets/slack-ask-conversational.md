---
state: done
state-note: shipped in 952cbc0
title: /kranz ask — conversational status and commentary in Slack (LLM-backed)
priority: 3
schedule: once
---

## Goal

`/kranz ask <question>` answers free-form questions about kranz state
and history in Slack — "why did m-acb3a0 cost so much", "what's blocking
sandbox-5", "summarize today's merges" — by grounding an agent turn in
the event logs, reports, and ticket/mission state, then posting the
answer to the thread.

## Context

Operator ask 2026-07-06: "allow general questions and chat from /kranz
that would give status updates and commentary." Distinct from
slack-todo-and-status (deterministic, free): this spawns a real agent
turn and therefore spends tokens — so it must be costed and gated. Open
design questions for the scoping/plan (flag as D-X): which backend/model
answers (cheap lane via the new DroidBackend/model-per-role is the
natural fit — a scrutiny-class read-only model, never a builder); read-
only guarantee (ask must never mutate state or spawn missions); context
budget (which logs/reports enter the prompt — reuse the future repo-
knowledge-store selection); allowlist-gating (spend-adjacent); and rate/
cost caps. Reuses the planning_turn seam conceptually but is a NEW
read-only Q&A path, not a mission.

## Acceptance hints

- `/kranz ask` grounds an answer in real mission/event/ticket state and
  posts it to the Slack thread, read-only (no state mutation, no mission
  spawn — asserted).
- Backend/model, context budget, and cost cap are config-driven; the
  spend is recorded like any other agent turn.
- Allowlist-gated; a test drives the grounding + read-only guarantee
  against a fixture repo.
