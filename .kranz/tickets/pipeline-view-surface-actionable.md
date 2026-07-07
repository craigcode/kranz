---
title: Pipeline view must surface actionable work and reach the backlog list
priority: 2
schedule: once
---

## Goal

The default pipeline view (#/) renders every work item across all nine
stages, so dozens of terminal `landed` rows bury the few actionable ones
(captured/reviewable/delivered/failed). And there is no top-level
navigation affordance to the browsable ticket backlog list (#/backlog,
BacklogPanel) — it is reachable only by typing the hash or via a single
ticket's back-link. Fix both so an operator opening the dashboard
immediately sees what needs doing and can reach the full backlog.

Two changes:
1. **Signal over history.** Default the pipeline view to actionable
   stages; collapse terminal `landed` rows under a "Landed (N) ▸"
   expander (or a filter/toggle), so captured/reviewable/delivered/
   failed are what shows first. Preserve the ability to see everything.
2. **A front door to the backlog.** Add a visible nav link/tab from the
   pipeline header to #/backlog (and ideally a reciprocal link back), so
   the ticket backlog list is discoverable, not orphaned.

## Context

Observed live 2026-07-06: after the M7 train, the pipeline landing shows
~30 `landed` rows with only 2 `captured` tickets among them; the
operator asked "what happened to the ticket backlog view." BacklogPanel
is intact and routed at #/backlog (App.tsx:50) but nothing links to it
from the pipeline view — the consolidation that replaced the old two-
list landing (MissionPicker + BacklogPanel) dropped the backlog's nav
entry point. This directly undermines the pipeline-view scoping's
"done when": "the pipeline view answers where is it, what's next, whose
move is it without a second question" (docs/scoping/pipeline-view.md).
Landed-row collapse and a backlog link restore that promise. Pure
dashboard work (apps/dashboard): PipelineView.tsx, App.tsx nav, styles.

## Acceptance hints

- Opening #/ shows actionable stages first; terminal landed rows are
  collapsed/filtered by default with a way to expand them.
- A visible, discoverable navigation affordance reaches #/backlog from
  the pipeline view.
- cd apps/dashboard && npx tsc --noEmit && npm run test && npm run build
  all pass; a component test asserts landed rows are collapsed by
  default and the backlog nav link is present.
