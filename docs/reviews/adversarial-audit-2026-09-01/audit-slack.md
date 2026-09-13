# Adversarial security review — `crates/slack` (kranz @ 33732c27)

## Summary

1. The bridge is **Socket Mode only** — no HTTP listener, no signing secret, no
   replay window, no raw-body handling. The entire class of "forged inbound
   request" findings does not apply: inbound bytes arrive over a TLS websocket
   the bridge dialed itself, authorized by the app-level token. No HMAC code
   exists, so none of it can be wrong. This is the strongest property in the crate.
2. **Authorization is complete for mutations.** I traced all 34 `Action` variants
   through `dispatch_action`: every mutating arm — all 9 button `action_id`s, all
   3 modal `callback_id`s, every spend slash verb, and thread guidance — calls
   `SlackConfig::is_authorized` (or a gate helper that does) before touching the
   engine. The gate keys on the stable Slack **user id**, not a display name, and
   fails closed on an empty allowlist. No bypass found.
3. **No secrets leak.** Tokens travel only as bearer headers, never in URLs;
   `SlackClient`'s `Debug` is redacted; no log site prints a token or a config.
4. The real findings are on the **outbound and read** sides: agent-authored
   mission text is posted as **unescaped mrkdwn** (`<!channel>`, `<@U…>`,
   `<url|label>` all live) in six builders, and the whole read-only surface is
   ungated and unscoped to the configured channel or repository.
5. One consent-integrity gap: the **Approve button carries only the mission id**,
   so it commits whatever plan is parked host-side *now* — not the plan the card
   displayed. Every other decision button (grant, revision, question) is bound to
   its parked decision and re-validated. Approve is the odd one out.

---

## MEDIUM

### M1. Agent-authored mission output is posted to Slack as unescaped mrkdwn

**Severity:** MEDIUM (see "case for HIGH" below)
**Confidence:** CONFIRMED (traced event → classifier → builder → `chat.postMessage`)

**Files**

| sink | file:line | source of the text |
|---|---|---|
| blocked reason | `crates/slack/src/format.rs:838` | `EventKind::MilestoneBlocked.reason` (agent) |
| completion summary | `crates/slack/src/format.rs:866` | `completion_summary()` = mission goal + tally; and `MissionFailed.reason` verbatim (`crates/slack/src/outbound.rs:151`) |
| needs-context questions | `crates/slack/src/format.rs:823` | orchestrator questions |
| planning reply | `crates/slack/src/format.rs:1162` | `host.planning_turn()` prose (agent) |
| new-mission ack | `crates/slack/src/format.rs:910`, `:924` | user goal + orchestrator opening reply |
| `/kranz status <id>` body | `crates/slack/src/format.rs:1182` ← `crates/slack/src/bridge.rs:2028-2032` | raw `state.mission.goal` |
| merge-gate failure output | `crates/slack/src/commands.rs:268-282` → `crates/slack/src/bridge.rs:1012-1016` | captured CI/test stdout (repo-controlled) |
| draft `WrongPlan` reason / verbatim engine refusal | `crates/slack/src/commands.rs:101-106`, `:260` | agent |

Contrast the escaped sinks that *do* get it right: `format.rs:645` (revision
instructions), `:716` (grant command), `:760`, `:778`, `:794` (question card),
`:598` / `:1580` (plan goal), `:1194-1196` (pipeline status rows).

**Evidence**

```rust
// format.rs:835-839  build_blocked
section(&format!(
    "Milestone `{}` is blocked:\n>{}",
    b.milestone_id,
    clip(b.reason.trim()).replace('\n', "\n>")   // no escape_mrkdwn
)),
```

```rust
// format.rs:1162  build_planning_reply
let mut blocks = vec![section(&format!("*Orchestrator*\n{}", clip(reply.trim())))];
```

`section()` (`format.rs:1814`) emits `{"type":"mrkdwn"}`, which Slack parses for
`<!channel>`, `<@Uxxx>` and `<url|label>`.

**Attacker + preconditions.** Anyone who can influence agent output: a
prompt injection planted in a repo file, ticket body, dependency README, or test
fixture that the worker/validator reads; or, for the goal fields, any allowlisted
user. No Slack permission is needed — the *bot* posts the message.

