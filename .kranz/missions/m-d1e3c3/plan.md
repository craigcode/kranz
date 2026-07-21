# Mission plan — m-d1e3c3

**Goal:** Add a flight-surgeon outcomes fold (autonomy ratio, grant-latency distribution, escalation ledger) computed per-request from existing event logs by a single engine-owned module, and project it to three thin surfaces — the dashboard panel, `kranz outcomes [--json]`, and a `/kranz outcomes` Slack summary card — with no second source of truth and no change to the events.rs/types.rs contract files.

Branch `kranz/mission-m-d1e3c3` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$10.32 – $51.59** (expected ~$23.80). Rough estimate — live usage is authoritative; based on 48 completed mission(s).

## Considered alternatives

**Chosen approach:** One engine-owned module (crates/engine/src/outcomes.rs) computes the metrics per-request by folding existing event logs — following the trace_export.rs pure-fold precedent and the rest.rs::list_missions enumeration — and the REST endpoint, CLI, Slack card, and dashboard panel are all thin projections of that single fold. It is split into a per-mission pure fold plus a cross-mission aggregator so each unit fits a fresh worker session and the boundary/ordering logic is unit-testable in isolation. This honors the anti-drift 'engine-owned core, thin clients' rule and needs zero change to the events.rs/types.rs contract files.

