# Mission report — m-b66d34

**Goal:** Add a fourth single-shot agent backend `backend_kimi` driving the headless Kimi Code CLI (validator-first, worker-capable behind role config), grounded in a committed probe fixture, with full BackendKind config/readiness/cost/dashboard plumbing.

Branch `kranz/mission-m-b66d34` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 5h 40m 26s
**Tokens:** 120929 in / 555157 out / 58867967 cache read / 3127892 cache write
**Cost:** $248.13 actual vs $12.73–$63.64 estimated (expected $28.51)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-b66d34-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Preflight:** preflight: 1 issue(s): [warn] command assertion [a1] runs `cargo test` without anti-vacuity (`ok. [1-9]`); a zero-test filter would pass vacuously

## What shipped

### Milestone 1 — M1 — Probe: auth proof, captured fixture, documented schema (the gate) ✅

- ✅ **Kimi headless-auth probe, evidence doc, and captured fixture** — 1 run
  - `665e232` [f-1-1] probe Kimi Code CLI headless auth, capture fixture, document schema
- ✅ **Document the Init/Result synthesis seam and tool-shape capture gap in kimi-cli-backend.md** *(fix)* — 1 run
  - `58e3b53` [ms-1-fix-1-1] document synthesis seam and tool-shape evidence gap in kimi-cli-backend.md
- ✅ **Fix finding: f-1-1 fixture 'ending in a terminal Result' / contract a4 ('parses the committed fixture into AgentEvents (Init + terminal Result)')** *(fix)* — 1 run
  - `e891766` [ms-1-fix-1-2] add regression test guarding the Init/Result synthesis-seam note
- ✅ **Fix finding: f-1-1 'stream-json event schema' completeness / contract a5 (Init/Text/ToolUse/ToolResult/Result schema)** *(fix)* — 1 run
  - `eda6fcf` [ms-1-fix-1-3] add regression test guarding the ToolUse/ToolResult evidence-gap disclosure
- ✅ **Rename kimi_fixture_test.rs test functions with a kimi_ prefix so a4's `cargo test --workspace kimi` filter runs non-empty** *(fix)* — 1 run
  - `0ce5c1d` [ms-1-fix-2-1] prefix kimi_fixture_test.rs test functions with kimi_

### Milestone 2 — M2 — Parser: backend_kimi.rs grounded in the fixture ✅

- ✅ **Implement backend_kimi.rs single-shot backend and fixture tests** — 1 run
  - `c039883` [f-2-1] implement backend_kimi single-shot backend and fixture tests
- ✅ **Fix finding: f-2-1: Binary discovery honors KRANZ_KIMI_BIN (exclusive override) then PATH then well-known paths, proven by a unit test** *(fix)* — 1 run
  - `0372a22` [ms-2-fix-1-1] add unit test proving kimi discovery falls through configured -> PATH -> well-known

### Milestone 3 — M3 — Plumbing: BackendKind::Kimi across cost, config/types/orchestrator/readiness, and surfaces ✅

- ✅ **cost.rs kimi model family (additive)** — 1 run
  - `c7cf962` [f-3-1] add kimi model family pricing to cost.rs
- ✅ **BackendKind::Kimi variant and all engine-crate exhaustive-match arms (atomic)** — 3 runs, 2 respawns
  - `b81a0df` [f-3-2] fix DEFAULT_KIMI_MODEL to use kimi-code/ CLI id prefix
- ✅ **CLI ready lane, dashboard pickers, and docs (additive)** — 1 run
  - `937fb46` [f-3-3] wire kimi into CLI ready lane, dashboard pickers, and docs

### Milestone 4 — M4 — Integration: mock-configured kimi validator runs end-to-end ✅

- ✅ **Stub-driven kimi scrutiny validator harness test and full green gate** — 1 run
  - `21933c7` [f-4-1] add stub-driven kimi scrutiny validator harness tests
- ✅ **Fix f-4-1 KRANZ_KIMI_BIN test-isolation race (single shared mutex) and relocate out-of-touchSet harness fixtures inline** *(fix)* — 1 run
  - `7938feb` [ms-4-fix-1-1] unify kimi env-mutating test lock; inline stub fixtures
  - `6832a97` [ms-4-fix-1-1] fold the fixture inlining into the src changes
- ✅ **Fix finding: a1 / a4 / f-4-1 (all three validation commands pass) — cargo test --workspace and cargo test --workspace kimi** *(fix)* — 1 run
- ✅ **Fix finding: a4** *(fix)* — 1 run
- ✅ **Fix finding: crates/engine/tests/fixtures/kimi_exec_scrutiny_report.jsonl** *(fix)* — 1 run
- ✅ **Fix finding: crates/engine/tests/fixtures/kimi_exec_scrutiny_report_no_report.jsonl** *(fix)* — 1 run

## Validation history

### ms-1 round 1 — M1 — Probe: auth proof, captured fixture, documented schema (the gate)

