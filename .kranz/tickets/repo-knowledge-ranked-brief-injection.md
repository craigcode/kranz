---
state: done
state-note: implemented
title: Ranked freshness-aware repo knowledge injection for planning
priority: 2
schedule: once
---

## Goal
Ship slice 2 of the repo knowledge store: inject a ranked, capped,
freshness-aware "Knowledge from this repo" block into initial planning and
revised planning only. Select reviewed Markdown notes from `docs/knowledge/`
with provenance visible, so the planner stops rediscovering stable repo facts
without drowning in stale global context.

## Context
Mission Control's Recall ranking is a useful mechanic reference; kranz keeps
committed Markdown under `docs/knowledge/` (slice 1 shipped). This ticket
implements **D-C** from `docs/scoping/repo-knowledge-store.md` — not a new
store, not SQLite, not worker/validator default injection.

Slice 3 (`kranz knowledge refresh` drift checks) stays out of scope here.

## Hard budgets (non-negotiable)
- Knowledge block: **≤ 4 KiB**, separate from the existing **≤ 2 KiB** lessons
  block. Never fold lessons into the knowledge budget.
- Missing vault: inject nothing; do not fail planning.
- Stale / unverified notes (`freshness: stale`, or failed/absent
  `verified_against`): **excluded** from automatic injection (still browsable
  on disk). Do not "label and include" stale notes by default.

## Selection order (deterministic)
1. Always include a truncated map from `docs/knowledge/index.md` (headings /
   top-level map only) — still under the 4 KiB total.
2. Notes explicitly referenced by ticket body or mission goal (path or title).
3. Notes whose `verified_against` paths overlap the ticket's likely touch set
   or changed files since base.
4. Stop when the budget is exhausted; prefer higher-ranked notes over partial
   truncation of many notes.

Workers/validators receive knowledge excerpts **only** when the approved plan
names them in a feature brief or validation assertion.

## Acceptance hints
- Planning and revised-planning seeds include the capped block with visible
  note paths + freshness/`verified_against` provenance.
- Unit tests pin: byte budget, selection order, stale exclusion, empty-vault
  no-op, and that lessons remain on a separate budget.
- Anti-vacuity: `cargo test --workspace <filter> 2>&1 | grep -qE 'test result: ok\. [1-9]'`.
