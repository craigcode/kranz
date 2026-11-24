# Mission plan — m-6112e3

**Goal:** Record, for each captured lesson, whether a deterministic check could catch the same mistake (orchestrator classifies at capture, operator overrides by hand, operator wins), in a hand-readable companion file that never changes lesson bytes or injection, and add a read-only `kranz lessons list [--json] [--needs-check]` that lists checkable lessons with no linked check, oldest first.

Branch `kranz/mission-m-6112e3` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Authority granted on approval

Approving this plan grants the following to the mission's sessions with no further prompt.

**Commands** (3; each is a prefix, so `x` also allows `x <anything>`):
- `cargo run -p kranz -- lessons list`
- `cargo run -p kranz -- lessons list --needs-check`
- `cargo run -p kranz -- lessons list --json`

**Write paths** (11; glob patterns the mission may change):
- `crates/engine/src/lesson_checks.rs`
- `crates/engine/src/lessons.rs`
- `crates/engine/src/lib.rs`
- `crates/engine/src/judgement.rs`
- `crates/engine/src/orchestrator/finalization.rs`
- `crates/engine/src/contract_sweep.rs`
- `crates/engine/tests/mission_test.rs`
- `crates/cli/src/**`
- `crates/cli/tests/cli_test.rs`
- `docs/knowledge/lessons.md`
- `docs/knowledge/decisions/positioning-governance-evidence-layer.md`

## Cost estimate

Estimated **$7.52 – $37.58** (expected ~$17.22). Rough estimate — live usage is authoritative; based on 52 completed mission(s).

Context-fit check (corpus p90 over 64 plan(s): 4556 chars / 24 files):
- **f-1-1** (Engine: lesson check record model, parser and read-only listing) — spec is 5868 chars (corpus p90: 4556); spec points at 31 files (corpus p90: 24)
- **f-2-1** (Capture turn records an orchestrator classification in the companion record) — spec is 5098 chars (corpus p90: 4556); spec points at 27 files (corpus p90: 24)

## Considered alternatives

**Chosen approach:** Per-lesson companion record `.kranz/lessons/<id>.check` (plain key: value, non-.md), classified by both the orchestrator at capture (trailing CHECK line stripped by the engine) and the operator by hand with the operator winning, plus a read-only `kranz lessons list [--json] [--needs-check]`. Lesson bytes and index are untouched so provenance and planning injection are unchanged, concurrent missions never conflict on a shared file, and the D-X decision is recorded as 'both, operator wins' (the human can reject this plan at approval to choose orchestrator-only or operator-only). Lesson checks stay a plain ticket/rule link, not the review_memory route.

Rejected shapes:
- **Classification as frontmatter or a trailing line inside the lesson .md file** — Changes the bytes the planner inlines and that provenance compares, so it alters injection and an operator edit would change prompt text.
- **Extra column on index.md lines** — The index summary is the injection fallback text and the index is append-only; in-place operator edits break both.
- **Single append-only classification ledger file** — Every concurrent mission appends to the same file, producing merge conflicts at integration.
- **Reuse review_memory.rs replay suite and promotion approval** — That route promotes reviewer-prompt rule text (frozen prompt-shaping side); a lesson check is Rust code already approved through a mission gate and PR.
- **Add a `kranz lessons classify` writer command** — More surface and a write path needing its own locking/safety story; a hand-editable file plus listing warnings meets the goal.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The engine lesson-check record tests pass: a checkable lesson with no link is in the needs-check set and adding a link removes it; legacy lessons without a companion record list as unclassified; the operator value overrides the orchestrator value; lesson rendering for planning is byte-identical with or without companion records; listing performs no filesystem writes.
  `out=$(cargo test -p kranz-engine --lib lesson_check_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && for t in lesson_check_checkable_without_link_needs_check lesson_check_link_removes_from_needs_check lesson_check_legacy_lessons_list_unclassified lesson_check_operator_overrides_orchestrator lesson_check_render_ignores_sidecars lesson_check_listing_performs_no_writes lesson_check_malformed_record_warns; do printf '%s\n' "$out" | grep -qE "^test (.*::)?$t \.\.\. ok$" || exit 1; done`
