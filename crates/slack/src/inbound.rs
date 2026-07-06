//! Pure Socket Mode envelope routing.
//!
//! Slack delivers every inbound interaction over the Socket Mode websocket as a
//! JSON *envelope*:
//!
//! ```json
//! { "type": "events_api" | "interactive" | "slash_commands" | "hello" | "disconnect",
//!   "envelope_id": "…",           // present on the three actionable types
//!   "payload": { … },              // the actual event/interaction/command
//!   "accepts_response_payload": false }
//! ```
//!
//! The bridge must **ack** every envelope carrying an `envelope_id` within 3s
//! by sending `{"envelope_id":"…"}` back over the socket — that is a transport
//! concern handled in [`crate::bridge`]. This module is the pure decision layer:
//! given the raw envelope JSON (and the current thread map), it returns the
//! [`Action`] to take. Keeping it pure means the whole routing table is unit
//! tested against captured fixtures without a live socket.
//!
//! Routing rules:
//! - `interactive` with a `block_actions` payload whose action id is
//!   [`crate::format::APPROVE_ACTION_ID`] → [`Action::Approve`] (mission id from
//!   the button `value`).
//! - `events_api` `message` in a thread we know (its `thread_ts` maps to a
//!   mission) → [`Action::Guidance`]. Bot's own messages, thread roots, and
//!   messages in unknown threads are ignored (else the bridge echoes itself).
//! - `slash_commands` `/kranz ticket <title>` → [`Action::NewTicket`];
//!   `/kranz help`, bare `/kranz`, or any unrecognized subcommand →
//!   [`Action::Help`] (the command list).
//! - anything else → [`Action::Ignore`].

use crate::format::{
    APPROVE_ACTION_ID, CONFIG_CALLBACK_ID, CONFIG_EFFORT_ACTION, CONFIG_EFFORT_BLOCK,
    CONFIG_MISSION_ACTION, CONFIG_MISSION_BLOCK, CONFIG_MODEL_ACTION, CONFIG_MODEL_BLOCK,
    CONFIG_ROLE_ACTION, CONFIG_ROLE_BLOCK, MERGE_ACTION_ID, NEW_MISSION_CALLBACK_ID,
    NEW_MISSION_GOAL_ACTION, NEW_MISSION_GOAL_BLOCK, START_ACTION_ID,
};
use serde_json::Value;

/// The routed intent of an inbound envelope. `envelope_id` (when the envelope
/// carries one) is returned alongside so the caller can ack even for an
/// otherwise-ignored envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Approve-and-queue button pressed for `mission_id`. A money-spending
    /// action: `user_id` (the clicker) is gated by the spend allowlist and
    /// `response_url` delivers a not-authorized ephemeral, mirroring the
    /// `/kranz approve` slash twin.
    Approve {
        mission_id: String,
        user_id: Option<String>,
        response_url: Option<String>,
    },
    /// Approve-and-START button pressed for `mission_id` (the primary button
    /// on a plan-review message). Spend-gated exactly like [`Action::Approve`];
    /// on authorization the bridge commits the pending plan and starts
    /// execution through the hosted registry instead of queueing.
    ApproveStart {
        mission_id: String,
        user_id: Option<String>,
        response_url: Option<String>,
    },
    /// A threaded reply on `mission_id`'s thread. On a RUNNING mission it
    /// becomes orchestrator guidance (a control-inbox message); on a PLANNING
    /// mission the bridge runs a hosted planning turn — a spend action, so
    /// `user_id` (the message author) rides along for the allowlist gate.
    Guidance {
        mission_id: String,
        text: String,
        user_id: Option<String>,
    },
    /// `/kranz ticket <title>` → scaffold a new ticket file.
    NewTicket {
        title: String,
        channel: String,
        thread_ts: Option<String>,
    },
    /// `/kranz ticket list` → one row per backlog ticket. Read-only, so not
    /// gated (no `user_id`); replies over `response_url`, same as
    /// [`Action::Status`].
    TicketList { response_url: Option<String> },
    /// `/kranz ticket show <slug>` → the ticket's detail (goal/state/blocked-by
    /// /needs-context). Read-only, so not gated; an unknown or invalid slug is
    /// a graceful ephemeral error, never a panic.
    TicketShow {
        slug: String,
        response_url: Option<String>,
    },
    /// `/kranz new <goal>` (or a new-mission modal submission) → create a
    /// mission and seed planning. A money-spending action: gated by the spend
    /// allowlist. `user_id` is the invoking Slack user (for the gate);
    /// `response_url` is where an ack / not-authorized ephemeral is posted
    /// (absent on the modal path — a `view_submission` has none, so the
    /// bridge falls back to `chat.postEphemeral`); `channel` roots the
    /// mission thread.
    NewMission {
        goal: String,
        user_id: Option<String>,
        response_url: Option<String>,
        channel: String,
    },
    /// Bare `/kranz new` → open the multiline new-mission modal (Slack slash
    /// commands are single-line, so long goals need this). Free to open —
    /// the SUBMISSION is the spend gate — but gated anyway so an unlisted
    /// user learns early. `trigger_id` expires ~3 s after the slash, so the
    /// bridge opens the view inline, never on a spawned task.
    NewMissionModal {
        trigger_id: String,
        user_id: Option<String>,
        response_url: Option<String>,
        channel: String,
    },
    /// `/kranz status [<id>]` → post a folded status summary. `mission_id`
    /// absent = "the most recent mission". Read-only, so not spend-gated.
    Status {
        mission_id: Option<String>,
        response_url: Option<String>,
    },
    /// `/kranz plan <id>` → demand the plan for a mission (request-plan turn).
    /// A money-spending action (it runs an orchestrator turn): spend-gated.
    RequestPlan {
        mission_id: String,
        user_id: Option<String>,
        response_url: Option<String>,
    },
    /// `/kranz approve <id>` → approve the plan and queue the mission. The
    /// slash-command twin of the [`Action::Approve`] button; spend-gated.
    ApproveMission {
        mission_id: String,
        user_id: Option<String>,
        response_url: Option<String>,
    },
    /// `/kranz draft <slug>` → run a non-interactive draft turn for a backlog
    /// ticket through the hosted engine (create the mission, seed it, demand
    /// the plan). A money-spending action: gated on the allowlist EXACTLY
    /// like [`Action::NewMission`] (same gate, same standard refusal). `slug`
    /// is validated via `Ticket::ensure_valid_slug` before any host call; an
    /// unknown ticket is a graceful error.
    Draft {
        slug: String,
        user_id: Option<String>,
        response_url: Option<String>,
    },
    /// `/kranz config [<id>] <role> <model> [effort]` → change a role's model
    /// (and optionally reasoning effort) mid-mission via a `config-change`
    /// control command. SPEND-ADJACENT (it re-shapes what future turns spend),
    /// so it is gated on the allowlist exactly like `/kranz new`. `mission_id`
    /// absent = the most-recent mission (like `/kranz status`). `role` /
    /// `effort` are the already-validated canonical spellings (see
    /// [`parse_role`] / [`parse_effort`]); a bad role or effort never reaches
    /// here — routing falls through to [`Action::Help`] instead.
    Config {
        mission_id: Option<String>,
        role: String,
        model: String,
        effort: Option<String>,
        user_id: Option<String>,
        response_url: Option<String>,
        /// Where a user-only reply can go when there is no response_url
        /// (the modal path): chat.postEphemeral needs the channel.
        channel: Option<String>,
    },
    /// Bare `/kranz config` → open the config modal (role/model/effort
    /// pickers). Free to open; the SUBMISSION is the gated change. The
    /// trigger_id expires ~3 s after the slash, so the bridge opens the view
    /// inline.
    ConfigModal {
        trigger_id: String,
        user_id: Option<String>,
        response_url: Option<String>,
        channel: String,
    },
    /// `/kranz pause [<id>]` → enqueue a `Pause` control command on the target
    /// mission. STEERING (not spend), but it disrupts a running mission, so it is
    /// gated on the allowlist exactly like `config`. `mission_id` absent = the
    /// single active mission (resolved via `resolve_active_config_target`, which
    /// refuses when ambiguous/terminal).
    Pause {
        mission_id: Option<String>,
        user_id: Option<String>,
        response_url: Option<String>,
    },
    /// `/kranz resume [<id>]` → enqueue a `Resume` control command on the target
    /// mission. The twin of [`Action::Pause`]; same gating and targeting.
    Resume {
        mission_id: Option<String>,
        user_id: Option<String>,
        response_url: Option<String>,
    },
    /// `/kranz work` → report the queue state (entries + whether the repo is
    /// busy) as an ephemeral, and point at the `kranz work` dispatcher for
    /// actually draining it. REPORT-ONLY: the bridge must never spawn a mission
    /// on the socket loop, so this reads the queue and hands off. Read-only, so
    /// not gated (`response_url` delivers the ephemeral).
    Work { response_url: Option<String> },
    /// `/kranz work run` → trigger the queue drain/claim/skip loop THROUGH the
    /// host (never on the socket read loop). A money-spending action (it can
    /// spawn `claude` sessions via the drained missions): gated on the
    /// allowlist EXACTLY like [`Action::NewMission`] / [`Action::Draft`].
    /// `work run <extra>` (any trailing token past `run`) falls through to
    /// help, same as a malformed `work <anything>`.
    WorkRun {
        user_id: Option<String>,
        response_url: Option<String>,
    },
    /// Merge button pressed for `mission_id` (Delivered card) OR `/kranz merge
    /// <slug|id>` (its slash twin). Spend-adjacent (runs the CI gate suite and
    /// a `--no-ff` merge): gated on the allowlist EXACTLY like
    /// [`Action::Approve`] / [`Action::WorkRun`]. `user_id` (the clicker/
    /// invoker) and `response_url` are captured so the bridge can refuse an
    /// unlisted user before touching the host.
    Merge {
        mission_id: String,
        user_id: Option<String>,
        response_url: Option<String>,
    },
    /// `app_home_opened` events_api envelope → publish this user's App Home tab
    /// (active missions + queue + open tickets). Read-only, so not spend-gated.
    AppHome { user_id: String },
    /// `/kranz help`, bare `/kranz`, or an unrecognized subcommand → reply with
    /// the command list. `response_url` (from the slash payload) is where the
    /// ephemeral help is posted.
    Help { response_url: Option<String> },
    /// Nothing to do (hello, disconnect, unknown thread, bot echo, …).
    Ignore,
}

