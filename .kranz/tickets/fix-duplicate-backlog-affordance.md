---
title: Two "Backlog" affordances in the pipeline view mean different things
priority: 3
schedule: once
---

## Goal
The pipeline view header shows a "Backlog ↗" nav link (navigates to the
standalone BacklogPanel at #/backlog) AND a "Backlog" lens button (filters
the pipeline in-place to ticket rows). Same word, two behaviors, adjacent
— confusing. Reconcile: the Backlog LENS is canonical (one data model,
many lenses). Retire the standalone BacklogPanel + its nav link, OR — if
BacklogPanel carries ticket detail/actions the pipeline rows lack — move
those into the lens/row and relabel the link so it is not a second thing
called "Backlog".

## Context
Introduced 2026-07-07 by pipeline-view-surface-actionable (m-a11f17):
its ticket asked for both a Backlog lens and a link to BacklogPanel
without reconciling them. Violates the "one data model, many lenses, not
parallel views" principle (the same reason MissionPicker was deleted as
drift-prone dead code). Decide first whether BacklogPanel still earns its
existence now that the lens exists. Pure dashboard (apps/dashboard):
PipelineView header, App.tsx routing, BacklogPanel.

## Acceptance hints
- Only one "Backlog" affordance, or two clearly-distinct labels; no two
  identically-labelled controls with different behavior.
- If BacklogPanel is retired, its unique ticket affordances (if any) are
  preserved in the pipeline lens/rows first.
- cd apps/dashboard && npx tsc --noEmit && npm run test && npm run build pass.
