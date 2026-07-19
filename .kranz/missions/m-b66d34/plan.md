# Mission plan — m-b66d34

**Goal:** Add a fourth single-shot agent backend `backend_kimi` driving the headless Kimi Code CLI (validator-first, worker-capable behind role config), grounded in a committed probe fixture, with full BackendKind config/readiness/cost/dashboard plumbing.

Branch `kranz/mission-m-b66d34` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$12.73 – $63.64** (expected ~$28.51). Rough estimate — live usage is authoritative; based on 44 completed mission(s).

## Considered alternatives

**Chosen approach:** Probe-first (M1 as a hard gate), then a fixture-grounded parser (M2), then split the compilation-atomic BackendKind plumbing from the additive cost and CLI/frontend/docs surfaces (M3), then a stub-driven end-to-end harness (M4) — mirroring the shipped codex/droid path so every line of code is grounded in a real captured fixture and the mission can honestly defer if headless auth cannot be proven.

Rejected shapes:
- **Implement the backend and all plumbing first, capture the fixture later.** — Violates probe-first discipline and risks shipping a parser against a guessed stream-json schema (cursor's exact failure mode); metered-vs-Meterless cost cannot be classified without the real Result object.
- **One mega-feature adding backend_kimi plus the enum, all plumbing, and surfaces together.** — Exceeds a fresh session's 50-turn budget and couples the compilation-atomic enum change with independent additive work (cost, CLI/frontend surfaces), making any respawn all-or-nothing.
- **Skip the probe and hand-author a fixture from official Kimi docs.** — An unverified schema passes unit tests yet can diverge from the live CLI (wrong event shapes / usage fields), and the Meterless-vs-metered cost decision would be a guess.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The entire workspace test suite passes, exercising every kimi unit and integration test alongside the existing suite. 
  `cargo test --workspace`
- **[a2]** Clippy is clean with warnings denied across the whole workspace and all targets. 
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[a3]** All Rust code is rustfmt-clean (CI enforces this gate). 
  `cargo fmt --all --check`
- **[a4]** The kimi-specific tests run and pass: backend_kimi parses the committed fixture into AgentEvents (Init + terminal Result, usage populated iff the fixture carries it); binary discovery honors KRANZ_KIMI_BIN then PATH then well-known paths; config::validate accepts backend="kimi" model="k3" with each effort in {low,high,max} and rejects effort medium/xhigh for k3 and rejects an unsupported kimi model; and a mock/stub-configured kimi scrutiny validator completes end-to-end in the orchestrator harness. (This filter must execute non-empty; a1 is the full-suite backstop.) 
  `cargo test --workspace kimi`
- **[a5]** docs/scoping/kimi-cli-backend.md plus the committed fixture document the probe evidence: auth classification (including whether auth survives a relocated $HOME), the stream-json Init/Text/ToolUse/ToolResult/Result event schema, the -m alias form selecting k3 and each effort level (low/high/max) and the kimi-for-coding[-highspeed] aliases, headless deny-rule/permission configuration, and whether the terminal Result carries token usage — with cost classified metered-vs-Meterless to match the backend_readiness "never invent a 0% quota bar" rule. No committed file leaks a real token or API-key value. *(agent judgement)*
- **[a6]** The config documentation states the kimi permission caveat mirroring codex/droid: Claude-CLI permission fields (permission_mode/allowed_tools/disallowed_tools/tools) are ignored on the kimi backend and enforcement degrades to the writable flag plus the OS sandbox plus the deterministic out-of-contract-write sweep. *(agent judgement)*
- **[a7]** The dashboard backend pickers and the glossary list kimi: 'kimi' is added to the AgentBackend union, BACKEND_OPTIONS, and MODEL_PLACEHOLDERS, the picker tests assert it, the frontend typechecks/builds, and docs/knowledge/glossary.md's Backend entry names kimi alongside claude/codex/droid. *(agent judgement)*

## Milestone 1 — M1 — Probe: auth proof, captured fixture, documented schema (the gate)

### 1.1 Kimi headless-auth probe, evidence doc, and captured fixture

Probe the Kimi Code CLI headlessly and commit verbatim evidence BEFORE any billed call. Follow the probe discipline of docs/scoping/cursor-cli-backend.md and the schema-doc format of docs/scoping/codex-backend.md.

1) Binary discovery (free): check KRANZ_KIMI_BIN, then `kimi` on PATH, then well-known paths (~/.local/bin, /opt/homebrew/bin, /usr/local/bin, ~/.npm-global/bin, plus any kimi/moonshot-specific install dir). Run `kimi --version` and record it.
2) Auth classification (FREE — no billed prompt): identify the login/auth-status surface (`kimi login` is a device flow that caches auth locally; console API keys serve third-party tools). Record the on-disk credential/token location (NAMES/paths only, never values) and whether an env-var API key (e.g. MOONSHOT_API_KEY / KIMI_API_KEY) authenticates headlessly. CRITICALLY test whether auth survives a relocated $HOME by re-running the auth-status check under a scratch HOME per docs/scoping/claude-cli-min-env.md — cursor lost auth exactly this way (docs/scoping/cursor-cli-backend.md).
3) ONLY IF headless auth is proven free-of-charge: capture ONE smallest read-only `kimi -p "<tiny prompt>" --output-format stream-json` session in a throwaway temp git repo and save the raw JSONL verbatim (secrets redacted to placeholders) as crates/engine/tests/fixtures/kimi_exec_scrutiny.jsonl, with a terminal Result line. If auth CANNOT be proven, DO NOT attempt a billed call — instead record a defer decision with the free evidence and STOP; the orchestrator will judge whether to defer the mission or re-scope to a synthetic fixture.
4) Write docs/scoping/kimi-cli-backend.md mirroring docs/scoping/codex-backend.md: an event-to-AgentEvent mapping table for the observed stream-json shapes (Init/Text/ToolUse/ToolResult/Result); whether the terminal Result carries token usage and the resulting cost decision (record usage if present; if Kimi Code is subscription/unmetered mark cost Meterless per crates/engine/src/backend_readiness.rs:1-5,70-82 — never invent a 0% quota bar); the -m alias form that selects k3 and each effort level (low/high/max) plus kimi-for-coding[-highspeed]; and how static deny rules / the auto permission policy are configured for headless `kimi -p`.

