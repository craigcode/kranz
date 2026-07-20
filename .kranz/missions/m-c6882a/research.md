# Research — m-c6882a

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/backend.rs
- crates/engine/src/backend_mock.rs
- crates/engine/src/backend_kimi.rs
- crates/engine/src/config.rs
- crates/engine/src/types.rs
- crates/engine/src/orchestrator.rs
- crates/engine/src/backend_readiness.rs
- crates/engine/src/cost.rs
- crates/engine/Cargo.toml
- Cargo.toml
- docs/scoping/local-inference-executor-tier.md

## External sources

- docs/scoping/local-inference-executor-tier.md (KRZ-205 + review addendum §1–5)
- git commit 7c1b130 (per-role backend selection precedent, per mission context)

## Facts

- AgentBackend/AgentSession is the backend seam and backend.rs is a declared CONTRACT FILE that must not be edited in implementation phases. — `crates/engine/src/backend.rs lines 3-4 and trait defs at 176-199`
- BackendKind is a closed enum {Claude,Codex,Droid,Kimi} with as_str and a backend_kind() string resolver; parse_backend/model_tier/effective_model/validate floors live in config.rs. — `crates/engine/src/types.rs:484-500,639-646; crates/engine/src/config.rs:32-130,225-340`
- effective_model panics for a non-Claude backend that lacks a default model (.expect), so Local must be handled to avoid a panic. — `crates/engine/src/config.rs:66-74`
- The engine crate does not currently depend on reqwest, but reqwest is a workspace dependency (0.12, rustls) ready to add. — `crates/engine/Cargo.toml has no reqwest; Cargo.toml:48 declares reqwest in [workspace.dependencies]`
- kimi is the closest reference: single-shot, synthesizes Init, rejects resume and send_user_message, computes cost client-side. — `crates/engine/src/backend_kimi.rs:422-506,628-688`
- Adding a BackendKind variant breaks exhaustive matches in orchestrator select_backend and backend_readiness (discover + auth), which must gain a Local arm to compile; cost.rs matches on model strings, not the enum. — `crates/engine/src/orchestrator.rs:915-1026; crates/engine/src/backend_readiness.rs:225-227,283-293; crates/engine/src/cost.rs:27-92`
- Surface pickers are optional to a working backend: the Slack backend picker still lists only claude/codex/droid and omits kimi, which functions fully in the engine. — `crates/slack/src/inbound.rs:1170 (const BACKENDS = ["claude","codex","droid"])`
- The review addendum specifies HTTP-in-engine sidesteps the worker sandbox (no localhost egress entry), local runs are $0 marginal, and per-role context budgets guard KV-cache blowout. — `docs/scoping/local-inference-executor-tier.md:181-211 (addendum §2,§5), :153-159 (§6 KV-cache risk)`

## Ambiguities & stale docs

- Exact contextBudget numeric bounds are not specified in the scoping doc; chosen 1024..=200000 as a meaningful-floor + fat-finger-ceiling guard (confirmable/adjustable by the operator).
- Local model ids are free-form (base_url+model are arbitrary), so model_tier cannot allowlist; chosen to classify any non-empty local model as BelowDefault, which forces the worker allowBelowDefaultWorkerModel opt-in and blocks a local orchestrator via the frontier floor.
- Scope confirmed as bare completion (single HTTP round-trip, no in-engine tool/file-edit loop), OpenAI-compatible only (no Anthropic /v1/messages impl), with CLI/Slack/dashboard pickers deferred as kimi's were — per user direction to proceed with the recommended defaults.

## Candidate knowledge updates

- Add a knowledge note: backend_local is the first HTTP-in-engine AgentBackend — pattern is a config-parameterized backend (base_url/temperature/contextBudget) constructed per-role in select_backend rather than a cached discovered binary, and contextBudget is honored at runtime by failing the session cleanly before sending.
- Record that surface pickers (Slack/CLI/dashboard) are decoupled from engine backend support (kimi and now local can function without picker entries), so a backend mission's core is engine-only.
