# Mission report — m-502576

**Goal:** Turn cross-mission learning into a designed loop: on completion the orchestrator captures at most one reusable lesson into repo-level .kranz/lessons/ (or explicitly records none), and every future mission's planning seed is injected with a byte-capped index of recent lessons.

Branch `kranz/mission-m-502576` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 1h 04m 18s
**Tokens:** 49724 in / 183149 out / 20366217 cache read / 687214 cache write
**Cost:** $51.92 actual vs $11.20–$56.00 estimated (expected $22.40)

## What shipped

### Milestone 1 — Lesson capture at mission completion ✅

- ✅ **Capture-turn helper: write a lesson file and append the lessons manifest** — 1 run
  - `dbbd41f` [f-1-1] add capture_lesson helper for cross-mission lesson capture
  - `b2c4ce2` [f-1-1] checkpoint (engine commit)
- ✅ **Wire the capture turn into the completion path and commit with the report** — 1 run
  - `c53ff67` [f-1-2] checkpoint (engine commit)
- ✅ **Unhang the two parallel-batch completion tests and close f-1-2's capture-path test gaps** *(fix)* — 1 run
  - `83dd0d6` [ms-1-fix-1-1] fix hanging capture-turn tests; prove best-effort error path and prose findings-empty folding

### Milestone 2 — Lessons index injected into the planning seed ✅

- ✅ **render_lessons_index(): build a byte-capped recent-lessons index** — 1 run
  - `55d9bd4` [f-2-1] add render_lessons_index for byte-capped lessons injection
- ✅ **Inject the lessons index into both planning-seed branches** — 1 run
  - `7cb0fd2` [f-2-2] inject lessons index into planning seed and lost-planning fallback seed

### Milestone 3 — Documentation of the learning loop ✅

- ✅ **Document both halves in orchestrator.md and add the design.md deviation note** — 1 run
  - `698e8ec` [f-3-1] document cross-mission learning loop in orchestrator prompt and design deviation notes

## Validation history

### ms-1 round 1 — Lesson capture at mission completion

- [critical] a1 — full engine test suite passes / f-1-2 'All pre-existing completion tests are updated so cargo test -p kranz-engine is fully green' — `cargo test -p kranz-engine` FAILS: `parallel_batch_runs_both_features_and_leaks_no_worktrees` (mission_test.rs:2082) and `parallel_batch_sessions_overlap_in_wall_clock` (mission_test.rs:2204) both ti… [truncated]
- [critical] a1 & a2 — 'planning seed includes the capped lessons index' and 'injected lesson bytes stay under the cap' (seed-includes-capped-index, byte-cap-enforced) — The planning-seed injection half of the loop is entirely absent. `grep lesson` over crates/engine/src shows `lessons_index()` is only WRITTEN (orchestrator.rs:2467) and never read; digest.rs and promp… [truncated]
- [critical] a5 — prompts/orchestrator.md documents both halves of the loop and explains .kranz/lessons/ — The orchestrator prompt lives at crates/engine/prompts/orchestrator.md; the diff does not touch it, and `grep -in lesson crates/engine/prompts/orchestrator.md` returns nothing. Neither the completion-… [truncated]
- [critical] a6 — docs/design.md records a deviation note describing the mission-to-mission lessons mechanism — `grep -qi lesson docs/design.md` returns exit 1 (confirmed A6-FAIL-no-match). docs/design.md (119 lines) has a 'Deviations from the plan document' section ending at item 6 with no lessons note. The di… [truncated]
- [major] f-1-2 — 'mission still reaches MissionCompleted even when the capture turn errors (best-effort verified by a test)' — No integration test asserts the mission reaches MissionCompleted when the capture turn errors. The only error test, `lessons_capture_turn_error_returns_none` (orchestrator.rs:4448), checks the helper … [truncated]
- [minor] f-1-2 — integration test for the findings-empty path with capture returning prose asserting the report commit — The only test asserting the lesson file + index land in the `[kranz] mission report for <id>` commit alongside report.md is `waive_at_final_gate_completes_mission` (mission_test.rs:762-769), which rea… [truncated]
- [minor] a2 — 'byte-cap-enforced' test asserts the wrong cap — The `lessons_normalize_body_*` tests (orchestrator.rs:4469,4479) exercise LESSON_SUMMARY_CAP=140 (orchestrator.rs:3606) — a per-summary-LINE character cap on a single lesson's first line. This differs… [truncated]
- [critical] a1 — cargo test -p kranz-engine: 'test parallel_batch_runs_both_features_and_leaks_no_worktrees ... FAILED' and 'test parallel_batch_sessions_overlap_in_wall_clock ... FAILED', both panicking with 'run mus… [truncated]
- [critical] a6 — grep -qi lesson docs/design.md returned exit 1 (no match found) — confirmed both via the exact contract invocation and a direct `grep -n -i lesson docs/design.md` which produced zero output lines.
- [major] a5 — The orchestrator prompt lives at crates/engine/prompts/orchestrator.md (not prompts/orchestrator.md at repo root). `grep -n -i lesson crates/engine/prompts/orchestrator.md` produced zero matches — the… [truncated]