/// A parsed envelope: the routed [`Action`] plus the `envelope_id` to ack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routed {
    pub action: Action,
    /// Present on `interactive` / `events_api` / `slash_commands` envelopes;
    /// `None` for `hello` (which needs no ack).
    pub envelope_id: Option<String>,
}

/// Look up a mission id given a thread root `ts`. The bridge passes a closure
/// backed by [`crate::threads::ThreadMap`]; tests pass a fixture closure. This
/// keeps [`route`] pure and free of any file access.
pub trait ThreadLookup {
    fn mission_for_thread(&self, thread_ts: &str) -> Option<String>;
}

impl<F> ThreadLookup for F
where
    F: Fn(&str) -> Option<String>,
{
    fn mission_for_thread(&self, thread_ts: &str) -> Option<String> {
        self(thread_ts)
    }
}

/// Route a raw Socket Mode envelope to an [`Action`], resolving message threads
/// via `lookup`. Pure: no I/O, no clock.
pub fn route(envelope: &Value, lookup: &impl ThreadLookup) -> Routed {
    let envelope_id = envelope
        .get("envelope_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let action = match envelope.get("type").and_then(Value::as_str) {
        Some("interactive") => route_interactive(payload(envelope)),
        Some("events_api") => route_event(payload(envelope), lookup),
        Some("slash_commands") => route_slash(payload(envelope)),
        // hello / disconnect / unknown: nothing to do (hello has no envelope_id
        // either, so nothing gets acked spuriously).
        _ => Action::Ignore,
    };
    Routed {
        action,
        envelope_id,
    }
}

fn payload(envelope: &Value) -> &Value {
    envelope.get("payload").unwrap_or(&Value::Null)
}

