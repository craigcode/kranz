# Mission report — m-c6882a

**Goal:** Add BackendKind::Local — a first-of-its-kind HTTP-in-engine, OpenAI-compatible completion backend plus base_url/contextBudget/temperature fields on the existing per-role RoleConfig, so any worker/validator role is retargetable to a local endpoint by config alone, reusing the shipped AgentBackend/BackendKind machinery.

Branch `kranz/mission-m-c6882a` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 2h 16m 52s
**Tokens:** 64815 in / 267351 out / 31165696 cache read / 1263081 cache write
**Cost:** $81.01 actual vs $9.02–$45.10 estimated (expected $20.96)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-c6882a-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Preflight:** preflight: 1 issue(s): [warn] command assertion [a1] runs `cargo test` without anti-vacuity (`ok. [1-9]`); a zero-test filter would pass vacuously

## What shipped

### Milestone 1 — Config: BackendKind::Local + validated per-role fields ✅

- ✅ **Add BackendKind::Local, per-role base_url/contextBudget/temperature, model_tier placement, and validation rules** — 1 run
  - `d46730a` [f-1-1] add BackendKind::Local plus per-role base_url/contextBudget/temperature validation
- ✅ **Tighten local baseUrl validation to reject host-less authorities** *(fix)* — 1 run
  - `0052dfb` [ms-1-fix-1-1] reject host-less local base_url authorities

### Milestone 2 — HTTP completion backend (backend_local) ✅

- ✅ **Implement the first HTTP-in-engine AgentBackend against an OpenAI-compatible endpoint, pinned by a stub server** — 2 runs, 1 respawn
  - `bae87a6` [f-2-1] assert wire-level request correctness in local_http roundtrip test

### Milestone 3 — Wire Local into the run loop (routing, preflight, cost, readiness) ✅

- ✅ **Route local roles to LocalBackend in select_backend and add a best-effort endpoint reachability preflight** — 1 run
  - `d742780` [f-3-1] route local roles to LocalBackend and add reachability preflight
- ✅ **Local readiness reachability arm and $0 marginal cost accounting** — 2 runs, 1 respawn
- ✅ **Fix nested-runtime panic in local preflight reachability probe** *(fix)* — 1 run
  - `8f97833` [ms-3-fix-1-1] make local preflight reachability probe runtime-free

## Validation history

### ms-1 round 1 — Config: BackendKind::Local + validated per-role fields

- [minor] f-1-1: validate() rejects an unparseable/well-formed base_url — config.rs base_url validation is hand-rolled (strip http/https prefix, then check the first path segment is non-empty) rather than using a real URL parser. It accepts host-less authorities: url="http:… [truncated]
- [minor] Integration seam: validate() accepts a local worker config that select_backend panics on — validate() now returns Ok() for a well-formed local worker (allowBelowDefaultWorkerModel=true), but orchestrator.rs:1030 has `BackendKind::Local => unreachable!("local backend wired in a later feature… [truncated]

Disposition: 1 fix feature(s) created.

### ms-1 round 2 — Config: BackendKind::Local + validated per-role fields

- [critical] a2 — cargo test -p kranz-engine local_http 2>&1 shows every test binary reporting 'running 0 tests' / 'test result: ok. 0 passed ... N filtered out' — no test named local_http exists anywhere. grep -rn 'lo… [truncated]
- [critical] a4 — cargo test -p kranz-engine local_http_context_budget 2>&1 shows 'running 0 tests' across all binaries — no matching test exists. grep confirms no 'local_http_context_budget' identifier anywhere in the… [truncated]
- [critical] a5 — cargo test -p kranz-engine local_select 2>&1 shows 'running 0 tests' across all binaries. grep confirms no 'local_select' test exists. Additionally, orchestrator.rs:1032 contains `BackendKind::Local =… [truncated]

Disposition: waived.
- a2: Out of scope for ms-1 (config-only); the backend_local HTTP impl and local_http tests are the deliverable of pending feature f-2-1 in ms-2, already planned.
- a4: Out of scope for ms-1; runtime contextBudget enforcement + the local_http_context_budget test land with the HTTP backend in pending feature f-2-1 (ms-2).
- a5: Out of scope for ms-1; orchestrator routing to LocalBackend and the local_select test are pending feature f-3-1 (ms-3), which replaces the intended f-1-1 unreachable!() placeholder already adjudicated last cycle.