Disposition: 1 fix feature(s) created.

### ms-1 round 2 — Lesson capture at mission completion

- [critical] a6 — docs/design.md deviation note describing the lessons mechanism — The allowed contract command `grep -qi lesson docs/design.md` exits 1 (no match). docs/design.md is not in the milestone diff (`git diff --name-only` lists only slack-threads.json, backend_mock.rs, or… [truncated]
- [critical] a5 — prompts/orchestrator.md documents both halves of the loop — `grep -ni lesson crates/engine/prompts/orchestrator.md` returns nothing; the file was not modified in the milestone. Neither the completion-time lesson capture, the planning-seed lessons index, nor th… [truncated]
- [critical] a1/a2 — planning seed includes the capped lessons index; injected lesson bytes stay under the cap — No implementation reads the lessons index into any seed. In orchestrator.rs:2545-2557 the Planning seed is built solely from `self.state.mission.goal`; `paths.lessons_index()` is only ever read inside… [truncated]

Disposition: waived.
- a1/a2 — planning seed includes the capped lessons index; injected bytes under cap (seed-includes-capped-index, byte-cap-enforced): Injection is the whole scope of pending milestone ms-2 (f-2-1 render_lessons_index + f-2-2 seed wiring), not yet run; a fix-feature duplicates queued work. Re-enforced at the final contract gate.
- a5 — prompts/orchestrator.md documents both halves and explains .kranz/lessons/: Documentation is pending milestone ms-3 (f-3-1), which edits crates/engine/prompts/orchestrator.md. Re-checked at the final agent-judgement gate.
- a6 — docs/design.md deviation note: Also f-3-1 (ms-3) scope; the design.md deviation note is an explicit f-3-1 criterion. Re-checked by the a6 grep at the final contract gate.

### ms-2 round 1 — Lessons index injected into the planning seed

- [critical] a6 — docs/design.md deviation note for the lessons mechanism — `grep -qi lesson docs/design.md` returns non-zero (NO MATCH). `git diff 83dd0d6..HEAD -- docs/` is empty — the milestone range touches no docs. The only 'lesson' hits in docs/ are unrelated pre-existi… [truncated]
- [critical] a5 — orchestrator.md must document both halves of the lessons loop — crates/engine/prompts/orchestrator.md (the actual prompt file; the contract's `prompts/orchestrator.md` path does not exist) contains zero lessons content — `grep -niE 'lesson|capture|past mission|\.k… [truncated]
- [major] a4 — a committed lesson file tracked by git so lessons survive kranz clean — `git ls-files .kranz/lessons/` is empty and `git ls-files .kranz/` shows no lessons entries — no lesson file is committed/tracked. The a4 command passes only vacuously: .gitignore has no rule for .kra… [truncated]

Disposition: waived.
- a5 — orchestrator.md documents both halves of the lessons loop: Pending milestone ms-3 (f-3-1) edits crates/engine/prompts/orchestrator.md; not an ms-2 defect. Re-verified at the final agent-judgement gate.
- a6 — docs/design.md deviation note: Also f-3-1 (ms-3) scope — an explicit f-3-1 criterion. Re-checked by the a6 grep at the final contract gate.
- a4 — a committed lesson file tracked by git so lessons survive kranz clean / mission deletion: Contract-strength critique, not a code defect: capture writes .kranz/lessons/<id>.md and f-1-2 unit-tests prove it folds into the report commit, so real completions commit tracked lesson files; the dir is structurally repo-level (outside .kranz/missions/*/ that clean/delete touch) and not gitignored. This mission's own completion will commit .kranz/lessons/m-502576.md, realizing a4 for real. Not worth a fresh worker.

### ms-3 round 1 — Documentation of the learning loop

No findings.

## Contract outcomes

- ✅ **[a1]** The full engine test suite passes, including tests proving: the capture turn writes .kranz/lessons/<id>.md when the orchestrator returns prose; it writes nothing when the orchestrator returns NONE; the planning seed context includes the capped lessons index; and the injected lesson bytes stay under the cap even with many/large lessons. *(command: `cargo test -p kranz-engine`)*
- ✅ **[a2]** The lessons-specific tests exist and pass in isolation (capture-on-prose, skip-on-NONE, seed-includes-capped-index, byte-cap-enforced). *(command: `cargo test -p kranz-engine lessons`)*
- ✅ **[a3]** The engine crate is clippy-clean across all targets with warnings denied. *(command: `cargo clippy -p kranz-engine --all-targets -- -D warnings`)*
- ✅ **[a4]** A committed lesson file at .kranz/lessons/ is tracked by git (not swallowed by .gitignore) so lessons survive mission deletion and kranz clean. *(command: `git check-ignore -q .kranz/lessons/probe.md; test $? -eq 1`)*
- ✅ **[a5]** prompts/orchestrator.md documents both halves of the loop — the completion-time lesson capture and the planning-seed lessons index — and explains what the .kranz/lessons/ directory is. *(agent judgement)*
- ✅ **[a6]** docs/design.md records a deviation note describing the mission-to-mission lessons mechanism. *(command: `grep -qi lesson docs/design.md`)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
