---
title: Slack /kranz command surface
owner: agent
freshness: check-on-touch
last_verified: 2026-09-16
verified_against:
  - crates/slack/src/catalog.rs
  - crates/slack/src/inbound.rs
  - crates/slack/src/bridge.rs
  - crates/slack/src/dispatch.rs
  - crates/slack/src/approve_flow.rs
  - crates/slack/src/format.rs
  - crates/slack/src/host.rs
  - crates/slack/src/config.rs
  - crates/cli/src/host_bridge.rs
  - crates/server/src/host.rs
  - crates/engine/src/planning.rs
  - 'command: cargo test -p kranz-slack'
---

# Slack /kranz command surface

The single Slack bridge speaks Socket Mode for every repository in the host
catalog. Every inbound interaction arrives as a JSON *envelope* which the
bridge must ack within Slack's 3 s budget. `crate::catalog` first resolves an
explicit `repo:<id>` selector, modal/thread affinity, channel mapping, or the
only/default healthy repository and fails closed on ambiguity. Routing is then
split from side effects: `crate::inbound` is a **pure decision layer** (no I/O,
no clock) that maps the resolved envelope to an [`Action`], and
`crate::bridge` applies it.

## Routing

`inbound::route(envelope, lookup)` dispatches on the envelope `type`:
- `interactive` → `route_interactive` (button clicks / modal `view_submission`)
- `events_api` → `route_event` (thread replies, `app_home_opened`)
- `slash_commands` → `route_slash` (the `/kranz` subcommand router)
- anything else (`hello`, `disconnect`, unknown) → `Action::Ignore`

It returns `Routed { action, envelope_id }` so the caller can ack even an
`Ignore`. Thread→mission resolution goes through the `ThreadLookup` trait (a
closure in tests, `SharedThreads` in production) — that is the only external
input, keeping routing pure and unit-testable against captured fixtures.

`route_slash` requires `command == "/kranz"`, then peels subcommands with
`strip_ci_prefix` (case-insensitive, word-boundary aware, so `ticketing` ≠
`ticket`). Ambiguous prefixes are checked in a deliberate order: `ticket
list`/`show`/`new` are matched *before* the bare `ticket <title>` fallback, so a
ticket literally titled "list" is the one surprising edge case. Any unrecognized
or incomplete subcommand falls through to `Action::Help` — a typo lands on the
command list, never a silent surprise. `clean_id` strips backticks/quotes/angle
brackets that a Slack copy-paste smuggles around id tokens (goals are never
cleaned).

## Auth model: ungated reads, gated spend/mutation

