# Mission plan — m-c6882a

**Goal:** Add BackendKind::Local — a first-of-its-kind HTTP-in-engine, OpenAI-compatible completion backend plus base_url/contextBudget/temperature fields on the existing per-role RoleConfig, so any worker/validator role is retargetable to a local endpoint by config alone, reusing the shipped AgentBackend/BackendKind machinery.

Branch `kranz/mission-m-c6882a` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$9.02 – $45.10** (expected ~$20.96). Rough estimate — live usage is authoritative; based on 45 completed mission(s).

## Considered alternatives

**Chosen approach:** Slot a fifth BackendKind::Local into the existing AgentBackend seam: a config-parameterized HTTP-in-engine backend that does a single OpenAI-compatible completion round-trip (Init→Text→Result), reusing BackendKind/RoleConfig/validate machinery and the kimi single-shot AgentSession pattern. This matches KRZ-205's exit ('any role retargetable by config alone'), the addendum's HTTP-in-engine preference, and the 'reuse, do not rebuild' constraint.

Rejected shapes:
- **Build a full agentic tool-execution/file-editing loop in-engine for the local backend so a local worker autonomously implements features.** — Massive scope, directly contradicts 'do NOT rebuild the abstraction', and is unnecessary for KRZ-205's config-retargetability exit (routing real work local is the separate KRZ-206/208).
- **Wrap a local CLI (e.g. `ollama run`) as another stream-json CLI backend like kimi/codex/droid.** — Addendum §2 explicitly prefers HTTP-in-engine; a CLI wrap re-enters the worker sandbox (needing a localhost egress-allowlist entry) and forfeits sharing the no-CLI api-only-harness machinery.
- **Introduce a new generic 'OpenAI/completion' trait abstraction (or adopt the Rig framework) alongside AgentBackend.** — AgentBackend is already the mockable seam; a second parallel abstraction is exactly the 'rebuild the abstraction' the mission forbids and would fragment backend selection/validation.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The entire workspace compiles and cargo test --workspace passes (no crate is left non-compiling by the new BackendKind variant or the reqwest dependency). 
  `cargo test --workspace`
- **[a2]** The backend_local HTTP path is exercised end-to-end against an in-process stub OpenAI-compatible endpoint: a worker-role local session POSTs to /v1/chat/completions and yields Init→Text→Result with token usage parsed from the response and cost_usd = 0.0. 
  `cargo test -p kranz-engine local_http 2>&1 | grep -Eq 'test result: ok\. [1-9]'`
- **[a3]** config::validate() enforces the local-role rules: base_url required+parseable, contextBudget required and within 1024..=200000, temperature (if set) within 0.0..=2.0, and the existing tier floors (a local worker needs allowBelowDefaultWorkerModel=true; a local orchestrator is always rejected). 
  `cargo test -p kranz-engine local_config 2>&1 | grep -Eq 'test result: ok\. [1-9]'`
- **[a4]** contextBudget is honored at runtime: a local session whose assembled prompt would exceed the configured contextBudget fails cleanly (SessionExit::Failed, no panic) and sends no HTTP request. 
  `cargo test -p kranz-engine local_http_context_budget 2>&1 | grep -Eq 'test result: ok\. [1-9]'`
- **[a5]** The orchestrator routes a worker role configured backend=local to a LocalBackend (kind Local, no claude fallback), not to the claude backend. 
  `cargo test -p kranz-engine local_select 2>&1 | grep -Eq 'test result: ok\. [1-9]'`
- **[a6]** The AgentBackend/AgentSession contract file (backend.rs) is not modified; the new backend implements against the existing trait surface. 
  `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/backend.rs`
- **[a7]** Code is formatted per the repo's enforced cargo fmt gate. 
  `cargo fmt --all --check`
- **[a8]** The workspace is clippy-clean under -D warnings. 
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[a9]** backend_local reuses the existing AgentBackend/AgentSession trait, the BackendKind enum, and the per-role RoleConfig/validate machinery rather than introducing a parallel completion abstraction — the abstraction is not rebuilt. *(agent judgement)*
- **[a10]** The local completion request is made inside the engine process (not the sandboxed worker subprocess), so no localhost egress-allowlist entry is added to the fs+net sandbox for it. *(agent judgement)*

