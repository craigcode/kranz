---
title: "Emit must not leave an unfoldable event in the log (emit-poison wedge)"
priority: 1
schedule: once
---

# Emit must not leave an unfoldable event in the log

Source: mission m-83d1ed (2026-07-31). An orchestrator proposed
fixfeature ms-3-fix-5-1, crashed, re-proposed a REVISED payload for the
same id, and the engine appended the event then failed the fold
(duplicate-with-different-payload). The append-only log was left holding
an event that can NEVER fold — every subsequent run wedged on replay
("invalid state for this operation: duplicate fixfeature.created").
Recovery required surgery on the audit log itself.

## Problem

`emit()` appends first and folds second. A fold-failure after a
successful append poisons the log permanently: the event is durably
recorded but makes the state machine unloadable. The append-only
guarantee (never lose an event) and the fold guarantee (every event is
reducible) can both be kept only if the append is CONDITIONAL on the
fold — or the fold result precedes the append.

## Design (locked)

1. **Fold-validate before append**: `emit()` computes the fold against
   the CURRENT in-memory state WITHOUT committing it; only on success
   does it append + fold-for-real + snapshot. A fold-invalid event is
   rejected at emit time (error to the caller, nothing appended) — the
   log can never contain an unfoldable event.
2. **The reducer stays total for HISTORY**: events already in old logs
   keep their current folding semantics (no behavior change on replay —
   the idempotent fixfeature rule from df6fa2e already covers the
   realistic duplicate class).
3. **Honest cost acknowledged in the comment**: computing the fold twice
   (validate + apply) is the price of the invariant; on a hot path it is
   still trivial next to an agent turn.

## Test gate

- `cargo test --workspace emit_never_appends 2>&1 | grep -qE 'test result: ok. [1-9]'`
  — an emit that would fail the fold appends NOTHING (log byte-identical,
  state unchanged, error surfaces); a valid emit appends + folds +
  snapshots exactly once.
- Workspace gates green, bare exit codes, never piped.
