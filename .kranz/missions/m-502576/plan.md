# Mission plan — m-502576

**Goal:** Turn cross-mission learning into a designed loop: on completion the orchestrator captures at most one reusable lesson into repo-level .kranz/lessons/ (or explicitly records none), and every future mission's planning seed is injected with a byte-capped index of recent lessons.

Branch `kranz/mission-m-502576` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The full engine test suite passes, including tests proving: the capture turn writes .kranz/lessons/<id>.md when the orchestrator returns prose; it writes nothing when the orchestrator returns NONE; the planning seed context includes the capped lessons index; and the injected lesson bytes stay under the cap even with many/large lessons. 
  `cargo test -p kranz-engine`
- **[a2]** The lessons-specific tests exist and pass in isolation (capture-on-prose, skip-on-NONE, seed-includes-capped-index, byte-cap-enforced). 
  `cargo test -p kranz-engine lessons`
- **[a3]** The engine crate is clippy-clean across all targets with warnings denied. 
  `cargo clippy -p kranz-engine --all-targets -- -D warnings`
- **[a4]** A committed lesson file at .kranz/lessons/ is tracked by git (not swallowed by .gitignore) so lessons survive mission deletion and kranz clean. 
  `git check-ignore -q .kranz/lessons/probe.md; test $? -eq 1`
- **[a5]** prompts/orchestrator.md documents both halves of the loop — the completion-time lesson capture and the planning-seed lessons index — and explains what the .kranz/lessons/ directory is. *(agent judgement)*
- **[a6]** docs/design.md records a deviation note describing the mission-to-mission lessons mechanism. 
  `grep -qi lesson docs/design.md`

## Milestone 1 — Lesson capture at mission completion

### 1.1 Capture-turn helper: write a lesson file and append the lessons manifest

Add capture logic to the engine (crates/engine/src/orchestrator.rs, plus a small helper module if cleaner) that runs ONE final orchestrator turn at mission completion and persists at most one reusable lesson.

Behaviour:
1. Add an async method on MissionEngine (e.g. `capture_lesson(&mut self) -> Option<Vec<PathBuf>>`) that issues a single orchestrator turn using the existing turn mechanism (see `orch_turn` / `json_decision` / `json_decision`-style helpers around orchestrator.rs:2404 and the final-gate turns near 2270-2331 for the pattern). The prompt MUST be exactly in spirit: "Distill at most ONE reusable lesson that a FUTURE mission in THIS repository would need — as a short imperative note — or reply with the single word NONE if there is nothing worth carrying forward." Give the turn the mission goal and a brief completion context.
2. Parse the reply. If the trimmed reply is exactly `NONE` (case-insensitive, ignoring surrounding whitespace/punctuation), write NOTHING and return None. Otherwise treat the reply prose as the lesson body.
3. On a non-NONE lesson: write the lesson body to `.kranz/lessons/<mission-id>.md` (create `.kranz/lessons/` if absent). The first line of the file must be a usable one-line summary of the lesson (the injection step reads the first line as the index entry) — if the model's prose does not start with a concise line, prepend/normalise so the first line is a short imperative summary. Then append one line to an append-only manifest `.kranz/lessons/index.md` recording this lesson in capture order — format `- <mission-id>.md · <first-line-summary>` (keep it single-line and stable; create the manifest if absent).
4. Return the list of filesystem paths written (the lesson file and the manifest) so the caller can commit them.

Constraints:
- The orchestrator NEVER writes files itself — the ENGINE writes the lesson file and manifest (same pattern as report.md in try_write_mission_report at orchestrator.rs:2354). The orchestrator turn only PRODUCES the lesson text.
- Best-effort by design: any error rendering/parsing/writing the lesson must be downgraded to a tracing::warn! and must NOT strand a mission that already passed its final gate — return None on error, mirroring write_mission_report's best-effort contract (orchestrator.rs:2337-2348).
- Do NOT change kranz clean and do NOT delete any lesson files anywhere. `.kranz/lessons/` is repo-level and deliberately survives mission deletion.
- Verify `.kranz/lessons/` is not matched by .gitignore (current ignores are only `.kranz/missions/*/...`); if a rule would swallow it, adjust .gitignore minimally so lesson files are tracked.

Use `crate::paths::MissionPaths` for path construction and add a `lessons_dir()` / `lessons_index()` accessor to paths.rs (kranz_dir().join("lessons") and lessons_dir().join("index.md")). Match existing code style, comment density, and error handling.

