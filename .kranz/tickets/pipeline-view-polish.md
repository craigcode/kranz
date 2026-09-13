---
state: done
title: Pipeline view polish: error states, id truncation, action gating on dead rows
priority: 2
schedule: once
---

## Goal
Four render bugs from the view's first live morning (screenshot-verified): (1) artifact-fetch failures render error text overlapping the UNMERGED badge — failure states need their own layout slot; (2) mission ids truncate from the left when titles are absent; (3) abandoned missions and index ghosts are offered Merge/Iterate/Redraft actions — dead rows must be inert with an honest label; (4) the api fetch layer must detect non-JSON (HTML fallback) responses and render 'endpoint unavailable — server restart needed?' instead of a raw JSON parse exception, since every future binary upgrade reproduces the stale-serve mismatch.

## Context


Fifth bug (observed same morning): direct-fixed tickets — done state but
no linked mission (fix-work-branch-isolation etc.) — render as 'captured'
with a Draft button; the stage derivation must respect ticket state done
even when no missionId is recorded.

## Scoping answers

## Acceptance hints