**Impact.** The agent can make the kranz bot render, in the mission thread,
`<!channel>` (broadcast ping to everyone in the channel) and
`<https://attacker.example/approve|Approve & start>` — a link styled identically
to the genuine, gated approve button sitting a few pixels away in the same card.
The consent surface is the product; a phishing primitive aimed at exactly the
operators who hold merge/approve authority is more than cosmetic.

**Case for HIGH:** it targets the humans who hold the gate, and the repo's own
security discipline already treats this as mandatory. **Case for MEDIUM (chosen):**
it cannot itself approve, grant, merge or steer anything; it requires a human to
be fooled.

**Documentation is wrong about this.** `docs/knowledge/surfaces/slack-commands.md:112-118`
claims `escape_mrkdwn` "is applied to **every** untrusted field", then lists only
the five that are actually covered. Six agent-authored fields are not.

**Suggested fix.** Wrap each sink above in `crate::format::escape_mrkdwn` before
`clip` (escape first, then clip — the `&<>` expansion changes length, exactly as
`ask_answer_blocks` at `bridge.rs:1031-1034` already does). For `error_blocks`
(`bridge.rs:1012`), either escape inside it or split it into a literal-text
variant and an untrusted-interpolation variant, since it is the shared funnel for
engine/agent error text.

**Existing test coverage.** Only the question card is covered:
`format.rs:2098-2115` `question_events_card_escapes_mrkdwn` asserts `<!channel>`
and `<@U123>` do not survive. `format.rs:2776-2781` unit-tests `escape_mrkdwn`
itself. `tests/formatting.rs:102` `blocked_block_kit` and `:122` `complete_block_kit`
assert shape only — neither feeds a control sequence in. **No test covers any of
the six unescaped sinks.**

---

### M2. Approve buttons bind to the mission id, not to the plan they display

**Severity:** MEDIUM
**Confidence:** CONFIRMED bridge-side; the host-side semantics are the documented
`PlanningHost::approve_pending` contract (`crates/slack/src/host.rs:66-73`), which
I read but did not trace into `kranz_server`.

**Files**
- `crates/slack/src/format.rs:614`, `:620`, `:1623`, `:1629` — both approve
  buttons carry `"value": p.mission_id` and nothing else.
- `crates/slack/src/inbound.rs:519-527` — `ButtonKind::Approve` / `Start` take the
  whole value as `mission_id`; no revision/hash component is parsed.
- `crates/slack/src/approve_flow.rs:50-65` — `host.approve_pending(mission_id)`.
- `crates/slack/src/host.rs:66-73` — "Approve the plan **PARKED by the last Ready**
  `request_plan` — the ONE pending-plan cache every surface shares".

**Evidence**

```rust
// approve_flow.rs:50-53
let approved = match host {
    None => None,
    Some(host) => match host.approve_pending(mission_id).await {
```

Compare the three buttons that get this right — each embeds the decision and
re-validates it at enqueue:
- grant: `<mission-id>:<command>`, checked byte-for-byte against
  `state.pending_grant_request` (`bridge.rs:2364-2372`);
- revision: `<mission-id>:<rev>`, checked against `state.pending_revision`
  (`bridge.rs:2293-2301`);
- question: `<mission-id>:<question-id>:<option>`, index range-checked against
  the parked question (`bridge.rs:2437-2447`).

**Attacker + preconditions.** No unauthorized actor. The realistic sequence:
`/kranz plan m-abc` posts card A (plan v1); the plan is re-requested (another
allowlisted operator, or the same one retrying a slow turn) and card B is posted
with plan v2; card A is **never retired** — `retire_plan_card`
(`bridge.rs:1147-1164`) only fires *after* a successful approve. An operator who
scrolls back and clicks card A's "Approve & start" commits v2 while reading v1's
milestones and cost estimate.

**Impact.** A human's recorded consent does not correspond to the artifact they
reviewed. In a governance harness whose whole point is auditable human approval,
that is the load-bearing property.

**Suggested fix.** Put the plan's identity in the button value —
`<mission-id>:<plan-revision-or-hash>` — and have `approve_pending` refuse when
the parked plan's identity differs, with the same "awaiting X, not Y" refusal
shape `enqueue_revision_control` already uses (`bridge.rs:2296-2299`).

