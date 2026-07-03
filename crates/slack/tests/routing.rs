//! Envelope routing against captured Socket Mode fixtures.
//!
//! These exercise the pure [`kranz_slack::inbound::route`] against realistic
//! Slack Socket Mode envelope JSON (see `tests/fixtures/`), asserting each
//! actionable envelope maps to the right [`Action`] and each envelope carrying
//! an `envelope_id` yields it for acking.

use kranz_slack::inbound::{route, Action, ThreadLookup};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = concat_fixture(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse fixture {path}: {e}"))
}

fn concat_fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

/// A lookup mapping the fixtures' thread root to a mission.
fn lookup() -> impl ThreadLookup {
    |ts: &str| (ts == "1700000000.000100").then(|| "m-7".to_string())
}

fn no_lookup() -> impl ThreadLookup {
    |_: &str| None
}

#[test]
fn block_actions_approve_fixture_routes_to_approve() {
    let routed = route(&fixture("block_actions_approve.json"), &no_lookup());
    assert_eq!(
        routed.envelope_id.as_deref(),
        Some("b1e5a7c2-9f3d-4a10-8c2b-7e6f0d1a2b3c")
    );
    assert_eq!(routed.action, Action::Approve { mission_id: "m-42".into() });
}

#[test]
fn thread_message_fixture_routes_to_guidance() {
    let routed = route(&fixture("message_thread_reply.json"), &lookup());
    assert_eq!(
        routed.envelope_id.as_deref(),
        Some("c2f6b8d3-0a4e-5b21-9d3c-8f7a1e2b3c4d")
    );
    assert_eq!(
        routed.action,
        Action::Guidance {
            mission_id: "m-7".into(),
            text: "use a token bucket, cap requests at 100 per minute per token".into(),
        }
    );
}

#[test]
fn slash_command_ticket_fixture_routes_to_new_ticket() {
    let routed = route(&fixture("slash_command_ticket.json"), &no_lookup());
    assert_eq!(
        routed.envelope_id.as_deref(),
        Some("d3a7c9e4-1b5f-6c32-ae4d-9a8b2f3c4d5e")
    );
    assert_eq!(
        routed.action,
        Action::NewTicket {
            title: "Rate-limit the notes API".into(),
            channel: "C0G9QF9GW".into(),
            thread_ts: None,
        }
    );
}

#[test]
fn bot_echo_fixture_is_ignored() {
    // Even though the bot's own reply lands in a known thread, it must not loop.
    let routed = route(&fixture("message_bot_echo.json"), &lookup());
    assert_eq!(routed.action, Action::Ignore);
    // It still carries an envelope_id so the bridge acks it.
    assert_eq!(
        routed.envelope_id.as_deref(),
        Some("e4b8dae5-2c6a-7d43-bf5e-0b9c3a4d5e6f")
    );
}

#[test]
fn unknown_thread_message_fixture_is_ignored() {
    // Same fixture, but the lookup knows no mission for the thread.
    let routed = route(&fixture("message_thread_reply.json"), &no_lookup());
    assert_eq!(routed.action, Action::Ignore);
}
