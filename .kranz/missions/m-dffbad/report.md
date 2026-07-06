# Mission report — m-dffbad

**Goal:** Deleting a mission prunes its missions/index.md catalog line, and list surfaces render a residual orphan (index line without readable mission data) as an explicit 'deleted mission (no data recorded)' placeholder instead of an error; the dashboard missions header reads 'N finished · M running'.

Branch `kranz/mission-m-dffbad` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 56m 28s
**Tokens:** 29283 in / 103299 out / 10972103 cache read / 545417 cache write
**Cost:** $30.61 actual vs $6.12–$30.62 estimated (expected $12.25)

## What shipped

### Milestone 1 — Deleted missions are pruned from the catalog and residual ghosts render as placeholders, not errors ✅

- ✅ **Engine: missions/index.md prune + id-parse helpers** — 1 run
  - `d6d8fd2` [f-1-1] add missions/index.md prune + id-parse helpers
- ✅ **Wire index pruning into the CLI and server delete/clean paths** — 1 run
  - `4bb20b8` [f-1-2] prune missions/index.md line on CLI clean and server delete
- ✅ **kranz missions and GET /api/missions render residual ghosts as an explicit placeholder** — 1 run
  - `a1852e4` [f-1-3] render residual index-only ghosts as placeholder, not error
- ✅ **Dashboard: render the deleted-mission placeholder and the 'N finished · M running' header** — 1 run
  - `a42664c` [f-1-4] add missionCounts helper, deleted-mission placeholder row, header copy
- ✅ **Remove the duplicate 'N closed missions' count so only the 'N finished · M running' header remains** *(fix)* — 1 run
  - `a48ae6e` [ms-1-fix-1-1] replace closed-count summary with finished/running header

## Validation history

### ms-1 round 1 — Deleted missions are pruned from the catalog and residual ghosts render as placeholders, not errors

- [minor] f-1-4: header copy 'replacing the old N closed missions copy' — MissionPicker.tsx:156-158 adds a new span rendering '{finished} finished · {running} running', but the pre-milestone '{closed.length} closed mission{s}' copy is still present at line 162 in the closed… [truncated]

Disposition: 1 fix feature(s) created.

### ms-1 round 2 — Deleted missions are pruned from the catalog and residual ghosts render as placeholders, not errors

No findings.

## Contract outcomes

- ✅ **[a1]** Deleting a mission via the engine-level filesystem prune removes that mission's line from missions/index.md (line matched by the [<id>]( marker), leaving the header and every other line intact; a missing index or missing line is tolerated. Proven by an engine test against a tempdir repo. *(command: `cargo test -p kranz-engine delete_prunes_missions_index 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a2]** The CLI clean/delete path (remove_missions) removes each deleted mission's line from missions/index.md, while still never touching branches or tags. Proven by a CLI integration test. *(command: `cargo test -p kranz clean_prunes_missions_index 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a3]** The server delete path (MissionHost::clean, POST /api/missions/:id/delete) removes the deleted mission's line from missions/index.md. Proven by a server integration test. *(command: `cargo test -p kranz-server delete_prunes_missions_index 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a4]** `kranz missions` lists a forged orphan (an id present in missions/index.md with no mission directory / no events.jsonl) and renders it as the literal 'deleted mission (no data recorded)' placeholder rather than an '(unreadable: ... not found ...)' error string. Proven by a CLI test. *(command: `cargo test -p kranz missions_forged_orphan_renders_placeholder 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a5]** GET /api/missions includes a forged orphan (index line without a mission dir) as a placeholder row carrying goal 'deleted mission (no data recorded)' rather than a status:failed/error row. Proven by a server API test. *(command: `cargo test -p kranz-server missions_forged_orphan_renders_placeholder 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a6]** A mission whose directory exists but whose events.jsonl is present-but-corrupt is still surfaced as an error (not masked as the 'deleted mission (no data recorded)' placeholder), preserving corruption for operator inspection. Proven by a CLI test. *(command: `cargo test -p kranz corrupt_log_stays_error 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a7]** The dashboard missions header renders 'N finished · M running' (finished = complete/failed/abandoned; running = approved/running/paused/blocked/validating) with N and M matching the fixture mission set, and a placeholder/ghost row counts toward neither. Proven by a vitest test. *(command: `cd apps/dashboard && npx vitest run src/components/MissionPicker.test.tsx`)*
- ✅ **[g1]** The whole Rust workspace test suite passes. *(command: `cargo test --workspace 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[g2]** Clippy is clean across the workspace with warnings denied. *(command: `cargo clippy --workspace --all-targets -- -D warnings`)*
- ✅ **[g3]** Rust formatting is clean. *(command: `cargo fmt --all --check`)*
- ✅ **[g4]** The dashboard TypeScript typechecks with no errors. *(command: `cd apps/dashboard && npx tsc --noEmit`)*
- ✅ **[g5]** The dashboard production build succeeds. *(command: `cd apps/dashboard && npm run build`)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
