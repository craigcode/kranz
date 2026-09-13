# Mission plan — m-db35a6

**Goal:** Make the queue dispatcher's drain/claim/skip loop host-callable and expose it as a token-gated POST /api/queue/drain (idempotent while a drain is live), with a dashboard 'Run queue' affordance, a Slack '/kranz work run' verb that triggers the drain on serve (never on the socket loop), and an optional autoWork config (default off) that drains automatically when entries queue.

Branch `kranz/mission-m-db35a6` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Validation contract

Defined before any feature; gates mission completion.

- **[a0]** The whole workspace is formatted, clippy-clean under -D warnings, and every test passes. 
  `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
- **[a1]** The hoisted engine-level queue-drain core runs a queued mission to a terminal state and retires its claim (claim file gone, ticket state advanced), exercised with a mock backend. 
  `cargo test -p kranz-engine`
- **[a2]** After the hoist, the `kranz work` CLI dispatcher still drains the per-repo queue one mission at a time (regression of the existing behaviour). 
  `cargo test -p kranz`
- **[a3]** POST /api/queue/drain with a valid mutation token returns 200 with a drain-state body and spawns a background drain; a second POST while a drain is live returns that same live drain's state instead of starting a second drainer; an empty queue returns an idle drain state (still 200). 
  `cargo test -p kranz-server`
- **[a4]** POST /api/queue/drain with a missing or wrong mutation token is rejected 401 (the POST /api/ token gate covers the new route by construction). 
  `cargo test -p kranz-server`
- **[a5]** Slack routing sends `/kranz work run` to a drain-triggering action that is gated on the spend allowlist, while bare `/kranz work` stays report-only and `/kranz work run <extra>` falls through to help. 
  `cargo test -p kranz-slack`
- **[a6]** The `work run` action is classified as a slow action and triggers the drain by calling the host (PlanningHost::drain); the bridge never resumes or runs a mission on the Socket Mode read loop. 
  `cargo test -p kranz-slack`
- **[a7]** The `autoWork` config key defaults to false, round-trips as camelCase `autoWork`, and a config layer that sets it true is honored by the layered loader. 
  `cargo test -p kranz-engine`
- **[a8]** With autoWork enabled, the serve-side watcher drains a non-empty queue automatically with no explicit POST/CLI trigger; with autoWork disabled (the default) a non-empty queue is left untouched. 
  `cargo test -p kranz-server`
- **[a9]** The dashboard typechecks, builds, and its 'Run queue' affordance component test passes. 
  `cd apps/dashboard && npm ci && npm run test && npm run build`
- **[a10]** The external `kranz work` dispatcher remains fully supported: serve's drain is just another dispatcher arbitrated by the existing claim/lock machinery, introducing no new way for two runners to double-run one mission or for a live drain to be double-spawned. *(agent judgement)*

## Milestone 1 — Host-callable queue drain (engine hoist + REST)

### 1.1 Hoist the drain/claim/skip loop into kranz-engine::work

Create a new module `crates/engine/src/work.rs` (add `pub mod work;` to `crates/engine/src/lib.rs`) that owns the reusable queue-dispatch logic currently living in `crates/cli/src/backlog.rs`. 

MOVE these items from `backlog.rs` into `kranz_engine::work` and re-export them from `backlog.rs` (`pub use kranz_engine::work::{WorkAction, next_work_action, work_skip_for_failed_blocker, ticket_state_for_mission};`) so existing CLI callers/tests compile unchanged — this mirrors how `kranz_engine::draft` was hoisted and re-exported in `backlog.rs`:
  - enum `WorkAction { Empty, Busy{mission_id}, Run{mission_id, ticket_slug} }`
  - `next_work_action(front, busy_with) -> WorkAction`
  - `work_skip_for_failed_blocker(repo_root, slug) -> Result<Option<String>>`
  - `ticket_state_for_mission(status) -> TicketState`
These are pure helpers; move them verbatim (adjust `use` paths — they already only use `kranz_engine::{deps, ticket, types}`).

ADD an async drain core in `kranz_engine::work` that IS the shared drain/claim/skip loop, parameterized over an injected 'run one mission' async closure so each surface supplies its own runner (the CLI tails events to stderr; a headless caller does not):

  pub struct DrainReport { pub ran: Vec<String>, pub skipped: Vec<String> }  // mission ids; extend as needed

  pub async fn drain_queue<R, Fut>(repo_root: &Path, once: bool, run_mission: R) -> Result<DrainReport>
  where R: Fn(String) -> Fut, Fut: Future<Output = anyhow::Result<i32>>

The loop must reproduce the CURRENT semantics of `cmd_work`'s body exactly (read `crates/cli/src/backlog.rs::cmd_work` before writing): call `queue::recover_dead_claims` once up front; then loop `queue::peek` + `queue::is_repo_busy` -> `next_work_action`; on Busy either return (once) or async-sleep 5s and continue; on Run call `queue::claim_front` (async-sleep 250ms + continue on a lost race), take the CLAIMED entry's mission id as authoritative, run the failed-blocker re-check for a ticket-born entry (finish_claim + write ticket Failed with the 'skipped: blocked-by <b> failed' note + continue), write ticket Running, invoke the injected `run_mission(mission_id)` to a terminal exit code, then finish/release the claim with the SAME rule as today (Ok -> finish; Err on a ticket entry -> finish; Err on a bare entry -> release), write the terminal ticket state via `mission_state_from_code`, and honor `once`. Do NOT do checkout restoration or stdout printing in the core — those stay in the CLI wrapper. The core returns Result so a bare-entry run error propagates the way `status?` does today.

DECISION SETTLED (do not redesign): the run step is injected, NOT run inside the engine core, precisely so the CLI keeps its live event tail and serve can run headless.

REWIRE `crates/cli/src/backlog.rs::cmd_work` to be a thin wrapper over `kranz_engine::work::drain_queue`: keep the operator-checkout capture + `restore_work_checkout`, the 'recovered N claimed queue entries' print (move recover_dead_claims's count reporting into the wrapper OR keep it printing from the returned/observed count — simplest is to print in the wrapper before calling the core, matching today), and pass a closure `|mission_id| drive_mission(repo.clone(), &mission_id)` (the existing `run_mission_loop(..., interactive=false)` path that tails events). The observable CLI behaviour (`kranz work` / `kranz work --once`, ticket state transitions, checkout restore, claim recovery) MUST be byte-for-byte unchanged.

TESTS: add unit tests in `kranz_engine::work` proving the drain core (with an injected fake runner and a real on-disk queue in a tempdir) runs a queued mission to Done and retires its claim, skips a ticket whose blocked-by failed (finish_claim + Failed note, no infinite re-claim), and honors `once`. The moved pure-helper tests must continue to pass (relocate them alongside the moved code or keep them in backlog.rs against the re-exports). Do not weaken any existing `cli/tests/backlog_test.rs` coverage.

Done when:
- A kranz-engine unit test drives `drain_queue` with an injected runner over an on-disk queue and asserts the claimed mission ran and its claim file was retired.
- A kranz-engine unit test asserts a ticket entry whose blocked-by dependency is Failed is skipped (claim finished, ticket set Failed with the blocked-by note) rather than re-run.
- `cargo test -p kranz` still passes, proving `cmd_work`'s observable dispatch/claim/checkout-restore behaviour is unchanged after the hoist.
- `WorkAction`, `next_work_action`, `work_skip_for_failed_blocker`, and `ticket_state_for_mission` are importable from both `kranz_engine::work` and (via re-export) `kranz_cli::backlog`.

### 1.2 MissionHost::drain + token-gated POST /api/queue/drain + GET /api/queue

Wire the hoisted `kranz_engine::work::drain_queue` core into the server host so a drain runs as a background task on the serve process, and expose it over REST. Read `crates/server/src/host.rs` (especially `MissionHost`, the `start`/`run_to_end` background-task pattern, the lazy `backend()` accessor, and `ensure_sweeper_started`) and `crates/server/src/lib.rs` (route table + `require_mutation_token`) before writing.

MissionHost changes (`crates/server/src/host.rs`):
  - Add a single-drain tracker to `MissionHost`, e.g. `drain: Arc<Mutex<Option<DrainHandle>>>` where `DrainHandle` holds the `JoinHandle<()>` plus an `Arc<Mutex<DrainState>>` shared with the task so progress is observable. `DrainState` should carry at least: `live: bool`, `current_mission_id: Option<String>`, `ran: Vec<String>`.
  - Add `pub async fn drain(&self) -> Result<Value, ApiError>` that is IDEMPOTENT-WHEN-LIVE: if a tracked drain task exists and is not finished, return that live drain's current state as JSON (do NOT spawn a second). Otherwise construct the backend (via the existing lazy `self.backend(cfg.claude_binary)` accessor, config from `config::load(&self.repo_root)`), spawn a background task that calls `kranz_engine::work::drain_queue(&repo_root, false, run_mission)` where `run_mission` is a HEADLESS runner that does `MissionEngine::resume(backend, repo, &id, LockForce::No)?.run()` and maps the terminal `MissionStatus` to an exit code (0 Complete / 2 Blocked / 1 else, matching `exit_code_for`); the task updates the shared `DrainState` as it goes and clears `live` on exit. Store the handle+state in the tracker and return the initial state JSON. serve's drain is simply ANOTHER dispatcher — it does NOT need to register missions in the `missions` planning registry; the queue claim files + the events.jsonl single-writer lock arbitrate against the external `kranz work` dispatcher exactly as today (do not add new locking).
  - Add `pub fn queue_state(&self) -> Value` returning `{ "entries": [...QueueEntry...], "busyWith": <id|null>, "drain": <DrainState json> }` from `queue::list`, `queue::is_repo_busy`, and the current tracker state.

Axum handlers + routes (`crates/server/src/host.rs` handlers, registered in `crates/server/src/lib.rs::router_with_shared_host`):
  - `POST /api/queue/drain` -> `host.drain()` -> 200 with the drain-state JSON. Register it as a `post(...)` route so `require_mutation_token` (which gates every `POST /api/...`) applies automatically — do NOT add a bespoke auth check. Accept an empty body like the other bodyless POSTs (`parse_body` tolerates empty).
  - `GET /api/queue` -> `host.queue_state()` -> 200. GET is tokenless by design (read-only), consistent with the other read routes.

Update `docs/protocol.md`'s route table with both new endpoints and their response shapes.

TESTS (`crates/server/tests/` or a `#[cfg(test)] mod tests` in host.rs, following the existing `MissionHost::with_backend(root, MockBackend)` + tower `oneshot` patterns already in host.rs): 
  - A drain over a queue containing one mission (mock backend, real on-disk queue + a resumable approved mission — reuse the harness the existing host/queue tests use to stand up a runnable mission; if standing up a full runnable mission is too heavy, assert the observable idempotency + gating contract with an empty/at-most-one queue) returns 200 and the mission is drained / claim retired.
  - Two back-to-back `drain()` calls while the first is live return the same live drain's state and do NOT spawn a second drainer (assert via the tracker / that only one task exists).
  - An empty-queue drain returns 200 with an idle (`live:false` or promptly-settling) state.
  - `POST /api/queue/drain` through the full router WITHOUT the `x-kranz-token` header (when the router is built `Some(token)`) is 401; WITH the correct token is not 401.

