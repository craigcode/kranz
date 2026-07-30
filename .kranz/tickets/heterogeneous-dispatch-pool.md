---
title: Heterogeneous dispatch — one unit of work to N backends
priority: 2
schedule: once
---

## Goal
Dispatch a single unit of work to N configured backends on different models
and harnesses concurrently, collecting all outputs as sibling candidates
tied to one unit — never silently picking a winner.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-303). The existing
backend set (claude, codex, droid, kimi, local) already forms a
heterogeneous pool, so this is not blocked on the ACP worker (which widens
the pool later). Reuses M3 worktree isolation: one worktree per stream.
Diversity across harnesses is the point — differently-shaped scaffolding
produces differently-shaped errors; cross-provider scrutiny
(docs/reviews/ampcode.md §1) is the two-stream degenerate case. Cost
multiplies by N: plan approval must show the multiplier as part of spend
consent, and the per-mission budget applies to the sum.

## Acceptance hints
- Dispatching one brief to two mock backends yields two sibling run records
  linked to one unit id, each in its own worktree.
- Approval consent names N and the multiplied estimate; a single-backend
  config degrades to today's behavior (regression).
- One stream failing does not abort its sibling; both terminal states are
  recorded.
- Anti-vacuity grep on a named filter unique to this work.
