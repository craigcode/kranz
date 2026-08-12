---
state: done
state-note: Done at c19c163: cap-std no-follow idiom (symlink_metadata per segment + open_dir_nofollow) guards mission dir resolution, event log acquire/reads, state snapshot reads, control enqueue, REST resolver; catalog ids filtered through is_safe_id at parse time (all four consumers). Symlink tests cfg(unix)-gated per lessons pattern. Tests 2>&1 | grep -qE 'test result: ok. [1-9]'
title: No-follow symlink checks on .kranz and mission paths; validate catalog IDs (P1)
priority: 1
schedule: once
---

## Goal
Mission path handling must not follow symlinks: list_missions, control
enqueue, and runtime file access reject symlinked .kranz, missions dirs,
mission directories, and runtime files. Catalog/repo IDs are validated
(is_safe_id) before use everywhere they are consumed (REST catalog IDs
included).

## Context
From the review (P1 #6): lesson_provenance_clean already implements the
capability-based no-follow idiom — extend it to the mission path layer
(paths.rs, control.rs, rest.rs catalog consumption).

## Acceptance hints
- A symlinked .kranz/missions/<id> is rejected with a clear error on
  enumerate, read, and control-enqueue — never followed into another repo.
- Catalog IDs with separators/traversal rejected before any FS access.
- cargo test --workspace green.
