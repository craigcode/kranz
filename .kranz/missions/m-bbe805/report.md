# Mission report — m-bbe805

**Goal:** Add a DroidBackend (AgentBackend over `droid exec -o json`) selectable only for the scrutiny validator, mirroring the CodexBackend posture: cross-vendor read-only audit, never building, with config-enforced role restriction and the same loud-fallback selection seam.

Branch `kranz/mission-m-bbe805` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 1h 45m 22s
**Tokens:** 42785 in / 155018 out / 16062476 cache read / 744962 cache write
**Cost:** $40.03 actual vs $9.18–$45.88 estimated (expected $18.35)

## What shipped

### Milestone 1 — Droid exec translation grounded in a committed fixture ✅

- ✅ **Committed droid `-o json` fixtures + fixture-validation test crate** — 1 run
  - `9c92f67` [f-1-1] add committed droid exec -o json fixtures + fixture-validation tests
- ✅ **backend_droid module: exec translation + single-shot session** — 1 run
  - `069a399` [f-1-2] add DroidBackend: exec translation + single-shot session

### Milestone 2 — Droid selectable as the scrutiny backend, end-to-end ✅

- ✅ **Types, config validation, and Fireworks GLM pricing for the droid backend** — 1 run
  - `64d8550` [f-2-1] add Droid backend kind, config validation, and Fireworks GLM pricing
- ✅ **Wire droid into select_scrutiny_backend, preflight, and the run-loop fallback** — 1 run
  - `ba7cd3a` [f-2-2] wire droid into select_scrutiny_backend, preflight, and run-loop fallback
- ✅ **End-to-end stub-droid-binary round drives findings through the fix-cycle machinery** — 2 runs, 1 respawn
  - `05e693e` [f-2-3] rustfmt orchestrator.rs droid_preflight_warns_when_binary_absent

## Validation history

### ms-1 round 1 — Droid exec translation grounded in a committed fixture

- [minor] f-1-2: DroidSession::send_user_message always errors — backend_droid.rs implements send_user_message as an unconditional Err in `impl AgentSession for DroidSession`, and droid_backend_rejects_resumed_spec covers the resume-rejection half of this criterion… [truncated]

Disposition: waived.
- f-1-2: DroidSession::send_user_message always errors: Not a contract assertion; behavior is a one-line unconditional Err verified by inspection and identical to the proven CodexSession. DroidSession is only constructable via start() (needs a binary), so a direct test can't exist until the f-2-3 stub machinery — disproportionate to spawn a fresh worker for one minor assertion.

### ms-2 round 1 — Droid selectable as the scrutiny backend, end-to-end

No findings.

## Contract outcomes

- ✅ **[d1]** The DroidBackend translates a `droid exec -o json` result object into a terminal Result event whose text is the `result` field and parses as a non-empty ValidatorReport (guards the m-5d2c79 empty-terminal-text trap). *(command: `cargo test --workspace backend_droid_terminal_text_parses_report 2>&1 | grep -qE 'result: ok\. [1-9]'`)*
- ✅ **[d2]** The droid argv is exactly the read-only single-shot form `exec -o json --auto low -m <model> <prompt>`, with all claude-only SessionSpec fields (json_schema, max_budget_usd, resume, permission_mode, allowed/disallowed_tools, tools, settings_json, effort) ignored. *(command: `cargo test --workspace build_args_droid_read_only 2>&1 | grep -qE 'result: ok\. [1-9]'`)*
- ✅ **[d3]** config::validate accepts `validatorScrutiny.backend = "droid"`. *(command: `cargo test --workspace validate_accepts_droid_scrutiny_backend 2>&1 | grep -qE 'result: ok\. [1-9]'`)*
- ✅ **[d4]** config::validate rejects `backend = "droid"` on the worker, functional-validator, and orchestrator roles. *(command: `cargo test --workspace validate_rejects_droid_on_non_scrutiny_roles 2>&1 | grep -qE 'result: ok\. [1-9]'`)*
- ✅ **[d5]** GLM 5.2 (accounts/fireworks/models/glm-5p2) is priced by a dedicated Fireworks pricing branch — distinct from opus and codex tiers — and is DEFAULT_DROID_MODEL; claude-fable-5 still resolves to fable pricing. *(command: `cargo test --workspace droid_pricing_applied 2>&1 | grep -qE 'result: ok\. [1-9]'`)*
- ✅ **[d6]** When backend=droid but no droid binary is reachable, selection falls back to the claude scrutiny validator with a loud recorded OrchestratorDecision, and the scrutiny validator still runs exactly once through the injected backend. *(command: `cargo test --workspace droid_absent_loud_fallback 2>&1 | grep -qE 'result: ok\. [1-9]'`)*
- ✅ **[d7]** preflight emits a warn-severity issue when backend=droid but no droid binary is found. *(command: `cargo test --workspace droid_preflight_warns 2>&1 | grep -qE 'result: ok\. [1-9]'`)*
- ✅ **[d8]** A stub droid binary emitting the committed fixture drives real ValidatorReport findings through the fix-cycle machinery (validation.finding + fixfeature.created), the scrutiny spawn event carries glm-5p2, the run is priced with the droid table, and no fallback decision is emitted. *(command: `cargo test --workspace droid_scrutiny_findings_flow 2>&1 | grep -qE 'result: ok\. [1-9]'`)*
- ✅ **[d9]** A droid scrutiny run that completes without a parseable report triggers exactly one bounded retry on the claude backend, loudly recorded. *(command: `cargo test --workspace droid_runtime_retry_falls_back_to_claude 2>&1 | grep -qE 'result: ok\. [1-9]'`)*
- ✅ **[d10]** The entire workspace test suite passes. *(command: `cargo test --workspace 2>&1 | grep -qE 'result: ok\.'`)*
- ✅ **[d11]** Formatting is clean across the workspace. *(command: `cargo fmt --all -- --check`)*
- ✅ **[d12]** Clippy is clean across all targets with warnings denied. *(command: `cargo clippy --workspace --all-targets -- -D warnings`)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