**Existing test coverage.** `tests/formatting.rs:244` asserts the two buttons
exist and carry the mission id; `inbound.rs:1441-1494` asserts routing. Nothing
tests approve-after-replan.

---

### M3. The entire read-only surface is unauthenticated and unscoped to the configured channel

**Severity:** MEDIUM
**Confidence:** CONFIRMED

**Files**
- `crates/slack/src/inbound.rs:842-870` — `route_slash` reads `channel_id` only to
  *carry* it forward; it is never compared to `cfg.channel`, and `team_id` is
  never read at all in single-repo mode.
- `crates/slack/src/dispatch.rs:48-190` — `Status`, `Todo`, `Roadmap`, `Outcomes`,
  `TicketList`, `TicketShow` arms: no `is_authorized` call.
- `crates/slack/src/dispatch.rs:918-928` — `Work` arm: no gate.
- `crates/slack/src/dispatch.rs:986-996` — `AppHome` arm: no gate, publishes to
  whichever `user_id` opened the tab.
- `crates/slack/src/bridge.rs:336-352` — `SocketContext::Single::route` passes the
  envelope straight to `route()` with no channel/team predicate.

**Attacker + preconditions.** Any member of the Slack workspace. Once the app is
installed, `/kranz` is invokable from any channel the app is in **and from a DM
with the bot** — neither is checked. The user does not need to be in
`slack.allowUsers`, and does not need to be in the configured channel.

**Impact.** Discloses, to any workspace member: every mission id, goal and status
(`build_status_reply` → `render_status_body`, `bridge.rs:2019-2042`, which
includes the full goal text and accumulated USD cost); every open ticket's slug,
title and body (`build_ticket_show_reply`, `bridge.rs:1878`); the contents of
`docs/roadmap-options.md` and the operator gates file (`bridge.rs:1694-1725`);
the execution queue and whether the repo is busy (`bridge.rs:2466`); the
autonomy/escalation outcome metrics (`bridge.rs:1377`). App Home
(`bridge.rs:2500-2551`) renders active missions, queue and open tickets for
anyone who opens the tab.

**Note.** `docs/knowledge/surfaces/slack-commands.md:49-74` documents "ungated
reads" as a deliberate choice. What it does **not** say is that reads are also
unscoped to the configured channel — the doc's framing ("the configured Slack
channel", `docs/reviews/m8-inbound-slack-routing-proof.md:8`) implies a channel
boundary that the code does not enforce.

**Suggested fix.** Cheapest correct fix: refuse any `slash_commands` envelope
whose `payload.channel_id` is not a configured route (and whose `team_id` is not
the installed workspace) before routing, in `SocketContext::route`. If ungated
reads are to stay, at minimum scope them to the configured channel so private
DM-based reconnaissance is not free.

**Existing test coverage.** None — no test asserts channel scoping, and no test
asserts a read verb is reachable/refused from a foreign channel.

---

### M4. Catalog mode: `repo:<id>` reaches any repository from any channel

