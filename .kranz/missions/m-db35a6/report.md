# Mission report — m-db35a6

**Goal:** Make the queue dispatcher's drain/claim/skip loop host-callable and expose it as a token-gated POST /api/queue/drain (idempotent while a drain is live), with a dashboard 'Run queue' affordance, a Slack '/kranz work run' verb that triggers the drain on serve (never on the socket loop), and an optional autoWork config (default off) that drains automatically when entries queue.

Branch `kranz/mission-m-db35a6` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 2h 43m 05s
**Tokens:** 60569 in / 254856 out / 34868817 cache read / 1252517 cache write
**Cost:** $140.09 actual vs $9.18–$45.88 estimated (expected $18.35)

## What shipped

### Milestone 1 — Host-callable queue drain (engine hoist + REST) ✅

- ✅ **Hoist the drain/claim/skip loop into kranz-engine::work** — 1 run
  - `b56af3d` [f-1-1] hoist queue drain/claim/skip loop into kranz-engine::work
- ✅ **MissionHost::drain + token-gated POST /api/queue/drain + GET /api/queue** — 2 runs, 1 respawn
  - `9cc5215` [f-1-2] finish queue drain: fix mock backend session accounting, drop debug scaffolding, gofmt
- ✅ **Close the cold-start race in MissionHost::drain idempotency guard** *(fix)* — 2 runs, 1 respawn
  - `a653d00` [ms-1-fix-1-1] add test proving two concurrent cold drains spawn one task
- ✅ **Harden queue-drain test coverage (regression-catching reservation test + engine ticket-success path)** *(fix)* — 1 run
  - `b1ac5be` [ms-1-fix-2-1] harden queue-drain test coverage

### Milestone 2 — Trigger surfaces — dashboard, Slack, autoWork ✅

- ✅ **Dashboard 'Run queue' affordance** — 1 run
  - `42d4910` [f-2-1] add Run queue affordance to the dashboard backlog panel
- ✅ **Slack '/kranz work run' verb (triggers drain via the host)** — 1 run
  - `bcf0e9d` [f-2-2] add /kranz work run verb that triggers drain through the host
- ✅ **autoWork config + serve auto-drain watcher** — 2 runs, 1 respawn
  - `767e848` [f-2-3] add serve watcher tests for autoWork enable/disable

## Validation history

### ms-1 round 1 — Host-callable queue drain (engine hoist + REST)

- [minor] a3 / a10 / f-1-2 crit 2 — idempotency guard against double-spawning a live drain — crates/server/src/host.rs:715-765 MissionHost::drain(): the drain-tracker lock is acquired and dropped in the scoped block at lines 716-725, then config::load + self.backend(...).await run at 727-728 … [truncated]
- [major] a5 — cargo test -p kranz-slack passes (57 tests, 0 failed) but no test or code exists for a `/kranz work run` action gated on the spend allowlist. grep -iE "work" test names shows only: build_work_reply_re… [truncated]
- [major] a6 — No references to PlanningHost::drain or a 'work run' action calling into a host drain exist in crates/slack (grep for 'work run'/'PlanningHost::drain' returns nothing). The bridge only builds a read-o… [truncated]
- [major] a7 — grep -rn "autoWork" crates/ returns no matches anywhere in the workspace — the config key does not exist. cargo test -p kranz-engine passes but contains no autoWork/auto_work test.
- [major] a8 — Since autoWork does not exist (see a7), there is no serve-side watcher behavior gated on it either. grep for autoWork/auto_work across crates/server also returns nothing.
- [major] a9 — `npm run test` (34 tests, 6 files) and `npm run build` both succeed in apps/dashboard, but grep -rniE "run queue|queue.*drain|drainQueue" apps/dashboard/src returns zero matches — no 'Run queue' affor… [truncated]

Disposition: 1 fix feature(s) created.

### ms-1 round 2 — Host-callable queue drain (engine hoist + REST)