The gate is `SlackConfig::is_authorized(user_id)` in
[config.rs](../../../crates/slack/src/config.rs) — deliberately kept out of the
router so routing stays pure and config-free. Policy: an **empty** `allowUsers`
list FAILS CLOSED for privileged actions (nobody authorized, including an
absent `user_id`) unless the operator deliberately opens spend with
`slack.allowAllUsers: true`; a **non-empty** list authorizes
only its members, and a blank/absent `user_id` is denied whenever a list is set
(a spoofed-empty user can't slip past). Config key is `slack.allowUsers` in
`~/.kranz/config.json` (camelCase in the file; `allow_users` in Rust).

Read-only slash actions don't even carry a `user_id` (structurally ungated). The
one read-only action that *does* carry a `user_id` is `app_home_opened`
(`Action::AppHome`), and it carries one only to target *which* user's home tab to
publish — it is still ungated (its dispatch arm makes no `is_authorized` check).
Spend/mutation actions carry the invoking `user_id`, and their dispatch path —
the `dispatch_action` arm itself or a gate helper it calls (`approve_flow`,
`steer`, `revision_control`, `gate_draft_command`, `run_approve_ticket_command`,
…) — checks `is_authorized` first, replying `not_authorized_blocks_for(cfg)` (a
fixed `:no_entry:` ephemeral; the fail-closed-empty variant says how an admin
opens spend deliberately) on refusal before touching the host.

**Ungated (read-only):** `status [<id>]`, `todo`, `roadmap`, `outcomes`, `ticket list`,
`ticket show <slug>`, `work`
(report-only — never drains on the socket loop), `help`, and `app_home_opened`.

**Gated:** `new <goal>` / bare `new` modal, `plan <id>`, `approve <id>`,
`queue <slug>`, `draft <slug>`, `ask <question>` (spends tokens), `merge
<slug|id>`, `work run`, `config …` / bare `config` modal, `pause`/`resume
[<id>]`, `revise <id> <instr>`, `revision approve|reject <id> <rev>`,
`ticket <title>` (scaffold writes backlog state), and
`ticket new <slug> <title…>` (the modal-open is gated). Buttons carry the same
gate: **Approve & queue** (`kranz_approve`), **Approve & start** (`kranz_start`),
**Merge** (`kranz_merge`), the todo **Queue** button (`kranz_queue_ticket`),
**Approve/Reject revision**, and **Approve/Deny grant** (`kranz_approve_grant` /
`kranz_deny_grant`, on the parked capability-grant card) all capture `user.id` +
`response_url` so the bridge refuses an unlisted clicker exactly like the slash
twin. A thread reply
(`Action::Guidance`) is gated too: a planning-mission reply runs a hosted
planning turn, and steering a running mission is gated as spend-adjacent.

## Action enum · dispatch_action · apply_action

`route` yields an [`Action`] variant (see the enum in
[inbound.rs](../../../crates/slack/src/inbound.rs)). The bridge has two appliers:
- `dispatch_action` — the async router. It handles everything that replies over
  a slash `response_url` / opens a modal / calls the host (status, help,
  new-mission, plan, approve, config, pause/resume, ask, merge, …), including
  every `is_authorized` check and every host call.
- `apply_action` — the **sync, pure-local-write** path. Only `Approve`,
  `Guidance`, and `NewTicket` do work there; every other variant is an explicit
  no-op. `dispatch_action`'s catch-all arm delegates ticket scaffolding to it.

In production the connection pump acks the envelope **first**, dedups by
envelope id (`SeenEnvelopes`, so a Slack redelivery can't double-approve), then
runs `is_slow_action` variants (claude-spawning or engine-touching) on a spawned
task so the read loop keeps answering pings; pure-local actions run inline. This
read loop lives in `pump_connection` — extracted out of `connect_once` (which
drives it in production) so tests can drive it over an in-memory stream.

## mrkdwn escaping for untrusted input

`format::escape_mrkdwn` escapes `&`, `<`, `>` (ampersand first, so output isn't
double-escaped) so Slack renders user text literally instead of parsing
`<!channel>`, `<@Uxxx>`, or `<url|label>` link/mention markup. It is **not
idempotent**: escape once, at the interpolation, then `clip`.

What is escaped, precisely (mrkdwn `section`/`context` text only):

- Revision instructions, todo/roadmap content, running-mission and unmerged
  rows in `/kranz status`, and the `instanceName` label.
- Since the 2026-09-01 adversarial audit: the needs-context questions, the
  blocked milestone id and reason, the completion card (summary, diff stat,
  PR handoff, branch and mission id), the new-mission ack (goal and opening
  reply), the planning reply, the `/kranz status <id>` body, and merge-gate
  output.
- Since that audit's follow-up review: the plan-review card's **considered
  alternatives** (`chosen`, and each rejected `approach`/`trade_off`), on the
  same card that carries the two real gated approve buttons (M-11); the
  `/kranz ticket list` rows (`slug`, `state`, `title`, `blocked_by`) and the
  `/kranz ticket show` mrkdwn body (`state`, `blocked_by`, `goal`, the
  orchestrator's needs-context questions, the planner's wrong-plan reason)
  (M-12); and every value interpolated into a `bridge::error_blocks` message
  from `dispatch.rs` and `approve_flow.rs` (host/engine error strings,
  mission ids, ticket slugs, branch names and plan identities), through the
  single-value funnel `bridge::esc`.

`bridge::error_blocks` itself is a LITERAL-TEXT funnel and never escapes:
callers frame their own backticks, `*bold*` and `>` quotes, so the escaping
belongs at the interpolation, which is what `bridge::esc` is for.

**plain_text fields (headers) must NOT be escaped**: they render verbatim and
the entities would show literally. That is why `/kranz ticket show`'s header
(slug then title) is deliberately unescaped while its body is escaped.

Approve buttons carry `<mission-id>:<plan-identity>` (the full sha256 of the
plan's canonical JSON, shared with dashboard previews through
`planning::plan_identity`). `approve_flow` commits through
`PlanningHost::approve_pending_if`, which compares the clicked card's identity
against the parked plan and commits while holding the engine and pending-plan
locks. Failed approval leaves the original plan parked without a later restore
that could overwrite a replacement.
The check and the commit are not separable, so a concurrent `/kranz plan` or
web-UI approve cannot slip a different plan in between (M-13). A mismatch is
refused naming both plans; a card that carries no identity at all (one posted
before the buttons were plan-bound) is refused the same way whenever a plan is
parked; and a card that named a plan which is no longer parked is refused
rather than falling through to starting the mission. The check runs on button
clicks only: the `/kranz approve` slash twin reviews no card, so it claims no
plan and takes the unbound approve.

Field lengths are capped by `clip`/`clip_to` (`MAX_FIELD` 2500, `MAX_HEADER`
150) to stay under Block Kit's per-text limits.

Everything in `format.rs` is a pure `data -> serde_json::Value` transform; the
button `action_id` constants (`APPROVE_ACTION_ID`, …) and modal `callback_id`s
are the shared contract that `route_interactive` keys on.

## Dogfood 2026-07-12 — live M2.9 Slack validation

This mission (m-6a20dc) was driven end-to-end through the Slack surface as its
own live validation of the M2.9 gate. The operator ran `kranz serve --slack`
once to bring the bridge up, then completed the rest of the loop entirely from
Slack: `/kranz draft <slug>` to draft, the **Approve & queue** button to
approve and queue, `/kranz work run` to execute, and the **Merge** button to
merge. The interactive deep-link buttons resolved via `slack.dashboardUrl`
(configured to http://127.0.0.1:4560/ for this run). No other CLI commands
were used — the CLI's role was limited to starting the bridge process.

## ACP one-call permission cards

`PermissionReady` shows the mission, run, workspace, deadline and complete action.
The card offers **Allow once** only when the action and workspace fit the card
and policy permits it; otherwise it points to the dashboard and retains deny.
Permission buttons carry the exact mission/request/binding digest. The bridge
requires a nonempty verified user ID even under `allowAllUsers`, checks the
existing allowlist, and enqueues a signed `ResolvePermission` command with that
Slack identity. A queued answer is distinct from durable resolution and delivery.
No response URL or display label can invent the actor.
See [setup and usage](../../acp-live-permissions.md).
