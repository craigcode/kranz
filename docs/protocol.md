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
Every path under an unavailable repository — and every unscoped path when
the explicit default repository is unavailable — returns
`503 {"error":"repository unavailable","repoId",…,"detail",…}`, so an
unhealthy root stays distinguishable from an unknown repo id (404).
Unknown `/api/*` paths always return JSON and never fall through to the
dashboard SPA.

## REST

| Method/Path | Response |
|---|---|
| `GET /api/missions` | `[{ "id", "status", "goal", "createdAt", "merged" }]` (folds each log; tolerate corrupt ones with `"status":"failed"` + `"error"`; `status` now also includes `"approved"` for an approved mission with no run activity yet — additive, backward-compatible). `merged` is a cheap `git merge-base --is-ancestor` probe of the mission branch tip against the LIVE base branch tip (not the pinned `base_sha`): `true` once the base has absorbed the mission's commits (Landed), `false` while still unmerged (Delivered), `null`/absent when there is no mission branch yet or a ref fails to resolve — a per-mission git failure degrades only that row, never the whole list |
| `GET /api/missions/:id/state` | full `MissionState` JSON (fold of events.jsonl; NOT the state.json cache) |
| `GET /api/missions/:id/workspace` | derived local workspace summary: `{"isolation","cwd","lifecycle","worktreeActive","sandboxes":[{role,enforce,extraWriteCount,egressCount}],"preflight":{status,summary,eventSeq},"pin","previews","takeover","contract":{present,services,previews}}`. Values come from folded config, the repository-namespaced deterministic integration-worktree path, and the latest existing `preflight:` decision event (including an explicit clean outcome that supersedes older warnings); no configured paths, hosts, or secrets are copied into the sandbox rows. `contract` is the `.kranz/workspace.json` presence flag (D-H): `present:false` with zero counts when no valid contract exists at the repo root. `pin` is the provider identity pinned at plan approval (`{provider,template,version}` from `workspace.provider.pinned`; `null` on missions approved before pinning existed). `previews`: for local kinds the contract's `[{name,urlTemplate}]` placeholders, UNFILLED and only once a readiness pass is on the log — never a fabricated URL — else `null`; for `remote` the substrate-reported `[{name,url,auth?}]` recorded on the latest `workspace.provisioned` (name-matched, same readiness-pass gate, `auth` omitted when the substrate did not say). `takeover` is how a human takes over: for local-worktree the plain truth "work locally in the workspace cwd (<cwd>)" (no SSH/remote fiction); for `remote` the substrate-reported SSH/web URL from `workspace.provisioned` (`null` until a provision lands); `null` when no pin names the provider |
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
| `GET /api/missions/:id/hook-status` | the EPHEMERAL hook-signal projection (ticket `agent-hooks-status-signals`): `{"missionId","authoritative":false,"note","runs":[{"runId","registeredAt","signal":{"signal","detail","receivedAt"}?}]}`. Signals are hook-derived lifecycle hints (`running` / `needs-input` / `interrupted` / `turn-finished`) re-read per request from the gitignored `.kranz/hook-status/` runtime dir — observability only, NEVER folded mission state; `authoritative` is always `false`. 404 for an unknown mission |
| `POST /api/hook-status` | the hook-status lane's ONLY write: body `{"token","missionId","runId","signal","detail"?}` from a session's `kranz hook-status` relay → `202 {"recorded":true}`. Authenticates with the per-RUN capability token (constant-time compared against the registration's SHA-256 hash) — NOT the serve mutation token, so the route is exempt from the mutation-token gate (like `POST /api/hooks/github`'s HMAC). The body is limited to 16 KiB (`413`); path-traversal ids and unknown runs are `404`; wrong/stale tokens are `401`; a `signal` outside the four-value vocabulary is a `4xx`. The handler's only write is the projection file — no event, no state mutation, no grant path |
| `GET /api/escalation-metrics` | the flight-surgeon console fold (crates/engine/src/escalation_metrics.rs): `{"autonomy":{closedMissions,zeroInterventionMissions,zeroInterventionShare,completed,failed},"rubberStamp":{decidedGrants,p50Ms,p90Ms,underTenSeconds},"falseGreens":{completedMissions,falseGreens,falseGreenRate,withInterventions,zeroIntervention,tracedDefects},"ledger":[{ts,missionId,kind,milestoneId,ask,decision,latencyMs}]}`. Per-host aggregate computed per-request from the mission event logs plus `traced-from-mission` ticket frontmatter; rate/percentile fields are `null` when their denominator is 0 |
| `GET /api/standards-metrics` | deterministic cross-mission Flight Rules fold, split by stable rule/revision: raw applicable/evaluated/advisory/fail/block/waiver/not-evaluated/false-green counts, honest optional rates, resolution time, score distribution, minimum-sample suppression, and evidence-backed checker smells. Recomputed from event logs and traced-defect tickets; no analytics store |
| `GET /api/missions/:id/standards` | approval-pinned manifest, six-state rule coverage, and exact current waiver candidates for one mission. Absence remains not-evaluated, never green |
| `POST /api/missions/:id/standards/waiver` | mutation-authenticated exact human waiver for one current waivable finding; body names rule/revision/finding/reason/expiry. The server re-derives the pin, diff, fingerprint, and authority binding |
| `GET /api/cost-per-merged-change?windowDays=30` | cost per merged change for the served repo (KRZ-329; crates/engine/src/outcomes.rs): `{"windowDays","closedInWindow","totalCostUsd","mergedChanges","usdPerMergedChange","zeroInterventionShare"}`. The denominator is derived at fold time from the event logs plus the live ancestry probe (merged.rs), never stored; `usdPerMergedChange`/`zeroInterventionShare` are `null` when their denominator is 0 (absent, never zero). `windowDays` defaults to 30 and is inclusive at both ends |
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
| `GET /api/tickets` | `[{ "slug", "priority", "state", "title", "blockedBy" }]` — one summary row per parseable ticket under `.kranz/tickets/`, matching what `kranz ticket list` renders; rows also carry the additive `trigger` field (webhook provenance, `null` for human-authored tickets) |
| `GET /api/tickets/:slug` | the full parsed ticket — `slug`, `title`, `priority`, `schedule`, `blockedBy`, `goal`, `context`, `scopingAnswers`, `acceptanceHints`, `trigger`, `state` — plus `needsContext`, the orchestrator's clarifying questions if a prior draft came back NEEDS-CONTEXT, and `wrongPlan`, the planner's escalation reason if it came back WRONG-PLAN (`null` otherwise). `400` for an invalid slug, `404` when no ticket file exists for it |
| `POST /api/tickets/:slug/draft` | long-running: mirrors `POST /api/missions/:id/start` by creating the planning mission synchronously (so a real mission id exists for the response) and spawning the draft turns as a background task. `202 {"missionId":"m-…"}`. Draft progress is observable over that mission's existing `GET /api/missions/:id/ws` WebSocket feed — **not** an SSE feed, since this server has no SSE transport. The terminal outcome (drafted into Review vs NEEDS-CONTEXT) shows up back on `GET /api/tickets/:slug`. `400` for an invalid slug, `404` for an unknown ticket — both checked synchronously before anything spawns |
| `POST /api/tickets/:slug/approve` | body `{"force": bool}` (default `false`) → `200 {"approved":true,"missionId":"m-…"}`. Runs the same gate as `kranz ticket approve`: `409` when the ticket is not in REVIEW; `409` naming the unsatisfied blocker(s) when a `blocked-by` entry has not reached mission-Complete and `force` is false; `409` with the cycle path (e.g. `a -> b -> a`) when a `blocked-by` cycle is reachable from `:slug` — a cycle is never overridable by `force`. `400` for an invalid slug |