Rejected shapes:
- **Persist a materialized outcomes cache or state.json field and read that from each surface** — Introduces a second source of truth, risks drift from the logs, and violates the 'pure fold, regenerable, no cache' requirement.
- **Compute the three metrics independently inside each surface (CLI, Slack, dashboard)** — Produces three divergent definitions of the autonomy ratio and latency buckets — exactly the anti-drift failure the ticket forbids.
- **Extend the reducer / MissionState to carry outcomes as part of per-mission fold** — Couples a cross-mission aggregate into per-mission state and would force edits to the frozen types.rs/events.rs contract files.
- **Introduce a distinct command name (e.g. `kranz console`) separate from outcomes-view's `kranz outcomes`** — Fragments the operator surface; the ticket frames both as 'outcomes', so this mission owns `kranz outcomes` with the three consent sections and leaves room for outcomes-view to graft cost/cycle-time onto the same command later.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The autonomy ratio folds correctly: interventions-per-closed-mission and zero-intervention share are computed over terminal missions (completed|failed|abandoned), counting user.message (only after plan.approved), grant.approved/denied, and plan.revised/plan.revision.rejected as interventions. 
  `cargo test --workspace outcomes_ratio 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a2]** Grant latencies (grant.requested → matching grant.approved/denied) bucket correctly at the <10s / <60s / <10m / >=10m boundaries. 
  `cargo test --workspace outcomes_latency 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a3]** The escalation ledger contains exactly one row per block / grant / revision event carrying what was proposed and what the operator decided, ordered newest-first. 
  `cargo test --workspace outcomes_ledger 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a4]** GET /api/missions/outcomes returns autonomyRatio, grantLatency buckets with counts, and the escalation rows, folded per-request across all of the repo's mission logs. 
  `cargo test --workspace outcomes_endpoint 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a5]** `kranz outcomes` renders the fold as text and `kranz outcomes --json` renders the identical data as JSON; an empty history renders the ratio block alone. 
  `cargo test --workspace outcomes_cli 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a6]** `/kranz outcomes` in Slack returns a summary card (autonomy ratio, the four latency buckets with counts, and the escalation count) built from the same engine fold with no Slack-specific metric math. 
  `cargo test --workspace outcomes_slack 2>&1 | grep -qE 'test result: ok\. [1-9]'`
- **[a7]** The dashboard OutcomesPanel renders the three sections with empty states and its tests (buckets, ledger ordering, empty state) pass; all dashboard gates are green including the embedded-bundle sync. 
  `cd apps/dashboard && npx tsc -b && npm run test && npm run build && npm run sync-embedded && npm run check-embedded && npm run lint`
- **[a8]** The contract files crates/engine/src/events.rs and crates/engine/src/types.rs are unchanged by the mission (additive-only discipline; the fold reuses existing events). 
  `test -z "$(git diff --name-only $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs)"`
- **[a9]** The full Rust workspace gate is green (fmt, clippy, build, test). 
  `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo build --workspace && cargo test --workspace`
- **[a10]** The metrics are computed by a single shared engine fold with no persisted cache or second source of truth (regenerable from logs), every surface is a thin projection reusing that fold, and the deferred false-green linkage appears only as a commented optional `defectOf` sketch in the dashboard TS types — no Rust field, endpoint field, or flow. *(agent judgement)*

## Milestone 1 — The outcomes fold is computable and served (engine core + REST + CLI)

### 1.1 Engine outcomes module: types + per-mission fold + latency bucketing

Create a new engine module `crates/engine/src/outcomes.rs` and register it with `pub mod outcomes;` in `crates/engine/src/lib.rs`. Do NOT modify `crates/engine/src/events.rs` or `crates/engine/src/types.rs` — every metric folds from EXISTING events (verify their shapes: GrantRequested/GrantApproved/GrantDenied at events.rs:83-111, MilestoneBlocked/Unblocked at 223-236, PlanRevisionProposed/PlanRevised/PlanRevisionRejected at 61-72, UserMessage at 256, PlanApproved at 52, MissionCompleted/Failed/Abandoned at 277-285). Follow the pure-fold style of `crates/engine/src/trace_export.rs`.

Define these public serde types (rename_all = camelCase):
- `AutonomyRatio { closed_missions: u64, total_interventions: u64, interventions_per_closed_mission: f64, zero_intervention_missions: u64, zero_intervention_share: f64 }` (both f64 fields are 0.0 when closed_missions == 0).
- `LatencyBucket { label: String, count: u64 }` and `GrantLatency { buckets: Vec<LatencyBucket>, total_decided: u64 }`. buckets is ALWAYS the four bucket labels in fixed order: "<10s", "<60s", "<10m", ">=10m".
- `EscalationKind` enum serializing to "block" | "grant" | "revision".
- `EscalationRow { ts: DateTime<Utc>, mission_id: String, kind: EscalationKind, summary: String, decision: String, latency_ms: Option<u64> }`.
- `Outcomes { autonomy_ratio: AutonomyRatio, grant_latency: GrantLatency, escalations: Vec<EscalationRow> }`.
- `MissionOutcomes { interventions: u64, is_closed: bool, latencies_ms: Vec<u64>, escalations: Vec<EscalationRow> }` (per-mission intermediate consumed by the aggregator feature).

Implement `pub fn mission_outcomes(mission_id: &str, events: &[crate::events::Event]) -> MissionOutcomes`:
- `is_closed` = any MissionCompleted/MissionFailed/MissionAbandoned event present.
- interventions: count UserMessage events whose ts >= the first PlanApproved event's ts (if there is no PlanApproved, count zero user messages), plus every GrantApproved, GrantDenied, PlanRevised, PlanRevisionRejected.
- latencies_ms: for each GrantRequested, find the earliest later GrantApproved/GrantDenied with the same `command` (fallback: the next grant decision in seq order if no command matches); latency = (decided.ts - requested.ts) in ms; skip unresolved or negative.
- escalations (one row each): GrantRequested → kind grant, summary = command, decision = "approved" / "denied: {reason}" / "pending", latency_ms as above; MilestoneBlocked → kind block, summary = reason, decision = matching later MilestoneUnblocked "unblocked: {reason}" else "open", latency_ms None; PlanRevisionProposed → kind revision, summary = instructions, decision = PlanRevised(same revision) "accepted (rev N)" / PlanRevisionRejected "rejected: {reason}" / "pending", latency_ms None.

Implement `pub fn bucketize(latencies_ms: &[u64]) -> GrantLatency` mapping each latency to <10s (<10000), <60s (<60000), <10m (<600000), >=10m, filling all four buckets (count 0 when none) and setting total_decided = latencies_ms.len().

Write TDD unit tests in the module over hand-built Event vectors. Name latency-boundary tests with the substring `outcomes_latency` (e.g. `outcomes_latency_bucket_boundaries`) and assert exact counts at 9_999/10_000/59_999/60_000/599_999/600_000 ms. Also test: interventions ignore pre-approval user messages but count post-approval ones and each decision kind; is_closed across the three terminal events; escalation decision strings for approved/denied/pending, block open/unblocked, revision accepted/rejected/pending.

Done when:
- `cargo test --workspace outcomes_latency 2>&1 | grep -qE 'test result: ok\. [1-9]'` passes with tests asserting bucket boundaries at 9999/10000/59999/60000/599999/600000 ms
- A test proves user.message before plan.approved is NOT counted as an intervention while one after it IS, and that grant.approved/denied and plan.revised/plan.revision.rejected each count
- A test proves is_closed is true for a mission with mission.completed, mission.failed, or mission.abandoned and false otherwise
- A test proves each escalation kind (block/grant/revision) yields one row with the correct proposed summary and decision string (including pending/open)
- `git diff --name-only $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs` is empty

### 1.2 Cross-mission aggregator: enumerate logs and compute Outcomes

In `crates/engine/src/outcomes.rs` (created by the per-mission-fold feature; that module already defines the `Outcomes`, `AutonomyRatio`, `MissionOutcomes`, `EscalationRow`, and `bucketize` items — reuse them, do not redefine), add `pub fn compute_outcomes(repo_root: &std::path::Path) -> anyhow::Result<Outcomes>`.

Enumerate every mission exactly as `crates/server/src/rest.rs::list_missions` does (rest.rs:40-81): take `kranz_engine::paths::MissionPaths::list_missions(repo_root)`, union in `kranz_engine::orchestrator::mission_index_ids(<contents of .kranz/missions/index.md>)`, sort, and for each id build `MissionPaths::new(repo_root, &id)`. Skip ids whose `events_file()` is not a file. Read events with the same loader the REST layer uses (`EventLog::read_events`, i.e. `kranz_engine::event_log`); a mission whose log fails to read is skipped (do not fail the whole aggregate) — degrade per-row, never panic.

For each readable mission, call `mission_outcomes(&id, &events)`. Aggregate:
- autonomy_ratio over CLOSED missions only: closed_missions = count where is_closed; total_interventions = sum of interventions over closed missions; interventions_per_closed_mission = total_interventions / closed_missions (0.0 if none); zero_intervention_missions = count of closed missions with interventions == 0; zero_intervention_share = zero_intervention_missions / closed_missions (0.0 if none).
- grant_latency = bucketize(all latencies_ms concatenated across ALL missions, closed or not).
- escalations = all EscalationRows across all missions, sorted newest-first by ts (stable within equal ts).

Write TDD tests that build a temporary repo directory with two or more seeded mission event logs (mirror the temp-repo + seeded-events pattern used in `crates/server/src/tickets.rs` tests and `crates/engine/tests`). Name ratio tests with substring `outcomes_ratio` (e.g. `outcomes_ratio_denominator_is_closed_missions`) and ledger tests with substring `outcomes_ledger` (e.g. `outcomes_ledger_newest_first`). Cover: a repo with a closed mission (some interventions) + an open mission (excluded from the ratio denominator) → correct interventions_per_closed_mission and zero_intervention_share; the ledger merges rows from multiple missions in newest-first order; an empty repo (no missions) → closed_missions 0, both ratios 0.0, four zero-count buckets, empty escalations.

Done when:
- `cargo test --workspace outcomes_ratio 2>&1 | grep -qE 'test result: ok\. [1-9]'` passes with a test proving the ratio denominator counts only closed missions and excludes open ones
- `cargo test --workspace outcomes_ledger 2>&1 | grep -qE 'test result: ok\. [1-9]'` passes with a test proving escalation rows from multiple missions are merged newest-first
- A test proves an empty repo yields closed_missions 0, interventions_per_closed_mission 0.0, zero_intervention_share 0.0, four zero-count latency buckets, and an empty escalation list
- A test proves a mission whose event log is unreadable/corrupt is skipped without failing compute_outcomes

### 1.3 REST endpoint GET /api/missions/outcomes

Add a read-only endpoint `GET /api/missions/outcomes` to the server that returns `kranz_engine::outcomes::compute_outcomes(&server.repo_root)` as JSON. Implement the handler in `crates/server/src/rest.rs` (mirror the style of `list_missions` at rest.rs:40 — takes `State<Arc<ServerState>>`, returns `Json<...>`; on the Result's Err, return a 500 via the existing `ApiError` path). Wire the route into the router where the other `/api/missions*` routes are registered (the router lives in `crates/server/src/lib.rs`; find it via the existing `router`/`router_with_token` constructors used in tickets.rs tests). IMPORTANT: register `/api/missions/outcomes` as a STATIC path so it is never captured by the `/api/missions/:id/*` parameterized routes — place/verify ordering so `outcomes` resolves to this handler, and add a test proving it does not 404 as an unknown mission id.

The endpoint computes per-request from the logs — no caching, no new persisted state.

Write TDD tests in rest.rs (reuse the `router(tmp.path(), None)` + tower `oneshot` request harness already used by the tickets/missions tests). Name them with substring `outcomes_endpoint`. Cover: an empty repo → 200 with body containing `autonomyRatio` (closedMissions 0), `grantLatency.buckets` of length 4 all count 0, and empty `escalations`; a repo seeded with one mission that has a resolved grant and a decided revision → the buckets and escalation rows are populated and the JSON field names are camelCase and match the engine types exactly.

Done when:
- `cargo test --workspace outcomes_endpoint 2>&1 | grep -qE 'test result: ok\. [1-9]'` passes
- A test proves GET /api/missions/outcomes on an empty repo returns HTTP 200 with autonomyRatio, four zero-count grantLatency buckets, and an empty escalations array
- A test proves the route resolves to the outcomes handler and is not swallowed by the /api/missions/:id/* routes (no unknown-mission 404)
- A test proves a seeded repo returns populated buckets and escalation rows with camelCase field names mirroring the engine Outcomes type

### 1.4 CLI command `kranz outcomes [--json]`

Add a `kranz outcomes [--json]` subcommand that prints the repo's outcomes fold. Add an `Outcomes { #[arg(long)] json: bool }` variant to the `Command` enum in `crates/cli/src/cli.rs` (mirror the existing read-only `Status { json }` variant at cli.rs:108), dispatch it where subcommands are handled (mirror how `Status` is dispatched — check `crates/cli/src/lib.rs`/`main.rs`/`commands.rs`), and implement it by calling `kranz_engine::outcomes::compute_outcomes(&repo_root)` using the same repo-root resolution the other read commands use (the global `--repo` flag / current dir).

Rendering (put the text renderer next to the other renderers, e.g. `crates/cli/src/output.rs`):
- `--json`: serialize the `Outcomes` struct with serde_json (pretty) and print it; this is the source of truth for a5's 'identical data' claim.
- default text: print three sections — an Autonomy section (interventions per closed mission, zero-intervention share, closed-mission count), a Grant latency section (the four buckets with counts and total decided), and an Escalation ledger section (newest-first rows: ts, mission, kind, summary, decision, latency). EMPTY-HISTORY RULE: when there are no closed missions AND no escalations AND no decided grants, print only the Autonomy section (zeros) plus a short 'no grants or escalations recorded yet' line — do not print empty bucket/ledger tables.

Write TDD tests (unit-test the render + json functions directly, or drive the command against a temp repo like other CLI tests). Name them with substring `outcomes_cli`. Cover: the `--json` output deserializes back to the same `Outcomes` produced by `compute_outcomes` (identical data); the text render on an empty history contains the Autonomy section and the 'no grants or escalations' line but NOT a bucket/ledger table; the text render on a populated history contains all three section headings.

Done when:
- `cargo test --workspace outcomes_cli 2>&1 | grep -qE 'test result: ok\. [1-9]'` passes
- A test proves `kranz outcomes --json` output round-trips to the same Outcomes value that compute_outcomes returns for the same repo
- A test proves the empty-history text render shows the ratio/Autonomy section alone (no bucket or ledger table)
- A test proves the populated text render includes all three sections (autonomy, grant latency, escalation ledger)


## Milestone 2 — Operator surfaces render the fold (dashboard panel + Slack card)

### 2.1 Dashboard OutcomesPanel with mirrored TS types and empty states

Add a dashboard panel that renders the outcomes fold, mirroring the `GET /api/missions/outcomes` contract exactly (contract-mirror discipline — the TS types must match the engine `Outcomes` serde shape field-for-field, camelCase).

Files (match the existing component style, e.g. `GrantRequestPanel.tsx` / `PipelineView.tsx`):
- Add TS types `Outcomes`, `AutonomyRatio`, `GrantLatency`, `LatencyBucket`, `EscalationRow` to the dashboard types (where the other API types live — `apps/dashboard/src/lib/api.ts` or its types module). On the `EscalationRow` type add a commented optional field sketch: `// DEFERRED (false-green linkage): not wired — see flight-surgeon-console ticket\n  defectOf?: string`. Add NOTHING else for the deferred feature: no fetch of it, no render, no Rust/endpoint counterpart.
- Add a fetch helper `getOutcomes(): Promise<Outcomes>` to `apps/dashboard/src/lib/api.ts` calling `/api/missions/outcomes` (follow the existing fetch helpers, including the read-auth token handling used by other GETs).
- Add `apps/dashboard/src/components/OutcomesPanel.tsx` rendering three sections: Autonomy ratio (interventions per closed mission, zero-intervention share as a percentage, closed-mission count), Grant latency (the four buckets as labeled counts — a simple bar or table), and the Escalation ledger (a newest-first table: ts, mission, kind, summary, decision, latency). Each section has an explicit empty state ('No closed missions yet' / 'No decided grants yet' / 'No escalations recorded').
- Wire the panel into the app so it is reachable (add it to `App.tsx` / the relevant view or sidebar, following how existing panels are mounted).

Write `apps/dashboard/src/components/OutcomesPanel.test.tsx` (mirror the existing `*.test.tsx` vitest + testing-library patterns). Cover: the four latency buckets render with their counts; the escalation ledger renders rows in the newest-first order it receives them; the empty-outcomes payload renders every section's empty state. Run the full dashboard gate and sync the embedded bundle so `check-embedded` passes.

Done when:
- OutcomesPanel.test.tsx asserts the four latency buckets render with their counts
- OutcomesPanel.test.tsx asserts escalation rows render in newest-first order as received
- OutcomesPanel.test.tsx asserts an empty Outcomes payload renders each section's empty state
- The TS Outcomes/AutonomyRatio/GrantLatency/LatencyBucket/EscalationRow types field-names match the engine serde JSON exactly (camelCase), and `defectOf?` exists only as a commented/optional sketch with no fetch or render
- `cd apps/dashboard && npx tsc -b && npm run test && npm run build && npm run sync-embedded && npm run check-embedded && npm run lint` is green

### 2.2 Slack `/kranz outcomes` summary card

Add a `/kranz outcomes` Slack command that returns a summary card built from the same engine fold — no Slack-specific metric math, formatting only.

Routing: in `crates/slack/src/inbound.rs`, add an `outcomes` branch alongside the existing `status`/`todo` handlers (mirror the `strip_ci_prefix(text, "status")` pattern around inbound.rs:918-966). Compute the data by calling `kranz_engine::outcomes::compute_outcomes(<repo root>)`, obtaining the repo root from the bridge's `MissionHost` the same way the other read commands do (the bridge holds an `Arc<MissionHost>`; find the existing repo-root accessor). Do NOT recompute any metric in Slack code.

Formatting: add a card renderer in `crates/slack/src/format.rs` (or wherever `status`/`todo` cards are formatted) that shows: the autonomy ratio (interventions per closed mission and zero-intervention share as a percentage), the four latency buckets with counts, and the total escalation count (escalations.len()). The full ledger table is intentionally web-only. Include a graceful empty-history rendering (zeros / 'no grants or escalations yet').

Write TDD tests (mirror `crates/slack/tests/formatting.rs` / the inbound routing tests). Name them with substring `outcomes_slack`. Cover: given an `Outcomes` value with populated buckets/escalations, the rendered card text contains the ratio, all four bucket labels with their counts, and the escalation count; given an empty `Outcomes`, the card renders without panicking and shows the zero/empty summary. If the router has an allow-list/help registry for `/kranz` subcommands, register `outcomes` there so it is dispatched (and add it to help).

Done when:
- `cargo test --workspace outcomes_slack 2>&1 | grep -qE 'test result: ok\. [1-9]'` passes
- A test proves the rendered card contains the autonomy ratio, all four latency bucket labels with counts, and the escalation count
- A test proves an empty Outcomes value renders a graceful zero/empty summary without panicking
- The Slack handler computes its numbers via kranz_engine::outcomes::compute_outcomes and performs no metric arithmetic of its own (formatting only)

