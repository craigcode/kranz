# Mission plan — m-dffbad

**Goal:** Deleting a mission prunes its missions/index.md catalog line, and list surfaces render a residual orphan (index line without readable mission data) as an explicit 'deleted mission (no data recorded)' placeholder instead of an error; the dashboard missions header reads 'N finished · M running'.

Branch `kranz/mission-m-dffbad` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** Deleting a mission via the engine-level filesystem prune removes that mission's line from missions/index.md (line matched by the [<id>]( marker), leaving the header and every other line intact; a missing index or missing line is tolerated. Proven by an engine test against a tempdir repo. 
  `cargo test -p kranz-engine delete_prunes_missions_index 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a2]** The CLI clean/delete path (remove_missions) removes each deleted mission's line from missions/index.md, while still never touching branches or tags. Proven by a CLI integration test. 
  `cargo test -p kranz clean_prunes_missions_index 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a3]** The server delete path (MissionHost::clean, POST /api/missions/:id/delete) removes the deleted mission's line from missions/index.md. Proven by a server integration test. 
  `cargo test -p kranz-server delete_prunes_missions_index 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a4]** `kranz missions` lists a forged orphan (an id present in missions/index.md with no mission directory / no events.jsonl) and renders it as the literal 'deleted mission (no data recorded)' placeholder rather than an '(unreadable: ... not found ...)' error string. Proven by a CLI test. 
  `cargo test -p kranz missions_forged_orphan_renders_placeholder 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a5]** GET /api/missions includes a forged orphan (index line without a mission dir) as a placeholder row carrying goal 'deleted mission (no data recorded)' rather than a status:failed/error row. Proven by a server API test. 
  `cargo test -p kranz-server missions_forged_orphan_renders_placeholder 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a6]** A mission whose directory exists but whose events.jsonl is present-but-corrupt is still surfaced as an error (not masked as the 'deleted mission (no data recorded)' placeholder), preserving corruption for operator inspection. Proven by a CLI test. 
  `cargo test -p kranz corrupt_log_stays_error 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a7]** The dashboard missions header renders 'N finished · M running' (finished = complete/failed/abandoned; running = approved/running/paused/blocked/validating) with N and M matching the fixture mission set, and a placeholder/ghost row counts toward neither. Proven by a vitest test. 
  `cd apps/dashboard && npx vitest run src/components/MissionPicker.test.tsx`
