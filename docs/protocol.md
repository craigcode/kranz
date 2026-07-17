# Kranz server protocol (REST + WebSocket)

Served by `kranz serve` (crate `kranz-server`, axum). The Tauri shell embeds the
same server and points its webview at it. The server NEVER writes
`events.jsonl` (single-writer rule §4.3) — writes go through the control inbox.

Base: `http://127.0.0.1:<port>` (default 4560). All JSON camelCase, matching
the engine's serde shapes.

When global `host.repos` is configured, every repository operation below is
also available under `/api/repos/:repoId/...` (for example,
`/api/repos/kranz/missions`). `GET /api/repos` returns the static operator
catalog with health, display, grouping, and pin metadata. The unscoped forms
remain migration aliases only for an explicit `host.defaultRepo` or one sole
healthy repository; otherwise they fail without selecting a repository.
Unknown `/api/*` paths always return JSON and never fall through to the
dashboard SPA.

## REST

| Method/Path | Response |
|---|---|
| `GET /api/missions` | `[{ "id", "status", "goal", "createdAt", "merged" }]` (folds each log; tolerate corrupt ones with `"status":"failed"` + `"error"`; `status` now also includes `"approved"` for an approved mission with no run activity yet — additive, backward-compatible). `merged` is a cheap `git merge-base --is-ancestor` probe of the mission branch tip against the LIVE base branch tip (not the pinned `base_sha`): `true` once the base has absorbed the mission's commits (Landed), `false` while still unmerged (Delivered), `null`/absent when there is no mission branch yet or a ref fails to resolve — a per-mission git failure degrades only that row, never the whole list |
| `GET /api/missions/:id/state` | full `MissionState` JSON (fold of events.jsonl; NOT the state.json cache) |
| `GET /api/missions/:id/workspace` | derived local workspace summary: `{"isolation","cwd","lifecycle","worktreeActive","sandboxes":[{role,enforce,extraWriteCount,egressCount}],"preflight":{status,summary,eventSeq}}`. Values come from folded config, the repository-namespaced deterministic integration-worktree path, and the latest existing `preflight:` decision event (including an explicit clean outcome that supersedes older warnings); no configured paths, hosts, or secrets are copied into the sandbox rows |
| `GET /api/missions/:id/events?since=<seq>` | `[Event]` with `seq > since` (omit `since` → all) |
| `GET /api/missions/:id/plan` | contents of plan.json (404 if not approved yet) |
| `GET /api/missions/:id/plan.md` | `{"markdown": "<plan.md contents>"}` (404 if not approved yet) |
| `GET /api/missions/:id/revision-diff` | pending revised-plan review artifact: `{"revision", "instructions", "markdown", "diff"}`. `markdown` is the proposed revised plan rendering; `diff` is a simple line diff from current plan.md to revised-plan.md. 404 if no revision is awaiting approval |
| `GET /api/missions/:id/report.md` | `{"markdown": "<report.md contents>"}` (404 until the mission completes) |
| `GET /api/missions/:id/diff-stat` | `{"diffStat", "baseSha", "tip"}` — `git diff --stat` of the pinned `base_sha` (set at plan approval) against the mission branch tip. 404 if the plan is not approved yet (no `base_sha`) or the mission branch does not exist yet |
| `GET /api/missions/:id/pr-handoff` | PR handoff for a COMPLETE mission (never pushes): `{"kind":"needsPush","command",…}` \| `{"kind":"readyToCreate",…}` \| `{"kind":"unavailable","reason"}`. Probes whether `origin` advertises the mission branch (`git ls-remote`); missing remote branch → copyable `git push` only |
| `POST /api/missions/:id/pr-handoff/create` | runs `gh pr create` only when handoff is `readyToCreate` (remote branch already present). Never `git push`. `409` otherwise |
| `GET /api/missions/:id/readiness` | backend readiness probe for the mission's config: `{"missionId","roles":[{role,backend,status,detail,nextAction}],"overall","warnings"}`. Status enum: `ok` / `missing` / `unauthenticated` / `rate_limited` / `unsupported` / `unknown` / `meterless`. Tokenless |
| `GET /api/missions/:id/runs/:runId/transcript` | JSONL parsed into a JSON array of raw stream values (404 if missing) |
| `POST /api/missions/:id/control` | body = `ControlCommand` JSON (`{"kind":"msg","text":"...","interrupt":false}`, `{"kind":"pause"}`, `{"kind":"resume"}`, `{"kind":"config-change","patch":{...}}`, `{"kind":"request-revision","instructions":"..."}`, `{"kind":"approve-revision","revision":1}`, `{"kind":"reject-revision","revision":1}`) → `202 {"queued":true}`; a config patch is merged with current state and validated synchronously, with invalid backend/model/floor selections returning `400` before enqueue |
| `POST /api/missions/:id/revise` | body `{"instructions":"..."}` → queues `request-revision` for an active, approved-plan mission; `202 {"queued":true}`. Empty instructions are `400`; Planning/terminal missions are `409` |
| `POST /api/missions/:id/revision/approve` | body `{"revision":1}` → queues `approve-revision` for the matching pending revision; `202 {"queued":true}`. Stale/missing revisions are `409` |
| `POST /api/missions/:id/revision/reject` | body `{"revision":1}` → queues `reject-revision` for the matching pending revision; `202 {"queued":true}`. Stale/missing revisions are `409` |
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
| `POST /api/missions/:id/start` | spawns `engine.run()` as a background task → `202 {"running":true}`. Re-invocable when the mission is Blocked (after queueing guidance via control) or after a server restart (`resume` semantics). `409` while already running, or when a multi-repository serve's `host.maxConcurrentRepos` budget is saturated |
| `POST /api/missions/:id/merge` | COMPLETE missions only. Acquires the repo-busy lock, pins live-base + mission-tip SHAs, merges in a detached scratch worktree, runs the base-owned bounded gate suite there, and fast-forwards the base only to the exact tested integration commit → `200 {"merged":true,"commit":"…"}`. Busy/non-complete/conflict/moving-base requests return `409`; invalid/failing gates or secrets return `422` |
| `POST /api/missions/:id/abandon` | optional body `{"reason":"..."}` → the engine's canonical abandon (terminal-refusing, `mission.abandoned` recorded) → `200 {"abandoned":true}`. A mission hosted here is taken out of the registry first (an idle engine is dropped; a running task is aborted and awaited). A lock held by a foreign process → `409` — the web never force-steals |
| `POST /api/missions/:id/delete` | optional body `{"all":true}` → removes a TERMINAL mission's directory, mirroring `kranz clean`: Failed/Abandoned (and planning husks) delete by default; Complete needs `"all":true` (completed missions feed the cost-calibration corpus); live missions and live locks → `409`. POST (not the DELETE verb) so the mutation-token gate applies by construction → `200 {"deleted":true}` |
| `POST /api/missions/:id/release` | un-hosts an idle in-planning mission: drops the engine from the registry and frees its single-writer lock so an external runner (a terminal `kranz plan`, `kranz work`) can take over → `200 {"released":true}`. `409` while a planning turn is in flight (the web never force-steals). `404` for an unknown mission. POST so the mutation-token gate applies by construction; idempotent — calling it on a mission that is not currently hosted still returns `200 {"released":true}`. Prefer the repo-scoped form `POST /api/repos/:repoId/missions/:id/release`; `kranz release` authenticates `GET /api/repos`, maps the selected root against that live catalog, and refuses an unmatched/empty catalog before posting. The unscoped path remains a migration alias only |
| `POST /api/queue/drain` | runs the queue's drain/claim/skip loop (`kranz_engine::work::drain_queue`) as a background task on the serve process — just ANOTHER dispatcher, arbitrating against an external `kranz work` process through the queue claim files and the events.jsonl single-writer lock exactly as today (no new locking) → `200 {"live":bool,"currentMissionId":string|null,"ran":[string],"parked":[string]}`. IDEMPOTENT while a drain is live: a second call while the tracked drain task has not finished returns that live drain's current state instead of spawning a second one. An empty queue settles `live:false` quickly. `409` when a multi-repository serve's `host.maxConcurrentRepos` budget is saturated. Gated by the mutation token like every other POST |
| `GET /api/queue` | the queue front-to-back, who (if anyone) currently holds the busy lock, and this host's own drain tracker → `200 {"entries":[{"missionId","ticketSlug"?,"priority","seq","readiness"?}],"busyWith":string|null,"drain":{"live":bool,"currentMissionId":string|null,"ran":[string],"parked":[string]},"maxConcurrentReposAvailable"?,"maxConcurrentReposSaturated"?}`. Each entry's `readiness` is the same shape as `GET /api/missions/:id/readiness` (best-effort; omitted when the probe fails). The optional `maxConcurrentRepos*` fields appear only when the host participates in a multi-repository `host.maxConcurrentRepos` budget — agents use `maxConcurrentReposSaturated:true` to distinguish "queue empty / autoWork idle" from "global cap blocking drains". Tokenless — read-only |

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
money. Single-repo compatibility stores it at `<repo>/.kranz/serve.token`;
an operator-catalog serve stores the one process token at
`~/.kranz/serve/<bound-endpoint>.token` with mode `0600`, never in every hosted
repository. Automatic CLI discovery matches the URL's complete local endpoint
and refuses host-ambiguous credentials. As a temporary compatibility fallback,
when no endpoint-scoped file matches, discovery also accepts a legacy
`~/.kranz/serve/<port>.token` from earlier serves (deprecated — the next
`kranz serve` rewrite writes the endpoint-scoped name).

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
