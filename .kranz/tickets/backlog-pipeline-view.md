---
title: One pipeline view: the ticket-to-merge journey, visible and driveable
priority: 1
schedule: once
---

## Goal
The dashboard shows two disconnected lists (missions, tickets) while the product is actually ONE pipeline: draft -> plan parked -> review -> approve -> queued -> running -> done -> merged. Operators cannot see where any piece of work stands or what the next human action is. Build a pipeline view: one row per ticket showing its full journey as stage chips, with the stage-appropriate artifact and action inline — the parked plan and its estimate at REVIEW (rendered from the mission branch, no git spelunking), Approve when reviewable, queue position when queued, live progress when running, and an explicit UNMERGED badge with the merge command (or a gated merge action) when a mission is complete but its branch has not landed on the base. Rename or contextualize the two 'approve' verbs so plan-approval and ticket-queueing stop sharing a word.

## Context
Confusion inventory from one real day of operating (2026-07-06), each a
symptom of the missing pipeline view:
1. Approved-idle vs executing ambiguity cost a 32-minute stall (since
   fixed with the Approved status — but the UI still renders states, not
   the JOURNEY).
2. Plans/reports/estimates live on mission branches and in terminal
   scrollback; reviewing a parked plan requires git or less.
3. 'Approve' is two unrelated actions (plan commit; ticket queueing).
4. Complete-but-unmerged is invisible: m-bc11fb sat unmerged for hours
   while its stale build was being clicked in the browser.
5. The drain (kranz work) is invisible to the UI entirely.
6. Ghost/stale rendering (see fix-missions-list-ghosts,
   fix-blocked-chip-derived) compounds the distrust.

Foundations that make this ticket cheap now: GET /api/tickets carries
state + blockedBy (+ isBlocked once fix-blocked-chip-derived lands);
tickets record their mission (sidecar linkage); missions record their
branch and base; the WS feed streams run progress; the panel and detail
components exist (m-bc11fb). Most of this is REST projection (e.g. a
merged/unmerged bit = does base contain the mission branch tip — cheap
git probe server-side) plus one honest screen.

Suggest design-first: a short docs/scoping/pipeline-view.md with the
stage model and verb renames, reviewed by the operator BEFORE the build
mission — the verb rename especially is a product decision, not an
implementation detail.

Scoping doc written 2026-07-05 night: docs/scoping/pipeline-view.md —
stage model, gap map, three flagged decisions (D-A verb renames, D-B
serve autoWork drain resolving gascity D5, D-C gated merge that never
pushes), five-slice build plan. OPERATOR REVIEW of the three decisions
required before drafting this ticket.

## Scoping answers

## Acceptance hints
- From a fresh dashboard load, an operator can answer for every ticket: where is it in the pipeline, what artifact should I look at, and what is MY next action — without a terminal.
- The parked plan (and estimate, once recorded) renders inline at the REVIEW stage from the mission branch.
- Complete missions with unmerged branches show an explicit unmerged indicator; merged ones show merged.
- The two approve verbs are visually and verbally distinct.
- npx tsc --noEmit, npm run build, vitest suites pass; cargo test --workspace piped through grep -qE 'result: ok\. [1-9][0-9]* passed'; clippy -D warnings; fmt clean.