### ms-2 round 1 — HTTP completion backend (backend_local)

- [critical] a5 — `cargo test -p kranz-engine local_select 2>&1 | grep -Eq 'test result: ok\. [1-9]'` finds zero matching tests (0 passed; 0 filtered matched 'local_select'). Direct inspection of crates/engine/src/orch… [truncated]

Disposition: waived.
- a5: Out of scope for ms-2 (HTTP backend); orchestrator routing to LocalBackend and the local_select test are pending feature f-3-1 (ms-3), which replaces the intended f-1-1 unreachable!() placeholder — already planned, not a gap.

### ms-3 round 1 — Wire Local into the run loop (routing, preflight, cost, readiness)

- [critical] f-3-1: Preflight emits only a severity "warn" issue (never "error") when a local role's base_url is unreachable — orchestrator.rs:6802-6822 `probe_local_endpoint_reachable` builds a fresh `tokio::runtime::Builder::new_current_thread()...build()` and calls `runtime.block_on(...)`. This runs inside `preflight()` (o… [truncated]
- [minor] f-3-1/f-3-2: end-to-end run-loop coverage for a local role — No test drives a full local mission through engine.run()/run_loop inside a tokio runtime; worker.backend = "local" is set only in synchronous unit tests (orchestrator select/preflight, backend_readine… [truncated]

Disposition: 1 fix feature(s) created.

### ms-3 round 2 — Wire Local into the run loop (routing, preflight, cost, readiness)

No findings.

## Contract outcomes

- ✅ **[a1]** The entire workspace compiles and cargo test --workspace passes (no crate is left non-compiling by the new BackendKind variant or the reqwest dependency). *(command: `cargo test --workspace`)*
- ✅ **[a2]** The backend_local HTTP path is exercised end-to-end against an in-process stub OpenAI-compatible endpoint: a worker-role local session POSTs to /v1/chat/completions and yields Init→Text→Result with token usage parsed from the response and cost_usd = 0.0. *(command: `cargo test -p kranz-engine local_http 2>&1 | grep -Eq 'test result: ok\. [1-9]'`)*
- ✅ **[a3]** config::validate() enforces the local-role rules: base_url required+parseable, contextBudget required and within 1024..=200000, temperature (if set) within 0.0..=2.0, and the existing tier floors (a local worker needs allowBelowDefaultWorkerModel=true; a local orchestrator is always rejected). *(command: `cargo test -p kranz-engine local_config 2>&1 | grep -Eq 'test result: ok\. [1-9]'`)*
- ✅ **[a4]** contextBudget is honored at runtime: a local session whose assembled prompt would exceed the configured contextBudget fails cleanly (SessionExit::Failed, no panic) and sends no HTTP request. *(command: `cargo test -p kranz-engine local_http_context_budget 2>&1 | grep -Eq 'test result: ok\. [1-9]'`)*
- ✅ **[a5]** The orchestrator routes a worker role configured backend=local to a LocalBackend (kind Local, no claude fallback), not to the claude backend. *(command: `cargo test -p kranz-engine local_select 2>&1 | grep -Eq 'test result: ok\. [1-9]'`)*
- ✅ **[a6]** The AgentBackend/AgentSession contract file (backend.rs) is not modified; the new backend implements against the existing trait surface. *(command: `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/backend.rs`)*
- ✅ **[a7]** Code is formatted per the repo's enforced cargo fmt gate. *(command: `cargo fmt --all --check`)*
- ✅ **[a8]** The workspace is clippy-clean under -D warnings. *(command: `cargo clippy --workspace --all-targets -- -D warnings`)*
- ✅ **[a9]** backend_local reuses the existing AgentBackend/AgentSession trait, the BackendKind enum, and the per-role RoleConfig/validate machinery rather than introducing a parallel completion abstraction — the abstraction is not rebuilt. *(agent judgement)*
- ✅ **[a10]** The local completion request is made inside the engine process (not the sandboxed worker subprocess), so no localhost egress-allowlist entry is added to the fs+net sandbox for it. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