- **[g1]** The whole Rust workspace test suite passes. 
  `cargo test --workspace 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[g2]** Clippy is clean across the workspace with warnings denied. 
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[g3]** Rust formatting is clean. 
  `cargo fmt --all --check`
- **[g4]** The dashboard TypeScript typechecks with no errors. 
  `cd apps/dashboard && npx tsc --noEmit`
- **[g5]** The dashboard production build succeeds. 
  `cd apps/dashboard && npm run build`

## Milestone 1 — Deleted missions are pruned from the catalog and residual ghosts render as placeholders, not errors

### 1.1 Engine: missions/index.md prune + id-parse helpers

In crates/engine/src/orchestrator.rs, beside the existing `upsert_mission_index` (~line 3249) and `mark_mission_index_report`, add two PURE functions and one filesystem helper. The catalog line format is `- <date> · [<id>](<id>/plan.md) — <goal>` and lines are matched by the marker `format!("[{id}](")` (same marker upsert/mark use). Union-merge-friendly newline-per-line format must be preserved.

1. `pub fn prune_mission_index(existing: &str, mission_id: &str) -> String` — return `existing` with the single line containing the `[<id>](` marker removed. Keep every other line and the header (`# Kranz missions\n\nApproved plans, newest last.\n`) byte-for-byte. Idempotent: pruning an id with no line returns the input unchanged (modulo trailing-newline normalization consistent with `mark_mission_index_report`, which pushes each kept line + '\n'). Empty/whitespace-only input returns unchanged.

2. `pub fn mission_index_ids(existing: &str) -> Vec<String>` — parse and return every mission id appearing as `[<id>](` in the catalog body, in file order, de-duplicated. Ignore the header and any non-matching lines. Extract the id as the text between `[` and `](` on lines that also contain `](`; be tolerant of the report-link suffix (` · [report](<id>/report.md)`) — the FIRST `[<id>](<id>/plan.md)` bracket on the line is the mission id, not `report`.

3. `pub fn prune_mission_index_file(repo_root: &std::path::Path, mission_id: &str)` — read `<repo>/.kranz/missions/index.md` (use `MissionPaths::new(repo_root, "_").missions_dir().join("index.md")` or equivalent), apply `prune_mission_index`, and write it back atomically-enough (a plain `std::fs::write` is fine — match how the index is written on approval). A missing index file is a no-op (return without error). Do NOT create the file if absent.

Export all three from the engine crate the same way `upsert_mission_index` is exported (it is used as `kranz_engine::orchestrator::upsert_mission_index` in tests).

TESTS (add to crates/engine/tests/mission_test.rs, near `mission_index_upserts_by_id` ~line 1864):
- A pure unit test that builds a two-line index via `upsert_mission_index`, prunes the first id, and asserts that id's line is gone, the other line and header remain, and pruning a non-existent id is a no-op.
- A unit test for `mission_index_ids` asserting it returns exactly the ids present (including for a line carrying a ` · [report](…)` suffix — the id is the plan.md bracket, not `report`).
- An integration test named EXACTLY `delete_prunes_missions_index` that: creates a tempfile::TempDir repo, writes `.kranz/missions/index.md` with two mission lines (via `upsert_mission_index`), calls `prune_mission_index_file(repo_root, "<id-1>")`, then reads the file back and asserts `<id-1>`'s line is gone and `<id-2>`'s line remains. Use the same construction style as existing engine tempdir tests.

Do not change event schemas, `upsert_mission_index`, or `mark_mission_index_report`. Run `cargo test -p kranz-engine`, `cargo clippy -p kranz-engine --all-targets -- -D warnings`, and `cargo fmt --all --check` before finishing.

Done when:
- `prune_mission_index(existing, id)` removes exactly the line matched by the `[<id>](` marker, preserves the header and all other lines, and is a no-op for an absent id or empty input
- `mission_index_ids(existing)` returns the mission ids present in the catalog in file order, taking the plan.md bracket (not a trailing `[report]` link) as the id
- `prune_mission_index_file(repo_root, id)` rewrites `.kranz/missions/index.md` with the id's line removed and is a no-op when the index file is absent
- An engine integration test named `delete_prunes_missions_index` against a tempdir repo proves a two-line index loses only the pruned mission's line
- cargo test -p kranz-engine, clippy -D warnings, and fmt --all --check all pass

### 1.2 Wire index pruning into the CLI and server delete/clean paths

Depends on the engine helpers from the previous feature (`kranz_engine::orchestrator::prune_mission_index_file`).

(1) CLI — crates/cli/src/commands.rs, `remove_missions` (~line 1023): after a mission directory is successfully `remove_dir_all`'d and pushed onto `removed`, also call `kranz_engine::orchestrator::prune_mission_index_file(repo, &e.id)` so the catalog line goes with the directory. Only prune ids that were actually removed (inside the `Ok(())` arm). Keep the existing behavior of never touching branches/tags and of skipping missions that became live. Update the now-stale doc comments on `remove_missions` and `cmd_clean` (~line 985) that claim the missions index is 'never touched' — they should say the deleted mission's own line is pruned while other lines, branches, and tags are left intact.

(2) Server — crates/server/src/host.rs, `MissionHost::clean` (~line 566): after the successful `std::fs::remove_dir_all(paths.mission_dir())`, call `kranz_engine::orchestrator::prune_mission_index_file(&self.repo_root, id)`. Update the stale doc comment (~line 564) that says missions/index.md is never touched.

TESTS:
- CLI: add a test named EXACTLY `clean_prunes_missions_index` to crates/cli/tests/cli_test.rs, modeled on the existing `remove_missions_deletes_selected_and_keeps_index_and_others` (~line 1160). Seed a repo whose `.kranz/missions/index.md` has lines for two missions and whose mission dirs exist; run the removal (via `remove_missions` or the clean command path used by that existing test) for ONE mission; assert the removed mission's line is gone from index.md while the other mission's line AND the header remain. (The existing test asserts index.md merely survives; the new one asserts the pruned line specifically is gone.)
- Server: add a test named EXACTLY `delete_prunes_missions_index` to crates/server/tests/host_test.rs, modeled on the existing approve/index test (~line 743-759 reads missions_dir().join("index.md")). Create a terminal (e.g. Abandoned or Failed, or Complete + all:true) mission with a committed index line, call the delete/clean host path (POST /api/missions/:id/delete or `MissionHost::clean(id, all)`), and assert its line is pruned from index.md while an unrelated mission's line remains.

Run `cargo test -p kranz`, `cargo test -p kranz-server`, clippy `-D warnings` on both crates, and `cargo fmt --all --check` before finishing.

Done when:
- CLI `remove_missions` prunes each successfully-removed mission's index.md line (only in the Ok removal arm) and still never touches branches/tags or lines of missions it skipped
- Server `MissionHost::clean` prunes the deleted mission's index.md line after removing its directory
- Stale doc comments claiming missions/index.md is 'never touched' are corrected in both crates
- CLI test `clean_prunes_missions_index` proves the pruned mission's line is gone and another mission's line + header remain
- Server test `delete_prunes_missions_index` proves the deleted mission's line is pruned while an unrelated line remains

### 1.3 kranz missions and GET /api/missions render residual ghosts as an explicit placeholder

Depends on `kranz_engine::orchestrator::mission_index_ids`. Goal: after this feature, both the CLI listing and the REST listing SEE ids that exist only in missions/index.md (residual ghosts arriving via git-history merges or partial deletes) and render them as the literal placeholder `deleted mission (no data recorded)` instead of an error — while a mission whose events.jsonl exists but is corrupt still surfaces as an error.

Define the ghost-vs-corrupt distinction precisely: a GHOST = an id whose `events.jsonl` file does NOT exist (whether the id came from the on-disk dir scan as a husk, or only from the index). A CORRUPT mission = `events.jsonl` exists but read/fold fails. Ghosts get the placeholder; corrupt missions keep the error.

(1) CLI — crates/cli/src/commands.rs, `cmd_missions` (~line 871): build the id set as the UNION of `MissionPaths::list_missions(repo)` and `mission_index_ids(<contents of .kranz/missions/index.md, or "" if absent>)`, sorted and de-duplicated. For each id: if its `events.jsonl` is absent, render a placeholder row — reuse the existing column layout but with a status label like `DELETED` and the goal text `deleted mission (no data recorded)` (no ticket suffix, no error). If `events.jsonl` exists, keep the current logic: `load_state` Ok → normal row; Err → the existing `(unreadable: {e:#})` error row. Detect 'absent events' by checking `MissionPaths::new(repo,&id).events_file().is_file()` BEFORE calling load_state, rather than string-matching the error.

(2) Server — crates/server/src/rest.rs, `list_missions` (~line 31): build the same union of on-disk ids and `mission_index_ids`. For an id whose `events_file()` is absent, push a placeholder row `{ "id": id, "status": "deleted", "goal": "deleted mission (no data recorded)" }` (no createdAt required). For an id with an events file, keep the current `fold_log` Ok → summary row / Err → `{ id, status: "failed", error }` behavior. Read the index via a helper that returns "" when the file is absent.

Do not change the MissionSummary/MissionState shapes otherwise; `status: "deleted"` is a new lenient string value (the TS side is handled in the dashboard feature).

TESTS:
- CLI: add `missions_forged_orphan_renders_placeholder` to crates/cli/tests/cli_test.rs — seed a repo with `.kranz/missions/index.md` containing a line for an id that has NO mission directory, run `cmd_missions`, and assert the output contains `deleted mission (no data recorded)` for that id and does NOT contain `unreadable` / `not found` for it.
- CLI: add `corrupt_log_stays_error` — create a mission dir whose `events.jsonl` exists but contains unparseable bytes, run `cmd_missions`, and assert that id's row is the error form (contains `unreadable`) and is NOT the placeholder.
- Server: add `missions_forged_orphan_renders_placeholder` to crates/server/tests/host_test.rs (or server_test.rs, wherever GET /api/missions is exercised — see server_test.rs ~line 194) — forge an index line with no mission dir, GET /api/missions, and assert the response array contains a row with that id whose goal is `deleted mission (no data recorded)` (and status `deleted`), not a status:failed/error row.

Run `cargo test -p kranz`, `cargo test -p kranz-server`, clippy `-D warnings`, and `cargo fmt --all --check` before finishing.

Done when:
- `cmd_missions` lists the union of on-disk mission ids and missions/index.md ids, de-duplicated and sorted
- An id with no events.jsonl renders as `deleted mission (no data recorded)` with a non-error status label; an id whose events.jsonl exists but is corrupt still renders the `(unreadable: …)` error row
- GET /api/missions returns a `{id,status:"deleted",goal:"deleted mission (no data recorded)"}` row for a forged orphan and keeps the status:failed/error row for a corrupt log
- CLI tests `missions_forged_orphan_renders_placeholder` and `corrupt_log_stays_error` pass
- Server test `missions_forged_orphan_renders_placeholder` passes

### 1.4 Dashboard: render the deleted-mission placeholder and the 'N finished · M running' header

Depends on the REST placeholder row shape from the previous feature (a GET /api/missions row can now be `{ id, status: "deleted", goal: "deleted mission (no data recorded)" }`).

Work in apps/dashboard/src/components/MissionPicker.tsx and add a small pure helper + a vitest.

(1) Counts helper — add a PURE, DOM-free function (either at the top of MissionPicker.tsx exported, or in a new apps/dashboard/src/lib/missionCounts.ts) e.g. `export function missionCounts(missions: {status:string}[]): { finished: number; running: number }` where finished counts status in {complete, failed, abandoned} and running counts status in {approved, running, paused, blocked, validating}. Any other status (planning, deleted, unknown) counts toward NEITHER. Keep the existing TERMINAL set semantics for the active/closed split unchanged.

(2) Header copy — replace the current closed-details summary text `{closed.length} closed mission{closed.length === 1 ? '' : 's'}` (MissionPicker.tsx ~line 156) so the missions view shows a header reading `${finished} finished · ${running} running` using the helper over the full `missions` array. Render it as a stable, test-targetable element (e.g. a `.picker-counts` span/summary containing exactly the text `N finished · M running`). The middot must be the '·' character to match the mission copy.

(3) Placeholder row — ensure a `status: "deleted"` row does not render as a broken/`?` row: classify `deleted` as its own bucket (neither active-actionable nor a normal closed row). Render its goal text (which the API already sets to `deleted mission (no data recorded)`) and its id, with a neutral status pill/label and NO abandon/delete action buttons (there is nothing on disk to delete). It should not crash on a missing createdAt (guard relTime for undefined/empty). Placeholder rows may be grouped under the closed/ghost section; they must not inflate the finished or running counts.

(4) Types — in apps/dashboard/src/lib/types.ts, MissionSummary.status is already a plain string, so no type change is required; if you narrow anywhere, include 'deleted'. Do not invent other fields.

TEST — add apps/dashboard/src/components/MissionPicker.test.tsx (vitest + @testing-library/react, modeled on StatusStrip.test.tsx) that:
- unit-tests `missionCounts` over a fixture mix (e.g. some complete/failed/abandoned, some approved/running/validating, one planning, one deleted) asserting finished and running match the expected numbers and that planning + deleted are excluded from both;
- renders MissionPicker (seed the zustand store's `missions` with the same fixture, following how other component tests provide store state) and asserts the header text `${finished} finished · ${running} running` is present with the correct numbers, and that a `deleted` row shows the `deleted mission (no data recorded)` text and no delete/abandon button.

Run `cd apps/dashboard && npx vitest run src/components/MissionPicker.test.tsx`, `npx tsc --noEmit`, and `npm run build` before finishing.

Done when:
- `missionCounts` counts finished = {complete,failed,abandoned} and running = {approved,running,paused,blocked,validating}, excluding planning and deleted from both
- The missions header renders exactly `N finished · M running` (with the '·' middot) using counts over the full missions array, replacing the old `N closed missions` copy
- A `status:"deleted"` row renders the `deleted mission (no data recorded)` text with no abandon/delete buttons and does not crash on missing createdAt
- MissionPicker.test.tsx asserts both the counts helper and the rendered header/placeholder against a fixture and passes under vitest
- npx tsc --noEmit and npm run build pass for the dashboard