## Milestone 1 — Config: BackendKind::Local + validated per-role fields

### 1.1 Add BackendKind::Local, per-role base_url/contextBudget/temperature, model_tier placement, and validation rules

CONFIG-ONLY feature (no HTTP, no backend impl). Files: crates/engine/src/types.rs and crates/engine/src/config.rs. Context: Kranz has an AgentBackend seam with BackendKind {Claude,Codex,Droid,Kimi} and per-role floors in config.rs; add a fifth kind Local for an OpenAI-compatible HTTP endpoint. Do exactly:
1) types.rs: add `BackendKind::Local`; `as_str()` => "local"; extend `backend_kind()` so Some("local") => Local. Add three RoleConfig fields, all `#[serde(default, skip_serializing_if = "Option::is_none")]`, camelCase: `base_url: Option<String>` (JSON baseUrl), `context_budget: Option<u32>` (JSON contextBudget), `temperature: Option<f64>`. Non-local roles must serialize byte-identically to today (all three default to None).
2) config.rs `parse_backend`: Some("local") => Ok(Local); update the invalid-backend error text to also list "local".
3) config.rs `effective_model`/`backend_default_model`: Local must NEVER panic. Make `backend_default_model(Local)=None` and guard `effective_model` so it rewrites only when a default exists; for Local the configured model string passes through verbatim (it is sent to the endpoint as-is; there is no Claude→backend default rewrite for Local).
4) config.rs `model_tier(Local, m)`: any non-empty trimmed model => Some(ModelTier::BelowDefault); empty => None. Local model ids are free-form and cannot be allowlisted, so classify uniformly below-default — this makes workers require the existing allowBelowDefaultWorkerModel opt-in and makes the orchestrator frontier floor reject a local orchestrator.
5) config.rs `validate()`: for any role whose backend is "local": (a) base_url is required, non-empty, and must parse as an http/https URL; (b) context_budget is required and must be an integer in 1024..=200000 inclusive (guards the KV-cache-blowout risk); (c) temperature, if present, must be finite and in 0.0..=2.0. Leave the existing tier floors unchanged (worker below-default needs allowBelowDefaultWorkerModel=true; orchestrator needs Frontier, so a local orchestrator is always rejected). All errors are EngineError::Config naming the role and the offending field.
COMPILE NOTE: adding the enum variant makes exhaustive `match BackendKind` arms in orchestrator.rs (`select_backend`) and backend_readiness.rs non-exhaustive. Add MINIMAL temporary arms so the crate compiles (e.g. orchestrator `BackendKind::Local => unreachable!("local backend wired in a later feature")`, and the readiness match returns its existing Unknown/uncertain variant). The real wiring lands in a later milestone; do not implement it here.
Tests: add #[cfg(test)] tests in config.rs whose names START WITH `local_config_` (the contract targets that prefix), each asserting real pass/fail through validate()/model_tier: `local_config_requires_base_url` (reject missing + unparseable, accept valid), `local_config_requires_context_budget_in_range` (reject 1023, 200001, and missing; accept 8192), `local_config_rejects_out_of_range_temperature` (reject 2.1 and -0.1; accept 0.7 and absent), `local_config_worker_below_default_needs_optin` (local worker rejected without allowBelowDefaultWorkerModel, accepted with it), `local_config_orchestrator_local_always_rejected`, `local_config_model_tier_below_default_for_any_nonempty`. Run `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` before finishing; the whole workspace must compile.

Done when:
- validate() rejects a local role missing base_url and one with an unparseable base_url; accepts a well-formed http/https base_url.
- validate() rejects context_budget of 1023, 200001, and absent; accepts 8192.
- validate() rejects temperature 2.1 and -0.1; accepts 0.7 and absent.
- A local worker is rejected unless allowBelowDefaultWorkerModel=true; a local orchestrator is always rejected by the frontier floor.
- model_tier(Local, non-empty) == BelowDefault and model_tier(Local, "") == None; effective_model for a Local role returns the configured model verbatim and never panics.
- The full workspace compiles with the new variant (temporary Local match arms added where needed) and cargo fmt/clippy are clean.