Done when:
- A unit test drives capture_lesson through the mock backend: when the mock orchestrator turn returns prose, `.kranz/lessons/<id>.md` exists with the lesson body and its first line is the summary, and `.kranz/lessons/index.md` gains one line referencing that file.
- A unit test drives capture_lesson through the mock backend returning exactly `NONE`: no lesson file is created and the manifest is not appended.
- A unit test asserts that when writing the lesson file fails or the turn errors, capture_lesson returns None and does not panic or strand the caller.
- paths.rs exposes lessons_dir() and lessons_index() accessors returning .kranz/lessons/ and .kranz/lessons/index.md.
- All new tests are discoverable under `cargo test -p kranz-engine lessons`.

### 1.2 Wire the capture turn into the completion path and commit with the report

Integrate the capture turn (from the previous feature) into the mission completion path in crates/engine/src/orchestrator.rs so it runs once, at completion, and its files are committed in the SAME commit as report.md.

Completion happens at two points in the final-contract path (around orchestrator.rs:2202-2264): the findings-empty branch and the FindingsConversion::Waive branch. In BOTH, the current sequence is `self.write_mission_report(); self.emit(EventKind::MissionCompleted{})`.

Changes:
1. Immediately before write_mission_report in each completion branch, call the async `capture_lesson()` turn to obtain `Option<Vec<PathBuf>>` of any lesson/manifest files written.
2. Thread those extra paths into the report commit: change `write_mission_report` / `try_write_mission_report` (orchestrator.rs:2344-2394) to accept the extra lesson paths and append them to the `commit` Vec passed to `self.repo.commit_paths(...)`, so the lesson file and manifest land in the existing `[kranz] mission report for <id>` commit. If there is no lesson (None / empty), the commit is unchanged. Keep the whole thing best-effort: a capture or commit failure must still allow MissionCompleted to be emitted.
3. Do NOT add a capture turn to the Blocked path — capture is a COMPLETION-only event.
4. Emit a lightweight OrchestratorDecision (or reuse an existing decision emission) noting whether a lesson was captured or NONE, so the event feed/report reflects it. Keep it non-fatal.

CRITICAL test-maintenance obligation: adding an orchestrator turn at completion changes the number of orchestrator turns every existing completion test expects. Audit all engine tests that drive a mission to Complete (findings-empty AND waive paths) and update their mock scripts (MockScript batches / responding(...)) so the extra capture turn is answered — default those existing tests' capture turn to return `NONE` unless the test is specifically about capture. The full `cargo test -p kranz-engine` suite MUST be green after this feature.

Done when:
- An integration-style test runs a mission to Complete via the findings-empty path with the mock capture turn returning prose, and asserts the lesson file and manifest are committed in the `[kranz] mission report for <id>` commit alongside report.md.
- A test covering the waive completion branch also triggers the capture turn (lesson captured or NONE) exactly once.
- No capture turn occurs on the Blocked / fix-cycle-cap path.
- All pre-existing completion tests are updated so `cargo test -p kranz-engine` is fully green with the added turn.
- The mission still reaches MissionCompleted even when the capture turn errors (best-effort verified by a test).


## Milestone 2 — Lessons index injected into the planning seed

### 2.1 render_lessons_index(): build a byte-capped recent-lessons index

Add a pure, unit-testable function to the engine (crates/engine/src/orchestrator.rs or a small sibling module, e.g. a `lessons` module) that renders the lessons index for injection into a planning seed.

Signature roughly: `fn render_lessons_index(repo_root: &Path) -> Option<String>` (return None/empty when there are no lessons).

Behaviour:
1. Read the append-only manifest `.kranz/lessons/index.md` for ordering (capture order; last line = newest). If the manifest is missing or empty, return None.
2. Take at most the 10 MOST RECENT lessons (last 10 manifest lines, newest first). For each, produce an index entry of `filename + first line` — read the first line from the actual `.kranz/lessons/<file>.md` (fall back to the manifest summary if the file is unreadable).
3. For the 3 NEWEST lessons, also include the FULL text of the lesson file body.
4. Enforce a HARD total byte cap of 2048 bytes on the ENTIRE rendered string. Build newest-first and stop adding content once the next addition would exceed the cap; the returned string MUST be <= 2048 bytes even with many and/or very large lesson files. Prefer keeping index entries over full bodies when space is tight (drop/truncate full bodies first). Never delete or modify any lesson file — capping is injection-only.
5. Wrap the output with a short header the orchestrator prompt can reference (e.g. a `## Lessons from past missions in this repo` heading) so the injected block is self-describing.