- **[a2]** The CLI `kranz lessons list` tests pass: legacy lessons print as unclassified; `--needs-check` shows a checkable unlinked lesson and drops it once a link is added; `--json` has the documented shape; the listing leaves every file under the repo byte-identical and succeeds while engine lock files held by a live PID are present.
  `out=$(cargo test -p kranz --test cli_test lesson_check_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && for t in lesson_check_cli_lists_legacy_as_unclassified lesson_check_cli_needs_check_drops_linked_lesson lesson_check_cli_json_output_shape lesson_check_cli_listing_is_read_only_and_lock_free; do printf '%s\n' "$out" | grep -qE "^test (.*::)?$t \.\.\. ok$" || exit 1; done`
- **[a3]** The capture-turn parsing tests pass: a trailing `CHECK:` line is stripped from the lesson body and becomes the orchestrator classification; a reply without it yields an unclassified lesson with an unchanged body; a NONE reply with a CHECK line captures nothing; the companion record write refuses symlinked destinations.
  `out=$(cargo test -p kranz-engine --lib lesson_check_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && for t in lesson_check_capture_strips_check_line lesson_check_capture_without_check_line_is_unclassified lesson_check_none_reply_with_check_line_is_none lesson_check_capture_malformed_check_line_is_stripped lesson_check_sidecar_write_refuses_symlink; do printf '%s\n' "$out" | grep -qE "^test (.*::)?$t \.\.\. ok$" || exit 1; done`
- **[a4]** A mission whose lesson reply ends with `CHECK: checkable` lands the lesson, the index entry and the companion `.check` record in the same `[kranz] mission report` commit, and the committed lesson body contains no CHECK line.
  `out=$(cargo test -p kranz-engine --test mission_test lesson_check_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && printf '%s\n' "$out" | grep -qE '^test (.*::)?lesson_check_capture_writes_sidecar_in_report_commit \.\.\. ok$'`
- **[a5]** The existing happy-path completion test still passes unchanged: a lesson reply with no CHECK line produces exactly the same four-file report commit as before.
  `out=$(cargo test -p kranz-engine --test mission_test happy_path_completes_mission_with_tag_and_contract_gate -- --exact 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. 1 passed'`
