# Design: Slack as a full control surface

Status: design. Target milestone: M2.9 (extends M2.75; rides on M2.5's host).

## Premise

Today Slack is a notification + light-steering layer (plan-ready/blocked/
complete, approve button, thread-reply→guidance, `/kranz ticket`). This makes
Slack a **full** management surface: create, plan, review, approve, start,
steer, configure, and run the backlog — without leaving Slack — while deep
forensic inspection hands off to the web UI via a deep link.

## Why it's a small lift: one registry, two clients

M2.5 turned the engine into a request/response API backed by `MissionHost`
(the hosted-engine registry). The web UI's create/plan/approve/start are HTTP
calls into it. **Slack management is a second client of the same registry** —
the Socket Mode handlers call `MissionHost::create / planning_turn /
request_plan / approve / start`, exactly like the axum handlers. No new engine
work; this is Slack-side routing + rendering.

```
              ┌───────── MissionHost (hosted-engine registry) ─────────┐
web UI  ──►   │  create · planning_turn · request_plan · approve · start │   ◄── Slack
(axum)        └────────────────────────────────────────────────────────┘   (socket mode)
```

## Interaction model (thread-centric)

- **New mission**: `/kranz new <goal>` (or an App Home button) → a modal
  (goal, optional model/effort/budget) → `MissionHost::create` → open a thread
  "Planning `m-xxxx`: <goal>"; the orchestrator's opening questions post
  in-thread.
- **Planning conversation**: every human message in a planning mission's thread
  → `planning_turn`; the reply posts threaded. (Requires `message.channels`
  for channels / `message.im` for DMs — already scoped for channels.) This is
  the web planning pane, in a thread.
- **Plan review**: a "Request plan" button / `/kranz plan` in-thread →
  `request_plan`. Ready → a Block Kit message with contract + milestones +
  calibrated estimate and **[Approve & start] [Approve & queue] [Back]**.
  NotReady → the orchestrator's prose posts in-thread (answer, request again).
- **Execution**: Start → `MissionHost::start`; the thread becomes the live
  feed (progress-log lines post threaded, throttled), plus blocked/complete.
- **Steering**: `/kranz pause|resume`, thread replies → guidance (built),
  blocked-milestone unblock (built), `/kranz msg --interrupt`.
- **Config**: `/kranz config` → a modal with per-role model text + effort
  select menus → `config-change`.
- **Backlog**: `/kranz tickets | show <slug> | draft <slug> | approve <slug> |
  queue | work`.
- **App Home tab**: a dashboard listing active missions (status pill), the
  queue, and tickets — each row links to its thread. Lightweight Mission
  Control inside Slack.

## What stays in the web UI (by design)

Deep forensic inspection — full worker/validator transcripts with tool calls,
the dense live four-pane view. These don't render in Slack and shouldn't be
forced. Every mission message carries an **"Open transcript / dashboard"**
deep link (`http://127.0.0.1:<port>/#/m/<id>` with the token fragment) that
jumps to the web UI at the exact run. Full lifecycle + backlog + steering in
Slack; forensics one tap away in the browser.

## Must-haves before it ships

1. **Spend authorization.** Web mutation is token-gated; Slack isn't. A
   `slack.allowUsers: [Uxxxx]` allowlist (Slack user ids) must gate every
   money-spending action (new/draft/start). Unlisted users get an ephemeral
   "not authorized" reply. Solo-workspace default can allow the installer.
2. **Always-on host.** Slack management needs `kranz serve --slack` running and
   hosting engines (the M2.5 registry). Same as web hosting; exactly what an
   M6 Railway deployment provides.
3. **Idempotent actions.** Slack retries envelopes; every handler must ack fast
   (<3s) and dedupe by envelope/action id so a retry can't double-create or
   double-start.

## Build slices (ship incrementally)

- **Slice 1 — lifecycle** (DONE, create-only at first): `/kranz new` + status;
  request-plan and thread-planning were honest stubs until slice 5, and the
  approve paths only queued (see the slice 5 note on the bug that exposed).
- **Slice 5 — hosted planning conversation** (DONE, 2026-07-04): the bridge is
  now a client of the SAME `MissionHost` registry the web UI uses (`kranz
  serve --slack` passes an adapter — `kranz_slack::PlanningHost`, implemented
  in the CLI's `host_bridge` — so `kranz_slack` still never depends on
  `kranz_server`). This lit up, found by the first live end-to-end test:
  - **Thread replies on a PLANNING mission run a planning turn** (acked
    in-thread first; allowlist-gated — it spends). Previously they were
    silently enqueued to a control inbox nothing drained until run time.
    Running-mission replies stay control-inbox guidance, unchanged.
  - **`/kranz plan <id>` works**: immediate ephemeral ack, then the plan-review
    block (goal, milestones, calibrated estimate, **Approve & start** /
    **Approve & queue**) posts to the mission thread; NotReady posts the
    orchestrator's prose. The reviewed plan parks HOST-SIDE in the
    `MissionHost` registry's `pending_plan` slot — ONE cache shared by every
    surface (Slack buttons, web, glasses ring), so any surface's approve
    consumes the same plan. In-memory: a serve restart or idle release
    forfeits it — re-run `/kranz plan`.
  - **Approve commits the plan** (`MissionHost::try_approve_pending` →
    `engine.approve_plan`, same plan.json/plan.md/index.md commit as the CLI)
    before queueing or starting. The first cut only inserted a queue entry, so
    `kranz work` later refused the un-approved mission — a silent dead end.
  - **`MissionHost` lazily attaches** an on-disk in-planning mission into its
    registry (planning_turn/request_plan/approve), so missions created by the
    CLI or by a pre-slice-5 bridge keep working after their engine was
    released. Trade-off: once attached, serve holds the mission's
    single-writer lock, so a concurrent `kranz plan --mission` in a terminal
    sees LockHeld until serve releases it. **closed by idle-release**: an
    idle attached planning engine is now auto-released after
    `planningIdleReleaseMinutes` (default 30; `0` disables the sweep), and
    `kranz release [--mission <id>]` (`POST /api/missions/:id/release`) frees
    it on demand, so a terminal `kranz plan`/`kranz work` is no longer
    stranded behind serve.
  - Slash commands typed INSIDE a thread are rejected by Slack itself
    (platform rule); the thread footers now say so and name the id to use
    from the channel.
- **Slice 5b — new-mission modal** (DONE, 2026-07-04): slash commands are also
  SINGLE-LINE (a pasted multiline goal never dispatches — Slack sends it as a
  plain message the bridge ignores), so bare `/kranz new` now opens a modal
  with a real multiline goal field (`views.open` on the slash `trigger_id`,
  which expires ~3 s — opened inline, never from a spawned task). The
  invoking channel rides in the view's `private_metadata`; the
  `view_submission` routes into the SAME `Action::NewMission` path as the
  one-line form, spend-gated identically. Modal-path replies (acks, errors)
  go out via `chat.postEphemeral` — a `view_submission` has no
  `response_url`. The `/kranz config` modal from the original design remains
  unbuilt.
- **Slice 2 — control & status** (DONE): `/kranz status` (slice 1) plus
  `/kranz config`, deep-link buttons, and the spend allowlist (which now also
  gates the approve *button*, not just `/kranz approve`).
- **Slice 3 — App Home** (DONE): the dashboard tab (missions + queue + tickets),
  published on `app_home_opened`.
- **Slice 4 — steering** (DONE): `/kranz pause [<id>]`, `/kranz resume [<id>]`,
  and `/kranz work`. `pause`/`resume` enqueue a `ControlCommand::Pause`/`Resume`
  on the target mission's control inbox — the **same mechanism** `kranz pause` /
  `kranz resume` use — so they need **no hosted engine**, just a control-command
  file write (LOW RISK). `work` is **report-only**: it reads the queue and points
  at the `kranz work` dispatcher (the bridge never runs a mission on the socket
  loop). See "Slice 4 implementation notes" below.
- **Slice 6 — backlog verbs** (DONE): `/kranz ticket list` and `/kranz ticket
  show <slug>` (read-only, no allowlist gate, ephemeral replies — same shape as
  `/kranz status`); `/kranz draft <slug>` (SPEND, gated on `slack.allowUsers`
  exactly like `/kranz new`: an `:hourglass_flowing_sand:` ack posts
  immediately, then the terminal draft outcome — ready-for-review, approved
  and queued, or NEEDS-CONTEXT with the orchestrator's questions appended);
  and approve-by-slug, `/kranz approve <slug>` (the slug-resolving twin of
  `/kranz approve <mission-id>`, same allowlist gate, running the identical
  `kranz_engine::deps::approve_ticket` gate the CLI/REST approve paths run so
  a blocked-by/cycle/not-REVIEW refusal is forwarded to the user verbatim).
  The dashboard's backlog panel (`#/backlog`) rides the same REST surface, so
  Slack and the web UI never drift on what "approvable" means.

## Slice 2 & 3 implementation notes

### `/kranz config [<id>] <role> <model> [effort]`

Per-role model/effort change, mid-mission. **Spend-adjacent** (it re-shapes what
future turns spend), so it is gated on the `slack.allowUsers` allowlist exactly
like `/kranz new`; unlisted users get the same ephemeral "not authorized" reply.

- **Roles**: `orchestrator` · `worker` · `scrutiny` · `functional`. The two
  validator roles use the short forms; they map to the engine's config keys
  `validatorScrutiny` / `validatorFunctional`. `effort` (optional) is one of
  `low` · `medium` · `high` · `xhigh` · `max`.
- **Mission targeting** (chosen convention): the id is **optional** and
  disambiguated positionally. If the first argument is a known role, there is no
  id and the change applies to the repo's **single active mission** — with
  several active it refuses and asks for an explicit id, and a terminal target
  is rejected (its control inbox is never drained, so the change would be a
  silent no-op). Otherwise the first argument is the mission id:
  `/kranz config m-42 worker sonnet high`. A bad role/effort or wrong arity
  falls through to `/kranz help` (a typo is discoverable, never a silent
  surprising action). An unknown mission id is a plain ephemeral error.
- **Wiring**: on authorization, the bridge enqueues
  `ControlCommand::ConfigChange { patch }` onto the target mission's control
  inbox, where `patch` is the camelCase engine patch, e.g.
  `{"worker":{"model":"sonnet","reasoningEffort":"high"}}`. The role→key mapping
  and patch shape are a pure, table-tested function (`inbound::config_patch`).
- **CLI twin**: `kranz config role <role> <model> [effort] [--mission <id>]`
  enqueues the identical patch through the same machinery; target resolution
  (active missions only, refuse ambiguous/terminal) is shared via
  `kranz_engine::control::resolve_active_mission`, so both surfaces refuse the
  same hazardous targets.

### Deep-link buttons (`slack.dashboardUrl`)

Optional config field `slack.dashboardUrl` (from `~/.kranz/config.json` or
`KRANZ_SLACK_DASHBOARD_URL`, env winning). When **set**, every mission
notification (plan-ready, blocked, complete) and every App Home mission row
carries an **"Open in dashboard"** link pointing at
`<dashboardUrl>#/m/<mission_id>`. When **unset**, no button is added — no
behavior change from before. The link is a Block Kit *link button* (`url`, no
`action_id` handler needed), so it opens the browser directly with no inbound
routing.

### App Home tab

On an `app_home_opened` event the bridge folds the repo **read-only**
(`EventLog::read_events` + `reducer::fold` per mission for status; `queue::list`;
`Ticket::list` + `Ticket::read_state`) and publishes a Block Kit **home** view
via `views.publish` (`SlackClient::publish_home_view`, bot token). The view lists
active (non-terminal) missions with a status pill, the execution queue, and open
(not Done/Failed) tickets.

**Manifest / scope prerequisite (must re-apply the manifest / reinstall):**
enabling the Home tab is a manifest change, so an existing install will NOT show
the tab until the app is updated. `docs/slack-app-manifest.yaml` now sets
`features.app_home.home_tab_enabled: true` and adds `app_home_opened` to the
subscribed `bot_events`. Re-paste the manifest (api.slack.com/apps → your app →
"App Manifest") and **reinstall the app to the workspace** so the new event
subscription + Home tab take effect. `views.publish` itself needs **no OAuth
scope beyond the existing `chat:write`** — it is authorized by the bot token and
the only prerequisite is the Home tab being enabled — so reinstalling does not
re-prompt for broader permissions. If a workspace hasn't reinstalled, Slack
returns `{"ok":false,"error":…}` from `views.publish`; the bridge logs it and
carries on (a missing Home tab never wedges the socket loop).

## Slice 4 implementation notes (steering)

### `/kranz pause [<id>]` · `/kranz resume [<id>]`

Enqueue `ControlCommand::Pause` / `Resume` onto the target mission's control
inbox (`kranz_engine::control::enqueue`) — the identical mechanism `kranz pause`
/ `kranz resume` use. **No hosted engine** is needed (unlike `request_plan`),
just a control-command file write, which is why this slice is LOW RISK and lands
without touching `kranz_server`.

- **Gating**: these are STEERING, not spend — but a stray pause disrupts a
  running mission, so they are gated on the `slack.allowUsers` allowlist exactly
  like `/kranz config` (consistent, and honest about who can perturb a run).
  Unlisted users get the same "not authorized" ephemeral.
- **Mission targeting**: the id is optional and resolved by the same
  `resolve_active_config_target` helper `/kranz config` uses — an explicit id
  must exist and be **active** (a terminal mission's control inbox is never
  drained, so pausing it would be a silent no-op reported as success → rejected);
  a bare command needs **exactly one** active mission (several → an honest error
  asking for an explicit id; none → an error). A bad/ambiguous/terminal target
  **enqueues nothing** and returns an ephemeral error. Extra tokens
  (`pause m-1 extra`) fall through to `/kranz help`.
- **Confirmation**: an ephemeral "paused `m-xxx` (takes effect between worker
  runs)" — the pause/resume, like the CLI, is applied by the engine between
  worker runs, not instantly.

### `/kranz work` (report-only)

Reports the per-repo execution queue (`queue::list`) and whether the repo is
currently busy (`queue::is_repo_busy`) as an ephemeral, and points at the
`kranz work` dispatcher for actually draining it. **The bridge must never spawn
`claude` on the socket read loop**, so it does not run missions inline — it
reports and hands off. Read-only, so no allowlist gate. (Draining runs via the
external `kranz work` CLI: `kranz work` drains the whole queue, `kranz work
--once` the front entry. There is no "please run the queue" signal file the
dispatcher watches — it polls the queue on its own timer — so report-only is the
correct and safe surface here.)

## Running multiple instances

Kranz is per-machine: two Macs today, a cloud host later, each running its own
`kranz serve --slack`. The supported topology is **one Slack app per
instance**. Clone the app for every machine; never point two bridges at the
same app's tokens.

### Why one app per instance

- **Socket Mode load-balances, it does not broadcast.** All open Socket Mode
  connections of a *single* app form one pool, and Slack delivers each inbound
  envelope (slash command, button click, thread reply) to **one** connection in
  that pool, round-robin-ish. Two Kranz instances sharing one app therefore
  each receive a random ~half of the commands: `/kranz approve m-42` lands on
  the studio Mac or the laptop by coin flip, and only one of them has mission
  `m-42`. There is **no channel-based filtering** — an instance cannot say
  "only give me envelopes from #kranz-studio" — so this cannot be routed around
  at the app layer. One app per instance means one connection pool per
  instance, and every envelope reaches the machine that owns the app.
- **Outbound is fine either way** — `chat.postMessage` goes wherever the poster
  says — it is *inbound* routing that forces the split.

### Setting it up per machine

1. **Clone the app** from [docs/slack-app-manifest.yaml](slack-app-manifest.yaml)
   (api.slack.com/apps → "Create New App" → "From an app manifest"), once per
   machine. **Name it per machine** — `Kranz (studio)`, `Kranz (laptop)`,
   `Kranz (cloud)` — so the workspace's app list and message author lines stay
   legible.
2. **Give each app a distinct slash-command name** — `/kranz-studio`,
   `/kranz-laptop`, … — in the manifest's `slash_commands` block. Slash-command
   names are workspace-global and **duplicates are last-install-wins**: if two
   apps both register `/kranz`, the most recently (re)installed app silently
   captures ALL `/kranz` invocations and the other app never sees any. A
   distinct name per app is required, not cosmetic.
3. **Per-machine `~/.kranz/config.json`**: each machine's `slack` block holds
   *its own app's* `botToken` / `appToken` (tokens are per-app), plus an
   `instanceName` naming the machine:

   ```json
   {
     "slack": {
       "botToken": "xoxb-…the studio app's bot token…",
       "appToken": "xapp-…the studio app's app token…",
       "channel": "C0123ABC",
       "instanceName": "studio"
     }
   }
   ```

   `instanceName` (or the `KRANZ_SLACK_INSTANCE` env var, env winning) makes
   the instance visibly distinct: every message the bridge posts —
   notifications, `/kranz help`, ephemeral approve/config/pause confirmations,
   the App Home header — carries a leading `[studio]` label, so you can always
   tell which machine is talking (and which one just answered your command).
   Unset, nothing is labeled — single-instance behavior is unchanged. The name
   is treated as plain text (mrkdwn-escaped), never interpreted.
4. **Channels: separate recommended, optional.** One channel per instance
   (`#kranz-studio`, `#kranz-laptop`) keeps feeds untangled and is the
   recommended default. A shared channel also works — inbound routing is
   per-app regardless of channel, and the `[instanceName]` label keeps the
   shared feed readable. Just remember which app's slash command drives which
   machine.

### Cloud shapes (see [docs/deploy.md](deploy.md))

- **Ephemeral `kranz exec` containers are outbound-only.** A CI-shaped
  one-mission container runs no bridge, opens no Socket Mode connection, and
  registers no commands — it (at most) posts progress. No app cloning needed:
  posting into a shared channel with a run-id tag is safe, because the
  misrouting hazard above is inbound-only.
- **A persistent cloud `serve --slack` is a full instance** and follows the
  same rule: its own cloned app, its own tokens — injected as `KRANZ_SLACK_*`
  env vars from the platform's secret store rather than a config file — and
  `KRANZ_SLACK_INSTANCE=cloud` so its messages are labeled.
- **Simplest consolidated topology:** make the cloud host the **only**
  Slack-connected instance. One app, one `/kranz` command, one always-on
  bridge; the Macs run missions locally without `--slack` (or drive the cloud
  host's queue). You give up per-machine Slack control but keep a single
  uncloned app and zero routing ambiguity.

## Done when

A mission goes goal → planning conversation → reviewed plan → approve → start →
complete entirely in Slack; the backlog is fully operable via `/kranz`;
unauthorized users cannot spend; and any mission's transcripts are one tap from
its thread in the web UI.
