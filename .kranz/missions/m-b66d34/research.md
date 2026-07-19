# Research — m-b66d34

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/backend_codex.rs
- crates/engine/src/backend_droid.rs
- crates/engine/src/cost.rs
- crates/engine/src/backend_readiness.rs
- crates/engine/src/config.rs
- crates/engine/src/types.rs
- crates/engine/src/orchestrator.rs
- crates/engine/tests/droid_fixture_test.rs
- crates/engine/tests/mission_test.rs
- docs/scoping/codex-backend.md
- docs/scoping/cursor-cli-backend.md
- docs/scoping/claude-cli-min-env.md
- docs/scoping/worker-auth-preflight.md
- .kranz/tickets/kimi-k3-backend.md

## External sources

- docs/scoping/codex-backend.md (event-to-AgentEvent mapping template)
- docs/scoping/cursor-cli-backend.md (probe-then-defer precedent and acceptance bar)
- docs/scoping/claude-cli-min-env.md (relocated-$HOME auth trap)
- Kimi Code CLI official docs (cited by the ticket; not fetched during planning — the probe feature must confirm the CLI surface first-hand)

## Facts

- BackendKind is a 3-variant enum (Claude/Codex/Droid); adding Kimi compiler-forces every exhaustive match in the engine crate. — `crates/engine/src/types.rs:482-497,636-642; matches at config.rs:43,76; orchestrator.rs:884; backend_readiness.rs:223,279`
- Codex/droid backends are single-shot: resume and send_user_message are rejected at the seam, and claude-ism SessionSpec fields are ignored when building argv. — `crates/engine/src/backend_codex.rs:1-13,469-474,708-712; backend_droid.rs:1-13`
- Effort is validated globally against VALID_EFFORTS=[low,medium,high,xhigh,max] and model_tier(kind,model) validates model only — so a model-constrained (k3 = low/high/max) rule is new and must be added inside validate() without changing model_tier's signature. — `crates/engine/src/config.rs:20-21,76-110,213-311`
- Cost pricing is a substring family match with an unknown-model opus-tier fallback; is_codex_model/is_droid_model currently have no external callers, and cost_usd falls back to usage_cost_usd when the CLI reports none. — `crates/engine/src/cost.rs:25-39,63-112; backend_codex.rs:401-405`
- Readiness has the 'Meterless' status meaning 'no quota API — warn+proceed, never fabricate a 0% bar', and auth-classify returns Unknown for a backend with no scriptable auth-status subcommand (droid) rather than Unauthenticated. — `crates/engine/src/backend_readiness.rs:17-28,70-82,191-203,279-315`
- The 'mock-configured validator in a mission_test-style harness' the ticket references lives inside orchestrator.rs's #[cfg(test)] module using on-PATH stub binaries plus MockBackend, not in tests/mission_test.rs. — `crates/engine/src/orchestrator.rs:9711-9950 (write_droid_stub, DroidStubEnvGuard, droid_scrutiny_findings_flow)`
- Readiness is wired in two places and the frontend enumerates backends via compile-forced constants. — `crates/engine/src/backend_readiness.rs:223,279 and crates/cli/src/ready.rs:516,522-529; apps/dashboard/src/lib/format.ts:54,56-60 and types.ts:238`

## Ambiguities & stale docs

- Whether the kimi CLI is installed and headlessly authenticatable on the worker host, and whether that auth survives a relocated $HOME — M1 is a hard gate that defers the mission (cursor-style) if it cannot be proven, per the flagged default.
- If auth is unprovable, whether to hard-defer with evidence (chosen default) or land the parser/plumbing against a synthetic docs-derived fixture behind an experimental flag.
- The exact (model,effort) matrix and CLI form: assumed k3 accepts only {low,high,max}; kimi-for-coding[-highspeed] take no effort constraint; whether effort is a separate flag or encoded in the -m alias is to be confirmed by the M1 probe.

## Candidate knowledge updates

- Add docs/scoping/kimi-cli-backend.md (event mapping, auth classification, -m alias/effort form, deny-rule config, usage/cost classification) as the fourth backend scoping doc.
- Update docs/knowledge/glossary.md Backend entry to enumerate kimi alongside claude/codex/droid.
- Record the durable lesson that kimi is the first backend with a model-constrained effort matrix, so config validation must check (model,effort) jointly inside validate() rather than via model_tier alone.
