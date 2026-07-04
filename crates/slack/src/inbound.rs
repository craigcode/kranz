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

use crate::format::APPROVE_ACTION_ID;
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
    Approve { mission_id: String, user_id: Option<String>, response_url: Option<String> },
    /// A threaded reply on `mission_id`'s thread → orchestrator guidance.
    Guidance { mission_id: String, text: String },
    /// `/kranz ticket <title>` → scaffold a new ticket file.
    NewTicket { title: String, channel: String, thread_ts: Option<String> },
    /// `/kranz new <goal>` → create a mission and seed planning (M2.9 slice 1).
    /// A money-spending action: gated by the spend allowlist. `user_id` is the
    /// invoking Slack user (for the gate); `response_url` is where an
    /// ack / not-authorized ephemeral is posted; `channel` roots the mission
    /// thread.
    NewMission {
        goal: String,
        user_id: Option<String>,
        response_url: Option<String>,
        channel: String,
    },
    /// `/kranz status [<id>]` → post a folded status summary. `mission_id`
    /// absent = "the most recent mission". Read-only, so not spend-gated.
    Status { mission_id: Option<String>, response_url: Option<String> },
    /// `/kranz plan <id>` → demand the plan for a mission (request-plan turn).
    /// A money-spending action (it runs an orchestrator turn): spend-gated.
    RequestPlan { mission_id: String, user_id: Option<String>, response_url: Option<String> },
    /// `/kranz approve <id>` → approve the plan and queue the mission. The
    /// slash-command twin of the [`Action::Approve`] button; spend-gated.
    ApproveMission { mission_id: String, user_id: Option<String>, response_url: Option<String> },
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
    let envelope_id = envelope.get("envelope_id").and_then(Value::as_str).map(str::to_string);
    let action = match envelope.get("type").and_then(Value::as_str) {
        Some("interactive") => route_interactive(payload(envelope)),
        Some("events_api") => route_event(payload(envelope), lookup),
        Some("slash_commands") => route_slash(payload(envelope)),
        // hello / disconnect / unknown: nothing to do (hello has no envelope_id
        // either, so nothing gets acked spuriously).
        _ => Action::Ignore,
    };
    Routed { action, envelope_id }
}

fn payload(envelope: &Value) -> &Value {
    envelope.get("payload").unwrap_or(&Value::Null)
}

/// `interactive` → an approve button click, else ignore. We only act on
/// `block_actions` whose action id is our approve button; every other
/// interaction (menus, other buttons) is ignored.
fn route_interactive(payload: &Value) -> Action {
    if payload.get("type").and_then(Value::as_str) != Some("block_actions") {
        return Action::Ignore;
    }
    let Some(actions) = payload.get("actions").and_then(Value::as_array) else {
        return Action::Ignore;
    };
    for action in actions {
        if action.get("action_id").and_then(Value::as_str) == Some(APPROVE_ACTION_ID) {
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
                    let response_url =
                        payload.get("response_url").and_then(Value::as_str).map(str::to_string);
                    return Action::Approve {
                        mission_id: mission_id.to_string(),
                        user_id,
                        response_url,
                    };
                }
            }
        }
    }
    Action::Ignore
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
    let text = event.get("text").and_then(Value::as_str).unwrap_or("").trim();
    if text.is_empty() {
        return Action::Ignore;
    }
    Action::Guidance { mission_id, text: text.to_string() }
}