- **[a6]** The existing lessons, judgement and contract-sweep unit tests still pass.
  `out=$(cargo test -p kranz-engine --lib -- lessons:: judgement:: contract_sweep:: 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
- **[a7]** The `lesson_check_` test-name prefix is new: no test function with that prefix existed at the mission base, so the filters above match only this mission's tests.
  `! git grep -qE 'fn lesson_check_' $KRANZ_BASE_SHA -- crates`
- **[a8]** The frozen prompt and knowledge surfaces and the review-memory route are untouched: prompts.rs, knowledge.rs, crates/engine/prompts/ and review_memory.rs have no diff against the mission base.
  `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/prompts.rs crates/engine/src/knowledge.rs crates/engine/prompts crates/engine/src/review_memory.rs`
- **[a9]** Existing lesson files are not modified, deleted or renamed, and the existing lesson index is preserved as a byte prefix of the current one (backward compatible, no backfill rewrite).
  `test -z "$(git diff --name-only --diff-filter=DMR $KRANZ_BASE_SHA -- '.kranz/lessons/*.md' ':!.kranz/lessons/index.md')" && base=$(git show $KRANZ_BASE_SHA:.kranz/lessons/index.md) && cur=$(cat .kranz/lessons/index.md) && case "$cur" in "$base"*) exit 0;; *) exit 1;; esac`
- **[a10]** Clippy is clean with warnings denied for the engine and CLI crates, all targets.
  `cargo clippy -p kranz-engine -p kranz --all-targets -- -D warnings`
- **[a11]** Formatting is clean.
  `cargo fmt --all --check`
- **[a12]** The change stays on the governance side of the positioning boundary: the only new model-facing text is the capture-turn question asking for a CHECK line; the classification and link are never rendered into any planning, worker or validator prompt; no lint, preflight or gate rule is added or generated; lesson checks stay a plain ticket/rule link rather than reusing the review-memory replay-and-promotion route. *(agent judgement)*
- **[a13]** docs/knowledge/lessons.md documents, accurately against the code, the `.check` companion file format, the operator-wins precedence, that existing lessons read as unclassified, the `kranz lessons list [--json] [--needs-check]` command, the D-X decision (both classify, operator wins), and why lesson checks are a plain link rather than the review-memory route. *(agent judgement)*
## Contract lint

Each `check: command` assertion above was run once against the untouched base tree at approval time. Suspects are assertions that already pass (or could not reach a verdict) before this plan's work lands — a possible polarity/vacuity bug in the assertion itself. This never blocks approval.

author-bug suspects (already pass / no verdict on the untouched base): [a5] out=$(cargo test -p kranz-engine --test mission_test happy_path_completes_mission_with_tag_and_contract_gate -- --exact 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. 1 passed'; [a6] out=$(cargo test -p kranz-engine --lib -- lessons:: judgement:: contract_sweep:: 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'; [a7] ! git grep -qE 'fn lesson_check_' $KRANZ_BASE_SHA -- crates; [a8] git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/prompts.rs crates/engine/src/knowledge.rs crates/engine/prompts crates/engine/src/review_memory.rs; [a9] test -z "$(git diff --name-only --diff-filter=DMR $KRANZ_BASE_SHA -- '.kranz/lessons/*.md' ':!.kranz/lessons/index.md')" && base=$(git show $KRANZ_BASE_SHA:.kranz/lessons/index.md) && cur=$(cat .kranz/lessons/index.md) && case "$cur" in "$base"*) exit 0;; *) exit 1;; esac; [a10] cargo clippy -p kranz-engine -p kranz --all-targets -- -D warnings; [a11] cargo fmt --all --check
base-expected-to-fail (benign): [a1] out=$(cargo test -p kranz-engine --lib lesson_check_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && for t in lesson_check_checkable_without_link_needs_check lesson_check_link_removes_from_needs_check lesson_check_legacy_lessons_list_unclassified lesson_check_operator_overrides_orchestrator lesson_check_render_ignores_sidecars lesson_check_listing_performs_no_writes lesson_check_malformed_record_warns; do printf '%s\n' "$out" | grep -qE "^test (.*::)?$t \.\.\. ok$" || exit 1; done; [a2] out=$(cargo test -p kranz --test cli_test lesson_check_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && for t in lesson_check_cli_lists_legacy_as_unclassified lesson_check_cli_needs_check_drops_linked_lesson lesson_check_cli_json_output_shape lesson_check_cli_listing_is_read_only_and_lock_free; do printf '%s\n' "$out" | grep -qE "^test (.*::)?$t \.\.\. ok$" || exit 1; done; [a3] out=$(cargo test -p kranz-engine --lib lesson_check_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && for t in lesson_check_capture_strips_check_line lesson_check_capture_without_check_line_is_unclassified lesson_check_none_reply_with_check_line_is_none lesson_check_capture_malformed_check_line_is_stripped lesson_check_sidecar_write_refuses_symlink; do printf '%s\n' "$out" | grep -qE "^test (.*::)?$t \.\.\. ok$" || exit 1; done; [a4] out=$(cargo test -p kranz-engine --test mission_test lesson_check_ 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed' && printf '%s\n' "$out" | grep -qE '^test (.*::)?lesson_check_capture_writes_sidecar_in_report_commit \.\.\. ok$'

Lint commands ran under shell: sh

named contract gates (defect classes):
- vacuous-filter: PASS
- wrong-polarity: PASS
- host-shell-syntax: PASS
- passes-on-base: FAIL
  [a5] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `out=$(cargo test -p kranz-engine --test mission_test happy_path_completes_mission_with_tag_and_contract_gate -- --exact 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. 1 passed'`
  [a6] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `out=$(cargo test -p kranz-engine --lib -- lessons:: judgement:: contract_sweep:: 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`
  [a7] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `! git grep -qE 'fn lesson_check_' $KRANZ_BASE_SHA -- crates`
  [a8] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/prompts.rs crates/engine/src/knowledge.rs crates/engine/prompts crates/engine/src/review_memory.rs`
  [a9] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `test -z "$(git diff --name-only --diff-filter=DMR $KRANZ_BASE_SHA -- '.kranz/lessons/*.md' ':!.kranz/lessons/index.md')" && base=$(git show $KRANZ_BASE_SHA:.kranz/lessons/index.md) && cur=$(cat .kranz/lessons/index.md) && case "$cur" in "$base"*) exit 0;; *) exit 1;; esac`
  [a10] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo clippy -p kranz-engine -p kranz --all-targets -- -D warnings`
  [a11] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo fmt --all --check`
- env-sensitive: PASS


## Milestone 1 — Lesson check records are parsed and listed read-only

### 1.1 Engine: lesson check record model, parser and read-only listing

Repository: kranz (Rust workspace). Engine crate `kranz-engine` at crates/engine. Lessons live in `.kranz/lessons/`: one `<mission-id>.md` per mission plus an append-only `index.md` with lines `- <mission-id>.md · <one-line summary>` (oldest first). Existing code: crates/engine/src/lessons.rs (safe no-follow lessons-dir access via cap-std: `open_lessons_dir`, `read_existing_regular`, `ensure_absent_or_regular`, `atomic_replace`, `parse_manifest_line`, `first_nonempty_line`; plus `write_lesson` and `render_lessons_manifest`).

HARD CONSTRAINTS: (1) Do NOT change the behaviour or output of `render_lessons_manifest` / `render_lessons_manifest_in_dir` or `write_lesson`. You may change visibility of private helpers in lessons.rs to `pub(crate)` so the new module can reuse them; do not otherwise edit their logic. (2) Do not touch crates/engine/src/prompts.rs, crates/engine/src/knowledge.rs, crates/engine/prompts/**, or crates/engine/src/review_memory.rs. (3) Do not edit any file under .kranz/lessons/ in the repo itself. (4) Every new test function name MUST start with `lesson_check_` (that prefix is unused anywhere today; contract filters rely on it).

Create crates/engine/src/lesson_checks.rs and register it in crates/engine/src/lib.rs as `pub mod lesson_checks;`. Design:
- Companion record file: `.kranz/lessons/<mission-id>.check` (deliberately NOT `.md`, so the renderer, which only reads `.md` names from the index, can never inject it). Plain text, hand-editable: blank lines and lines starting with `#` are ignored; every other line is `key: value` (split on the first `:`, trim both sides). Known keys: `orchestrator`, `operator`, `link`. Classification values (case-insensitive on parse): `checkable`, `not-checkable`, `unclassified`.
- `pub enum CheckClass { Checkable, NotCheckable, Unclassified }` with `as_str()` returning the kebab-case words, serde serialize as those strings.
- `pub struct LessonCheckRecord { pub orchestrator: Option<CheckClass>, pub operator: Option<CheckClass>, pub link: Option<String> }`. A field whose value is `unclassified` is treated as unset. An empty `link:` value is no link.
- Resolution: effective classification = operator value if set, else orchestrator value if set, else Unclassified. Expose `pub enum CheckSource { Operator, Orchestrator, None }` (serialize `operator`/`orchestrator`/`none`).
- `pub fn parse_check_record(text: &str) -> (LessonCheckRecord, Vec<String> /* warnings */)`: unknown keys, lines without `:`, and unknown classification values each produce a warning naming the line number and text; the offending field is left unset. Duplicate keys: last one wins, with a warning.
- `pub fn render_check_record(record: &LessonCheckRecord, mission_id: &str) -> String`: writes a leading `#` comment line explaining that the operator may set `operator:` and `link:` by hand and that the operator's value wins, then the set fields. Parse(render(r)) must round-trip.
- Listing: `pub struct LessonEntry { pub lesson: String /* e.g. m-abc.md */, pub mission_id: String, pub summary: String, pub classification: CheckClass, pub source: CheckSource, pub link: Option<String> }` and `pub struct LessonListing { pub entries: Vec<LessonEntry>, pub warnings: Vec<String> }`, both serde Serialize with camelCase. `pub fn list_lessons(repo_root: &Path) -> Result<LessonListing>`: open the lessons dir read-only (`open_lessons_dir(root, false)`; a missing `.kranz` or `.kranz/lessons` or missing index yields an empty listing, not an error), read `index.md` through the no-follow helper, iterate lines in file order (oldest first) using `parse_manifest_line` (skip unparsable lines with a warning), summary = first non-empty line of the lesson file read no-follow, falling back to the index summary. For each entry read `<mission-id>.check` no-follow if present; parse; attach warnings prefixed with the filename. A `.check` that is a symlink or otherwise not a regular file is ignored with a warning (never followed). `.check` files with no matching index entry produce an `orphan` warning. Do NOT filter by git provenance (this is an operator view of the tree). `pub fn needs_check(listing: &LessonListing) -> Vec<&LessonEntry>`: entries whose effective classification is Checkable and whose link is None, preserving index (oldest-first) order.
- The listing must not create, modify or delete any file, and must not acquire any engine lock (do not call event_log acquire, queue mutation/busy/merge locks, or open the event log).

