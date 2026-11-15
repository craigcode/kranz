# Reshaping a parked draft (scoping)

Status: scoped 2026-11-14. D-A is OPEN. This doc records the question
without deciding it; see docs/tickets.md's "Reshaping a parked draft" section
for the route operators use today.

## Why

A parked draft is a mission already APPROVED (plan committed on its mission
branch) whose linked ticket sits in REVIEW. The CLI's parked-draft hint
(crates/cli/src/backlog.rs) and docs/tickets.md now both point at the route
that works (add answers to the ticket, `kranz abandon <id>`, `kranz draft
<slug>`) because the two commands that look like reshaping actually refuse
a parked draft:

- **`kranz plan --mission <id>` refuses.** `cmd_plan` in
  crates/cli/src/commands.rs accepts only a mission whose status is
  `Planning` and errors otherwise. `approve_plan` (crates/engine/src/orchestrator.rs)
  is structurally once-only (it rejects any status but Planning), and the
  reducer (crates/engine/src/reducer.rs) never folds a mission back to
  Planning once `plan.approved` has landed.
- **`kranz revise` does not fit.** It targets a Running or Blocked mission,
  is extend-only (it proposes a revised remainder, not a replacement plan),
  and cannot change assertions already in the committed plan
  (docs/scoping/mid-mission-replanning.md D-E). A parked draft is Approved,
  not Running or Blocked, and its whole plan, not just the remainder, is
  what the operator wants to revisit.

## D-A. Reshape a parked draft in place? (OPEN)

**Today's route** (no code, already shipped): add answers under `##
Scoping answers` in the ticket, `kranz abandon <id>` (reconciles the ticket
to FAILED), `kranz draft <slug>` (a fresh mission, re-parks a new plan). This
is what the CLI hint and docs/tickets.md's "Reshaping a parked draft" section
say today.

**Options:**

1. **Keep abandon + redraft.** No code change. Trade-off: leaves an
   abandoned mission directory and branch behind for every reshape, and the
   operator runs two commands instead of one.
2. **A convenience verb**, e.g. `kranz redraft <slug>`, that abandons the
   parked mission and drafts again in one step. Trade-off: no contract
   change, since it composes the two existing operations, but it still produces a
   fresh mission id and branch each time, same as option 1 under the hood.
3. **True in-place reshape**: a feedback line → planning turn → re-parked
   plan, the shape docs/scoping/pipeline-view.md's "Reshape" verb already
   describes for the Reviewable row. Trade-off: needs a new reverse
   transition from Approved back to Planning, or a plan-replaced event:
   an additive change to the event/reducer contract
   (crates/engine/src/reducer.rs), not just CLI/UI wiring.

A "proposal" ranking among these is not offered here. Which option (or
none) to build is the operator's call.

**Related gaps, left unchanged pending this decision:**

- The dashboard's Reviewable row offers a secondary **Reshape** action
  (apps/dashboard/src/lib/pipelineStage.ts:122, wired in
  apps/dashboard/src/components/PipelineView.tsx's `renderSecondary`), but
  that link only opens the ticket detail page. For a REVIEW ticket the
  detail page offers Approve and no reshape action, so the button's label
  promises a route that does not exist yet.
- The Slack parked-draft message (`run_draft` in crates/slack/src/commands.rs)
  tells the operator the ticket is in review but names no reshape route.

Both are left as-is until D-A is decided; changing either now would commit
to an option this note is explicitly not choosing.
