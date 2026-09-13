---
state: done
title: Deleted missions: prune index.md and render ghosts honestly
priority: 2
schedule: once
---

## Goal
Mission m-2bc647 appears in the missions catalog with an unreadable '?' entry: mission delete removes the mission directory but never prunes its missions/index.md line, and every list surface (kranz missions, dashboard picker, App Home) renders the orphan as an error/'?'. Fix both ends: delete prunes the mission's index.md entry, and list surfaces render a residual ghost as an explicit 'deleted mission (no data recorded)' placeholder instead of an error string. Also fix the missions header copy: 'N closed missions' becomes 'N finished · M running'.

## Context
m-2bc647: kranz missions prints '(unreadable: mission not found ...)' and
the dashboard picker shows '?' — the mission dir was deleted (web delete /
kranz clean) but upsert_mission_index (crates/engine/src/orchestrator.rs)
only ever ADDS catalog lines; nothing prunes on delete. Fix at both ends:
(1) the delete/clean paths remove the mission's line from
missions/index.md (keep the union-merge-friendly format); (2) list
surfaces — cmd_missions (crates/cli/src/commands.rs), GET /api/missions,
dashboard picker, App Home — render a residual orphan as an explicit
'deleted mission (no data recorded)' placeholder rather than an error
string (ghosts can still arrive via git history or partial deletes).
Header copy: the missions header currently says 'N closed missions';
change to 'N finished · M running' (finished = terminal states, running =
everything post-approval and live). Small, cosmetic-plus-hygiene; no
event schema changes.

## Scoping answers

## Acceptance hints
- Deleting a mission removes its missions/index.md line (engine test with a tempdir repo, passed-count guard on the suite).
- A forged orphan (index line without a mission dir) renders the placeholder in kranz missions output and in GET /api/missions (tests at both layers).
- Dashboard header shows 'N finished · M running' with counts matching the fixture state (vitest).
- cargo test --workspace piped through grep -qE 'result: ok\. [1-9][0-9]* passed'; clippy -D warnings; fmt clean; npx tsc --noEmit and npm run build pass.