## Milestone 2 — HTTP completion backend (backend_local)

### 2.1 Implement the first HTTP-in-engine AgentBackend against an OpenAI-compatible endpoint, pinned by a stub server

Implement crates/engine/src/backend_local.rs, register `pub mod backend_local;` in crates/engine/src/lib.rs, and add `reqwest.workspace = true` to crates/engine/Cargo.toml (reqwest is already a workspace dependency but the engine crate does not yet pull it). Reference crates/engine/src/backend_kimi.rs for the single-shot AgentSession shape (synthesize Init, reject resume, reject send_user_message, exit_status semantics). backend.rs is a CONTRACT FILE — do NOT edit it; implement against its existing AgentBackend/AgentSession/AgentEvent/SessionExit types.
Build `LocalBackend` constructed from role config: `LocalBackend::new(base_url: String, temperature: Option<f64>, context_budget: u32)` holding a cloneable `reqwest::Client`. `AgentBackend::start(spec)`:
- If `spec.resume.is_some()` return Err(EngineError::Backend(..)) (single-shot).
- Assemble an OpenAI chat-completions request for `{base_url}/v1/chat/completions`: `model = spec.model`; `messages` = optional system message from `spec.append_system_prompt` (when non-empty) followed by a user message with the prompt text (from PromptMode SingleShot/Streaming); include `temperature` when Some.
- CONTEXT BUDGET (runtime honoring): before sending, compute a deterministic prompt-size estimate (a documented chars/4 token estimate over the assembled messages is acceptable) and if it exceeds `context_budget`, DO NOT send any HTTP request — the session yields a synthesized Init then ends as SessionExit::Failed with a message naming the context budget. Never panic.
- Otherwise POST (non-streaming JSON is sufficient; SSE is optional). On a 2xx JSON response emit, in order: Init{session_id = spec.session_id, model} (synthesized, mirroring kimi), Text{assistant message content}, then Result{text = content, is_error = false, usage from response.usage (prompt_tokens→input, completion_tokens→output; zero defaults if absent), cost_usd = Some(0.0) because a local run is $0 marginal, num_turns = Some(1)}. On transport error, non-2xx status, or unparseable body, end as SessionExit::Failed with a descriptive message including the HTTP status and a bounded body tail.
- `send_user_message` => Err (single-shot). `abort` => best-effort, exit Aborted. `exit_status` per kimi.
Because this HTTP call is made inside the engine process (not the sandboxed worker subprocess), no localhost egress-allowlist entry is required — do not add one.
TESTS (critical — analog of the kimi lesson that mock/stub tests must exercise the real client boundary): stand up an in-process stub HTTP server that serves /v1/chat/completions and drive the REAL reqwest client + response parsing through it (prefer a workspace-available approach: a hand-rolled tokio TcpListener returning a canned HTTP/1.1 JSON response, or an existing stub crate already in the lock/workspace — do not add a heavy new test dependency without need). Name every test with the prefix `local_http_`, and include `local_http_context_budget_exceeds_fails_cleanly` (a dedicated contract command targets `local_http_context_budget`). Cover: (1) roundtrip yields Init→Text→Result with usage parsed from the stub body and cost_usd == 0.0; (2) resume rejected; (3) send_user_message errors; (4) stub HTTP 500 → SessionExit::Failed (no panic), message includes the status; (5) prompt exceeding context_budget → clean SessionExit::Failed AND the stub records zero received requests. Run cargo fmt and clippy (-D warnings) clean.