Done when:
- `POST /api/queue/drain` with the correct token returns 200 and a JSON drain-state body; without/with a wrong token it returns 401.
- A second `drain()` while one is live returns the live drain's state and starts no second drain task.
- An empty-queue drain returns 200 with an idle drain state (no error).
- `GET /api/queue` returns entries + busyWith + drain state and requires no token.
- docs/protocol.md documents POST /api/queue/drain and GET /api/queue.


## Milestone 2 — Trigger surfaces — dashboard, Slack, autoWork

### 2.1 Dashboard 'Run queue' affordance

Add a 'Run queue' affordance to the dashboard that drains the per-repo queue via the new REST endpoints, following the design principle 'simple lists, easy buttons — one obvious action' (docs/scoping/pipeline-view.md). Read `apps/dashboard/src/lib/api.ts` (the `postJson`/`getJson` + token-retry pattern), `apps/dashboard/src/lib/types.ts`, and an existing panel with an action button (e.g. `apps/dashboard/src/components/BacklogPanel.tsx`, and `MissionPicker.test.tsx` / `BacklogPanel.test.tsx` for the vitest + Testing Library conventions) before writing.

api.ts additions (mirror the existing typed wrappers):
  - `queue(): Promise<QueueState>` -> GET `/api/queue`.
  - `drainQueue(): Promise<DrainState>` -> `postJson('/api/queue/drain', {})`.
  Add matching `QueueState` / `DrainState` types to `types.ts` mirroring the server JSON (`entries`, `busyWith`, `drain:{live,currentMissionId,ran}`).