Tests (in `#[cfg(test)] mod tests` of lesson_checks.rs, tempdir-based, names exact): `lesson_check_checkable_without_link_needs_check` (index with two lessons, one with `orchestrator: checkable` and no link → needs_check returns exactly it); `lesson_check_link_removes_from_needs_check` (same, then write `link: .kranz/tickets/x.md` into the record → needs_check empty, entry still lists as checkable with the link); `lesson_check_legacy_lessons_list_unclassified` (index + lesson files in the existing format, no .check files → every entry Unclassified, source None, no warnings); `lesson_check_operator_overrides_orchestrator` (orchestrator checkable + operator not-checkable → NotCheckable/Operator and not in needs_check; and the reverse case); `lesson_check_render_ignores_sidecars` (render_lessons_manifest output with `|_| true` predicate is byte-identical before and after adding .check files for every lesson); `lesson_check_listing_performs_no_writes` (snapshot every file path+bytes under the tempdir before/after list_lessons → identical, no lock or temp files created); `lesson_check_malformed_record_warns` (unknown value `maybe`, unknown key, no-colon line → warnings, field unset, entry Unclassified); plus a round-trip test `lesson_check_record_round_trips`. Run `cargo fmt` and `cargo clippy -p kranz-engine --all-targets -- -D warnings` before finishing.