Done when:
- Against an in-process stub /v1/chat/completions, start() with a writable worker spec yields Init, then Text carrying the stub's content, then a non-error Result whose usage.input/usage.output equal the stub's prompt_tokens/completion_tokens and whose cost_usd == 0.0.
- A spec with resume = Some(..) is rejected; send_user_message returns Err.
- A stub returning HTTP 500 produces SessionExit::Failed (no panic) with a message that includes the status.
- A prompt exceeding context_budget produces a clean SessionExit::Failed and the stub receives zero HTTP requests.
- reqwest is added to crates/engine/Cargo.toml, backend_local is registered in lib.rs, and crates/engine/src/backend.rs is unchanged versus the mission base.


## Milestone 3 — Wire Local into the run loop (routing, preflight, cost, readiness)

### 3.1 Route local roles to LocalBackend in select_backend and add a best-effort endpoint reachability preflight

File: crates/engine/src/orchestrator.rs. Replace the temporary compile-stub `BackendKind::Local` arm in `select_backend` (added in the config milestone) with real construction: read the role's config base_url/temperature/context_budget and build `crate::backend_local::LocalBackend::new(base_url, temperature, context_budget)`, returning `SelectedBackend { backend, kind: BackendKind::Local, cfg (with the same effective_model normalization the other arms apply), fallback_reason: None }`. There is NO binary to probe and therefore NO claude fallback for local — validate() has already guaranteed base_url and context_budget are present, so unwrap them via the role config (do not silently fall back to claude). Constructing a fresh LocalBackend per selection is fine (reqwest::Client is cheap/cloneable); do not attempt to reuse the single cached-binary pattern since base_url is per-role.
Also add a Local arm to the preflight role loop that currently probes codex/droid/kimi binaries: for a role whose backend is local, perform a best-effort, short-timeout (~2s) reachability probe of base_url (e.g. an HTTP GET of base_url or base_url + "/v1/models") and push a PreflightIssue with severity "warn" (never "error", never a block) when it is unreachable, mirroring the existing "binary not found → warn" style.
Add an orchestrator test named `local_select_routes_worker_to_local_backend` proving that a worker role with backend=local and a base_url is dispatched to a LocalBackend (SelectedBackend.kind == Local, fallback_reason == None) rather than the injected claude backend. Keep all non-local role selection byte-for-byte unchanged. Run cargo fmt and clippy (-D warnings) clean.

Done when:
- select_backend for a worker role with backend=local returns kind Local with fallback_reason None (it does not fall back to the claude backend).
- Test local_select_routes_worker_to_local_backend passes.
- Preflight emits only a severity "warn" issue (never "error") when a local role's base_url is unreachable.
- Backend selection for non-local roles (claude/codex/droid/kimi) is unchanged.

### 3.2 Local readiness reachability arm and $0 marginal cost accounting

Files: crates/engine/src/backend_readiness.rs and crates/engine/src/cost.rs.
backend_readiness.rs: add the `BackendKind::Local` arm(s) to the exhaustive matches (the discover/reachability function around lines 220–230 and the auth-status probe around lines 283–293). Local has no binary, so readiness = a best-effort, short-timeout HTTP reachability check of the role's base_url; if base_url is absent, return the same Unknown/uncertain variant the other arms use for indeterminate cases. Never panic and never block on failure — an unreachable endpoint yields a not-ready/uncertain result, not an error.
cost.rs: ensure a local run is priced at $0 marginal (per the scoping-doc review addendum §5). The backend already emits cost_usd = Some(0.0); make any model-string-derived pricing path (`usage_cost_usd` and its helpers) also return 0.0 for the local case so downstream recomputation agrees. Do NOT change pricing for any existing backend/model.
Tests: name new tests with a `local_` prefix; assert that (a) readiness for a local role with an unreachable base_url returns a non-panicking not-ready/uncertain result, and (b) local cost accounting yields $0. Run cargo fmt and clippy (-D warnings) clean.

Done when:
- backend_readiness compiles with BackendKind::Local handled and returns a non-panicking result for a local role; an unreachable base_url yields a not-ready/uncertain (never error/panic) outcome.
- Local run cost accounting yields $0 marginal.
- Readiness and pricing for existing backends (claude/codex/droid/kimi) are unchanged.