- [major] ms-1-fix-1-1: 'Two concurrent drain() calls from a cold/Idle tracker spawn exactly one drain task; a new test asserts this' — crates/server/src/host.rs test `two_concurrent_cold_drains_spawn_exactly_one` runs on the default current-thread runtime and its own comment states the first call's poll runs synchronously all the way… [truncated]
- [minor] a1 / f-1-1: 'runs a queued mission to a terminal state and retires its claim (claim file gone, ticket state advanced)' — The three engine drain tests in crates/engine/src/work.rs cover only: a bare (ticket_slug: None) entry run to success, a ticket-born entry skipped to Failed, and --once. None drives a ticket-born entr… [truncated]
- [minor] ms-1-fix-1-1 sub-criteria: 'failed backend construction clears the reservation' and 'queue_state() reports a Starting drain as live' — The code implements both (host.rs sets DrainSlot::Idle on config/backend error, and queue_state matches DrainSlot::Starting(state) => drain_state_json), but no test asserts either. No test forces a ba… [truncated]
- [critical] a9 — cd apps/dashboard && npm ci && npm run test && npm run build all succeed (34 tests passed, build produced dist/), but grepping apps/dashboard/src for 'Run queue', 'RunQueue', 'drain', 'Drain' finds no… [truncated]

Disposition: 1 fix feature(s) created.

### ms-1 round 3 — Host-callable queue drain (engine hoist + REST)

No findings.

### ms-2 round 1 — Trigger surfaces — dashboard, Slack, autoWork

No findings.

## Contract outcomes

- ✅ **[a0]** The whole workspace is formatted, clippy-clean under -D warnings, and every test passes. *(command: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`)*
- ✅ **[a1]** The hoisted engine-level queue-drain core runs a queued mission to a terminal state and retires its claim (claim file gone, ticket state advanced), exercised with a mock backend. *(command: `cargo test -p kranz-engine`)*
- ✅ **[a2]** After the hoist, the `kranz work` CLI dispatcher still drains the per-repo queue one mission at a time (regression of the existing behaviour). *(command: `cargo test -p kranz`)*
- ✅ **[a3]** POST /api/queue/drain with a valid mutation token returns 200 with a drain-state body and spawns a background drain; a second POST while a drain is live returns that same live drain's state instead of starting a second drainer; an empty queue returns an idle drain state (still 200). *(command: `cargo test -p kranz-server`)*
- ✅ **[a4]** POST /api/queue/drain with a missing or wrong mutation token is rejected 401 (the POST /api/ token gate covers the new route by construction). *(command: `cargo test -p kranz-server`)*
- ✅ **[a5]** Slack routing sends `/kranz work run` to a drain-triggering action that is gated on the spend allowlist, while bare `/kranz work` stays report-only and `/kranz work run <extra>` falls through to help. *(command: `cargo test -p kranz-slack`)*
- ✅ **[a6]** The `work run` action is classified as a slow action and triggers the drain by calling the host (PlanningHost::drain); the bridge never resumes or runs a mission on the Socket Mode read loop. *(command: `cargo test -p kranz-slack`)*
- ✅ **[a7]** The `autoWork` config key defaults to false, round-trips as camelCase `autoWork`, and a config layer that sets it true is honored by the layered loader. *(command: `cargo test -p kranz-engine`)*
- ✅ **[a8]** With autoWork enabled, the serve-side watcher drains a non-empty queue automatically with no explicit POST/CLI trigger; with autoWork disabled (the default) a non-empty queue is left untouched. *(command: `cargo test -p kranz-server`)*
- ✅ **[a9]** The dashboard typechecks, builds, and its 'Run queue' affordance component test passes. *(command: `cd apps/dashboard && npm ci && npm run test && npm run build`)*
- ✅ **[a10]** The external `kranz work` dispatcher remains fully supported: serve's drain is just another dispatcher arbitrated by the existing claim/lock machinery, introducing no new way for two runners to double-run one mission or for a live drain to be double-spawned. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
