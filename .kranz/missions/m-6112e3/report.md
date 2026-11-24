# Mission report — m-6112e3

**Goal:** Record, for each captured lesson, whether a deterministic check could catch the same mistake (orchestrator classifies at capture, operator overrides by hand, operator wins), in a hand-readable companion file that never changes lesson bytes or injection, and add a read-only `kranz lessons list [--json] [--needs-check]` that lists checkable lessons with no linked check, oldest first.

Branch `kranz/mission-m-6112e3` (from `main`). Plan of record: [plan.md](plan.md).

## The plan

2 milestones, 4 features, gated by 13 contract assertions (11 command, 2 judgement).

1. **Lesson check records are parsed and listed read-only**
   - Engine: lesson check record model, parser and read-only listing — Repository: kranz (Rust workspace).
   - CLI: read-only `kranz lessons list` command — Repository: kranz (Rust workspace).
2. **Captured lessons carry the orchestrator's classification**
   - Capture turn records an orchestrator classification in the companion record — Repository: kranz (Rust workspace), engine crate `kranz-engine`.
   - Document lesson check records, the D-X decision and the plain-link route — Repository: kranz.

**Chosen approach:** Per-lesson companion record `.kranz/lessons/<id>.check` (plain key: value, non-.md), classified by both the orchestrator at capture (traili…
**Elapsed:** 3h 41m 21s
**Tokens:** 464 in / 137124 out / 15905632 cache read / 853119 cache write
**Cost:** $9.62 actual vs $7.52–$37.58 estimated (expected $17.22)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-6112e3-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Provider:** local-worktree (source isolation) · template: worktree · no workspace contract
- **Workspace contract:** no workspace contract (source isolation only)
- **Preflight:** preflight: 1 issue(s): [warn] command assertion [a7] uses '!', which was not found on PATH

## What shipped

### Milestone 1 — Lesson check records are parsed and listed read-only ✅

- ✅ **Engine: lesson check record model, parser and read-only listing** — 2 runs, 1 respawn
  Repository: kranz (Rust workspace).
  - `2047699` [f-1-1] add lesson check record model, parser and read-only listing
  - `4a68415` [f-1-1] treat unclassified as unset, sort orphan check warnings
  - ✓ cargo test -p kranz-engine --lib lesson_check_ passes with at least the seven named tests reported ok
  - ✓ A checkable record with no link appears in needs_check; adding a link removes it
  - ✓ Lessons with no .check file list as unclassified with source none
  - ✓ render_lessons_manifest output is byte-identical with and without .check files present
  - ✓ list_lessons leaves every file under the repo byte-identical and creates no lock or temp file
  - ✓ Malformed record lines produce warnings rather than silent classification
- ✅ **CLI: read-only `kranz lessons list` command** — 1 run
  Repository: kranz (Rust workspace).
  - `4914ab0` [f-1-2] add read-only `kranz lessons list` command
  - ✓ cargo test -p kranz --test cli_test lesson_check_ passes with the four named tests reported ok
  - ✓ Legacy lessons print as unclassified
  - ✓ --needs-check shows a checkable unlinked lesson and drops it once a link is recorded
  - ✓ --json output has lessons[] with lesson, missionId, summary, classification, source, link and a warnings[] array
  - ✓ Running the listing leaves the repo byte-identical and succeeds while live-PID engine lock files exist

### Milestone 2 — Captured lessons carry the orchestrator's classification ✅

- ✅ **Capture turn records an orchestrator classification in the companion record** — 2 runs, 1 respawn
  Repository: kranz (Rust workspace), engine crate `kranz-engine`.
  - `0d62740` [f-2-1] checkpoint (engine commit)
  - ✓ cargo test -p kranz-engine --lib lesson_check_ reports the five named capture/writer tests ok
  - ✓ cargo test -p kranz-engine --test mission_test lesson_check_ reports lesson_check_capture_writes_sidecar_in_report_commit ok
  - ✓ A trailing CHECK line is stripped from the lesson body and recorded as the orchestrator classification
  - ✓ A reply without a CHECK line yields today's body, no .check file and the unchanged four-file report commit
  - ✓ happy_path_completes_mission_with_tag_and_contract_gate still passes
  - ✓ prompts.rs, knowledge.rs, crates/engine/prompts/ and review_memory.rs have no diff
- ✅ **Document lesson check records, the D-X decision and the plain-link route** — 1 run
  Repository: kranz.
  - `c4dc64e` [f-2-2] document lesson check records, D-X and the plain-link route
  - ✓ docs/knowledge/lessons.md describes the .check format, operator-wins precedence, unclassified default, the listing command and flags, the D-X decision and the plain-link rationale, consistent with the code
  - ✓ lessons.md frontmatter last_verified is 2026-11-23 and verified_against includes crates/engine/src/lesson_checks.rs
  - ✓ The positioning note gains exactly one new Rechecked line and no other change
  - ✓ No code or .kranz files are changed by this feature

## Validation history

### ms-1 round 1 — Lesson check records are parsed and listed read-only

- [major] a13 / docs/knowledge/lessons.md documentation — The milestone's diff touches only crates/cli/src/cli.rs, crates/cli/src/commands.rs, crates/cli/tests/cli_test.rs, crates/engine/src/lesson_checks.rs, crates/engine/src/lessons.rs, crates/engine/src/l… [truncated]
- [critical] a13 — git diff 8ca522142980abe413e91011e67acf5d4aebb1cd..HEAD --name-only shows only crates/cli/src/cli.rs, crates/cli/src/commands.rs, crates/cli/tests/cli_test.rs, crates/engine/src/lesson_checks.rs, crat… [truncated]

Disposition: waived.
- a13 / docs/knowledge/lessons.md documentation: Too early, not missing: a13's docs are already scheduled as feature f-2-2 in milestone ms-2, the next milestone. That spec covers everything this finding asks for: the .check format, operator-wins precedence, the unclassified default, the `kranz lessons list` flags, the D-X decision, the plain-link rationale and the frontmatter updates. a13 is a mission-level judgement assertion with no milestone tag, so ms-1 is not where it gets judged. A fix-feature now would duplicate f-2-2. It also couldn't yet describe the capture-turn CHECK line accurately, because f-2-1 has not implemented it.
- a13: Duplicate of the finding above: same evidence (docs/knowledge/lessons.md unchanged after ms-1), same suggested fix. Waived for the same reason — the docs are planned for f-2-2 in ms-2, which runs after the capture change they must describe. The 'critical' severity reflects the validator not knowing about the later milestone, not a defect in ms-1's scope.

### ms-2 round 1 — Captured lessons carry the orchestrator's classification

No findings.

## Contract outcomes

- ✅ **[a1]** The engine lesson-check record tests pass: a checkable lesson with no link is in the needs-check set and adding a link removes it; legacy lessons without a companion record list as unclassified; the operator value overrides the orchestrator value; lesson rendering for planning is byte-identical with or without companion records; listing performs no filesystem writes. *(command: `out=$(cargo test -p kranz-engine --lib lesson_check_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && for t in lesson_check_checkable_without_link_needs_check lesson_check_link_removes_from_needs_check lesson_check_legacy_lessons_list_unclassified lesson_check_operator_overrides_orchestrator lesson_check_render_ignores_sidecars lesson_check_listing_performs_no_writes lesson_check_malformed_record_warns; do printf '%s\n' "$out" | grep -qE "^test (.*::)?$t \.\.\. ok$" || exit 1; done`)*
- ✅ **[a2]** The CLI `kranz lessons list` tests pass: legacy lessons print as unclassified; `--needs-check` shows a checkable unlinked lesson and drops it once a link is added; `--json` has the documented shape; the listing leaves every file under the repo byte-identical and succeeds while engine lock files held by a live PID are present. *(command: `out=$(cargo test -p kranz --test cli_test lesson_check_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && for t in lesson_check_cli_lists_legacy_as_unclassified lesson_check_cli_needs_check_drops_linked_lesson lesson_check_cli_json_output_shape lesson_check_cli_listing_is_read_only_and_lock_free; do printf '%s\n' "$out" | grep -qE "^test (.*::)?$t \.\.\. ok$" || exit 1; done`)*
- ✅ **[a3]** The capture-turn parsing tests pass: a trailing `CHECK:` line is stripped from the lesson body and becomes the orchestrator classification; a reply without it yields an unclassified lesson with an unchanged body; a NONE reply with a CHECK line captures nothing; the companion record write refuses symlinked destinations. *(command: `out=$(cargo test -p kranz-engine --lib lesson_check_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && for t in lesson_check_capture_strips_check_line lesson_check_capture_without_check_line_is_unclassified lesson_check_none_reply_with_check_line_is_none lesson_check_capture_malformed_check_line_is_stripped lesson_check_sidecar_write_refuses_symlink; do printf '%s\n' "$out" | grep -qE "^test (.*::)?$t \.\.\. ok$" || exit 1; done`)*
- ✅ **[a4]** A mission whose lesson reply ends with `CHECK: checkable` lands the lesson, the index entry and the companion `.check` record in the same `[kranz] mission report` commit, and the committed lesson body contains no CHECK line. *(command: `out=$(cargo test -p kranz-engine --test mission_test lesson_check_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && printf '%s\n' "$out" | grep -qE '^test (.*::)?lesson_check_capture_writes_sidecar_in_report_commit \.\.\. ok$'`)*
- ✅ **[a5]** The existing happy-path completion test still passes unchanged: a lesson reply with no CHECK line produces exactly the same four-file report commit as before. *(command: `out=$(cargo test -p kranz-engine --test mission_test happy_path_completes_mission_with_tag_and_contract_gate -- --exact 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. 1 passed'`)*
- ✅ **[a6]** The existing lessons, judgement and contract-sweep unit tests still pass. *(command: `out=$(cargo test -p kranz-engine --lib -- lessons:: judgement:: contract_sweep:: 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a7]** The `lesson_check_` test-name prefix is new: no test function with that prefix existed at the mission base, so the filters above match only this mission's tests. *(command: `! git grep -qE 'fn lesson_check_' $KRANZ_BASE_SHA -- crates`)*
- ✅ **[a8]** The frozen prompt and knowledge surfaces and the review-memory route are untouched: prompts.rs, knowledge.rs, crates/engine/prompts/ and review_memory.rs have no diff against the mission base. *(command: `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/prompts.rs crates/engine/src/knowledge.rs crates/engine/prompts crates/engine/src/review_memory.rs`)*
- ✅ **[a9]** Existing lesson files are not modified, deleted or renamed, and the existing lesson index is preserved as a byte prefix of the current one (backward compatible, no backfill rewrite). *(command: `test -z "$(git diff --name-only --diff-filter=DMR $KRANZ_BASE_SHA -- '.kranz/lessons/*.md' ':!.kranz/lessons/index.md')" && base=$(git show $KRANZ_BASE_SHA:.kranz/lessons/index.md) && cur=$(cat .kranz/lessons/index.md) && case "$cur" in "$base"*) exit 0;; *) exit 1;; esac`)*
- ✅ **[a10]** Clippy is clean with warnings denied for the engine and CLI crates, all targets. *(command: `cargo clippy -p kranz-engine -p kranz --all-targets -- -D warnings`)*
- ✅ **[a11]** Formatting is clean. *(command: `cargo fmt --all --check`)*
- ✅ **[a12]** The change stays on the governance side of the positioning boundary: the only new model-facing text is the capture-turn question asking for a CHECK line; the classification and link are never rendered into any planning, worker or validator prompt; no lint, preflight or gate rule is added or generated; lesson checks stay a plain ticket/rule link rather than reusing the review-memory replay-and-promotion route. *(agent judgement)*
- ✅ **[a13]** docs/knowledge/lessons.md documents, accurately against the code, the `.check` companion file format, the operator-wins precedence, that existing lessons read as unclassified, the `kranz lessons list [--json] [--needs-check]` command, the D-X decision (both classify, operator wins), and why lesson checks are a plain link rather than the review-memory route. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