Done when:
- cargo test -p kranz-engine --lib lesson_check_ passes with at least the seven named tests reported ok
- A checkable record with no link appears in needs_check; adding a link removes it
- Lessons with no .check file list as unclassified with source none
- render_lessons_manifest output is byte-identical with and without .check files present
- list_lessons leaves every file under the repo byte-identical and creates no lock or temp file
- Malformed record lines produce warnings rather than silent classification

### 1.2 CLI: read-only `kranz lessons list` command

Repository: kranz (Rust workspace). CLI crate is package `kranz` at crates/cli (test with `cargo test -p kranz`, never `-p kranz-cli`). Subcommands are declared in crates/cli/src/cli.rs (clap `Command` enum, with nested enums like `TicketCommand`) and dispatched in crates/cli/src/commands.rs (see `Command::Ticket { command } => dispatch_ticket(...)` and read-only commands such as `Command::Missions` and `Command::Provenance`). Integration tests live in crates/cli/tests/cli_test.rs and run the built binary against temp repos — follow the existing helpers and patterns there.

The engine (already implemented in this mission) exposes `kranz_engine::lesson_checks::{list_lessons, needs_check, LessonListing, LessonEntry, CheckClass, CheckSource}`. `list_lessons(repo_root)` returns entries in index order (oldest first) plus warnings; `needs_check(&listing)` returns the checkable entries with no link, same order. Do not modify engine code except trivially if an export is missing.