- [major] f-1-1 fixture 'ending in a terminal Result' / contract a4 ('parses the committed fixture into AgentEvents (Init + terminal Result)') — crates/engine/tests/fixtures/kimi_exec_scrutiny.jsonl has two lines: {"role":"assistant","content":"OK"} and a {"role":"meta","type":"session.resume_hint",...} line. By the doc's own schema (kimi-cli-… [truncated]
- [minor] f-1-1 'stream-json event schema' completeness / contract a5 (Init/Text/ToolUse/ToolResult/Result schema) — Only Text ({role:assistant,content}) and the resume_hint terminal frame are backed by real captured stdout. Init, ToolUse, and ToolResult stdout wire shapes were never observed (kimi-cli-backend.md:13… [truncated]

### Final gate

- [critical] primary-checkout *(final gate)* — primary checkout has tracked changes while a worktree-mode mission is running

Disposition: 3 fix feature(s) created.

### ms-1 round 2 — M1 — Probe: auth proof, captured fixture, documented schema (the gate)

- [major] a4 — kimi test filter must execute non-empty — `cargo test --workspace kimi` runs 0 tests ('0 passed; ... filtered out' across every binary). The four tests added by ms-1-fix-1-2/1-3 in crates/engine/tests/kimi_fixture_test.rs are named `fixture_h… [truncated]
- [minor] placeholder — placeholder

Disposition: 1 fix feature(s) created.

### ms-1 round 3 — M1 — Probe: auth proof, captured fixture, documented schema (the gate)

- [minor] fix-1-2 / fix-1-3 regression guards (kimi_doc_explicitly_notes_the_init_and_result_synthesis_seam, kimi_doc_notes_tool_use_and_tool_result_need_a_second_capture) — crates/engine/tests/kimi_fixture_test.rs asserts the presence of specific magic substrings in docs/scoping/kimi-cli-backend.md (e.g. "synthesize at stream start", "route any unrecognized"+"AgentEvent:… [truncated]

Disposition: milestone blocked — 1 validation finding(s) but the fix-cycle cap (2) is reached

### ms-1 round 4 — M1 — Probe: auth proof, captured fixture, documented schema (the gate)

No findings.

### ms-2 round 1 — M2 — Parser: backend_kimi.rs grounded in the fixture

- [minor] f-2-1: Binary discovery honors KRANZ_KIMI_BIN (exclusive override) then PATH then well-known paths, proven by a unit test — crates/engine/src/backend_kimi.rs:763 `kimi_discovery_honors_env_override_exclusively` is the only discovery test; it asserts a broken KRANZ_KIMI_BIN fails immediately without falling through to `conf… [truncated]

Disposition: 1 fix feature(s) created.

### ms-2 round 2 — M2 — Parser: backend_kimi.rs grounded in the fixture

- [major] a4 — grep -riE 'backend.*kimi|kimi.*model|k3' crates/ finds only backend_kimi.rs, kimi_fixture_test.rs, and lib.rs's `pub mod backend_kimi;` — crates/engine/src/config.rs has zero kimi references. No test … [truncated]
- [major] a6 — `grep -rl "kimi" --include="*.md" docs/` returns only docs/scoping/kimi-cli-backend.md and docs/scoping/cursor-probe-evidence/preflight.md (an unrelated file). No config-documentation file states that… [truncated]
- [major] a7 — `grep -rl "kimi" --include='*.ts*'` across the repo returns no files. AgentBackend union, BACKEND_OPTIONS, MODEL_PLACEHOLDERS, dashboard picker tests, and docs/knowledge/glossary.md's Backend entry co… [truncated]

Disposition: waived.
- a4: Not an ms-2 regression: the ms-2-scoped half (backend_kimi parses the fixture into AgentEvents) is complete; config::validate kimi accept/reject handling + tests are already planned as f-3-2 (ms-3, pending) and the end-to-end mock/stub kimi validator harness test is f-4-1 (ms-4, pending). A fix-feature would duplicate planned work; final mission validation re-checks a4.
- a6: Not ms-2 scope: the config-documentation permission caveat (permission_mode/allowed_tools/etc. ignored on kimi; enforcement degrades to writable+OS sandbox+contract sweep) is already planned as f-3-3 (ms-3, pending).
- a7: Not ms-2 scope: adding kimi to the frontend AgentBackend union, BACKEND_OPTIONS, MODEL_PLACEHOLDERS, picker tests, and docs/knowledge/glossary.md's Backend entry is already planned as f-3-3 (ms-3, pending).

### ms-3 round 1 — M3 — Plumbing: BackendKind::Kimi across cost, config/types/orchestrator/readiness, and surfaces

No findings.

### ms-4 round 1 — M4 — Integration: mock-configured kimi validator runs end-to-end

- [critical] a1 / a4 / f-4-1 (all three validation commands pass) — cargo test --workspace and cargo test --workspace kimi — The new orchestrator tests mutate the process-global env var KRANZ_KIMI_BIN via KimiStubEnvGuard, serialized only on a fresh KIMI_ENV_LOCK (crates/engine/src/orchestrator.rs, new code). The pre-existi… [truncated]
- [critical] a4 — `cargo test --workspace kimi` (the exact command mapped to a4) failed 3 out of 3 consecutive runs with: thread 'backend_kimi::tests::kimi_discovery_falls_through_configured_to_path_then_well_known' pa… [truncated]

### Final gate

- [major] crates/engine/tests/fixtures/kimi_exec_scrutiny_report.jsonl *(final gate)* — commit 21933c7692858da00aa4b282fa87e3d18285580d ([f-4-1] add stub-driven kimi scrutiny validator harness tests) touched crates/engine/tests/fixtures/kimi_exec_scrutiny_report.jsonl which matches none … [truncated]
- [major] crates/engine/tests/fixtures/kimi_exec_scrutiny_report_no_report.jsonl *(final gate)* — commit 21933c7692858da00aa4b282fa87e3d18285580d ([f-4-1] add stub-driven kimi scrutiny validator harness tests) touched crates/engine/tests/fixtures/kimi_exec_scrutiny_report_no_report.jsonl which mat… [truncated]

Disposition: 5 fix feature(s) created.

### Final gate

- [major] crates/engine/tests/fixtures/kimi_exec_scrutiny_report.jsonl *(final gate)* — commit 21933c7692858da00aa4b282fa87e3d18285580d ([f-4-1] add stub-driven kimi scrutiny validator harness tests) touched crates/engine/tests/fixtures/kimi_exec_scrutiny_report.jsonl which matches none … [truncated]
- [major] crates/engine/tests/fixtures/kimi_exec_scrutiny_report_no_report.jsonl *(final gate)* — commit 21933c7692858da00aa4b282fa87e3d18285580d ([f-4-1] add stub-driven kimi scrutiny validator harness tests) touched crates/engine/tests/fixtures/kimi_exec_scrutiny_report_no_report.jsonl which mat… [truncated]

Disposition: waived.
- crates/engine/tests/fixtures/kimi_exec_scrutiny_report.jsonl: Net base..HEAD diff is touchSet-clean: this file was added by intermediate commit 21933c7 but fully deleted and its payload inlined into orchestrator.rs (in-touchSet) by ms-4-fix-1-1 — it does not exist in the final deliverable (verified absent from disk and from `git diff --name-only $KRANZ_BASE_SHA HEAD`). The finding fires on the per-commit sweep seeing the reverted intermediate commit, not on any real out-of-touchSet output; the touchSet-glob remedy was operator-denied 3× and a fresh worker cannot clear a historical-commit hit without unwarranted history rewriting.
- crates/engine/tests/fixtures/kimi_exec_scrutiny_report_no_report.jsonl: Same as its twin: added by 21933c7, then deleted and inlined into orchestrator.rs by ms-4-fix-1-1; absent from the net base..HEAD diff and from disk (verified). The finding is a per-commit-sweep artifact on the reverted intermediate commit, the touchSet-glob remedy was operator-denied, and no further worker action can clear it — waive.

## Contract outcomes

- ✅ **[a1]** The entire workspace test suite passes, exercising every kimi unit and integration test alongside the existing suite. *(command: `cargo test --workspace`)*
- ✅ **[a2]** Clippy is clean with warnings denied across the whole workspace and all targets. *(command: `cargo clippy --workspace --all-targets -- -D warnings`)*
- ✅ **[a3]** All Rust code is rustfmt-clean (CI enforces this gate). *(command: `cargo fmt --all --check`)*
- ✅ **[a4]** The kimi-specific tests run and pass: backend_kimi parses the committed fixture into AgentEvents (Init + terminal Result, usage populated iff the fixture carries it); binary discovery honors KRANZ_KIMI_BIN then PATH then well-known paths; config::validate accepts backend="kimi" model="k3" with each effort in {low,high,max} and rejects effort medium/xhigh for k3 and rejects an unsupported kimi model; and a mock/stub-configured kimi scrutiny validator completes end-to-end in the orchestrator harness. (This filter must execute non-empty; a1 is the full-suite backstop.) *(command: `cargo test --workspace kimi`)*
- ✅ **[a5]** docs/scoping/kimi-cli-backend.md plus the committed fixture document the probe evidence: auth classification (including whether auth survives a relocated $HOME), the stream-json Init/Text/ToolUse/ToolResult/Result event schema, the -m alias form selecting k3 and each effort level (low/high/max) and the kimi-for-coding[-highspeed] aliases, headless deny-rule/permission configuration, and whether the terminal Result carries token usage — with cost classified metered-vs-Meterless to match the backend_readiness "never invent a 0% quota bar" rule. No committed file leaks a real token or API-key value. *(agent judgement)*
- ✅ **[a6]** The config documentation states the kimi permission caveat mirroring codex/droid: Claude-CLI permission fields (permission_mode/allowed_tools/disallowed_tools/tools) are ignored on the kimi backend and enforcement degrades to the writable flag plus the OS sandbox plus the deterministic out-of-contract-write sweep. *(agent judgement)*
- ✅ **[a7]** The dashboard backend pickers and the glossary list kimi: 'kimi' is added to the AgentBackend union, BACKEND_OPTIONS, and MODEL_PLACEHOLDERS, the picker tests assert it, the frontend typechecks/builds, and docs/knowledge/glossary.md's Backend entry names kimi alongside claude/codex/droid. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