/// `slash_commands` → the `/kranz` subcommand router. Recognized subcommands:
/// `ticket <title>`, `new <goal>`, `status [<id>]`, `plan <id>`, `approve <id>`.
/// A bare `/kranz`, `help`, or an unrecognized/incomplete subcommand shows the
/// command list — a typo lands on help rather than silently doing something
/// surprising, which is what keeps the surface discoverable.
///
/// The spend-gated subcommands (`new`, `plan`, `approve`) carry the invoking
/// `user_id` so [`crate::bridge`] can consult the allowlist before acting; the
/// gate itself lives in [`crate::config::SlackConfig::is_authorized`], not here
/// (routing stays pure and config-free).
fn route_slash(payload: &Value) -> Action {
    // Slack sends the invoked command; accept `/kranz` regardless of the exact
    // registration but require it to be our command.
    if payload.get("command").and_then(Value::as_str) != Some("/kranz") {
        return Action::Ignore;
    }
    let text = payload.get("text").and_then(Value::as_str).unwrap_or("").trim();
    let response_url = payload.get("response_url").and_then(Value::as_str).map(str::to_string);
    let user_id = payload.get("user_id").and_then(Value::as_str).map(str::to_string);
    let channel =
        payload.get("channel_id").and_then(Value::as_str).unwrap_or("").to_string();

    // `ticket <title>` scaffolds a ticket; the thread is captured so the
    // scaffolder can seed from it and reply in place.
    if let Some(rest) = strip_ci_prefix(text, "ticket") {
        let title = rest.trim();
        if !title.is_empty() {
            // Slash commands can be invoked from a thread; `thread_ts` is present then.
            let thread_ts = payload.get("thread_ts").and_then(Value::as_str).map(str::to_string);
            return Action::NewTicket { title: title.to_string(), channel, thread_ts };
        }
        // `ticket` with no title → fall through to help.
    }

    // `new <goal>` → create + seed a mission (spend-gated in the bridge).
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
        // `new` with no goal → help.
    }

    // `status [<id>]` → folded status summary; the optional id selects a
    // mission, otherwise the bridge picks the most recent one.
    if let Some(rest) = strip_ci_prefix(text, "status") {
        let id = rest.trim();
        let mission_id = (!id.is_empty()).then(|| id.to_string());
        return Action::Status { mission_id, response_url };
    }

    // `plan <id>` → demand the plan (spend-gated: runs an orchestrator turn).
    if let Some(rest) = strip_ci_prefix(text, "plan") {
        let id = rest.trim();
        if !id.is_empty() {
            return Action::RequestPlan { mission_id: id.to_string(), user_id, response_url };
        }
        // `plan` with no id → help.
    }

    // `approve <id>` → approve + queue (spend-gated). The slash twin of the
    // approve button.
    if let Some(rest) = strip_ci_prefix(text, "approve") {
        let id = rest.trim();
        if !id.is_empty() {
            return Action::ApproveMission { mission_id: id.to_string(), user_id, response_url };
        }
        // `approve` with no id → help.
    }

    Action::Help { response_url }
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
                text: "use the token bucket, cap at 100/min".into()
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
        assert_eq!(route(&env, &lookup_one("1700000000.000100", "m-7")).action, Action::Ignore);
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
        assert_eq!(route(&env, &lookup_one("1700000000.000100", "m-7")).action, Action::Ignore);
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
        assert_eq!(route(&env, &lookup_one("1700000000.000100", "m-7")).action, Action::Ignore);
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
        assert_eq!(route(&env, &lookup_one("1700000000.000100", "m-7")).action, Action::Ignore);
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
    fn slash_ticket_without_title_falls_through_to_help() {
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "ticket   ", "channel_id": "C1",
                         "response_url": "https://hooks.slack/x" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::Help { response_url: Some("https://hooks.slack/x".into()) }
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
    fn slash_new_without_goal_falls_through_to_help() {
        let env = json!({
            "type": "slash_commands",
            "payload": { "command": "/kranz", "text": "new   ",
                         "response_url": "https://hooks.slack/x" }
        });
        assert_eq!(
            route(&env, &lookup_none()).action,
            Action::Help { response_url: Some("https://hooks.slack/x".into()) }
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
            Action::Status { mission_id: None, response_url: Some("https://hooks.slack/s".into()) }
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
                Action::Help { response_url: Some("https://hooks.slack/h".into()) },
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
            assert_eq!(routed.envelope_id.as_deref(), Some("env-h"), "text={text:?}");
            assert_eq!(
                routed.action,
                Action::Help { response_url: Some("https://hooks.slack/r".into()) },
                "text={text:?} should route to help"
            );
        }
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
