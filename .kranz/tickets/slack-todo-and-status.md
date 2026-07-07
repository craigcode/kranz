---
title: /kranz todo and /kranz status — operator worklist and pipeline snapshot in Slack
priority: 2
schedule: once
---

## Goal

Deterministic (no-LLM) Slack commands that answer "where is everything
and whose move is it" without opening the dashboard:

- `/kranz status` — a compact snapshot: running mission (+ live cost),
  queue depth, count at each pipeline stage, and any mission Delivered-
  but-unmerged (the UNMERGED set that needs a human merge).
- `/kranz todo` — the operator's worklist, split into two sections:
  (1) PIPELINE ACTIONS kranz is waiting on a human for — tickets at
  Reviewable (plan+estimate to approve), missions Delivered (report to
  merge), tickets Needs-you (drafter questions); each a one-tap row.
  (2) GATED ITEMS — a curated list of human-only roadmap gates
  (repo-public + history-scrub, M6 deploy, M2.9 live validation, G2
  hardware) that live nowhere queryable today.

## Context

Operator ask 2026-07-06: "can I request a /kranz todo / can I see [what
I have to do] in the Slack channel." The dashboard pipeline view answers
"whose move is it"; Slack has no equivalent, and the human-gated roadmap
items surface on no queryable surface at all. Section (1) reads the same
stage model the pipeline view consumes (apps/dashboard pipelineStage.ts
+ the ticket/mission REST projections) — render it, don't re-derive.
Section (2) needs a source: propose a tracked docs/operator-gates.md (or
frontmatter on a designated ticket) the command reads, so the list is
version-controlled, not hardcoded in the bridge. Companion to the
conversational slack-ask-conversational ticket (that one is LLM-backed
and costed; this one is pure rendering). Every surface parity: the same
snapshot should have a dashboard home widget, but that can ride later.

## Acceptance hints

- `/kranz status` returns the running mission, queue depth, per-stage
  counts, and the unmerged-set in one Slack message.
- `/kranz todo` lists pipeline actions awaiting the operator with tap
  affordances, plus the gated-items section read from a tracked source.
- No LLM/engine spawn — deterministic rendering; a test drives the
  formatter off a fixed pipeline state.