/// `interactive` → an approve / approve-and-start button click or a
/// new-mission modal submission, else ignore. We only act on `block_actions`
/// whose action id is one of ours ([`APPROVE_ACTION_ID`] → queue,
/// [`START_ACTION_ID`] → start) and on `view_submission`s carrying our
/// [`NEW_MISSION_CALLBACK_ID`]; every other interaction is ignored.
fn route_interactive(payload: &Value) -> Action {
    if payload.get("type").and_then(Value::as_str) == Some("view_submission") {
        return route_view_submission(payload);
    }
    if payload.get("type").and_then(Value::as_str) != Some("block_actions") {
        return Action::Ignore;
    }
    let Some(actions) = payload.get("actions").and_then(Value::as_array) else {
        return Action::Ignore;
    };
    enum ButtonKind {
        Approve,
        Start,
        Merge,
    }
    for action in actions {
        let kind = match action.get("action_id").and_then(Value::as_str) {
            Some(id) if id == APPROVE_ACTION_ID => ButtonKind::Approve,
            Some(id) if id == START_ACTION_ID => ButtonKind::Start,
            Some(id) if id == MERGE_ACTION_ID => ButtonKind::Merge,
            _ => continue,
        };
        // The mission id rides in the button `value`.
        if let Some(mission_id) = action.get("value").and_then(Value::as_str) {
            let mission_id = mission_id.trim();
            if !mission_id.is_empty() {
                // Capture the clicker + response_url so the bridge can gate
                // the button on the spend allowlist (block_actions carries
                // `user.id` and `response_url`, same as a slash command).
                let user_id = payload
                    .get("user")
                    .and_then(|u| u.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let response_url = payload
                    .get("response_url")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                let mission_id = mission_id.to_string();
                return match kind {
                    ButtonKind::Start => Action::ApproveStart {
                        mission_id,
                        user_id,
                        response_url,
                    },
                    ButtonKind::Approve => Action::Approve {
                        mission_id,
                        user_id,
                        response_url,
                    },
                    ButtonKind::Merge => Action::Merge {
                        mission_id,
                        user_id,
                        response_url,
                    },
                };
            }
        }
    }
    Action::Ignore
}

/// A modal `view_submission` → [`Action::NewMission`] when it is our
/// new-mission modal. The goal comes from `view.state.values`, the channel
/// from `private_metadata` (stashed at open — submissions carry no channel),
/// the user from `payload.user.id` (the spend gate). The plain envelope ack
/// closes the modal; there is no `response_url`, so replies go out as
/// `chat.postEphemeral`.
fn route_view_submission(payload: &Value) -> Action {
    let Some(view) = payload.get("view") else {
        return Action::Ignore;
    };
    match view.get("callback_id").and_then(Value::as_str) {
        Some(NEW_MISSION_CALLBACK_ID) => {}
        Some(CONFIG_CALLBACK_ID) => return route_config_submission(payload, view),
        _ => return Action::Ignore,
    }
    let goal = view
        .pointer(&format!(
            "/state/values/{NEW_MISSION_GOAL_BLOCK}/{NEW_MISSION_GOAL_ACTION}/value"
        ))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let channel = view
        .get("private_metadata")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if goal.is_empty() || channel.is_empty() {
        // Slack enforces the required input; an empty goal or a lost channel
        // means a malformed submission — nothing sane to create.
        return Action::Ignore;
    }
    let user_id = payload
        .get("user")
        .and_then(|u| u.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    Action::NewMission {
        goal: goal.to_string(),
        user_id,
        response_url: None,
        channel,
    }
}

/// The config modal's `view_submission` → [`Action::Config`], canonicalized
/// through the same [`parse_role`]/[`parse_effort`] tables as the slash form
/// so the bridge never sees an unvalidated role/effort. Missing/blank
/// mission id targets the single active mission (bridge-side resolution).
fn route_config_submission(payload: &Value, view: &Value) -> Action {
    let val = |block: &str, action: &str| -> Option<String> {
        view.pointer(&format!("/state/values/{block}/{action}/value"))
            .or_else(|| {
                view.pointer(&format!(
                    "/state/values/{block}/{action}/selected_option/value"
                ))
            })
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let Some(role) =
        val(CONFIG_ROLE_BLOCK, CONFIG_ROLE_ACTION).and_then(|r| parse_role(&r).map(str::to_string))
    else {
        return Action::Ignore;
    };
    let Some(model) = val(CONFIG_MODEL_BLOCK, CONFIG_MODEL_ACTION) else {
        return Action::Ignore;
    };
    let effort = val(CONFIG_EFFORT_BLOCK, CONFIG_EFFORT_ACTION)
        .and_then(|e| parse_effort(&e).map(str::to_string));
    let mission_id =
        val(CONFIG_MISSION_BLOCK, CONFIG_MISSION_ACTION).map(|id| clean_id(&id).to_string());
    let user_id = payload
        .get("user")
        .and_then(|u| u.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let channel = view
        .get("private_metadata")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    Action::Config {
        mission_id,
        role,
        model,
        effort,
        user_id,
        response_url: None,
        channel,
    }
}

/// `events_api` → a threaded human message on a known mission thread becomes
/// guidance. Everything that would cause an echo loop or has no thread is
/// ignored:
/// - non-`message` events,
/// - messages with a `bot_id` / `app_id` (our own posts, or any bot),
/// - message subtypes (edits, joins, thread-broadcasts we didn't author…),
/// - messages with no `thread_ts`, or a `thread_ts` == the message `ts` (a
///   thread root, i.e. our own mission announcement),
/// - messages whose `thread_ts` maps to no mission.
fn route_event(payload: &Value, lookup: &impl ThreadLookup) -> Action {
    let Some(event) = payload.get("event") else {
        return Action::Ignore;
    };
    // App Home opened → publish this user's dashboard tab. It carries the
    // opening user's id in `event.user` and no thread; handle it before the
    // message path (a home-open is not a message).
    if event.get("type").and_then(Value::as_str) == Some("app_home_opened") {
        return match event.get("user").and_then(Value::as_str).map(str::trim) {
            Some(user) if !user.is_empty() => Action::AppHome {
                user_id: user.to_string(),
            },
            _ => Action::Ignore,
        };
    }
    if event.get("type").and_then(Value::as_str) != Some("message") {
        return Action::Ignore;
    }
    // Ignore anything a bot/app authored (prevents the bridge answering itself)
    // and any message subtype (edits/deletes/joins carry a `subtype`).
    if event.get("bot_id").is_some()
        || event.get("app_id").is_some()
        || event.get("subtype").is_some()
    {
        return Action::Ignore;
    }
    let Some(thread_ts) = event.get("thread_ts").and_then(Value::as_str) else {
        return Action::Ignore;
    };
    // A message whose thread_ts equals its own ts is the thread root itself.
    if event.get("ts").and_then(Value::as_str) == Some(thread_ts) {
        return Action::Ignore;
    }
    let Some(mission_id) = lookup.mission_for_thread(thread_ts) else {
        return Action::Ignore;
    };
    let text = event
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return Action::Ignore;
    }
    // The author: gate fuel for the planning-turn path (a plain `user` string
    // on message events, unlike the `user.id` object interactive payloads use).
    let user_id = event
        .get("user")
        .and_then(Value::as_str)
        .map(str::to_string);
    Action::Guidance {
        mission_id,
        text: text.to_string(),
        user_id,
    }
}

/// `slash_commands` → the `/kranz` subcommand router. Recognized subcommands:
/// `ticket <title>`, `ticket list`, `ticket show <slug>`, `new <goal>`,
/// `status [<id>]`, `plan <id>`, `approve <id>`, `draft <slug>`,
/// `config [<id>] <role> <model> [effort]`, `pause [<id>]`, `resume [<id>]`,
/// `work`. A bare `/kranz`, `help`, or an unrecognized/incomplete subcommand
/// shows the command list — a typo lands on help rather than silently doing
/// something surprising, which is what keeps the surface discoverable.
///
/// The spend-gated subcommands (`new`, `plan`, `approve`, `draft`) carry the invoking
/// `user_id` so [`crate::bridge`] can consult the allowlist before acting; the
/// gate itself lives in [`crate::config::SlackConfig::is_authorized`], not here
/// (routing stays pure and config-free).
fn route_slash(payload: &Value) -> Action {
    // Slack sends the invoked command; accept `/kranz` regardless of the exact
    // registration but require it to be our command.
    if payload.get("command").and_then(Value::as_str) != Some("/kranz") {
        return Action::Ignore;
    }
    let text = payload
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    let response_url = payload
        .get("response_url")
        .and_then(Value::as_str)
        .map(str::to_string);
    let user_id = payload
        .get("user_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let channel = payload
        .get("channel_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    // `ticket list` / `ticket show <slug>` are the read-only backlog verbs
    // (not gated — checked BEFORE the title branch so a ticket literally
    // titled "list" or "show ..." is the one surprising edge case; see the
    // module doc). `ticket <title>` scaffolds a ticket; the thread is
    // captured so the scaffolder can seed from it and reply in place.
    if let Some(rest) = strip_ci_prefix(text, "ticket") {
        let arg = rest.trim();
        if let Some(after_list) = strip_ci_prefix(arg, "list") {
            if after_list.trim().is_empty() {
                return Action::TicketList { response_url };
            }
        }
        if let Some(after_show) = strip_ci_prefix(arg, "show") {
            let slug = after_show.trim();
            if !slug.is_empty() {
                return Action::TicketShow {
                    slug: slug.to_string(),
                    response_url,
                };
            }
        }
        let title = arg;
        if !title.is_empty() {
            // Slash commands can be invoked from a thread; `thread_ts` is present then.
            let thread_ts = payload
                .get("thread_ts")
                .and_then(Value::as_str)
                .map(str::to_string);
            return Action::NewTicket {
                title: title.to_string(),
                channel,
                thread_ts,
            };
        }
        // `ticket` with no title → fall through to help.
    }

    // `new <goal>` → create + seed a mission (spend-gated in the bridge).
    // Bare `new` → the multiline goal modal (slash commands are single-line;
    // a pasted multiline goal never even dispatches, so the modal is the
    // long-form path).
    if let Some(rest) = strip_ci_prefix(text, "new") {
        let goal = rest.trim();
        if !goal.is_empty() {
            return Action::NewMission {
                goal: goal.to_string(),
                user_id,
                response_url,
                channel,
            };
        }
        if let Some(trigger_id) = payload
            .get("trigger_id")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            return Action::NewMissionModal {
                trigger_id: trigger_id.to_string(),
                user_id,
                response_url,
                channel,
            };
        }
        // `new` with no goal AND no trigger_id (shouldn't happen) → help.
    }

    // `status [<id>]` → folded status summary; the optional id selects a
    // mission, otherwise the bridge picks the most recent one.
    if let Some(rest) = strip_ci_prefix(text, "status") {
        let id = clean_id(rest);
        let mission_id = (!id.is_empty()).then(|| id.to_string());
        return Action::Status {
            mission_id,
            response_url,
        };
    }

    // `plan <id>` → demand the plan (spend-gated: runs an orchestrator turn).
    if let Some(rest) = strip_ci_prefix(text, "plan") {
        let id = clean_id(rest);
        if !id.is_empty() {
            return Action::RequestPlan {
                mission_id: id.to_string(),
                user_id,
                response_url,
            };
        }
        // `plan` with no id → help.
    }

    // `approve <id>` → approve + queue (spend-gated). The slash twin of the
    // approve button.
    if let Some(rest) = strip_ci_prefix(text, "approve") {
        let id = clean_id(rest);
        if !id.is_empty() {
            return Action::ApproveMission {
                mission_id: id.to_string(),
                user_id,
                response_url,
            };
        }
        // `approve` with no id → help.
    }

    // `merge <slug|id>` → the gated Merge action (spend-adjacent, gated like
    // `work run`). The slash twin of the Delivered card's Merge button.
    if let Some(rest) = strip_ci_prefix(text, "merge") {
        let id = clean_id(rest);
        if !id.is_empty() {
            return Action::Merge {
                mission_id: id.to_string(),
                user_id,
                response_url,
            };
        }
        // `merge` with no id → help.
    }

    // `draft <slug>` → run a non-interactive draft turn for a backlog ticket
    // (spend-gated, same gate as `new`). No slug → help.
    if let Some(rest) = strip_ci_prefix(text, "draft") {
        let slug = rest.trim();
        if !slug.is_empty() {
            return Action::Draft {
                slug: slug.to_string(),
                user_id,
                response_url,
            };
        }
        // `draft` with no slug → help.
    }

    // `config [<id>] <role> <model> [effort]` → per-role model/effort change
    // (spend-adjacent, gated in the bridge). A bad role/effort or too few args
    // falls through to help so a typo is discoverable rather than silent.
    if let Some(rest) = strip_ci_prefix(text, "config") {
        if rest.trim().is_empty() {
            // Bare `config` → the modal (pickers beat positional args).
            if let Some(trigger_id) = payload
                .get("trigger_id")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
            {
                return Action::ConfigModal {
                    trigger_id: trigger_id.to_string(),
                    user_id,
                    response_url,
                    channel,
                };
            }
        }
        if let Some(cfg) = parse_config_args(
            rest,
            user_id.clone(),
            response_url.clone(),
            Some(channel.clone()).filter(|c| !c.is_empty()),
        ) {
            return cfg;
        }
        // Malformed config → help.
    }

    // `pause [<id>]` / `resume [<id>]` → enqueue a Pause/Resume control command
    // on the target mission (allowlist-gated in the bridge, disruptive steering).
    // The id is optional; a bare command targets the single active mission (the
    // bridge resolves it and refuses when ambiguous/terminal). More than one
    // trailing token is a typo → help.
    if let Some(rest) = strip_ci_prefix(text, "pause") {
        if let Some(mission_id) = parse_optional_id(rest) {
            return Action::Pause {
                mission_id,
                user_id,
                response_url,
            };
        }
        // Too many tokens → help.
    }
    if let Some(rest) = strip_ci_prefix(text, "resume") {
        if let Some(mission_id) = parse_optional_id(rest) {
            return Action::Resume {
                mission_id,
                user_id,
                response_url,
            };
        }
        // Too many tokens → help.
    }

    // `work` → report the queue state and point at the `kranz work` dispatcher
    // (report-only); `work run` → actually trigger the drain THROUGH the host
    // (spend-gated in the bridge). Any other trailing text is a typo → help.
    if let Some(rest) = strip_ci_prefix(text, "work") {
        let rest = rest.trim();
        if rest.is_empty() {
            return Action::Work { response_url };
        }
        if let Some(after_run) = strip_ci_prefix(rest, "run") {
            if after_run.trim().is_empty() {
                return Action::WorkRun {
                    user_id,
                    response_url,
                };
            }
            // `work run <extra>` → help.
        }
        // `work <anything-else>` → help.
    }

    Action::Help { response_url }
}

/// Canonical role names accepted by `/kranz config` (the friendly spellings) and
/// their [`crate::types`]-side camelCase config keys are mapped in
/// [`config_patch`]. `scrutiny` / `functional` are the short forms of the two
/// validator roles.
const ROLES: [&str; 4] = ["orchestrator", "worker", "scrutiny", "functional"];

/// Reasoning-effort values accepted by `/kranz config` (mirrors the engine's
/// `claude --effort` set).
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Parse a role token (case-insensitively) to its canonical spelling, or `None`
/// if it is not one of the four roles.
fn parse_role(token: &str) -> Option<&'static str> {
    ROLES
        .iter()
        .copied()
        .find(|r| r.eq_ignore_ascii_case(token))
}

/// Parse an effort token (case-insensitively) to its canonical spelling, or
/// `None` if it is not a valid effort.
fn parse_effort(token: &str) -> Option<&'static str> {
    EFFORTS
        .iter()
        .copied()
        .find(|e| e.eq_ignore_ascii_case(token))
}

/// Parse the arguments after `config` into an [`Action::Config`], or `None`
/// when the shape is wrong (which routes to help). Accepts two shapes,
/// disambiguated by whether the FIRST token is a known role:
/// - `<role> <model> [effort]`         → applies to the most-recent mission
/// - `<id> <role> <model> [effort]`    → applies to `<id>`
///
/// `model` is any non-empty token (model aliases/ids are open-ended, so we
/// don't validate them). `effort`, when present, must be a valid effort.
fn parse_config_args(
    rest: &str,
    user_id: Option<String>,
    response_url: Option<String>,
    channel: Option<String>,
) -> Option<Action> {
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    // If the first token is a role, there's no explicit mission id; otherwise the
    // first token is the mission id and the role starts at index 1.
    let (mission_id, role_idx) = if parse_role(tokens[0]).is_some() {
        (None, 0)
    } else {
        (Some(clean_id(tokens[0]).to_string()), 1)
    };

    let role = parse_role(tokens.get(role_idx)?)?.to_string();
    let model = tokens
        .get(role_idx + 1)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())?;
    // At most one trailing effort token; extra tokens make it ambiguous → help.
    let effort = match tokens.get(role_idx + 2) {
        Some(tok) => Some(parse_effort(tok)?.to_string()),
        None => None,
    };
    if tokens.len() > role_idx + 3 {
        return None;
    }

    Some(Action::Config {
        mission_id,
        role,
        model: model.to_string(),
        effort,
        user_id,
        response_url,
        channel,
    })
}

/// Parse the arguments after a `pause` / `resume` subcommand into an optional
/// mission id. The id is optional (a bare command targets the single active
/// mission), so:
/// - no tokens         → `Some(None)`     (target the single active mission)
/// - exactly one token → `Some(Some(id))` (explicit id)
/// - two or more tokens → `None`          (a typo → routes to help)
///
/// The outer `Option` distinguishes "valid, resolve later" (`Some`) from
/// "malformed, show help" (`None`); the inner `Option<String>` is the id itself.
fn parse_optional_id(rest: &str) -> Option<Option<String>> {
    let mut tokens = rest.split_whitespace();
    match (tokens.next(), tokens.next()) {
        (None, _) => Some(None),
        (Some(id), None) => Some(Some(clean_id(id).to_string())),
        // A second token means the input isn't a clean `pause`/`resume [<id>]`.
        (Some(_), Some(_)) => None,
    }
}

/// Strip the wrapper characters a Slack copy-paste smuggles in around an id:
/// copying a rendered code span yields the text WITH its backticks (observed
/// live: `/kranz plan `m-c9c915`` → "unknown mission"), and quotes/angle
/// brackets arrive from other clients. Interior characters are never touched;
/// goals are never cleaned (only id tokens).
fn clean_id(raw: &str) -> &str {
    raw.trim()
        .trim_matches(|c| matches!(c, '`' | '\'' | '"' | '<' | '>'))
}

/// Build the camelCase `config-change` patch for a canonical `role` (as parsed
/// by [`parse_role`]) with the given `model` and optional `effort`. Returns
/// `None` for an unknown role (defensive — routing only ever passes a canonical
/// one). The mapping is the load-bearing bit: the friendly `/kranz config` role
/// names map to the engine's `MissionConfig` keys —
/// `scrutiny → validatorScrutiny`, `functional → validatorFunctional`, the
/// other two unchanged — and `effort → reasoningEffort`.
pub fn config_patch(role: &str, model: &str, effort: Option<&str>) -> Option<Value> {
    let key = match role {
        "orchestrator" => "orchestrator",
        "worker" => "worker",
        "scrutiny" => "validatorScrutiny",
        "functional" => "validatorFunctional",
        _ => return None,
    };
    let mut role_obj = serde_json::Map::new();
    role_obj.insert("model".to_string(), Value::String(model.to_string()));
    if let Some(effort) = effort {
        role_obj.insert(
            "reasoningEffort".to_string(),
            Value::String(effort.to_string()),
        );
    }
    let mut patch = serde_json::Map::new();
    patch.insert(key.to_string(), Value::Object(role_obj));
    Some(Value::Object(patch))
}

/// Strip a leading case-insensitive word `prefix` from `text`, requiring a word
/// boundary (end of string or whitespace) after it. Returns the remainder.
fn strip_ci_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() < prefix.len() {
        return None;
    }
    let (head, rest) = text.split_at(prefix.len());
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    // Require the next char to be whitespace (or nothing) so "ticketing" ≠ "ticket".
    match rest.chars().next() {
        None => Some(rest),
        Some(c) if c.is_whitespace() => Some(rest),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A lookup that knows exactly one thread→mission mapping.
    fn lookup_one(thread: &'static str, mission: &'static str) -> impl ThreadLookup {
        move |ts: &str| (ts == thread).then(|| mission.to_string())
    }

    /// A lookup that knows nothing.
    fn lookup_none() -> impl ThreadLookup {
        |_: &str| None
    }

    #[test]
    fn block_actions_approve_routes_to_approve() {
        let env = json!({
            "type": "interactive",
            "envelope_id": "env-1",
            "payload": {
                "type": "block_actions",
                "user": { "id": "Uclicker" },
                "response_url": "https://hooks.slack/b",
                "actions": [
                    { "action_id": APPROVE_ACTION_ID, "value": "m-42", "type": "button" }
                ]
            }
        });
        let routed = route(&env, &lookup_none());
        assert_eq!(routed.envelope_id.as_deref(), Some("env-1"));
        // The clicker identity + response_url are captured so the bridge can
        // gate the button on the spend allowlist.
        assert_eq!(
            routed.action,
            Action::Approve {
                mission_id: "m-42".into(),
                user_id: Some("Uclicker".into()),
                response_url: Some("https://hooks.slack/b".into()),
            }
        );
    }

    #[test]
    fn block_actions_start_button_routes_to_approve_start() {
        let env = json!({
            "type": "interactive",
            "envelope_id": "env-9",
            "payload": {
                "type": "block_actions",
                "user": { "id": "Uclicker" },
                "response_url": "https://hooks.slack/s",
                "actions": [
                    { "action_id": START_ACTION_ID, "value": "m-42", "type": "button" }
                ]
            }
        });
        let routed = route(&env, &lookup_none());
        assert_eq!(
            routed.action,
            Action::ApproveStart {
                mission_id: "m-42".into(),
                user_id: Some("Uclicker".into()),
                response_url: Some("https://hooks.slack/s".into()),
            }
        );
    }

    #[test]
    fn block_actions_other_button_is_ignored() {
        let env = json!({
            "type": "interactive",
            "envelope_id": "e",
            "payload": {
                "type": "block_actions",
                "actions": [{ "action_id": "something_else", "value": "m-1" }]
            }
        });
        assert_eq!(route(&env, &lookup_none()).action, Action::Ignore);
    }

    #[test]
    fn thread_message_routes_to_guidance() {
        let env = json!({
            "type": "events_api",
            "envelope_id": "env-2",
            "payload": {
                "event": {
                    "type": "message",
                    "text": "use the token bucket, cap at 100/min",
                    "ts": "1700000000.000200",
                    "thread_ts": "1700000000.000100",
                    "user": "U123"
                }
            }
        });
        let routed = route(&env, &lookup_one("1700000000.000100", "m-7"));
        assert_eq!(routed.envelope_id.as_deref(), Some("env-2"));
        assert_eq!(
            routed.action,
            Action::Guidance {
                mission_id: "m-7".into(),
                text: "use the token bucket, cap at 100/min".into(),
                user_id: Some("U123".into())
            }
        );
    }

    #[test]
    fn bot_message_in_known_thread_is_ignored() {
        // Our own announcement replies (bot_id present) must not loop back.
        let env = json!({
            "type": "events_api",
            "envelope_id": "e",
            "payload": {
                "event": {
                    "type": "message",
                    "text": "Mission completed",
                    "ts": "1700000000.000300",
                    "thread_ts": "1700000000.000100",
                    "bot_id": "B999"
                }
            }
        });
        assert_eq!(
            route(&env, &lookup_one("1700000000.000100", "m-7")).action,
            Action::Ignore
        );
    }

    #[test]
    fn thread_root_message_is_ignored() {
        // ts == thread_ts → the root post itself, not a reply.
        let env = json!({
            "type": "events_api",
            "payload": { "event": {
                "type": "message",
                "text": "root",
                "ts": "1700000000.000100",
                "thread_ts": "1700000000.000100"
            }}
        });
        assert_eq!(
            route(&env, &lookup_one("1700000000.000100", "m-7")).action,
            Action::Ignore
        );
    }

    #[test]
    fn message_in_unknown_thread_is_ignored() {
        let env = json!({
            "type": "events_api",
            "payload": { "event": {
                "type": "message",
                "text": "hi",
                "ts": "1700000000.000200",
                "thread_ts": "9999999999.000000"
            }}
        });
        assert_eq!(
            route(&env, &lookup_one("1700000000.000100", "m-7")).action,
            Action::Ignore
        );
    }

    #[test]
    fn message_edit_subtype_is_ignored() {
        let env = json!({
            "type": "events_api",
            "payload": { "event": {
                "type": "message",
                "subtype": "message_changed",
                "text": "edited",
                "ts": "1700000000.000200",
                "thread_ts": "1700000000.000100"
            }}
        });
        assert_eq!(
            route(&env, &lookup_one("1700000000.000100", "m-7")).action,
            Action::Ignore
        );
    }

    #[test]
    fn top_level_message_without_thread_is_ignored() {
        let env = json!({
            "type": "events_api",
            "payload": { "event": {
                "type": "message",
                "text": "channel chatter",
                "ts": "1700000000.000200"
            }}
        });
        assert_eq!(route(&env, &lookup_none()).action, Action::Ignore);
    }

    #[test]
    fn slash_ticket_routes_to_new_ticket() {
        let env = json!({
            "type": "slash_commands",
            "envelope_id": "env-3",
            "payload": {
                "command": "/kranz",
                "text": "ticket Rate-limit the notes API",
                "channel_id": "C123",
                "thread_ts": "1700000000.000100"
            }
        });
        let routed = route(&env, &lookup_none());
        assert_eq!(routed.envelope_id.as_deref(), Some("env-3"));
        assert_eq!(
            routed.action,
            Action::NewTicket {
                title: "Rate-limit the notes API".into(),
                channel: "C123".into(),
                thread_ts: Some("1700000000.000100".into())
            }
        );
    }

    #[test]
    fn slash_ticket_case_insensitive_subcommand() {
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "TICKET  Fix the thing ", "channel_id": "C1" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::NewTicket {
                title: "Fix the thing".into(),
                channel: "C1".into(),
                thread_ts: None
            }
        );
    }

    #[test]
    fn slash_ticket_list_routes_to_ticket_list() {
        let env = json!({
            "type": "slash_commands",
            "envelope_id": "env-tl",
            "payload": { "command": "/kranz", "text": "ticket list",
                         "channel_id": "C1", "response_url": "https://hooks.slack/tl" }
        });
        let routed = route(&env, &lookup_none());
        assert_eq!(routed.envelope_id.as_deref(), Some("env-tl"));
        assert_eq!(
            routed.action,
            Action::TicketList {
                response_url: Some("https://hooks.slack/tl".into())
            }
        );
        // Case-insensitive, trailing whitespace tolerated.
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "TICKET  LIST  ",
                         "channel_id": "C1", "response_url": "https://hooks.slack/tl" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::TicketList {
                response_url: Some("https://hooks.slack/tl".into())
            }
        );
    }

    #[test]
    fn slash_ticket_show_routes_to_ticket_show() {
        let env = json!({
            "type": "slash_commands",
            "envelope_id": "env-ts",
            "payload": { "command": "/kranz", "text": "ticket show rate-limit-notes",
                         "channel_id": "C1", "response_url": "https://hooks.slack/ts" }
        });
        let routed = route(&env, &lookup_none());
        assert_eq!(routed.envelope_id.as_deref(), Some("env-ts"));
        assert_eq!(
            routed.action,
            Action::TicketShow {
                slug: "rate-limit-notes".into(),
                response_url: Some("https://hooks.slack/ts".into())
            }
        );
    }

    #[test]
    fn slash_ticket_show_without_slug_falls_through_to_title() {
        // `ticket show` with nothing after it isn't a valid show — it becomes
        // the (unusual but not our job to police) ticket title "show".
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "ticket show   ", "channel_id": "C1" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::NewTicket {
                title: "show".into(),
                channel: "C1".into(),
                thread_ts: None
            }
        );
    }

    #[test]
    fn slash_ticket_without_title_falls_through_to_help() {
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "ticket   ", "channel_id": "C1",
                         "response_url": "https://hooks.slack/x" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::Help {
                response_url: Some("https://hooks.slack/x".into())
            }
        );
    }

    #[test]
    fn slash_new_routes_to_new_mission() {
        let env = json!({
            "type": "slash_commands",
            "envelope_id": "env-n",
            "payload": {
                "command": "/kranz",
                "text": "new Rate-limit the notes API",
                "channel_id": "C123",
                "user_id": "U777",
                "response_url": "https://hooks.slack/n"
            }
        });
        let routed = route(&env, &lookup_none());
        assert_eq!(routed.envelope_id.as_deref(), Some("env-n"));
        assert_eq!(
            routed.action,
            Action::NewMission {
                goal: "Rate-limit the notes API".into(),
                user_id: Some("U777".into()),
                response_url: Some("https://hooks.slack/n".into()),
                channel: "C123".into(),
            }
        );
    }

    #[test]
    fn slash_new_without_goal_opens_the_modal() {
        // Slash commands are single-line, so bare `new` is the doorway to the
        // multiline form; the trigger_id is what views.open needs.
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "new   ",
                         "trigger_id": "13345224609.738474920.8088930838d88f008e0",
                         "user_id": "U777", "channel_id": "C123",
                         "response_url": "https://hooks.slack/x" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::NewMissionModal {
                trigger_id: "13345224609.738474920.8088930838d88f008e0".into(),
                user_id: Some("U777".into()),
                response_url: Some("https://hooks.slack/x".into()),
                channel: "C123".into(),
            }
        );
    }

    #[test]
    fn slash_new_without_goal_or_trigger_falls_through_to_help() {
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "new   ",
                         "response_url": "https://hooks.slack/x" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::Help {
                response_url: Some("https://hooks.slack/x".into())
            }
        );
    }

    #[test]
    fn new_mission_modal_submission_routes_to_new_mission() {
        let env = json!({
            "type": "interactive",
            "envelope_id": "env-modal",
            "payload": {
                "type": "view_submission",
                "user": { "id": "U777" },
                "view": {
                    "callback_id": NEW_MISSION_CALLBACK_ID,
                    "private_metadata": "C123",
                    "state": { "values": {
                        NEW_MISSION_GOAL_BLOCK: {
                            NEW_MISSION_GOAL_ACTION: {
                                "type": "plain_text_input",
                                "value": "  Fix F1 and F2 from the review.\nAdd regression tests.  "
                            }
                        }
                    }}
                }
            }
        });
        let routed = route(&env, &lookup_none());
        assert_eq!(routed.envelope_id.as_deref(), Some("env-modal"));
        assert_eq!(
            routed.action,
            Action::NewMission {
                goal: "Fix F1 and F2 from the review.\nAdd regression tests.".into(),
                user_id: Some("U777".into()),
                response_url: None,
                channel: "C123".into(),
            }
        );
    }

    #[test]
    fn bare_config_opens_the_modal() {
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "config",
                         "trigger_id": "t-123", "user_id": "U777",
                         "channel_id": "C123", "response_url": "https://hooks.slack/c" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::ConfigModal {
                trigger_id: "t-123".into(),
                user_id: Some("U777".into()),
                response_url: Some("https://hooks.slack/c".into()),
                channel: "C123".into(),
            }
        );
    }

    #[test]
    fn config_modal_submission_routes_canonicalized() {
        let env = json!({
            "type": "interactive",
            "envelope_id": "env-cfg",
            "payload": {
                "type": "view_submission",
                "user": { "id": "U777" },
                "view": {
                    "callback_id": CONFIG_CALLBACK_ID,
                    "private_metadata": "C123",
                    "state": { "values": {
                        CONFIG_MISSION_BLOCK: { CONFIG_MISSION_ACTION: { "type": "plain_text_input", "value": " `m-42` " } },
                        CONFIG_ROLE_BLOCK: { CONFIG_ROLE_ACTION: { "type": "static_select", "selected_option": { "value": "worker" } } },
                        CONFIG_MODEL_BLOCK: { CONFIG_MODEL_ACTION: { "type": "plain_text_input", "value": "sonnet" } },
                        CONFIG_EFFORT_BLOCK: { CONFIG_EFFORT_ACTION: { "type": "static_select", "selected_option": { "value": "high" } } }
                    }}
                }
            }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::Config {
                mission_id: Some("m-42".into()),
                role: "worker".into(),
                model: "sonnet".into(),
                effort: Some("high".into()),
                user_id: Some("U777".into()),
                response_url: None,
                channel: Some("C123".into()),
            }
        );
    }

    #[test]
    fn config_modal_submission_blank_mission_and_effort_are_none() {
        let env = json!({
            "type": "interactive",
            "envelope_id": "env-cfg2",
            "payload": {
                "type": "view_submission",
                "user": { "id": "U777" },
                "view": {
                    "callback_id": CONFIG_CALLBACK_ID,
                    "private_metadata": "C123",
                    "state": { "values": {
                        CONFIG_ROLE_BLOCK: { CONFIG_ROLE_ACTION: { "type": "static_select", "selected_option": { "value": "orchestrator" } } },
                        CONFIG_MODEL_BLOCK: { CONFIG_MODEL_ACTION: { "type": "plain_text_input", "value": "opus" } }
                    }}
                }
            }
        });
        match route(&env, &lookup_none()).action {
            Action::Config {
                mission_id,
                effort,
                role,
                model,
                ..
            } => {
                assert_eq!(mission_id, None, "blank id -> single-active resolution");
                assert_eq!(effort, None);
                assert_eq!(role, "orchestrator");
                assert_eq!(model, "opus");
            }
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[test]
    fn foreign_view_submission_is_ignored() {
        let env = json!({
            "type": "interactive",
            "envelope_id": "env-other",
            "payload": {
                "type": "view_submission",
                "user": { "id": "U777" },
                "view": { "callback_id": "someone_elses_modal", "private_metadata": "C123",
                          "state": { "values": {} } }
            }
        });
        assert_eq!(route(&env, &lookup_none()).action, Action::Ignore);
    }

    #[test]
    fn pasted_backticked_ids_are_cleaned() {
        // Observed live: Slack's copy of a rendered code span keeps the
        // backticks, so `/kranz plan `m-c9c915`` reached the router with a
        // literal-backtick id and failed as "unknown mission".
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "plan `m-c9c915`",
                         "user_id": "U777", "response_url": "https://hooks.slack/p" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::RequestPlan {
                mission_id: "m-c9c915".into(),
                user_id: Some("U777".into()),
                response_url: Some("https://hooks.slack/p".into()),
            }
        );
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "approve \"m-42\"",
                         "user_id": "U777", "response_url": "https://hooks.slack/a" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::ApproveMission {
                mission_id: "m-42".into(),
                user_id: Some("U777".into()),
                response_url: Some("https://hooks.slack/a".into()),
            }
        );
    }

    #[test]
    fn slash_status_with_id_routes_to_status() {
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "status m-42",
                         "response_url": "https://hooks.slack/s" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::Status {
                mission_id: Some("m-42".into()),
                response_url: Some("https://hooks.slack/s".into())
            }
        );
    }

    #[test]
    fn slash_status_bare_routes_to_status_without_id() {
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "status",
                         "response_url": "https://hooks.slack/s" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::Status {
                mission_id: None,
                response_url: Some("https://hooks.slack/s".into())
            }
        );
    }

    #[test]
    fn slash_plan_routes_to_request_plan() {
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "PLAN  m-7 ", "user_id": "U9",
                         "response_url": "https://hooks.slack/p" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::RequestPlan {
                mission_id: "m-7".into(),
                user_id: Some("U9".into()),
                response_url: Some("https://hooks.slack/p".into())
            }
        );
    }

    #[test]
    fn slash_approve_routes_to_approve_mission() {
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "approve m-7", "user_id": "U9",
                         "response_url": "https://hooks.slack/a" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::ApproveMission {
                mission_id: "m-7".into(),
                user_id: Some("U9".into()),
                response_url: Some("https://hooks.slack/a".into())
            }
        );
    }

    #[test]
    fn slash_plan_and_approve_without_id_fall_through_to_help() {
        for text in ["plan", "plan   ", "approve", "approve  "] {
            let env = json!({
                "type": "slash_commands",
                "payload": { "command": "/kranz", "text": text,
                             "response_url": "https://hooks.slack/h" }
            });
            assert_eq!(
                route(&env, &lookup_none()).action,
                Action::Help {
                    response_url: Some("https://hooks.slack/h".into())
                },
                "text={text:?} with no id should route to help"
            );
        }
    }

    #[test]
    fn slash_help_bare_and_unknown_all_route_to_help() {
        for text in ["help", "", "  ", "ticketing system", "wat"] {
            let env = json!({
                "type": "slash_commands",
                "envelope_id": "env-h",
                "payload": { "command": "/kranz", "text": text,
                             "response_url": "https://hooks.slack/r" }
            });
            let routed = route(&env, &lookup_none());
            assert_eq!(
                routed.envelope_id.as_deref(),
                Some("env-h"),
                "text={text:?}"
            );
            assert_eq!(
                routed.action,
                Action::Help {
                    response_url: Some("https://hooks.slack/r".into())
                },
                "text={text:?} should route to help"
            );
        }
    }

    // --- /kranz config -----------------------------------------------------

    /// Build a `/kranz config <text>` slash envelope.
    fn config_env(text: &str) -> Value {
        json!({
            "type": "slash_commands",
            "envelope_id": "env-cfg",
            "payload": {
                "command": "/kranz",
                "text": format!("config {text}"),
                "user_id": "Ucfg",
                "response_url": "https://hooks.slack/c"
            }
        })
    }

    #[test]
    fn slash_config_role_model_routes_to_config_most_recent() {
        // No id → applies to the most-recent mission (mission_id None).
        assert_eq!(
            route(&config_env("worker sonnet"), &lookup_none()).action,
            Action::Config {
                mission_id: None,
                role: "worker".into(),
                model: "sonnet".into(),
                effort: None,
                user_id: Some("Ucfg".into()),
                response_url: Some("https://hooks.slack/c".into()),
                channel: None,
            }
        );
    }

    #[test]
    fn slash_config_role_model_effort_routes_to_config() {
        assert_eq!(
            route(&config_env("ORCHESTRATOR opus XHIGH"), &lookup_none()).action,
            Action::Config {
                mission_id: None,
                role: "orchestrator".into(),
                model: "opus".into(),
                effort: Some("xhigh".into()),
                user_id: Some("Ucfg".into()),
                response_url: Some("https://hooks.slack/c".into()),
                channel: None,
            }
        );
    }

    #[test]
    fn slash_config_with_explicit_id_routes_to_config() {
        // First token is not a role → it's the mission id.
        assert_eq!(
            route(&config_env("m-42 scrutiny opus high"), &lookup_none()).action,
            Action::Config {
                mission_id: Some("m-42".into()),
                role: "scrutiny".into(),
                model: "opus".into(),
                effort: Some("high".into()),
                user_id: Some("Ucfg".into()),
                response_url: Some("https://hooks.slack/c".into()),
                channel: None,
            }
        );
    }

    #[test]
    fn slash_config_bad_role_or_effort_or_arity_falls_through_to_help() {
        for text in [
            "",                         // no args
            "worker",                   // no model
            "notarole sonnet",          // bad role, and "sonnet" isn't a role either
            "worker sonnet turbo",      // bad effort
            "worker sonnet high extra", // too many tokens
            "m-42 worker",              // id + role but no model
        ] {
            assert_eq!(
                route(&config_env(text), &lookup_none()).action,
                Action::Help {
                    response_url: Some("https://hooks.slack/c".into())
                },
                "config {text:?} should fall through to help"
            );
        }
    }

    #[test]
    fn config_patch_maps_roles_to_camelcase_keys() {
        // Table of (friendly role, expected top-level camelCase key).
        let cases = [
            ("orchestrator", "orchestrator"),
            ("worker", "worker"),
            ("scrutiny", "validatorScrutiny"),
            ("functional", "validatorFunctional"),
        ];
        for (role, key) in cases {
            let patch = config_patch(role, "sonnet", Some("high")).expect("known role");
            assert_eq!(
                patch,
                json!({ key: { "model": "sonnet", "reasoningEffort": "high" } }),
                "role {role} maps to {key} with reasoningEffort"
            );
        }
    }

    #[test]
    fn config_patch_omits_effort_when_absent_and_none_for_unknown_role() {
        assert_eq!(
            config_patch("worker", "sonnet", None).unwrap(),
            json!({ "worker": { "model": "sonnet" } }),
            "no effort → only model in the patch"
        );
        assert!(
            config_patch("nope", "sonnet", None).is_none(),
            "unknown role → None"
        );
    }

    // --- /kranz pause | resume | work --------------------------------------

    /// Build a `/kranz <text>` slash envelope carrying a user id + response url.
    fn steer_env(text: &str) -> Value {
        json!({
            "type": "slash_commands",
            "envelope_id": "env-steer",
            "payload": {
                "command": "/kranz",
                "text": text,
                "user_id": "Usteer",
                "response_url": "https://hooks.slack/steer"
            }
        })
    }

    #[test]
    fn slash_pause_with_id_routes_to_pause() {
        assert_eq!(
            route(&steer_env("pause m-7"), &lookup_none()).action,
            Action::Pause {
                mission_id: Some("m-7".into()),
                user_id: Some("Usteer".into()),
                response_url: Some("https://hooks.slack/steer".into()),
            }
        );
    }

    #[test]
    fn slash_pause_bare_routes_to_pause_without_id() {
        // A bare `pause` targets the single active mission (resolved in the
        // bridge), so it routes with mission_id None — NOT to help.
        assert_eq!(
            route(&steer_env("PAUSE"), &lookup_none()).action,
            Action::Pause {
                mission_id: None,
                user_id: Some("Usteer".into()),
                response_url: Some("https://hooks.slack/steer".into()),
            }
        );
    }

    #[test]
    fn slash_resume_with_and_without_id_routes_to_resume() {
        assert_eq!(
            route(&steer_env("resume m-9"), &lookup_none()).action,
            Action::Resume {
                mission_id: Some("m-9".into()),
                user_id: Some("Usteer".into()),
                response_url: Some("https://hooks.slack/steer".into()),
            }
        );
        assert_eq!(
            route(&steer_env("resume"), &lookup_none()).action,
            Action::Resume {
                mission_id: None,
                user_id: Some("Usteer".into()),
                response_url: Some("https://hooks.slack/steer".into()),
            }
        );
    }

    #[test]
    fn slash_work_routes_to_work() {
        assert_eq!(
            route(&steer_env("work"), &lookup_none()).action,
            Action::Work {
                response_url: Some("https://hooks.slack/steer".into())
            }
        );
        // Trailing whitespace is still a bare `work`.
        assert_eq!(
            route(&steer_env("WORK   "), &lookup_none()).action,
            Action::Work {
                response_url: Some("https://hooks.slack/steer".into())
            }
        );
    }

    #[test]
    fn slash_pause_resume_work_with_extra_tokens_fall_through_to_help() {
        // Two-token pause/resume, or work with an argument, are typos → help.
        for text in ["pause m-1 extra", "resume a b", "work now", "work m-1"] {
            assert_eq!(
                route(&steer_env(text), &lookup_none()).action,
                Action::Help {
                    response_url: Some("https://hooks.slack/steer".into())
                },
                "text={text:?} should route to help"
            );
        }
    }

    #[test]
    fn slash_work_run_routes_to_work_run() {
        assert_eq!(
            route(&steer_env("work run"), &lookup_none()).action,
            Action::WorkRun {
                user_id: Some("Usteer".into()),
                response_url: Some("https://hooks.slack/steer".into())
            }
        );
        // Case-insensitive, trailing whitespace tolerated.
        assert_eq!(
            route(&steer_env("WORK  RUN   "), &lookup_none()).action,
            Action::WorkRun {
                user_id: Some("Usteer".into()),
                response_url: Some("https://hooks.slack/steer".into())
            }
        );
    }

    #[test]
    fn slash_work_run_with_extra_tokens_falls_through_to_help() {
        for text in ["work run extra", "work run now"] {
            assert_eq!(
                route(&steer_env(text), &lookup_none()).action,
                Action::Help {
                    response_url: Some("https://hooks.slack/steer".into())
                },
                "text={text:?} should route to help"
            );
        }
    }

    // --- app_home_opened ---------------------------------------------------

    #[test]
    fn app_home_opened_routes_to_app_home() {
        let env = json!({
            "type": "events_api",
            "envelope_id": "env-home",
            "payload": { "event": { "type": "app_home_opened", "user": "Uhome", "tab": "home" } }
        });
        let routed = route(&env, &lookup_none());
        assert_eq!(routed.envelope_id.as_deref(), Some("env-home"));
        assert_eq!(
            routed.action,
            Action::AppHome {
                user_id: "Uhome".into()
            }
        );
    }

    #[test]
    fn app_home_opened_without_user_is_ignored() {
        let env = json!({
            "type": "events_api",
            "payload": { "event": { "type": "app_home_opened", "tab": "home" } }
        });
        assert_eq!(route(&env, &lookup_none()).action, Action::Ignore);
    }

    #[test]
    fn hello_and_unknown_envelopes_ignore_without_envelope_id() {
        let hello = json!({ "type": "hello" });
        let routed = route(&hello, &lookup_none());
        assert_eq!(routed.action, Action::Ignore);
        assert!(routed.envelope_id.is_none());

        let disconnect = json!({ "type": "disconnect", "reason": "refresh_requested" });
        assert_eq!(route(&disconnect, &lookup_none()).action, Action::Ignore);
    }
}
