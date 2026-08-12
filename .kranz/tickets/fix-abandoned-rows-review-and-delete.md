---
state: done
state-note: codex-built, opus-reviewed, gate-verified, merged
title: Abandoned pipeline rows need review + delete affordances (inert ≠ dead-end)
priority: 2
schedule: once
---

## Goal

Abandoned mission rows in the pipeline view are fully inert: no action,
and the id/title are plain text with no link, so there is no way to
review what the mission was or to remove it from the pipeline. It sits
forever (visible under the All lens). "Inert" should mean "no WORK
actions (Merge/Iterate/Redraft)" — not "no navigation and no cleanup."
Give abandoned rows:
1. **Review** — the id (and/or title) links to the mission detail
   (#/m/<id>) so the operator can read the husk (plan, events, why it
   died).
2. **Delete/dismiss** — a Delete action that removes the mission from
   the pipeline, reusing the existing mission-delete mechanism (m-dffbad
   prunes a deleted mission's missions/index.md line and list surfaces
   render no ghost). Confirm-gated like the abandon affordance.

## Context

Observed 2026-07-06: m-a6fd73 (an abandoned husk from a prose-plan draft
incident; the feature was redone as m-3b2f03) shows in the pipeline with
its goal text and no affordances. The operator asked "no way to review
or delete — needs a fix?" — correct. m-ba8d58 (pipeline-view-polish)
correctly removed the DISHONEST Merge/Iterate/Redraft actions from
abandoned/dead rows (PRIMARY_ACTIONS.abandoned = null) but went too far,
leaving no review or cleanup path. General nuance worth fixing at the
same time: NO pipeline row currently links to its detail via the id —
navigation is only via action buttons — so rows whose action is null
(abandoned, and landed-without-missionId) are unreachable. Make the id a
detail link for mission-backed rows generally. Delete reuses the
deleted-mission status/prune path (m-dffbad); abandon lives on the
mission detail StatusStrip (m-836e99) — the pipeline Delete should route
to or reuse that same host-side mechanism. Pure dashboard + existing
delete endpoint; no new engine primitive expected (verify at build).

## Acceptance hints

- An abandoned row's id links to #/m/<id> (review); a Delete action
  (confirm-gated) removes the mission and it disappears from the
  pipeline (index pruned, no ghost).
- Genuinely failed rows still offer Redraft; delivered/landed unchanged;
  no dishonest Merge/Iterate returns to abandoned rows.
- cd apps/dashboard && npx tsc --noEmit && npm run test && npm run build
  pass; a component test asserts the abandoned row has a detail link and
  a Delete affordance and NO Merge/Iterate/Redraft.