Add `kranz lessons list [--json] [--needs-check]`:
- `Command::Lessons { command: LessonsCommand }` with `LessonsCommand::List { #[arg(long)] json: bool, #[arg(long)] needs_check: bool }`. Doc comments for `--help`: list lessons with whether a deterministic check could catch the mistake; `--needs-check` shows only lessons classified checkable with no linked check, oldest first.
- Text output: one line per entry: `<lesson>  <classification> (<source>)  link: <link or ->  — <summary>`. With `--needs-check` and no matches print `no checkable lessons without a linked check`. With no lessons at all print `no lessons recorded`. Warnings go to stderr, each prefixed `warning: `. Exit 0 whenever the listing itself succeeds (warnings do not change the exit code).
- JSON output (`--json`): pretty JSON `{"lessons":[{"lesson":"m-x.md","missionId":"m-x","summary":"...","classification":"checkable|not-checkable|unclassified","source":"operator|orchestrator|none","link":null|"..."}],"warnings":[...]}`; with `--needs-check` the `lessons` array is the filtered set.
- The command is strictly read-only: it must not acquire any engine lock (mission `events.jsonl.lock`, `.kranz/queue/.mutate.lock`, `.repo.busy.lock`, `.repo.merge.lock`), must not open or create an event log, and must not create, modify or delete any file. Make sure whatever repo-discovery/startup path the CLI uses for this command does not write either.

