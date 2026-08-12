---
state: done
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

The fix is a **lens filter bar** on the pipeline view — one data source,
several filtered views — NOT new standalone list components (those drift
and rot: MissionPicker was exactly that and became unrouted dead code we
deleted 2026-07-06). Lenses:

| Lens | Shows | Answers |
|------|-------|---------|
| Actionable (default) | captured/reviewable/delivered/failed | "what needs me" |
| Backlog | tickets only | "what could I work" |
| Missions | running + recent missions | "what's executing" (the current-missions selector) |
| All | everything (today's behavior) | "full history" |

Concretely:
1. **Signal over history.** Default to the Actionable lens; terminal
   `landed`/`abandoned` rows are hidden there and visible under All (or a
   collapsed "Landed (N) ▸" group). The operator sees what needs doing on
   open.
2. **A Missions lens** = the "current missions button" — a filter to
   running + recent missions, one tap; clicking a row still opens the
   existing mission detail (#/m/<id>) for live monitoring/steering. No
   separate MissionPicker-style component.
3. **Reach the backlog.** A Backlog lens (tickets) plus a visible link to
   the full BacklogPanel (#/backlog), so it is discoverable, not orphaned.

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
Landed-row collapse and a backlog link restore that promise. Operator
follow-up 2026-07-06: "if pipeline is everything, do we also need a
current-missions selector?" — answer: yes, but as a Missions LENS on the
same data, not a separate component (MissionPicker's rot is the receipt).
Pure dashboard work (apps/dashboard): PipelineView.tsx, App.tsx nav,
styles.

## Acceptance hints

- A lens filter bar (Actionable | Backlog | Missions | All) filters the
  single pipeline data source; the default lens hides terminal rows so
  actionable work shows on open.
- The Missions lens shows running + recent missions and rows still open
  the existing #/m/<id> detail — no new standalone list component.
- A visible, discoverable affordance reaches the full backlog (#/backlog).
- cd apps/dashboard && npx tsc --noEmit && npm run test && npm run build
  all pass; component tests assert the default lens excludes landed rows,
  the Missions lens filters to missions, and the backlog link is present.
