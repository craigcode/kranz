---
title: Draft must detect a plan-shaped NotReady reply instead of misfiling it as questions
priority: 2
schedule: once
---

## Goal
A pipeline-4 draft produced a complete plan but the orchestrator emitted it as prose; the draft core's NotReady path split it into 'questions' and appended a full plan JSON blob to the ticket's needs-context section. The engine draft core should detect a plan-shaped reply (the slack bridge's looks_like_plan_json heuristic exists for exactly this) and either re-prompt the orchestrator to return it through the plan channel (one bounded retry) or fail with an honest 'plan emitted as prose' error — never file JSON as questions. Also cap the size of what needs-context appends (a multi-KB blob in a ticket file is never a question).

## Context

## Scoping answers

## Acceptance hints