HARD CONSTRAINTS: every new test function name MUST start with `lesson_check_`. Do not touch crates/engine/src/prompts.rs, crates/engine/src/knowledge.rs, crates/engine/prompts/**, crates/engine/src/review_memory.rs, or any file under the real repo's .kranz/lessons/.

Tests in crates/cli/tests/cli_test.rs (names exact): `lesson_check_cli_lists_legacy_as_unclassified` (temp repo with an index and two lessons in today's format, no .check files → exit 0, both printed as unclassified); `lesson_check_cli_needs_check_drops_linked_lesson` (one lesson with `<id>.check` containing `orchestrator: checkable` → `--needs-check` prints it; write `link: .kranz/tickets/x.md` into the record → `--needs-check` no longer prints it); `lesson_check_cli_json_output_shape` (parse `--json` output, assert keys and values above); `lesson_check_cli_listing_is_read_only_and_lock_free` (create `.kranz/missions/m-held/events.jsonl.lock` and `.kranz/queue/.mutate.lock`, `.kranz/queue/.repo.busy.lock`, `.kranz/queue/.repo.merge.lock`, each containing the test process's own live PID; snapshot every file path + bytes under the temp repo; run `lessons list`, `lessons list --needs-check` and `lessons list --json`; assert each exits 0 and the snapshot is identical afterwards — no file created, removed or modified). Run `cargo fmt` and `cargo clippy -p kranz --all-targets -- -D warnings` before finishing.

Done when:
- cargo test -p kranz --test cli_test lesson_check_ passes with the four named tests reported ok
- Legacy lessons print as unclassified
- --needs-check shows a checkable unlinked lesson and drops it once a link is recorded
- --json output has lessons[] with lesson, missionId, summary, classification, source, link and a warnings[] array
- Running the listing leaves the repo byte-identical and succeeds while live-PID engine lock files exist


## Milestone 2 — Captured lessons carry the orchestrator's classification

### 2.1 Capture turn records an orchestrator classification in the companion record

Repository: kranz (Rust workspace), engine crate `kranz-engine`. Lesson capture happens at mission completion: crates/engine/src/judgement.rs `prepare_lesson` (around line 305) sends one orchestrator turn asking for at most one lesson or NONE, then `is_none_reply` / `normalize_lesson_body`; `write_prepared_lesson` calls `lessons::write_lesson`, which returns the paths written (lesson file, index) and those paths are folded into the `[kranz] mission report` commit by `write_mission_report` (called from crates/engine/src/orchestrator/finalization.rs `complete_mission`, around lines 1075-1095; also `capture_lesson`/`try_capture_lesson` in judgement.rs). The record format and parser already exist in crates/engine/src/lesson_checks.rs (`CheckClass`, `LessonCheckRecord`, `parse_check_record`, `render_check_record`); the companion file is `.kranz/lessons/<mission-id>.check`.

Implement:
1. Extend ONLY the capture-turn message string in `prepare_lesson` (judgement.rs) with: if you give a lesson, end your reply with one final line, exactly `CHECK: checkable` if a deterministic check (an approval-lint rule, a preflight rule or a gate) could catch the same mistake, otherwise `CHECK: not-checkable`; omit it when replying NONE. Do NOT edit crates/engine/src/prompts.rs, crates/engine/src/knowledge.rs, crates/engine/prompts/** (including orchestrator.md) or crates/engine/src/review_memory.rs.
2. Parsing: trim the reply; if its last non-empty line, trimmed, starts with `CHECK:` (case-insensitive), remove that line. If its value (trimmed, case-insensitive) is `checkable` or `not-checkable` it becomes the orchestrator classification; any other value leaves the classification unset (the line is still stripped so it never pollutes the lesson body). Then apply the existing empty / `is_none_reply` / `normalize_lesson_body` logic to the remaining text exactly as today. A NONE reply (with or without a CHECK line) captures nothing. A reply with no CHECK line produces exactly today's lesson body and no classification.
3. Change `prepare_lesson` to return `Result<Option<PreparedLesson>>` where `pub(crate) struct PreparedLesson { body: String, check: Option<CheckClass> }`, and update `write_prepared_lesson`, `try_capture_lesson` and `complete_mission` accordingly. `write_prepared_lesson` still calls `lessons::write_lesson` unchanged, and when `check` is Some also writes `<mission-id>.check` (content from `render_check_record` with only `orchestrator` set) into the same active lessons dir using the same no-follow, symlink-refusing, atomic-replace discipline as lessons.rs (add a `pub(crate)` writer in lesson_checks.rs or lessons.rs that reuses `open_lessons_dir`, `ensure_absent_or_regular`, `atomic_replace`; do not change `write_lesson`'s behaviour or `render_lessons_manifest`). Append the `.check` path to the returned paths so it lands in the SAME report commit. When `check` is None, write no `.check` file (so the commit is exactly the current four files). Capture stays best-effort: a `.check` write failure must downgrade to a warning like the existing lesson path does, never block completion; prefer to still commit the lesson if only the record failed.
4. Confirm that the sweep in crates/engine/src/contract_sweep.rs does not exempt `.kranz/lessons/<id>.check` (`is_mission_record_path` must return false for it); add a unit test asserting that.

HARD CONSTRAINT: every new test function name MUST start with `lesson_check_`. The existing test `happy_path_completes_mission_with_tag_and_contract_gate` in crates/engine/tests/mission_test.rs (its lesson reply has no CHECK line and asserts a four-file report commit) must keep passing unchanged.

Tests: unit tests in judgement.rs (names exact): `lesson_check_capture_strips_check_line` (reply `Do X.\n\nCHECK: checkable` → body `Do X.`, check Some(Checkable)); `lesson_check_capture_without_check_line_is_unclassified` (body identical to today's normalization, check None); `lesson_check_none_reply_with_check_line_is_none` (`NONE\nCHECK: not-checkable` → None); `lesson_check_capture_malformed_check_line_is_stripped` (`CHECK: maybe` → stripped, check None). Unit test for the writer (name exact): `lesson_check_sidecar_write_refuses_symlink` (unix: a symlinked `<id>.check` destination is refused and the link target is untouched). Unit test `lesson_check_sweep_does_not_exempt_record` in contract_sweep.rs. Integration test in crates/engine/tests/mission_test.rs (name exact): `lesson_check_capture_writes_sidecar_in_report_commit` — model it on `happy_path_completes_mission_with_tag_and_contract_gate` (same mock backend/orch_script helpers) but with a lesson reply ending in `CHECK: checkable`; assert the HEAD report commit's files are exactly index.md, `<id>.md`, `<id>.check`, missions index.md and report.md; the committed lesson body contains no `CHECK:` line; the `.check` file parses to orchestrator Checkable; and `kranz_engine::lesson_checks::list_lessons` + `needs_check` on the repo return that lesson. Run `cargo fmt` and `cargo clippy -p kranz-engine --all-targets -- -D warnings` before finishing.

Done when:
- cargo test -p kranz-engine --lib lesson_check_ reports the five named capture/writer tests ok
- cargo test -p kranz-engine --test mission_test lesson_check_ reports lesson_check_capture_writes_sidecar_in_report_commit ok
- A trailing CHECK line is stripped from the lesson body and recorded as the orchestrator classification
- A reply without a CHECK line yields today's body, no .check file and the unchanged four-file report commit
- happy_path_completes_mission_with_tag_and_contract_gate still passes
- prompts.rs, knowledge.rs, crates/engine/prompts/ and review_memory.rs have no diff

### 2.2 Document lesson check records, the D-X decision and the plain-link route

Repository: kranz. Update docs/knowledge/lessons.md (a curated knowledge note with YAML frontmatter: title, owner, freshness, last_verified, verified_against; and dated `Rechecked YYYY-MM-DD (UTC) ...` lines at the top of the body, newest first). Read the implemented code first: crates/engine/src/lesson_checks.rs, the capture change in crates/engine/src/judgement.rs (`prepare_lesson`), and the CLI command in crates/cli/src/cli.rs / commands.rs. Make the documentation accurate to the code.

Changes:
1. Add a new newest-first `Rechecked 2026-11-23 (UTC) ...` line saying lessons gained an optional companion check record and a read-only listing; injection and provenance are unchanged. Set `last_verified: 2026-11-23` and add `crates/engine/src/lesson_checks.rs` to `verified_against`.
2. Add a section `## Could a check catch it?` (place it after `## Where the lessons live`) covering: the `.kranz/lessons/<mission-id>.check` file and its `key: value` format (`orchestrator`, `operator`, `link`; values `checkable`, `not-checkable`, `unclassified`; `#` comments), with a short example; precedence (operator wins over orchestrator; no file or no value = `unclassified`; all lessons captured before this change read as `unclassified`); why the record is a separate non-`.md` file (lesson bytes and index stay unchanged so provenance and planning injection are unaffected, and the renderer never reads it); that the orchestrator proposes a value at capture via a trailing `CHECK:` line that the engine strips; that operators classify or link by editing the file by hand and committing it; the listing `kranz lessons list [--json] [--needs-check]` (oldest first, read-only, takes no engine lock); the D-X operator decision recorded as: both classify, the operator's answer wins; and why lesson checks stay a plain ticket/rule link instead of reusing review_memory.rs's replay-suite-and-promotion route (that route promotes rule text fed to a reviewer model, which is prompt-shaping on the frozen side of docs/knowledge/decisions/positioning-governance-evidence-layer.md; a lesson check is deterministic Rust code that ships through a normal mission gate and PR review, which already supplies human approval; a replay suite of required/prohibited findings fits model-judged rules, not code checks). State that no lint rule is generated automatically.
3. Add one newest-first `Rechecked 2026-11-23 (UTC) ...` line to docs/knowledge/decisions/positioning-governance-evidence-layer.md stating the lesson check record and listing are read-only evidence/record surfaces; the only new model-facing text is the capture-turn CHECK question; no prompt-injection, lint-generation or execution primitive is added. Do not change anything else in that note.

Do not edit any code, any file under .kranz/, prompts.rs, knowledge.rs or crates/engine/prompts/**.

Done when:
- docs/knowledge/lessons.md describes the .check format, operator-wins precedence, unclassified default, the listing command and flags, the D-X decision and the plain-link rationale, consistent with the code
- lessons.md frontmatter last_verified is 2026-11-23 and verified_against includes crates/engine/src/lesson_checks.rs
- The positioning note gains exactly one new Rechecked line and no other change
- No code or .kranz files are changed by this feature