UI: render a 'Run queue' button in a sensible existing surface (the backlog/queue area). It must: be disabled (or show 'queue empty') when there are no queued entries; show the queued count when there are; on click POST the drain and reflect the live state ('Draining… <currentMissionId>' while `drain.live`); surface a failed POST as an inline error (reuse the ApiError message). Poll `GET /api/queue` (or reuse whatever refresh cadence the panel already uses) so the button reflects live drain state. Use the verb copy 'Run queue' (NOT 'Approve'/'Queue' — per D-A those are reserved for plan approval and ticket-queueing respectively; this is the run-the-queue action).

TESTS: a vitest component test (Testing Library, following the existing `*.test.tsx` files) asserting: the button is disabled/empty-labeled with an empty queue; enabled and showing the count with queued entries; clicking it calls `api.drainQueue` (mock the api module) and renders the draining state from the response. Ensure `npm run test` and `npm run build` (tsc -b && vite build) both pass.

Done when:
- `api.ts` exposes `queue()` (GET /api/queue) and `drainQueue()` (POST /api/queue/drain) sending the mutation token via the existing postJson path.
- A vitest component test asserts the Run-queue button is disabled/empty when the queue is empty and enabled with a count when entries exist.
- A vitest component test asserts clicking the button calls the drain API and renders the live 'draining' state.
- The button copy is 'Run queue' (it does not reuse 'Approve' or 'Queue').
- `npm run test` and `npm run build` both pass in apps/dashboard.

