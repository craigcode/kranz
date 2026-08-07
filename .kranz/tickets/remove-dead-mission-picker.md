---
state: done
title: Remove dead MissionPicker component (replaced by PipelineView)
priority: 3
schedule: once
---

## Goal

apps/dashboard/src/components/MissionPicker.tsx is no longer routed —
App.tsx renders PipelineView at the root and BacklogPanel at #/backlog;
MissionPicker survives only in comments. Delete the component and its
tests, and update the comments in PipelineView.tsx/BacklogPanel.tsx
that reference it.

## Context

Found during the m-ba8d58 merge review (2026-07-06): a global
.picker-item CSS change would have regressed MissionPicker's abandon
button — dead code that still attracts review effort and CSS coupling
is a hazard, not a spare part. Verify nothing else imports it (grep for
MissionPicker outside its own file/test) before deleting; the abandon
affordance it carried must already exist in the pipeline view or the
mission detail view — confirm before removal, and if it doesn't exist
anywhere else, STOP and report instead of deleting.

## Acceptance hints

- MissionPicker.tsx and its test file deleted; no remaining imports.
- Abandon-mission affordance confirmed available on a live surface
  (pipeline view or mission detail) before deletion.
- cd apps/dashboard && npx tsc --noEmit && npm run test && npm run
  build all pass.
