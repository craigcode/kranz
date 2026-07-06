//! `/kranz ticket list` and `/kranz ticket show <slug>` — the read-only
//! backlog verbs (docs/backlog-and-slack.md). Covers: routing dispatches from
//! the single-line slash-command path and replies over `response_url`
//! (ephemeral); neither verb carries a `user_id` so there is structurally no
//! allowlist gate (mirrors `/kranz status`); an unknown/invalid slug is a
//! graceful error, never a panic; and a thread-context message never dispatches
//! these verbs (only `slash_commands` envelopes do — an `events_api` message,
//! even one that says "ticket list", stays on the thread-guidance path).

use kranz_slack::bridge::{build_ticket_list_reply, build_ticket_show_reply};
use kranz_slack::inbound::{route, Action, ThreadLookup};
use serde_json::{json, Value};
use tempfile::TempDir;

fn no_lookup() -> impl ThreadLookup {
    |_: &str| None
}

fn slash_env(text: &str) -> Value {
    json!({
        "type": "slash_commands",
        "envelope_id": "env-tix",
        "payload": {
            "command": "/kranz",
            "text": text,
            "channel_id": "C1",
            "response_url": "https://hooks.slack/tix"
        }
    })
}

fn write_ticket(repo: &std::path::Path, slug: &str, markdown: &str) {
    let dir = repo.join(".kranz").join("tickets");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{slug}.md")), markdown).unwrap();
}

fn all_text(blocks: &[Value]) -> String {
    serde_json::to_string(blocks).unwrap()
}

// -- ticket list: dispatches ephemerally, no allowlist gate -----------------

#[test]
fn ticket_list_dispatches_from_single_line_slash_and_replies_ephemeral() {
    // "Ephemeral" here means the reply travels over the slash `response_url`
    // (chat.responseUrl / postEphemeral path), same mechanism as `/kranz
    // status` — never posted to the channel at large.
    let routed = route(&slash_env("ticket list"), &no_lookup());
    assert_eq!(routed.envelope_id.as_deref(), Some("env-tix"));
    match routed.action {
        Action::TicketList { response_url } => {
            assert_eq!(response_url.as_deref(), Some("https://hooks.slack/tix"));
        }
        other => panic!("expected TicketList, got {other:?}"),
    }
}

#[test]
fn ticket_list_action_carries_no_user_id_so_it_cannot_be_allowlist_gated() {
    // Action::TicketList has no `user_id` field at all: there is nothing for
    // `SlackConfig::is_authorized` to consult, which is how the read-only
    // verbs are architecturally exempt from the spend allowlist (same shape
    // as Action::Status / Action::Work).
    let routed = route(&slash_env("ticket list"), &no_lookup());
    assert_eq!(
        routed.action,
        Action::TicketList {
            response_url: Some("https://hooks.slack/tix".into())
        }
    );
}

#[test]
fn ticket_list_reply_lists_ticket_rows_with_blocked_by_note() {
    let tmp = TempDir::new().unwrap();
    write_ticket(
        tmp.path(),
        "rate-limit-notes",
        "---\ntitle: Rate-limit the notes API\npriority: 1\n---\n## Goal\nAdd a token bucket.\n",
    );
    write_ticket(
        tmp.path(),
        "downstream-thing",
        "---\ntitle: Downstream thing\npriority: 2\nblocked-by: [rate-limit-notes]\n---\n## Goal\nBuild on top.\n",
    );

    let blocks = build_ticket_list_reply(tmp.path());
    let text = all_text(&blocks);
    assert!(text.contains("rate-limit-notes"), "lists first ticket slug");
    assert!(
        text.contains("Rate-limit the notes API"),
        "lists first ticket title"
    );
    assert!(
        text.contains("downstream-thing"),
        "lists second ticket slug"
    );
    assert!(
        text.contains("blocked by"),
        "notes the blocked-by relationship: {text}"
    );
    assert!(text.contains("New"), "shows the default pipeline state");
}

#[test]
fn ticket_list_reply_on_empty_backlog_is_a_friendly_empty_state() {
    let tmp = TempDir::new().unwrap();
    let blocks = build_ticket_list_reply(tmp.path());
    let text = all_text(&blocks);
    assert!(text.to_lowercase().contains("no backlog tickets"));
}

