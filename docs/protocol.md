# Kranz server protocol (REST + WebSocket)

Served by `kranz serve` (crate `kranz-server`, axum). The Tauri shell embeds the
same server and points its webview at it. The server NEVER writes
`events.jsonl` (single-writer rule §4.3) — writes go through the control inbox.

Base: `http://127.0.0.1:<port>` (default 4560). All JSON camelCase, matching
the engine's serde shapes.

## REST

| Method/Path | Response |
|---|---|
| `GET /api/missions` | `[{ "id", "status", "goal", "createdAt" }]` (folds each log; tolerate corrupt ones with `"status":"failed"` + `"error"`; `status` now also includes `"approved"` for an approved mission with no run activity yet — additive, backward-compatible) |
| `GET /api/missions/:id/state` | full `MissionState` JSON (fold of events.jsonl; NOT the state.json cache) |
| `GET /api/missions/:id/events?since=<seq>` | `[Event]` with `seq > since` (omit `since` → all) |
| `GET /api/missions/:id/plan` | contents of plan.json (404 if not approved yet) |
| `GET /api/missions/:id/plan.md` | `{"markdown": "<plan.md contents>"}` (404 if not approved yet) |
| `GET /api/missions/:id/report.md` | `{"markdown": "<report.md contents>"}` (404 until the mission completes) |
| `GET /api/missions/:id/diff-stat` | `{"diffStat", "baseSha", "tip"}` — `git diff --stat` of the pinned `base_sha` (set at plan approval) against the mission branch tip. 404 if the plan is not approved yet (no `base_sha`) or the mission branch does not exist yet |
| `GET /api/missions/:id/runs/:runId/transcript` | JSONL parsed into a JSON array of raw stream values (404 if missing) |
| `POST /api/missions/:id/control` | body = `ControlCommand` JSON (`{"kind":"msg","text":"...","interrupt":false}`, `{"kind":"pause"}`, `{"kind":"resume"}`, `{"kind":"config-change","patch":{...}}`) → `202 {"queued":true}` |
| `GET /api/health` | `{"ok":true,"version":"<crate version>"}` |

Static dashboard files served from a configurable dir at `/` (SPA fallback to
index.html).

## Tickets (backlog over REST)

Read-only routes re-parse `.kranz/tickets/<slug>.md` from disk on every
request (no cache, matching the rest of this crate). Both `POST` routes are
mutation-token gated like every other `POST` (see §Authority below), and
every route validates `:slug` at the route boundary before touching the
filesystem — an invalid slug (bad chars, traversal) is `400`, never a
filesystem error.

| Method/Path | Behavior |
|---|---|
| `GET /api/tickets` | `[{ "slug", "priority", "state", "title", "blockedBy" }]` — one summary row per parseable ticket under `.kranz/tickets/`, matching what `kranz ticket list` renders |
| `GET /api/tickets/:slug` | the full parsed ticket — `slug`, `title`, `priority`, `schedule`, `blockedBy`, `goal`, `context`, `scopingAnswers`, `acceptanceHints`, `state` — plus `needsContext`, the orchestrator's clarifying questions if a prior draft came back NEEDS-CONTEXT. `400` for an invalid slug, `404` when no ticket file exists for it |
| `POST /api/tickets/:slug/draft` | long-running: mirrors `POST /api/missions/:id/start` by creating the planning mission synchronously (so a real mission id exists for the response) and spawning the draft turns as a background task. `202 {"missionId":"m-…"}`. Draft progress is observable over that mission's existing `GET /api/missions/:id/ws` WebSocket feed — **not** an SSE feed, since this server has no SSE transport. The terminal outcome (drafted into Review vs NEEDS-CONTEXT) shows up back on `GET /api/tickets/:slug`. `400` for an invalid slug, `404` for an unknown ticket — both checked synchronously before anything spawns |
| `POST /api/tickets/:slug/approve` | body `{"force": bool}` (default `false`) → `200 {"approved":true,"missionId":"m-…"}`. Runs the same gate as `kranz ticket approve`: `409` when the ticket is not in REVIEW; `409` naming the unsatisfied blocker(s) when a `blocked-by` entry has not reached mission-Complete and `force` is false; `409` with the cycle path (e.g. `a -> b -> a`) when a `blocked-by` cycle is reachable from `:slug` — a cycle is never overridable by `force`. `400` for an invalid slug |

See docs/tickets.md for the `blocked-by` dependency primitive itself
(satisfaction semantics, cycle detection, the CLI's `--force`, and the
work-time skip-with-warning) — this section covers only the REST shapes.

## Mission lifecycle (server-hosted engine; M2.5)

`kranz serve` can HOST missions: for missions it creates, the server process
IS the single-writer engine (it holds the mission lock; a concurrent
`kranz run` correctly refuses, and either side can resume what the other
started — the event log is the source of truth).

