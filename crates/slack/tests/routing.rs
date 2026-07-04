//! Envelope routing against captured Socket Mode fixtures.
//!
//! These exercise the pure [`kranz_slack::inbound::route`] against realistic
//! Slack Socket Mode envelope JSON (see `tests/fixtures/`), asserting each
//! actionable envelope maps to the right [`Action`] and each envelope carrying
//! an `envelope_id` yields it for acking.

use kranz_slack::inbound::{route, Action, ThreadLookup};
use serde_json::{json, Value};

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
    // The clicker's user id and response_url must be captured so the bridge
    // can gate the button on the spend allowlist (regression: without user_id
    // the button bypassed the allowlist entirely).
    assert_eq!(
        routed.action,
        Action::Approve {
            mission_id: "m-42".into(),
            user_id: Some("U0263M3QW".into()),
            response_url: Some("https://hooks.slack.com/actions/T024BE7LD/1234/abcd".into()),
        }
    );
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

// --- M2.9 slice 1 lifecycle subcommands ------------------------------------

#[test]
fn slash_command_new_fixture_routes_to_new_mission() {
    let routed = route(&fixture("slash_command_new.json"), &no_lookup());
    assert_eq!(
        routed.envelope_id.as_deref(),
        Some("f5c9db06-3d7b-8e54-cf6f-1c0d4b5e6f70")
    );
    assert_eq!(
        routed.action,
        Action::NewMission {
            goal: "Rate-limit the notes API".into(),
            user_id: Some("U0263M3QW".into()),
            response_url: Some("https://hooks.slack.com/commands/T024BE7LD/1234/newurl".into()),
            channel: "C0G9QF9GW".into(),
        }
    );
}

#[test]
fn slash_command_approve_fixture_routes_to_approve_mission() {
    let routed = route(&fixture("slash_command_approve.json"), &no_lookup());
    assert_eq!(
        routed.envelope_id.as_deref(),
        Some("a6d0ec17-4e8c-9f65-d070-2d1e5c6f7081")
    );
    assert_eq!(
        routed.action,
        Action::ApproveMission {
            mission_id: "m-42".into(),
            user_id: Some("U0263M3QW".into()),
            response_url: Some("https://hooks.slack.com/commands/T024BE7LD/1234/approveurl".into()),
        }
    );
}

#[test]
fn slash_status_and_plan_route_at_the_boundary() {
    // status with an id
    let status = json!({
        "type": "slash_commands",
        "envelope_id": "env-status",
        "payload": {
            "command": "/kranz",
            "text": "status m-9",
            "channel_id": "C1",
            "user_id": "U1",
            "response_url": "https://hooks.slack/s"
        }
    });
    assert_eq!(
        route(&status, &no_lookup()).action,
        Action::Status {
            mission_id: Some("m-9".into()),
            response_url: Some("https://hooks.slack/s".into())
        }
    );

    // plan <id>
    let plan = json!({
        "type": "slash_commands",
        "envelope_id": "env-plan",
        "payload": {
            "command": "/kranz",
            "text": "plan m-9",
            "channel_id": "C1",
            "user_id": "U1",
            "response_url": "https://hooks.slack/p"
        }
    });
    assert_eq!(
        route(&plan, &no_lookup()).action,
        Action::RequestPlan {
            mission_id: "m-9".into(),
            user_id: Some("U1".into()),
            response_url: Some("https://hooks.slack/p".into())
        }
    );
}

// --- M2.9 slices 2 & 3: config + App Home ----------------------------------

#[test]
fn slash_command_config_fixture_routes_to_config() {
    let routed = route(&fixture("slash_command_config.json"), &no_lookup());
    assert_eq!(
        routed.envelope_id.as_deref(),
        Some("b7e1f028-5f9d-0a76-e181-3e2f6d708192")
    );
    // First arg is a role → no explicit id (applies to the most-recent mission).
    assert_eq!(
        routed.action,
        Action::Config {
            mission_id: None,
            role: "worker".into(),
            model: "sonnet".into(),
            effort: Some("high".into()),
            user_id: Some("U0263M3QW".into()),
            response_url: Some("https://hooks.slack.com/commands/T024BE7LD/1234/configurl".into()),
        }
    );
}

#[test]
fn app_home_opened_fixture_routes_to_app_home() {
    let routed = route(&fixture("app_home_opened.json"), &no_lookup());
    assert_eq!(
        routed.envelope_id.as_deref(),
        Some("c8f2013a-609e-1b87-f292-4f307e819203")
    );
    assert_eq!(routed.action, Action::AppHome { user_id: "U0263M3QW".into() });
}
