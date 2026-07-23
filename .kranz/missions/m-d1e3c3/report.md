# Mission report — m-d1e3c3

**Goal:** Add a flight-surgeon outcomes fold (autonomy ratio, grant-latency distribution, escalation ledger) computed per-request from existing event logs by a single engine-owned module, and project it to three thin surfaces — the dashboard panel, `kranz outcomes [--json]`, and a `/kranz outcomes` Slack summary card — with no second source of truth and no change to the events.rs/types.rs contract files.

Branch `kranz/mission-m-d1e3c3` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 6h 32m 11s
**Tokens:** 38270 in / 180936 out / 25541770 cache read / 1015962 cache write
**Cost:** $49.76 actual vs $10.32–$51.59 estimated (expected $23.80)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-d1e3c3-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Preflight:** preflight: clear — no advisory issues recorded

## What shipped

### Milestone 1 — The outcomes fold is computable and served (engine core + REST + CLI) ✅

- ✅ **Engine outcomes module: types + per-mission fold + latency bucketing** — 1 run
  - `7dffb56` [f-1-1] add engine outcomes module: autonomy ratio, grant latency, escalation ledger
- ✅ **Cross-mission aggregator: enumerate logs and compute Outcomes** — 1 run
  - `2132283` [f-1-2] add compute_outcomes: cross-mission aggregator over event logs
- ✅ **REST endpoint GET /api/missions/outcomes** — 1 run
  - `44fd2f2` [f-1-3] add GET /api/missions/outcomes REST endpoint
- ✅ **CLI command `kranz outcomes [--json]`** — 1 run
  - `917a197` [f-1-4] add kranz outcomes CLI command

### Milestone 2 — Operator surfaces render the fold (dashboard panel + Slack card) ✅

- ✅ **Dashboard OutcomesPanel with mirrored TS types and empty states** — 1 run
  - `d81ca02` [f-2-1] add dashboard OutcomesPanel with mirrored TS types and empty states
- ✅ **Slack `/kranz outcomes` summary card** — 1 run
  - `abde4b6` [f-2-2] add /kranz outcomes Slack summary card

## Validation history

### ms-1 round 1 — The outcomes fold is computable and served (engine core + REST + CLI)

No findings.

### ms-2 round 1 — Operator surfaces render the fold (dashboard panel + Slack card)

No findings.

## Contract outcomes

- ✅ **[a1]** The autonomy ratio folds correctly: interventions-per-closed-mission and zero-intervention share are computed over terminal missions (completed|failed|abandoned), counting user.message (only after plan.approved), grant.approved/denied, and plan.revised/plan.revision.rejected as interventions. *(command: `cargo test --workspace outcomes_ratio 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a2]** Grant latencies (grant.requested → matching grant.approved/denied) bucket correctly at the <10s / <60s / <10m / >=10m boundaries. *(command: `cargo test --workspace outcomes_latency 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a3]** The escalation ledger contains exactly one row per block / grant / revision event carrying what was proposed and what the operator decided, ordered newest-first. *(command: `cargo test --workspace outcomes_ledger 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a4]** GET /api/missions/outcomes returns autonomyRatio, grantLatency buckets with counts, and the escalation rows, folded per-request across all of the repo's mission logs. *(command: `cargo test --workspace outcomes_endpoint 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a5]** `kranz outcomes` renders the fold as text and `kranz outcomes --json` renders the identical data as JSON; an empty history renders the ratio block alone. *(command: `cargo test --workspace outcomes_cli 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a6]** `/kranz outcomes` in Slack returns a summary card (autonomy ratio, the four latency buckets with counts, and the escalation count) built from the same engine fold with no Slack-specific metric math. *(command: `cargo test --workspace outcomes_slack 2>&1 | grep -qE 'test result: ok\. [1-9]'`)*
- ✅ **[a7]** The dashboard OutcomesPanel renders the three sections with empty states and its tests (buckets, ledger ordering, empty state) pass; all dashboard gates are green including the embedded-bundle sync. *(command: `cd apps/dashboard && npx tsc -b && npm run test && npm run build && npm run sync-embedded && npm run check-embedded && npm run lint`)*
- ✅ **[a8]** The contract files crates/engine/src/events.rs and crates/engine/src/types.rs are unchanged by the mission (additive-only discipline; the fold reuses existing events). *(command: `test -z "$(git diff --name-only $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs)"`)*
- ✅ **[a9]** The full Rust workspace gate is green (fmt, clippy, build, test). *(command: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo build --workspace && cargo test --workspace`)*
- ✅ **[a10]** The metrics are computed by a single shared engine fold with no persisted cache or second source of truth (regenerable from logs), every surface is a thin projection reusing that fold, and the deferred false-green linkage appears only as a commented optional `defectOf` sketch in the dashboard TS types — no Rust field, endpoint field, or flow. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