| Method/Path | Behavior |
|---|---|
| `POST /api/missions` | body `{"goal":"...", "config": {optional partial MissionConfig patch}}` → creates the mission (engine held in the server registry) → `201 {"id":"m-…"}` |
| `POST /api/missions/:id/planning/turn` | body `{"text":"..."}` → runs one planning turn → `200 {"reply":"..."}`. Seed replies are prepended. Activity streams over the WS feed as usual. `409` if the mission is not hosted here, not in planning, or a turn is already in flight |
| `POST /api/missions/:id/planning/request-plan` | → `200 {"ready":true, "plan":{...}, "estimate":{...CostEstimate}}` or `200 {"ready":false, "reply":"<orchestrator prose>"}` (NotReady returns to conversation) |
| `POST /api/missions/:id/approve` | body `{"plan":{...}}` (the plan previously returned) → commits plan.json/plan.md/index.md exactly like the CLI → `200 {"branch":"kranz/mission-…"}` |
| `POST /api/missions/:id/start` | spawns `engine.run()` as a background task → `202 {"running":true}`. Re-invocable when the mission is Blocked (after queueing guidance via control) or after a server restart (`resume` semantics). `409` while already running |
| `POST /api/missions/:id/abandon` | optional body `{"reason":"..."}` → the engine's canonical abandon (terminal-refusing, `mission.abandoned` recorded) → `200 {"abandoned":true}`. A mission hosted here is taken out of the registry first (an idle engine is dropped; a running task is aborted and awaited). A lock held by a foreign process → `409` — the web never force-steals |
| `POST /api/missions/:id/delete` | optional body `{"all":true}` → removes a TERMINAL mission's directory, mirroring `kranz clean`: Failed/Abandoned (and planning husks) delete by default; Complete needs `"all":true` (completed missions feed the cost-calibration corpus); live missions and live locks → `409`. POST (not the DELETE verb) so the mutation-token gate applies by construction → `200 {"deleted":true}` |
| `POST /api/missions/:id/release` | un-hosts an idle in-planning mission: drops the engine from the registry and frees its single-writer lock so an external runner (a terminal `kranz plan`, `kranz work`) can take over → `200 {"released":true}`. `409` while a planning turn is in flight (the web never force-steals). `404` for an unknown mission. POST so the mutation-token gate applies by construction; idempotent — calling it on a mission that is not currently hosted still returns `200 {"released":true}` |

Hosted-engine rules: planning endpoints serialize per mission (one turn at a
time) and lazily ATTACH an on-disk in-planning mission into the registry
(resume under the single-writer lock); `start` consumes the hosted engine
into the run task; when the run ends (Complete/Blocked/Failed) the registry
entry is dropped and the lock released — the mission is then
observable/resumable from anywhere.

## Authority: mutation token

Every `POST /api/...` requires the per-serve session token via the
`x-kranz-token` header (WS and GETs stay tokenless — read-only observation).
The token is generated at serve start (or passed in by the embedding Tauri
shell), printed to the operator, and appended by `--open` as `#token=<t>` in
the launched URL; the dashboard stores it (sessionStorage) and shows a
paste-token field when a mutation is attempted without one. Missing/wrong
token → `401 {"error":"missing or invalid token"}`. Rationale: 127.0.0.1
binding + CORS stop the network and the browser; the token stops other local
processes and link-borne CSRF from creating or steering missions that spend
money.

## WebSocket `GET /api/missions/:id/ws?since=<seq>`

Server → client messages (JSON text frames):

```json
{ "type": "snapshot", "seq": 412, "state": { ...MissionState } }
{ "type": "event",    "seq": 413, "event": { ...Event } }
{ "type": "state",    "seq": 420, "state": { ...MissionState } }
```

- On connect: one `snapshot` (current fold + its seq).
- If `?since=` was given and the gap from `since` to head is ≤ 5000 events, the
  server instead replays `event` frames from `since+1` (no snapshot) — client
  keeps its state. Larger gap (or `since` ahead of head, or unparsable): fresh
  `snapshot`. Either way the client just handles both frame kinds.
- Live tail: server polls events.jsonl (`read_events_after`) every 250 ms and
  pushes `event` frames in seq order.
- After every NON-`worker.message` event (lifecycle), the server also pushes a
  `state` frame (full re-fold) — clients may use it instead of client-side
  reduction (the Rust reducer stays the single source of truth; the UI is a
  pure view).
- Client → server frames are ignored except `{"type":"ping"}` → `{"type":"pong"}`.

Reconnect contract (plan Phase 3 acceptance): client tracks the last `seq`
seen; on reconnect passes it as `?since=`; kill/restart of the dashboard or
server loses nothing because the log is the source of truth.
