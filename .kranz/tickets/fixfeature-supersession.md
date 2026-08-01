---
title: "fixfeature supersession: a re-proposal supersedes the prior registration"
priority: 3
schedule: once
---

# fixfeature supersession

Source: mission m-83d1ed (2026-07-31). A crashed orchestrator re-proposed
fixfeature ms-3-fix-5-1 with a REVISED payload (same id, refined spec —
the normal shape of a re-plan after findings). The reducer treats any
payload-differing duplicate as corruption, which is right for genuine
shadowing but leaves no sanctioned path for "the same feature,
revised".

## Problem

A fixfeature id is derived from the milestone + ordinal (`ms-N-fix-M-K`),
so a re-plan after new findings naturally reuses ids. Today the only
options are: identical replay (idempotent, df6fa2e), or invalid state
(corruption). There is no way to revise a fixfeature's spec honestly
when the first attempt crashed before its run.

## Design (locked)

1. **New additive event `fixfeature.superseded`**: `{ milestoneId,
   featureId, reason }`. Emitted by the orchestrator when it revises an
   unstarted (Pending/Failed-with-no-commits) fixfeature. The reducer
   marks the prior registration superseded (kept in history) and allows
   the next `fixfeature.created` with the same id to fold as the
   successor.
2. **Guard rails**: supersession is rejected when the feature has
   started, has commits, or is complete — at that point a revision is a
   NEW feature id (the audit trail of work done must not be rewritten).
3. **Fold of `fixfeature.created` after a supersession**: the new
   registration records `supersedes: <event seq>` for provenance.

## Test gate

- `cargo test --workspace fixfeature_supersede 2>&1 | grep -qE 'test result: ok. [1-9]'`
  — supersede an unstarted feature then re-create with a new payload
  (folds, provenance recorded); supersede a started/committed feature
  (rejected); supersede a nonexistent id (rejected).
- Workspace gates green, bare exit codes, never piped.