Guardrail (repo lesson m-0f1abd): this is auth/security code — name any introduced local binding `authority`, never `let token = …` (the secret scanner refuses that binding and the unblock cycle can silently discard the milestone's work). Commit your work within the session against $KRANZ_BASE_SHA. Redact all secret values in committed files.

Done when:
- docs/scoping/kimi-cli-backend.md exists and documents auth classification (including relocated-$HOME survival), the stream-json event schema, the -m k3/effort alias form, headless deny-rule/permission config, and whether the Result carries usage with a metered-vs-Meterless cost decision.
- If auth was proven, crates/engine/tests/fixtures/kimi_exec_scrutiny.jsonl is committed containing a real (redacted) stream-json session ending in a terminal Result; if auth was not proven, the doc records a defer decision with the captured free evidence and NO fabricated fixture.
- No committed file contains a real token or API-key value.


## Milestone 2 — M2 — Parser: backend_kimi.rs grounded in the fixture

### 2.1 Implement backend_kimi.rs single-shot backend and fixture tests

Implement crates/engine/src/backend_kimi.rs as a single-shot AgentBackend/AgentSession mirroring crates/engine/src/backend_codex.rs (the closest analogue — kimi's `--output-format stream-json` is a JSONL event stream like `codex --json`). Ground the parser in the committed fixture crates/engine/tests/fixtures/kimi_exec_scrutiny.jsonl and the schema in docs/scoping/kimi-cli-backend.md. If M1 deferred (no fixture), do not proceed — the orchestrator will re-scope.

Include:
- `discover_kimi_binary(configured)`: KRANZ_KIMI_BIN (exclusive override, error immediately if it fails) → configured → `kimi` on PATH → well-known fallbacks; validate each via crate::backend_probe::probe_version with the shared 3s timeout. Copy the structure of backend_codex::discover_codex_binary (backend_codex.rs:55-150).
- `build_args(spec)`: `-p`, effective_prompt (fold append_system_prompt ahead of the prompt like codex, backend_codex.rs:160-169), `--output-format stream-json`, `-m <model>` including the effort form documented in the scoping doc, and the writable→permission mapping per the probe. Deliberately ignore the claude-ism SessionSpec fields (json_schema, max_budget_usd, resume, permission_mode, allowed_tools/disallowed_tools, tools, settings_json) and document them as ignored in the module docstring, mirroring backend_codex.rs:1-13,190-222.
- A stateful stream parser mirroring CodexStreamParser (backend_codex.rs:340-417): map the fixture's event shapes to AgentEvent (Init/Text/ToolUse/ToolResult/Result); stitch terminal text from the last assistant message if the Result event carries none; populate TokenUsage from the Result if present; set cost_usd from the CLI if reported, else via cost::usage_cost_usd(&usage, model). Unparseable lines → AgentEvent::Other{raw:{"unparsed":line}}.
- Single-shot seam: reject a resumed SessionSpec in start() and reject send_user_message (mirror backend_codex.rs:469-474,708-712).
- Session lifecycle copied from CodexSession (kill_group/win_job, stderr capture task, finish_at_eof, abort) adapted to kimi naming.
- Register `pub mod backend_kimi;` in crates/engine/src/lib.rs after backend_droid (lib.rs:45).
- In-module unit tests (names containing "kimi"): parse the fixture into events asserting a non-empty Init session id and a terminal Result (usage populated iff the schema carries it); a discovery-order test proving KRANZ_KIMI_BIN is an exclusive override; a build_args argv test; a resumed-spec-rejected test.
- Add crates/engine/tests/kimi_fixture_test.rs mirroring crates/engine/tests/droid_fixture_test.rs: validate the committed fixture's terminal text parses as a ValidatorReport via kranz_engine::runner::parse_validator_report.

Guardrail (m-0f1abd): name introduced local bindings `authority`, never `let token = …`; commit within-session against $KRANZ_BASE_SHA. Run `cargo fmt --all` before committing.

Done when:
- backend_kimi parses the committed fixture into AgentEvents including a non-empty Init session id and a terminal Result; token usage is populated iff the fixture Result carries it.
- Binary discovery honors KRANZ_KIMI_BIN (exclusive override) then PATH then well-known paths, proven by a unit test.
- A resumed SessionSpec and send_user_message are both rejected at the seam.
- `pub mod backend_kimi;` is declared in lib.rs and `cargo test -p kranz-engine backend_kimi` passes.


## Milestone 3 — M3 — Plumbing: BackendKind::Kimi across cost, config/types/orchestrator/readiness, and surfaces

### 3.1 cost.rs kimi model family (additive)

Add the kimi model family to crates/engine/src/cost.rs additively (no enum change; this compiles independently of the BackendKind work).
- Add `pub const DEFAULT_KIMI_MODEL: &str = "k3";` (confirm the flagship alias against docs/scoping/kimi-cli-backend.md), mirroring DEFAULT_CODEX_MODEL (cost.rs:21) / DEFAULT_DROID_MODEL (cost.rs:32).
- Add `pub fn is_kimi_model(model: &str) -> bool` — case-insensitive substring match on the kimi family (e.g. contains "k3" or "kimi"), mirroring is_codex_model/is_droid_model (cost.rs:25-39). Like those, it may have no external caller yet; provide it for symmetry.
- In `pricing_for_model` (cost.rs:63-102) add a kimi branch placed so it does not collide with the existing fable/gpt/glm/opus/sonnet/haiku substring matches. Kimi Code is subscription; even if the scoping doc concluded usage is Meterless, still supply conservative per-Mtok numbers here (usage_cost_usd is only a fallback ESTIMATE, never a bill) and add a `// TODO(pricing): confirm kimi $/Mtok before ship` note mirroring the GLM note (cost.rs:76).
- Add a unit test mirroring droid_pricing_applied (cost.rs:701-728): assert pricing_for_model(DEFAULT_KIMI_MODEL) returns the kimi numbers, differs from opus/codex/droid, and usage_cost_usd computes as expected.

Guardrail (m-0f1abd): bindings named `authority` not `token`; commit vs $KRANZ_BASE_SHA; run `cargo fmt --all` before committing.

Done when:
- pricing_for_model("k3") / pricing_for_model(DEFAULT_KIMI_MODEL) returns kimi-family pricing distinct from the opus, codex, and droid tiers, proven by a unit test.
- DEFAULT_KIMI_MODEL and is_kimi_model are defined and `cargo test -p kranz-engine cost` passes.

### 3.2 BackendKind::Kimi variant and all engine-crate exhaustive-match arms (atomic)

Add `BackendKind::Kimi` and update EVERY exhaustive match in the engine crate in ONE atomic change — the crate will not compile until all are handled. Depends on DEFAULT_KIMI_MODEL from the cost feature. Mirror codex/droid throughout. All paths under crates/engine/src.
- types.rs: add `Kimi` to `enum BackendKind` (types.rs:482-487); add the arm to `as_str` → "kimi" (types.rs:489-497); add `Some("kimi") => BackendKind::Kimi` to `MissionConfig::backend_kind` (types.rs:636-642); update the `backend` field doc (types.rs:445-449) to mention kimi.
- config.rs: add "kimi" to `parse_backend` (config.rs:32-39) AND to its error literal '...must be one of None, "claude", "codex", "droid"...' (config.rs:278-281); add the Kimi arm to `backend_default_model` → DEFAULT_KIMI_MODEL (config.rs:43-49); add the Kimi arm to `model_tier` (config.rs:76-110) accepting k3 (Frontier) and kimi-for-coding[-highspeed] (tier per the scoping doc), returning None otherwise.
- NEW joint (model,effort) validation: kimi is the first backend where effort is model-constrained. In `validate()` (config.rs:213-311), AFTER the existing VALID_EFFORTS check (config.rs:221-226), add a localized kimi rule: when a role's backend resolves to Kimi and the effective model is k3, reasoning_effort MUST be one of {low, high, max} — reject medium/xhigh with a clear message; kimi-for-coding[-highspeed] impose no effort constraint. Do NOT change model_tier's signature (it ripples to role_model_tier and its callers) — keep the effort rule inside validate().
- orchestrator.rs: add a `kimi_backend: Option<Arc<dyn AgentBackend>>` cache field mirroring codex_backend (orchestrator.rs:311) / droid_backend (:315) and initialize it wherever those are initialized; add the Kimi arm to the preflight match (orchestrator.rs:668-700, mirroring the Codex arm :676-686) via crate::backend_kimi::discover_kimi_binary; add the Kimi arm to `select_backend` (orchestrator.rs:884-983, mirroring Codex :898-935) — discover → KimiBackend::new → cache → set_effective_model(&mut cfg, BackendKind::Kimi), with a loud Claude fallback_reason on discovery failure.
- backend_readiness.rs: add the Kimi arm to the discover match (backend_readiness.rs:223-227) → crate::backend_kimi::discover_kimi_binary(None); add the Kimi arm to `probe_cli_login` (backend_readiness.rs:279-315) using the auth-status form the probe documented, or return AuthProbe::Unknown like the Droid arm (:283) if kimi has no scriptable auth-status subcommand (never treat a missing subcommand as unauthenticated).
Tests (names containing "kimi"): extend the acceptance table test validate_allows_scrutiny_on_any_supported_tier (config.rs:570-588) with kimi/k3 rows; add a test that backend="kimi" model="k3" is ACCEPTED for each effort in {low,high,max} and REJECTED for medium and xhigh; add a test rejecting an unsupported kimi model (mirror validate_rejects_unknown_backend_model_combos, config.rs:481-496); mirror missing_binary_parks for a kimi role in readiness.

Guardrail (m-0f1abd): engine/security-adjacent code — introduced local bindings named `authority`, never `let token = …`; commit within-session against $KRANZ_BASE_SHA; run `cargo fmt --all` before committing.

Done when:
- The engine crate compiles with BackendKind::Kimi handled in every match (types, config, orchestrator, backend_readiness); `cargo test -p kranz-engine` builds and passes.
- config::validate accepts backend="kimi" model="k3" for each effort in {low, high, max}, REJECTS effort medium and xhigh for k3, and rejects an unsupported kimi model — proven by unit tests.
- A role configured backend="kimi" resolves through backend_kind/parse_backend and select_backend to a KimiBackend (or a loud Claude fallback when the binary is absent), and readiness probe_role classifies a kimi role.

### 3.3 CLI ready lane, dashboard pickers, and docs (additive)

Wire kimi into the additive surfaces and docs (separate builds from the engine crate).
- crates/cli/src/ready.rs: add "kimi" to the probe backend array (ready.rs:516) and the Kimi arm to the discover match (ready.rs:522-529) → kranz_engine::backend_kimi::discover_kimi_binary; update the test backend_lanes_probe_only_backends_some_role_selects (ready.rs:1064-1073) to include kimi=skipped when no role selects it.
- Frontend (apps/dashboard/src): add 'kimi' to the AgentBackend union (types.ts:238); add 'kimi' to BACKEND_OPTIONS (lib/format.ts:54) and a kimi entry to MODEL_PLACEHOLDERS (lib/format.ts:56-60; the Record<AgentBackend,string> is TS-compile-forced) with a placeholder such as "k3 (effort low/high/max)". The pickers ModelPanel.tsx and NewMission.tsx are data-driven off these constants and need no hardcoded change. Update ModelPanel.test.tsx and NewMission.test.tsx to assert kimi appears as a backend option, mirroring the existing codex/droid cases.
- Docs: update docs/knowledge/glossary.md:54-55 Backend entry to list kimi alongside claude/codex/droid. Add the kimi permission caveat mirroring codex/droid — on the kimi backend, Claude-CLI permission fields (permission_mode/allowed_tools/disallowed_tools/tools) are ignored and enforcement degrades to the writable flag + OS sandbox (SessionSpec.sandbox) + the deterministic out-of-contract-write sweep (crates/engine/src/contract_sweep.rs); reference docs/scoping/worker-sandboxing.md. Place it wherever codex/droid's equivalent config guidance lives, or in docs/scoping/kimi-cli-backend.md if no central config doc enumerates it.

Guardrail (m-0f1abd): bindings `authority` not `token`; commit vs $KRANZ_BASE_SHA; run `cargo fmt --all` before committing.

Done when:
- `kranz ready` enumerates a kimi lane and its test passes: `cargo test -p kranz-cli backend_lanes` passes.
- The dashboard lists kimi as a backend option — 'kimi' is in the AgentBackend union, BACKEND_OPTIONS, and MODEL_PLACEHOLDERS — the picker tests assert it, and the frontend typechecks/builds.
- docs/knowledge/glossary.md's Backend entry lists kimi and the config docs state the kimi permission-degrade-to-sandbox+sweep caveat.


## Milestone 4 — M4 — Integration: mock-configured kimi validator runs end-to-end

### 4.1 Stub-driven kimi scrutiny validator harness test and full green gate

Add a mock/stub-driven end-to-end test proving a kimi-configured validator runs in the orchestrator harness, mirroring the droid harness already in crates/engine/src/orchestrator.rs's #[cfg(test)] module. Depends on the KimiBackend + fixture (M2) and BackendKind::Kimi + config (M3).
- Add `write_kimi_stub` and `write_kimi_stub_no_report` helpers mirroring write_droid_stub (orchestrator.rs:9711) / write_droid_stub_no_report (:9741): write a stub `kimi` executable that, when invoked, emits the committed fixture's stream-json (a valid terminal Result whose text is a ValidatorReport).
- Add a `KimiStubEnvGuard` mirroring DroidStubEnvGuard (orchestrator.rs:9772-9799) that points KRANZ_KIMI_BIN at the stub, and a `kimi_scrutiny_cfg` mirroring droid_scrutiny_cfg (:9801) setting validator_scrutiny.backend="kimi", model="k3", effort within the {low,high,max} matrix.
- Add `kimi_scrutiny_findings_flow` mirroring droid_scrutiny_findings_flow (orchestrator.rs:9867): a milestone validated by a kimi scrutiny validator yields findings from a trusted report and the run is attributed to BackendKind::Kimi. Optionally add a cost-attribution test mirroring droid_scrutiny_run_priced_with_droid_table (:9950) asserting the kimi run is priced via the kimi table (or recorded Meterless).
- Ensure the full gate is green: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`.

Guardrail (m-0f1abd): bindings `authority` not `token`; commit within-session against $KRANZ_BASE_SHA; run `cargo fmt --all` before committing (CI enforces fmt --check).

Done when:
- A mock/stub-configured kimi scrutiny validator runs end-to-end in the orchestrator test harness and yields a trusted ValidatorReport with findings, attributed to BackendKind::Kimi.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all --check` all pass.