This function must be deterministic and side-effect-free (no filesystem writes). Add a `LESSONS_INJECT_MAX_BYTES` const = 2048.

Done when:
- A unit test writes a manifest + several lesson files and asserts the rendered index lists newest-first, includes up to 10 filename+first-line entries and full text for the 3 newest.
- A byte-cap test writes many and/or very large lesson files and asserts the rendered string length in bytes is <= 2048.
- render_lessons_index returns None (or empty) when .kranz/lessons/index.md is absent or empty.
- The function performs no filesystem writes (side-effect-free).
- Tests are discoverable under `cargo test -p kranz-engine lessons`.

### 2.2 Inject the lessons index into both planning-seed branches

Wire render_lessons_index (previous feature) into the orchestrator planning seed so every future mission's planning context carries the capped lessons index.

In crates/engine/src/orchestrator.rs `ensure_orchestrator()` (around 2449-2516) there are TWO planning seed constructions: the primary planning seed (~2463-2476) and the lost-planning fallback seed inside the resume-failed branch (~2488-2496). In BOTH, when the mission is in Planning, append the rendered lessons index (if any) to the seed string that is passed to `start_orchestrator`.

Details:
- Call render_lessons_index(&self.paths.repo_root) (or repo root accessor). If it returns Some(index), append it to the seed after the existing goal/instructions text, separated by a blank line. If None, leave the seed unchanged.
- Do NOT inject lessons into the resume-ack seed (the `The engine resumed this orchestrator session...` branch) or the non-planning digest reseed — lessons belong in the PLANNING seed only.
- The seed reaches the backend as `SessionSpec.prompt = PromptMode::Streaming(seed)`, which the mock records via `started_specs()`. Tests assert the injected index text is present in the started spec's streaming prompt.

Done when:
- A test seeds .kranz/lessons/ with lessons, starts a planning orchestrator via the mock backend, and asserts the started SessionSpec's streaming prompt contains the rendered lessons index (filename + first line and newest full text).
- A test with no lessons present asserts the planning seed is unchanged (no lessons header injected).
- The lessons index is NOT injected into the resume-ack seed nor the non-planning reseed path.
- cargo test -p kranz-engine remains fully green.


## Milestone 3 — Documentation of the learning loop

### 3.1 Document both halves in orchestrator.md and add the design.md deviation note

Document the mission-to-mission learning loop so it is discoverable and the design deviation is recorded.

1. crates/engine/prompts/orchestrator.md — add prose (do NOT disturb the existing KRANZ_BASE_SHA guidance; the prompts test at prompts.rs:59 requires every prompt still contain KRANZ_BASE_SHA) explaining BOTH halves of the loop from the orchestrator's point of view: (a) at mission completion the orchestrator will be asked for exactly ONE reusable lesson for a future mission in this repo, as a short imperative note, or the single word NONE — and what makes a lesson worth recording (a durable, repo-specific gotcha future planning needs, like the git-diff-vs-main race that m-c9c915 avoided), versus NONE; (b) during planning the seed may include a `Lessons from past missions in this repo` block drawn from .kranz/lessons/, and the orchestrator should treat those as hard-won constraints to honour when shaping the contract and plan. Explain plainly what the .kranz/lessons/ directory is: a repo-level, append-only store of distilled lessons that survives mission deletion and kranz clean, capped only at injection time.
2. docs/design.md — under the existing `## Deviations from the plan document (all deliberate)` section, add a note describing the lessons mechanism: repo-level .kranz/lessons/ with an append-only index.md manifest, one-lesson-or-NONE capture at completion committed with the report, and a byte-capped (2048) newest-first injection into the planning seed (<=10 entries, 3 newest full-text). Keep the tone and format consistent with the surrounding deviation notes.

Keep both edits concise and accurate to what the code actually does after milestones 1-2.

Done when:
- prompts/orchestrator.md explains the completion-time one-lesson-or-NONE capture and what makes a good lesson.
- prompts/orchestrator.md explains the planning-seed lessons index and that .kranz/lessons/ is a repo-level store surviving mission deletion.
- prompts/orchestrator.md still contains KRANZ_BASE_SHA (the prompts.rs test still passes).
- docs/design.md contains a deviation note mentioning the lessons mechanism (grep -qi lesson docs/design.md succeeds).
- cargo test -p kranz-engine remains green (prompt-hash / prompt-content tests updated if any assert exact text).