// -- ticket show: dispatches ephemerally, no allowlist gate, graceful errors --

#[test]
fn ticket_show_dispatches_from_single_line_slash_and_replies_ephemeral() {
    let routed = route(&slash_env("ticket show rate-limit-notes"), &no_lookup());
    assert_eq!(routed.envelope_id.as_deref(), Some("env-tix"));
    assert_eq!(
        routed.action,
        Action::TicketShow {
            slug: "rate-limit-notes".into(),
            response_url: Some("https://hooks.slack/tix".into())
        }
    );
}

#[test]
fn ticket_show_action_carries_no_user_id_so_it_cannot_be_allowlist_gated() {
    match route(&slash_env("ticket show anything"), &no_lookup()).action {
        Action::TicketShow { slug, .. } => assert_eq!(slug, "anything"),
        other => panic!("expected TicketShow, got {other:?}"),
    }
}

#[test]
fn ticket_show_reply_renders_goal_state_blocked_by_and_needs_context() {
    let tmp = TempDir::new().unwrap();
    write_ticket(
        tmp.path(),
        "rate-limit-notes",
        "---\ntitle: Rate-limit the notes API\nblocked-by: [auth-refactor]\n---\n\
         ## Goal\nAdd a token bucket, cap at 100/min.\n\n\
         ## Needs context\n- Which endpoint exactly?\n- Per-user or per-token?\n",
    );

    let blocks = build_ticket_show_reply(tmp.path(), "rate-limit-notes");
    let text = all_text(&blocks);
    assert!(text.contains("Rate-limit the notes API"));
    assert!(text.contains("token bucket"));
    assert!(text.contains("auth-refactor"), "shows blocked-by");
    assert!(
        text.contains("Which endpoint exactly?"),
        "shows needs-context questions"
    );
    assert!(text.contains("Per-user or per-token?"));
}

#[test]
fn ticket_show_unknown_slug_is_a_graceful_ephemeral_error_no_panic() {
    let tmp = TempDir::new().unwrap();
    let blocks = build_ticket_show_reply(tmp.path(), "does-not-exist");
    let text = all_text(&blocks);
    assert!(
        text.to_lowercase().contains("unknown ticket"),
        "got: {text}"
    );
}

#[test]
fn ticket_show_invalid_slug_is_a_graceful_ephemeral_error_no_panic() {
    let tmp = TempDir::new().unwrap();
    // A path-traversal attempt must be rejected by ensure_valid_slug before
    // any filesystem access, not panic and not escape .kranz/tickets/.
    for bad in ["../../etc/passwd", "..", "", "has space"] {
        let blocks = build_ticket_show_reply(tmp.path(), bad);
        let text = all_text(&blocks);
        assert!(
            text.to_lowercase().contains("invalid"),
            "slug {bad:?} should be a graceful invalid-slug error, got: {text}"
        );
    }
}

// -- thread-context invocation is never dispatched --------------------------

#[test]
fn ticket_verbs_typed_in_a_thread_message_are_not_dispatched() {
    // Only `slash_commands` envelopes reach `route_slash`; an `events_api`
    // message (even one that says "ticket list" or "ticket show <slug>")
    // follows the thread-guidance rules instead and is never turned into
    // Action::TicketList / Action::TicketShow.
    for text in ["ticket list", "ticket show rate-limit-notes"] {
        let env = json!({
            "type": "events_api",
            "payload": { "event": {
                "type": "message",
                "text": text,
                "ts": "1700000000.000200",
                "thread_ts": "1700000000.000100",
                "user": "U123"
            }}
        });
        // Unknown thread -> Ignore; a known thread would fold to Guidance.
        // Either way it is never TicketList/TicketShow.
        let action = route(&env, &no_lookup()).action;
        assert_ne!(action, Action::TicketList { response_url: None });
        match action {
            Action::TicketList { .. } | Action::TicketShow { .. } => {
                panic!("thread message {text:?} must not dispatch a ticket verb")
            }
            _ => {}
        }
    }
}