### 2.2 Slack '/kranz work run' verb (triggers drain via the host)

Add a `/kranz work run` verb that triggers the queue drain THROUGH the host (serve), never running a mission on the Socket Mode read loop. Read `crates/slack/src/inbound.rs` (the `work` parsing near the `strip_ci_prefix(text, "work")` block and the `Action` enum), `crates/slack/src/bridge.rs` (`dispatch_action`, `is_slow_action`, the `Action::Work` report-only handler, and the `gate_draft_command`/`run_draft` gate+ack split used for the SPEND verb `draft`), `crates/slack/src/host.rs` (the `PlanningHost` trait), and `crates/cli/src/host_bridge.rs` (the `HostedPlanning` adapter) before writing.

Routing (`inbound.rs`): change the `work` parse so that `work` (bare) stays `Action::Work` (report-only, unchanged), `work run` (case-insensitive, trailing whitespace tolerated) becomes a new `Action::WorkRun { user_id, response_url }`, and `work <anything-else>` (including `work run extra`) still falls through to help. `WorkRun` must carry `user_id` so it can be allowlist-gated.

Trait + adapter: add `fn drain<'a>(&'a self) -> BoxFuture<'a, anyhow::Result<()>>` (or returning a short status string) to `PlanningHost` in `crates/slack/src/host.rs`, and implement it in `HostedPlanning` (`crates/cli/src/host_bridge.rs`) by calling `self.0.drain().await` on the `MissionHost` and mapping the error via the existing `plain` helper. This is the 'POST to serve' seam — the bridge calls the host, which spawns the background drain task; the bridge itself must not resume/run any mission.

Bridge dispatch (`bridge.rs`): add `Action::WorkRun` to `is_slow_action` (it touches a live host / spawns work). In `dispatch_action`, handle `WorkRun` as a SPEND action gated EXACTLY like `/kranz new` / `/kranz draft`: if `!cfg.is_authorized(user_id)` reply `not_authorized_blocks()`; if `host.is_none()` reply an honest 'no hosted engine (started without kranz serve) — use `kranz work` in a terminal' error; otherwise post an immediate ack ('Running the queue — draining now; progress posts per mission') THEN call `host.drain()` and reply with a short confirmation (or the error verbatim on failure). Keep the synchronous gate/ack phase separable from the host call so it is unit-testable without a live SlackClient (mirror `gate_draft_command`). The existing bare `Action::Work` report-only handler and its copy stay unchanged.

Update the help text (`crate::format::build_help`) to mention `/kranz work run`.