See docs/tickets.md for the `blocked-by` dependency primitive itself
(satisfaction semantics, cycle detection, the CLI's `--force`, and the
work-time skip-with-warning) — this section covers only the REST shapes.

## Webhooks (external triggers; design D-F)

`POST /api/hooks/github` accepts GitHub webhooks for CI failures and PR
review comments and drafts ONE audited ticket per trigger through the normal
ticket pipeline — never a prompt loop, never a run, never a land (ticket
`trigger-ci-pr-fix-mission`). The route authenticates with the per-repo
`hooks.secret` HMAC (`X-Hub-Signature-256`: hex HMAC-SHA256 of the raw body),
NOT the mutation token, and refuses CLOSED when no secret is configured.
Config (additive, `.kranz/config.json` — gitignored, so the secret is never
committed): `hooks.secret`, `hooks.fixLabel` (default `kranz:fix`),
`hooks.queueLabel` (default `kranz:fix-and-queue`). In a multi-repository
serve the repo-scoped twin `/api/repos/:repoId/hooks/github` verifies against
THAT repository's config and identity.

| Method/Path | Behavior |
|---|---|
| `POST /api/hooks/github` | Headers `X-GitHub-Event`, `X-Hub-Signature-256`, JSON body. `401` on a bad/missing signature; `403` when no `hooks.secret` is configured (refused closed), when the payload's `repository.full_name` does not match the served repo's origin identity, or when that identity cannot be established (no origin / non-github.com URL); `202 {"outcome":"ignored"}` for non-allowlisted event kinds (anything but `workflow_run`, `issue_comment`, `pull_request_review_comment`) and for allowlisted events matching no trigger rule (non-`completed`/`failure` run, run off the default and `kranz/mission-*` branches, comment without the trigger label, comment not on a PR, non-`created` action); `202 {"outcome":"duplicate","ticketSlug"}` when a ticket already exists for that workflow run id / PR number (dedup — never a second ticket); `202 {"outcome":"drafted","ticketSlug","missionId","queued"}` otherwise. The ticket (`trigger-ci-<run id>` / `trigger-pr-<n>`) carries `trigger: ci-failure\|pr-comment` frontmatter and a provenance block in its Context (source url, actor, consent state, bounded + scrubbed excerpt), then goes through the exact `POST /api/tickets/:slug/draft` pipeline. `queued` is true ONLY when the comment carried the queue label — operator pre-consent to auto-queue on an approved plan; plan approval itself is never skipped, and a queued mission still starts only via `kranz work` / queue drain. Every accepted/ignored/refused request emits one decision log line (source, actor, event kind, consent state, outcome); the secret, the signature, and the raw body are never logged |

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
`x-kranz-token` header. GETs and WS stay tokenless on the default loopback
posture; `--read-auth` or any non-loopback bind gates them with either the
mutation token or the separate read-only token described below.
ONE exemption: `POST /api/hooks/github` (and its repo-scoped twin) — GitHub
cannot present the token, so that route authenticates with its own per-repo
HMAC signature and refuses closed when unconfigured (see §Webhooks).
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
`kranz serve` rewrite writes the endpoint-scoped name). A live single-repo
`.kranz/serve.token` takes precedence over this legacy-only fallback so a
credential left by an ungraceful older serve cannot mask the current token.

## Authority: read-only token

Alongside the mutation token, every `kranz serve` mints a second, READ-ONLY
token (`--read-token` / `$KRANZ_READ_TOKEN` to pin it; generated otherwise)
and stores it next to the mutation token with the same `0600` discipline:
`<repo>/.kranz/serve.read.token` single-repo, or
`~/.kranz/serve/<bound-endpoint>.read.token` for an operator-catalog serve.
Wherever the read gate is armed (`--read-auth`, or any non-loopback bind),
GET/HEAD `/api/...` and the WS upgrade accept EITHER token via
`x-kranz-token` or `?token=`; mutating routes accept ONLY the mutation token
(presenting the read token there is the same `401` as any wrong token). The
read token is therefore the one safe to hand to dashboards and agents — and
the one a deployment should prefer for anything that only observes. Sandboxed
workers can reach neither file: the tier-2 Seatbelt/bwrap profiles explicitly
deny reads of `.kranz/serve.token`, `.kranz/serve.read.token`, and
`.kranz/config.json`, and the tier-3 container masks any such file under the
session root with a `/dev/null` bind.

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