**Severity:** MEDIUM
**Confidence:** CONFIRMED (the crate's own test proves the routing)

**Files**
- `crates/slack/src/catalog.rs:302-316` — `strip_explicit_repo` peels a leading
  `repo:<id>` token off any slash command's text and resolves it against the
  whole catalog, with **no check that the invoking channel maps to that repo**.
- `crates/slack/src/catalog.rs:127-134` — the metadata/selector branches run
  *before* the channel-mapping branch (`:170-179`), so the selector wins.
- `crates/slack/src/bridge.rs:370-372` — `scoped_cfg.channel = resolved.channel_id`,
  i.e. the **invoking** channel, not the repo's configured route.

**Evidence** — the existing test at `catalog.rs:365-383` is the proof:

```rust
let resolved = catalog
    .resolve_envelope(&slash("repo:beta status", "T1", "CA"))  // CA maps to alpha
    .unwrap().unwrap();
assert_eq!(resolved.repo.id, "beta");
```

**Attacker + preconditions.** Any workspace member, in any channel where the app
is present (including one mapped to a different repo, or a DM). Requires the
multi-repo catalog (`serve_slack_catalog`).

**Impact.** Combined with M3, every read verb becomes cross-repository: a user in
`#alpha` runs `/kranz repo:beta ticket show <slug>` and reads beta's backlog.
Spend verbs are still gated, but by `merge_repo_allow_users`
(`config.rs:117-123`): a repo with no list of its own **inherits the global list**,
so a globally-allowlisted operator can spend on any repo in the catalog from any
channel. Worse, because `scoped_cfg.channel` becomes the invoking channel, a
`repo:beta new <goal>` from `#alpha` roots beta's mission thread — and every
subsequent plan/blocked/complete card for it — **in `#alpha`**
(`bridge.rs:1101-1116`, `post_to_mission_thread` posts to `cfg.channel`).

**Suggested fix.** Require the invoking `(team_id, channel_id)` to be one of the
target repo's configured routes before honoring `repo:<id>`; refuse otherwise
with the existing ambiguity-style error. Separately, post mission threads to the
repo's own primary route rather than the invoking channel.

**Existing test coverage.** `catalog.rs:365-383` tests that the selector *works*.
No test asserts that it should be constrained.

---

### M5. Outbound post failures retry at 2 Hz forever with no backoff

**Severity:** MEDIUM
**Confidence:** CONFIRMED

**Files**
- `crates/slack/src/outbound_engine.rs:404-409` — on a post failure the cursor is
  rolled back and the loop `break`s without advancing.
- `crates/slack/src/outbound_engine.rs:30` — `POLL_INTERVAL = 500ms`; the next
  tick re-reads the same event and re-posts.
- `crates/slack/src/client.rs:124-155` — `post_message` has no 429 / `Retry-After`
  handling; `check_ok` (`:230-239`) turns `{"ok":false,"error":"ratelimited"}` into
  a plain error, which is exactly the failure that triggers the retry.

**Evidence**

```rust
// outbound_engine.rs:404-409
cursor.state = state_before_event;
tracing::warn!(mission = %mission_id, seq = event.seq, error = %e, "slack post failed; will retry");
break;
```

**Attacker + preconditions.** No attacker needed. A misconfigured channel id
(`channel_not_found`), a revoked bot token, or a message that Slack rejects
(`msg_too_long`, `invalid_blocks`) makes the failure **permanent**, and the retry
is unconditional and immediate. `is_repo_busy`-style backoff exists nowhere on
this path.

**Impact.** 2 requests/second per stuck mission, forever, against Slack's Web API —
which rate-limits `chat.postMessage` at roughly 1/sec/channel. Multiple stuck
missions multiply it. Realistic outcomes: sustained rate-limiting of the whole
app (so *legitimate* plan-ready and blocked notifications stop arriving — a
governance-visibility failure), and Slack app-level throttling or suspension.
A permanently-rejected message also wedges that mission's cursor, so every
subsequent notification for it is blocked behind the poison event.

**Suggested fix.** Exponential backoff per mission on post failure (mirror
`BACKOFF_MIN`/`BACKOFF_MAX` from `bridge.rs:116-117`), a distinct branch for
`ratelimited` that honors `Retry-After`, and a bounded retry count after which the
poison event is skipped with a loud warning rather than retried forever.

**Existing test coverage.** `poll_mission_with_poster` exists specifically so
"cursor retry semantics can be exercised without a live Slack endpoint"
(`outbound_engine.rs:311-313`) — so the rollback-on-failure behavior is tested,
but nothing tests the *rate* of retry or a permanent failure.

---

## LOW

### L1. Envelope dedup is in-memory and lost across restarts

**Confidence:** CONFIRMED.
`crates/slack/src/bridge.rs:822-857` (`SeenEnvelopes`, `CAP = 512`, FIFO, plain
`HashSet`), consulted at `:680`. The set lives in `run_socket_context`'s stack and
is dropped when the process exits. Slack redelivers an envelope it believes was
not acked. A bridge restart between Slack's send and its retry therefore re-runs
the side effect — a second `Approve` click enqueues again (mostly absorbed by
`approve_flow`'s state guards and `queue::enqueue`'s no-op re-queue), a second
`Guidance` enqueues a duplicate control message. Also, >512 envelopes in one
process lifetime evict the oldest ids. Fix: persist recent envelope ids next to
`notify-cursors.json`, or key dedup on an action-level idempotency token.
**Coverage:** the dedup itself is covered (doc claims it at
`docs/knowledge/surfaces/slack-commands.md:103-105`); restart behavior is not.

### L2. `SlackConfig` derives `Debug` with both tokens in plaintext

**Confidence:** CONFIRMED that the derive exists; **no leak site found.**
`crates/slack/src/config.rs:51-89` — `#[derive(Debug, …)] pub struct SlackConfig`
holds `bot_token` and `app_token` as plain `String`. Same for `SlackFileConfig`
(`:129`) and `EnvVars` (`:196`). `SlackClient` deliberately hand-implements a
redacted `Debug` (`client.rs:31-36`) — the asymmetry is the hazard. I grepped the
whole workspace for a `{cfg:?}` / `?cfg` print site and found none, so this is
latent, not live. Any future `tracing::debug!(?cfg, …)` writes `xoxb-…`/`xapp-…`
to the log. Fix: hand-implement `Debug` for `SlackConfig` the way `SlackClient`
does, or wrap the tokens in a redacting newtype.

### L3. `/kranz config` accepts an arbitrary `model` string

**Confidence:** CONFIRMED for the path; **no injection.**
`crates/slack/src/inbound.rs:1275-1279` takes `model` as free text with no
validation (unlike `role` and `backend`, which are table-checked at `:1218-1236`).
It flows through `config_patch_with_backend` (`inbound.rs:1382-1404`) →
`kranz_engine::config::apply_validated_patch` → `crates/engine/src/config.rs:376-385`,
whose `validate` checks `reasoningEffort`, cycle counts and worker counts but
**never the model string** → the agent CLI's `--model` argument
(`crates/engine/src/backend_claude.rs:375`, `backend_codex.rs:311`).
This is `Command` argv, not a shell, so a single token cannot become two
arguments — **no argument injection**. The residual risk is that an allowlisted
user can point a role at a nonexistent or unentitled model and wedge the mission
with a confusing backend error. Fix: validate against a known model set, or at
minimum reject values beginning with `-`.

### L4. No `team_id` validation anywhere

**Confidence:** CONFIRMED for the code path; the exploitation scenario is
PLAUSIBLE and requires an unusual install.
Single-repo mode (`bridge.rs:336-352`) never reads `team_id`. Catalog mode reads
it (`catalog.rs:251-262`) but the sole-healthy-repo fallback (`catalog.rs:184-191`)
routes an envelope with *any* team id when only one repo is healthy. With a
single-workspace app-level token every envelope is from that workspace, so this is
inert today. It becomes live for an org-wide or distributed install, where a
foreign-workspace user id would then be matched against `allow_users` by string
equality (`config.rs:107`). Fix: assert `team_id` against the configured routes
before routing, in both modes.

---

## INFO — verified-good properties worth keeping

- **No inbound HTTP surface exists in this crate.** `health.rs` is an in-process
  `Arc<Mutex<HealthState>>` with no listener; `host.rs` is a trait definition.
  `Cargo.toml` pulls no HTTP server. Every "unauthenticated endpoint" question is
  structurally N/A.
- **Every mutating action is gated.** Verified arm by arm in `dispatch.rs`:
  buttons `kranz_approve`/`kranz_start`/`kranz_merge`/`kranz_queue_ticket`/
  approve-revision/reject-revision/approve-grant/deny-grant/answer-question all
  route into `approve_flow` (`approve_flow.rs:41`), `gate_merge_command`
  (`commands.rs:177`), `run_approve_ticket_command` (`commands.rs:236`),
  `revision_control` (`bridge.rs:2248`), `grant_control` (`bridge.rs:2317`) or
  `question_control` (`bridge.rs:2391`) — each of which calls `is_authorized`
  first. All three modal `view_submission`s are gated (`dispatch.rs:379`, `:713`,
  `change_config` at `bridge.rs:2128`). Thread guidance is gated on **both** the
  planning branch (`dispatch.rs:1058`) and the running branch (`dispatch.rs:1104`).
- **The gate is fail-closed and identity-stable.** `config.rs:102-110`: empty
  allowlist denies unless `allowAllUsers: true`; a blank/absent user id is denied
  when a list is set; matching is on `Uxxxx`, never a display name. Blank entries
  are stripped at resolve (`config.rs:282-287`) so a stray `""` cannot wash a
  configured list into the open shape. Well covered by `tests/authz.rs:28-79` and
  `config.rs:434-750`.
- **Grant, revision and question decisions are bound and re-validated**
  (`bridge.rs:2293-2301`, `:2364-2372`, `:2437-2447`) — the discipline M2 says
  approve should adopt.
- **Tokens never enter a URL.** Both tokens go out as `bearer_auth` headers
  (`client.rs:57`, `:141`, `:167`, `:186`, `:217`). `response_url` and `trigger_id`
  come from Slack-delivered envelopes only.
- **Path-safety on ids is checked before filesystem access.** `mission_status`
  (`bridge.rs:1085-1088`), `build_status_reply` (`bridge.rs:1234-1236`),
  `mission_dir_exists` (`bridge.rs:1002`) and `is_ticket_slug` (`bridge.rs:983-986`)
  all gate on `MissionPaths::is_safe_id` / `Ticket::valid_slug` first. No traversal
  found via mission id, ticket slug, or `clean_id`.
- **`strip_ci_prefix` is UTF-8-safe** (`inbound.rs:1414-1416` refuses a non-char-boundary
  split) — covered by `tests/routing.rs:296`.
- **`/kranz queue <slug>` cannot fall through to plan approval** — deliberately
  separated, with a strong regression test (`dispatch.rs:1204-1276`).

---

## Areas checked with no finding

- **Inbound request verification (HMAC `v0=`, timestamp replay window, raw-body
  handling, per-route coverage, `url_verification` challenge).** N/A by
  architecture — Socket Mode only. No `hmac`/`sha2`/`subtle` dependency exists in
  `crates/slack/Cargo.toml`, and there is no HTTP route table to under-cover. The
  app-level token is held only in memory and in `~/.kranz/config.json`, and is
  used exactly once per connect (`client.rs:53-70`).
- **Authorization bypass on any mutating path.** Traced all 34 `Action` variants;
  no ungated mutation. The `dispatch_action` catch-all (`dispatch.rs:1136-1140`)
  reaches only `Action::Ignore` — `apply_action`'s exhaustive match
  (`bridge.rs:1184-1230`) makes every other variant an explicit no-op, so a future
  variant cannot silently inherit an ungated path.
- **Spoofable identity.** No code path reads `user.name`, `user.username`,
  `user_name`, or a profile field for authorization.
- **Cross-request approval replay** (approving request B with request A's payload,
  as distinct from M2's stale-card issue). Grant/revision/question buttons are
  bound to their parked decision, and `approve_pending` is a
  consume-once host-side cache, so a replayed approve finds nothing parked.
- **Approval race between two concurrent approvals.** `approve_pending` is
  documented as consuming the single parked plan and re-parking only on failure
  (`host.rs:66-73`); the second concurrent click gets `Ok(None)` and falls into the
  state-aware branch, which refuses an already-executing mission via
  `host.release()`/`is_repo_busy` (`approve_flow.rs:222-264`). No double-start found.
  (Host-side atomicity is outside this crate.)
- **Thread → mission mapping.** `ThreadMap`/`AffinityMap` entries are written only
  by the bridge itself from `chat.postMessage` `ts` values (`bridge.rs:212-234`,
  `:1110-1114`); a user cannot craft a `thread_ts` that maps to a mission the
  bridge never posted about. In catalog mode the lookup is additionally filtered
  by `repo_id` (`bridge.rs:250-259`). Atomic temp+rename persistence
  (`threads.rs:44-51`).
- **Slash-argument injection into shell or git.** No `std::process::Command`,
  `sh -c`, or git invocation anywhere in `crates/slack/src`. All arguments reach
  the engine as typed values (`ControlCommand`, `serde_json::Value` patches).
- **`ask` answer rendering.** `ask_answer_blocks` (`bridge.rs:1020-1064`) escapes
  both the question and the model's answer before clipping — the one place that
  gets escape-then-clip ordering explicitly right.
- **Slack markup in header blocks.** `header()` (`format.rs:1809-1812`) emits
  `plain_text`, which Slack does not parse — correctly *not* escaped, per the
  documented rule.
- **Dashboard deep links.** `dashboard_deep_link` builds `<base>#/m/<id>` from
  operator config only; no user-supplied URL reaches a button `url` field.
- **Instance label injection.** `label_blocks` (`format.rs:487-517`) escapes the
  config-supplied name for mrkdwn and re-clips for plain_text headers.
