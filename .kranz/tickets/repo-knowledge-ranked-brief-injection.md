---
title: Ranked freshness-aware repo knowledge injection for planning
priority: 2
schedule: once
---

## Goal
Ship slice 2 of the repo knowledge store: inject a ranked, capped, freshness
aware "Knowledge from this repo" block into initial planning and revised
planning. The injected block should select only relevant reviewed Markdown
notes from `docs/knowledge/`, with provenance and freshness visible, so the
planner stops rediscovering stable repo facts without drowning in stale global
context.

## Context
Mission Control's Recall/code-graph brief has useful mechanics: pinned facts,
type weights, recency, usage counts, confidence, stale penalties, and an
"architecture at a glance" summary. Kranz already chose a better canonical
store for its lane: committed Markdown under `docs/knowledge/`, with generated
indexes treated as disposable cache.

Borrow the ranking and freshness posture, not the SQLite memory system. The
prompt-side rule from `docs/scoping/repo-knowledge-store.md` still stands:
small, evidence-linked excerpts enter planning; workers and validators receive
knowledge only when the approved plan calls for it.

## Acceptance hints
- Planning and revised-planning seeds include a capped knowledge block whose
  sources are visible by note path and `verified_against` metadata.
- Selection always includes the knowledge index map, then relevant notes based
  on ticket text, goal text, repo refs, changed files, and verified paths.
- Stale or unverified notes are either excluded or clearly labeled.
- Tests pin budgeting, ranking, stale-note behavior, and absence of prompt
  injection when the vault is missing.
- The knowledge block is additive to the existing lessons block, with separate
  budgets.
