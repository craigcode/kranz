---
title: Ticket state must distinguish Delivered (unmerged) from Landed (merged)
priority: 2
schedule: once
---

## Goal

A ticket's state flips to `done` when its mission reaches completion,
independent of whether the mission branch has been merged to main. So
the backlog shows a mission as `done` while it is actually Delivered-
but-unmerged, awaiting a human merge. Split the terminal states (or add
a merged bit to the ticket projection) so the ticket surfaces honestly
distinguish Delivered from Landed, exactly as the pipeline view's
mission rows already do via the merged-ancestor probe.

## Context

Hit live 2026-07-06: after the overnight drain, four tickets read `done`
while all four missions were still unmerged (verified via
`git merge-base --is-ancestor`). The pipeline view (mission rows) got
this right — Delivered shows UNMERGED, Landed shows merged — because
m-836459/m-ba8d58 built the merged bit. The TICKET projection never
adopted it: ticket state collapses both into `done`. Reuse the existing
merged probe (crates/server rest/tickets projection + the engine
is_ancestor primitive) so `/kranz todo`, the backlog panel, and Slack
all show a ticket as landed only once its mission is actually on the
base branch. This is the honesty gap the whole pipeline-view scoping
exists to close, still open on the ticket surface.

## Acceptance hints

- A ticket whose mission is complete-but-unmerged renders as Delivered
  (or done+UNMERGED), NOT plain done, on every surface that shows ticket
  state; it flips to Landed only when the mission branch is an ancestor
  of the live base.
- Reuses the existing merged/is_ancestor probe (no new git logic).
- Test: a completed-but-unmerged mission's ticket projection reports the
  unmerged/Delivered state; after merge it reports Landed.