TESTS (`crates/slack/tests/routing.rs`, `authz.rs`, and/or bridge unit tests, following existing patterns): `work run` routes to `Action::WorkRun`; bare `work` still routes to `Action::Work`; `work run extra` (and `work now`) route to help; `WorkRun` from an unlisted user is refused with the standard not-authorized blocks and makes no host call; `is_slow_action(Action::WorkRun{..})` is true. If a fake `PlanningHost` is used in tests, assert the authorized path calls its `drain` and that no mission is resumed/run on the read loop.

Done when:
- `/kranz work run` routes to a new `Action::WorkRun`; bare `/kranz work` still routes to `Action::Work` (report-only); `/kranz work run <extra>` routes to help.
- `Action::WorkRun` is allowlist-gated: an unlisted user gets the standard not-authorized reply and triggers no drain.
- `Action::WorkRun` is classified by `is_slow_action` and, when authorized with a host present, triggers the drain via `PlanningHost::drain` — the bridge never resumes/runs a mission on the socket loop.
- With no host wired, `/kranz work run` degrades to an honest ephemeral refusal pointing at the `kranz work` CLI.
- `cargo test -p kranz-slack` passes.

### 2.3 autoWork config + serve auto-drain watcher

Add an opt-in `autoWork` config that makes the serve process drain the queue automatically whenever entries are waiting (default OFF). Read `crates/engine/src/types.rs` (the `MissionConfig` struct + its `Default` impl, and how `planning_idle_release_minutes` is declared/defaulted/serialized as camelCase), `crates/engine/src/config.rs` (layered load + its tests), and `crates/server/src/host.rs` (`ensure_sweeper_started` — the existing lazy background-poll task that reads config fresh each tick) before writing.

Config (`crates/engine/src/types.rs`): add `pub auto_work: bool` to `MissionConfig` (serde camelCase `autoWork` via the existing struct-level rename_all), defaulting to `false` in the `Default` impl. No `validate` constraint needed (a bool is always valid). Add config tests in `crates/engine/src/config.rs` mirroring the existing `planning_idle_release_minutes` tests: default is false; it serializes as camelCase `autoWork`; a config layer setting `{"autoWork": true}` is honored; an absent key keeps the default.

Serve watcher (`crates/server/src/host.rs`): add a lazily-started background task (mirror `ensure_sweeper_started`'s shape: `Mutex<Option<JoinHandle>>`, started at most once, `SWEEP_INTERVAL`-style loop) — call it e.g. `ensure_auto_work_started`. Each tick it re-reads `config::load(&repo_root)` FRESH (so a live config edit takes effect without restart, exactly like the sweeper reads `planning_idle_release_minutes`); when `cfg.auto_work` is true AND the queue is non-empty (`queue::peek(&repo_root).is_some()`) AND no drain is currently live, it calls `self.drain()` (the idempotent host drain from the M1 feature) to kick one off. When `auto_work` is false it does nothing that tick. It must NOT drain when autoWork is off. Start this watcher from the serve command (`crates/cli/src/commands.rs` — the `cmd_serve` path around where `MissionHost::new` is constructed and, under `--slack`, the bridge is spawned): call `host.ensure_auto_work_started()` after constructing the shared host so autoWork works even if no endpoint is ever hit. (Do not start it inside `MissionHost::new` — keep test hosts inert unless they opt in, matching how the sweeper is started on first mission host.)

TESTS (`crates/server/tests/` or host.rs `#[cfg(test)]`, using `MissionHost::with_backend(root, MockBackend)`): with `autoWork:true` written into the repo's `.kranz/config.json` and a runnable mission queued, the watcher (invoke one tick directly, or start it and observe) drains the queue with no explicit `drain()`/POST call; with `autoWork:false` a non-empty queue is left untouched over the same window. If standing up a fully runnable mission is too heavy for this test, at minimum assert the watcher's decision function (autoWork && queue-non-empty && no-live-drain) triggers `drain()` only when enabled — factor that decision into a small pure/testable helper.

Done when:
- `MissionConfig.auto_work` exists, defaults to false, and serializes/deserializes as camelCase `autoWork`.
- A kranz-engine config test proves a `{"autoWork": true}` layer is honored and an absent key keeps the default false.
- With autoWork enabled the serve watcher drains a non-empty queue automatically; with autoWork disabled it leaves the queue untouched.
- The watcher re-reads config each tick (a live toggle takes effect without a restart) and is started from the serve command, not from `MissionHost::new`.
- `cargo test -p kranz-engine` and `cargo test -p kranz-server` pass.

