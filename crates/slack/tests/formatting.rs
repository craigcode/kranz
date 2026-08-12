//! Block Kit formatting for each outbound event class, at the integration
//! boundary — asserts the produced blocks are valid Block Kit shapes and carry
//! the mission id / reason / branch / questions a reader needs.

use kranz_engine::outcomes::{
    AutonomyRatio, EscalationKind, EscalationRow, GrantLatency, LatencyBucket, Outcomes,
};
use kranz_slack::format::{
    build_blocked, build_complete, build_help, build_home_view, build_needs_context,
    build_new_mission_ack, build_outcomes_summary, build_plan_ready, build_plan_review,
    build_status, dashboard_button, dashboard_deep_link, Blocked, Complete, HomeMission,
    HomeQueueItem, HomeTicket, NeedsContext, NewMissionAck, Outcome, PlanAlternativesReview,
    PlanReady, PlanReview, RejectedAlternativeReview, StatusSummary, APPROVE_ACTION_ID,
    START_ACTION_ID,
};
use serde_json::Value;

/// Every block must have a recognized `type` (a crude Block Kit sanity check
/// so a typo in a block builder is caught).
fn assert_valid_blocks(blocks: &[Value]) {
    const KNOWN: [&str; 5] = ["header", "section", "context", "divider", "actions"];
    assert!(!blocks.is_empty(), "at least one block");
    for b in blocks {
        let ty = b["type"].as_str().expect("block has a string type");
        assert!(KNOWN.contains(&ty), "unexpected block type {ty}");
    }
}

/// Flatten all `text` strings anywhere in the blocks.
fn all_text(blocks: &[Value]) -> String {
    fn walk(v: &Value, out: &mut String) {
        match v {
            Value::Object(map) => {
                for (k, val) in map {
                    if k == "text" {
                        if let Some(s) = val.as_str() {
                            out.push_str(s);
                            out.push('\n');
                        }
                    }
                    walk(val, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
            _ => {}
        }
    }
    let mut out = String::new();
    blocks.iter().for_each(|b| walk(b, &mut out));
    out
}

#[test]
fn plan_ready_block_kit() {
    let blocks = build_plan_ready(
        &PlanReady {
            mission_id: "m-42".into(),
            goal: "Rate-limit the notes API".into(),
            milestone_titles: vec!["Token bucket".into(), "429 responses".into()],
            assertion_count: 2,
        },
        None,
    );
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);
    assert!(text.contains("m-42"));
    assert!(text.contains("Rate-limit the notes API"));
    assert!(text.contains("Token bucket") && text.contains("429 responses"));
    assert!(text.contains("2 validation assertions"));

    // Plan-ready surface: Approve & start / Approve & queue (draft path) and
    // still serializable (it goes straight into a chat.postMessage body).
    let action_ids: Vec<&str> = blocks
        .iter()
        .filter(|b| b["type"] == "actions")
        .flat_map(|b| b["elements"].as_array().into_iter().flatten())
        .filter_map(|e| e["action_id"].as_str())
        .collect();
    assert!(
        action_ids.contains(&"kranz_approve") && action_ids.contains(&"kranz_start"),
        "approve affordances present: {action_ids:?}"
    );
    assert!(text.contains("Plan ready for review"));
    assert!(serde_json::to_string(&blocks).is_ok());
}

#[test]
fn needs_context_block_kit() {
    let blocks = build_needs_context(&NeedsContext {
        ticket_slug: "rate-limit".into(),
        questions: vec!["What's the test command?".into(), "Which endpoints?".into()],
    });
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);
    assert!(text.contains("rate-limit"));
    assert!(text.contains("What's the test command?"));
    assert!(text.contains("Which endpoints?"));
    assert!(text.to_lowercase().contains("reply in this thread"));
}

#[test]
fn blocked_block_kit() {
    let blocks = build_blocked(
        &Blocked {
            mission_id: "m-7".into(),
            milestone_id: "ms-2".into(),
            reason: "fix-cycle cap exceeded after 2 rounds".into(),
        },
        None,
    );
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);
    assert!(text.contains("m-7"));
    assert!(text.contains("ms-2"));
    assert!(text.contains("fix-cycle cap exceeded"));
    assert!(text
        .to_lowercase()
        .contains("reply in this thread to unblock"));
}

#[test]
fn complete_block_kit() {
    let blocks = build_complete(
        &Complete {
            mission_id: "m-9".into(),
            outcome: Outcome::Completed,
            summary: "Added rate limiting; all tests pass.".into(),
            branch: "kranz/mission-m-9".into(),
            cost_usd: Some(4.2),
            diff_stat: None,
            pr_handoff: None,
        },
        None,
    );
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);
    assert!(text.contains("m-9"));
    assert!(text.contains("completed"));
    assert!(text.contains("Added rate limiting"));
    assert!(text.contains("kranz/mission-m-9"));
    assert!(text.contains("$4.20"));
}

#[test]
fn failed_block_kit_reads_failed() {
    let blocks = build_complete(
        &Complete {
            mission_id: "m-9".into(),
            outcome: Outcome::Failed,
            summary: "worker exhausted respawns".into(),
            branch: "kranz/mission-m-9".into(),
            cost_usd: None,
            diff_stat: None,
            pr_handoff: None,
        },
        None,
    );
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);
    assert!(text.contains("failed"));
    assert!(text.contains("worker exhausted respawns"));
}

#[test]
fn help_lists_the_commands() {
    let blocks = build_help();
    assert_valid_blocks(&blocks);
    let text = serde_json::to_string(&blocks).unwrap();
    assert!(
        text.contains("/kranz ticket"),
        "help lists the ticket command"
    );
    assert!(text.contains("/kranz help"), "help lists itself");
    assert!(
        text.to_lowercase().contains("approve"),
        "help mentions the approve button"
    );
    assert!(
        text.to_lowercase().contains("guidance"),
        "help mentions thread-reply guidance"
    );
    // M2.9 slice 1 lifecycle commands are now listed.
    assert!(text.contains("/kranz new"), "help lists new");
    assert!(text.contains("/kranz plan"), "help lists plan");
    assert!(text.contains("/kranz approve"), "help lists approve");
    assert!(text.contains("/kranz status"), "help lists status");
}

#[test]
fn new_mission_ack_block_kit() {
    let blocks = build_new_mission_ack(&NewMissionAck {
        mission_id: "m-42".into(),
        goal: "Rate-limit the notes API".into(),
        opening_reply: Some("Which endpoints are in scope?".into()),
    });
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);
    assert!(text.contains("m-42"), "ack carries the mission id");
    assert!(
        text.contains("Rate-limit the notes API"),
        "ack carries the goal"
    );
    assert!(
        text.contains("Which endpoints are in scope?"),
        "opening questions surfaced"
    );
    assert!(text.to_lowercase().contains("reply in this thread"));
}

#[test]
fn status_block_kit() {
    let blocks = build_status(&StatusSummary {
        mission_id: "m-7".into(),
        status: "Running".into(),
        summary: "2/3 milestones complete · cost $1.20".into(),
    });
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);
    assert!(text.contains("m-7"), "status carries the mission id");
    assert!(text.contains("Running"), "status pill present");
    assert!(
        text.contains("2/3 milestones complete"),
        "summary body present"
    );
}

#[test]
fn approved_status_card_reads_approved_not_running() {
    let blocks = build_status(&StatusSummary {
        mission_id: "m-8".into(),
        status: "Approved".into(),
        summary: "0/3 milestones complete · cost $0.00".into(),
    });
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);
    assert!(text.contains("Approved"), "status pill reads Approved");
    assert!(
        !text.contains("Running"),
        "an approved mission's card must not read Running"
    );
}

#[test]
fn plan_review_block_kit_has_start_and_queue_buttons() {
    let blocks = build_plan_review(&PlanReview {
        mission_id: "m-42".into(),
        goal: "Rate-limit the notes API".into(),
        milestone_titles: vec!["Token bucket".into(), "429 responses".into()],
        assertion_count: 2,
        considered_alternatives: Some(PlanAlternativesReview {
            chosen: "small vertical slice".into(),
            rejected: vec![
                RejectedAlternativeReview {
                    approach: "rewrite all handlers".into(),
                    trade_off: "too much blast radius".into(),
                },
                RejectedAlternativeReview {
                    approach: "docs only".into(),
                    trade_off: "does not enforce the limit".into(),
                },
            ],
        }),
        estimate: Some("~$3.10 · ~9 min".into()),
    });
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);
    assert!(text.contains("m-42"));
    assert!(text.contains("Rate-limit the notes API"));
    assert!(text.contains("Token bucket") && text.contains("429 responses"));
    assert!(text.contains("small vertical slice"));
    assert!(text.contains("rewrite all handlers"));
    assert!(text.contains("2 validation assertions"));
    assert!(text.contains("~$3.10 · ~9 min"), "estimate rendered");

    // Two buttons, both carrying the mission id under their distinct action ids.
    let actions = blocks
        .iter()
        .find(|b| b["type"] == "actions")
        .expect("actions block");
    let elements = actions["elements"].as_array().expect("button elements");
    assert_eq!(elements.len(), 2, "approve & start plus approve & queue");
    let start = elements
        .iter()
        .find(|e| e["action_id"] == START_ACTION_ID)
        .expect("approve & start button");
    let queue = elements
        .iter()
        .find(|e| e["action_id"] == APPROVE_ACTION_ID)
        .expect("approve & queue button");
    assert_eq!(start["value"], "m-42");
    assert_eq!(queue["value"], "m-42");
    // The whole thing must serialize (it goes straight into a postMessage body).
    assert!(serde_json::to_string(&blocks).is_ok());
}

// --- M2.9 slices 2 & 3: deep-link buttons + App Home -----------------------

/// The `url` of every link button across all `actions` blocks.
fn actions_button_urls(blocks: &[Value]) -> Vec<String> {
    let mut out = Vec::new();
    for b in blocks {
        if b["type"] == "actions" {
            if let Some(elems) = b["elements"].as_array() {
                for e in elems {
                    if let Some(url) = e["url"].as_str() {
                        out.push(url.to_string());
                    }
                }
            }
        }
    }
    out
}

#[test]
fn deep_link_shape_and_button_presence() {
    // The deep link is <dashboardUrl>#/m/<id>, one slash regardless of the base.
    assert_eq!(
        dashboard_deep_link("http://h:4600", "m-1"),
        "http://h:4600/#/m/m-1"
    );
    assert_eq!(
        dashboard_deep_link("http://h:4600/", "m-1"),
        "http://h:4600/#/m/m-1"
    );

    // No button when unset/blank; a link button when set.
    assert!(dashboard_button(None, "m-1").is_none());
    assert!(dashboard_button(Some("  "), "m-1").is_none());
    let btn = dashboard_button(Some("http://h:4600"), "m-1").expect("button");
    assert_eq!(btn["elements"][0]["url"], "http://h:4600/#/m/m-1");
}

#[test]
fn plan_ready_deep_link_present_only_when_url_set() {
    let p = PlanReady {
        mission_id: "m-42".into(),
        goal: "g".into(),
        milestone_titles: vec![],
        assertion_count: 1,
    };
    assert!(actions_button_urls(&build_plan_ready(&p, None)).is_empty());
    assert_eq!(
        actions_button_urls(&build_plan_ready(&p, Some("http://h:4600"))),
        vec!["http://h:4600/#/m/m-42".to_string()]
    );
}

#[test]
fn home_view_is_valid_block_kit_with_missions_queue_and_tickets() {
    let view = build_home_view(
        &[HomeMission {
            mission_id: "m-1".into(),
            status: "Running".into(),
        }],
        &[HomeQueueItem {
            mission_id: "m-2".into(),
            priority: 3,
        }],
        &[HomeTicket {
            slug: "rate-limit".into(),
            title: "Rate-limit the notes API".into(),
            state: "New".into(),
        }],
        Some("http://h:4600"),
    );
    assert_eq!(view["type"], "home", "views.publish expects a home view");
    let blocks = view["blocks"].as_array().expect("home blocks");
    assert_valid_blocks(blocks);
    let text = all_text(blocks);
    assert!(
        text.contains("m-1") && text.contains("Running"),
        "mission + status pill"
    );
    assert!(
        text.contains("m-2") && text.contains("priority 3"),
        "queue row"
    );
    assert!(
        text.contains("rate-limit") && text.contains("Rate-limit the notes API"),
        "ticket"
    );
    // Mission row deep-links to the dashboard when configured.
    let has_link = blocks
        .iter()
        .any(|b| b["accessory"]["url"] == "http://h:4600/#/m/m-1");
    assert!(has_link, "mission row carries a dashboard deep link");
    assert!(
        serde_json::to_string(&view).is_ok(),
        "the view must serialize"
    );
}

#[test]
fn home_view_empty_state_renders_and_validates() {
    let view = build_home_view(&[], &[], &[], None);
    let blocks = view["blocks"].as_array().expect("home blocks");
    assert_valid_blocks(blocks);
    let text = all_text(blocks).to_lowercase();
    assert!(text.contains("no active missions"));
    assert!(text.contains("queue is empty"));
    assert!(text.contains("no open tickets"));
}

#[test]
fn help_lists_the_config_command() {
    let text = serde_json::to_string(&build_help()).unwrap();
    assert!(
        text.contains("/kranz config"),
        "help lists the config command"
    );
}

#[test]
fn help_lists_the_steering_commands() {
    // M2.9 steering slice: pause / resume / work are now discoverable in help.
    let text = serde_json::to_string(&build_help()).unwrap();
    assert!(text.contains("/kranz pause"), "help lists pause");
    assert!(text.contains("/kranz resume"), "help lists resume");
    assert!(text.contains("/kranz work"), "help lists work");
    // work points at the dispatcher, not an inline drain.
    assert!(text.contains("kranz work"), "help names the dispatcher");
}

#[test]
fn outcomes_slack_card_shows_ratio_buckets_and_escalation_count() {
    let outcomes = Outcomes {
        autonomy_ratio: AutonomyRatio {
            closed_missions: 4,
            total_interventions: 6,
            interventions_per_closed_mission: 1.5,
            zero_intervention_missions: 1,
            zero_intervention_share: 0.25,
        },
        grant_latency: GrantLatency {
            buckets: vec![
                LatencyBucket {
                    label: "<10s".into(),
                    count: 3,
                },
                LatencyBucket {
                    label: "<60s".into(),
                    count: 2,
                },
                LatencyBucket {
                    label: "<10m".into(),
                    count: 1,
                },
                LatencyBucket {
                    label: ">=10m".into(),
                    count: 0,
                },
            ],
            total_decided: 6,
        },
        escalations: vec![EscalationRow {
            ts: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            mission_id: "m-1".into(),
            kind: EscalationKind::Grant,
            summary: "cargo test".into(),
            decision: "approved".into(),
            latency_ms: Some(5_000),
            rubber_stamp: Some(true),
        }],
        cost_per_change: kranz_engine::outcomes::CostPerChange {
            total_cost_usd: 42.50,
            non_meta_commits: 5,
            usd_per_commit: Some(8.50),
        },
        cycle_time: kranz_engine::outcomes::CycleTime {
            closed_missions: 4,
            total_ms: 14_400_000,
            mean_ms: Some(3_600_000.0),
        },
        task_classes: vec![],
        context_reuse: vec![],
        rubber_stamp: kranz_engine::outcomes::RubberStampReport {
            threshold_ms: 10_000,
            approved_decisions: 1,
            flagged: 1,
            share: Some(1.0),
        },
        gate_score_flags: kranz_engine::gate_score_flags::GateScoreFlagsReport::default(),
        divergences: None,
        comparison: None,
    };

    let blocks = build_outcomes_summary(&outcomes);
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);

    assert!(text.contains("1.50"), "renders the autonomy ratio: {text}");
    // Bucket labels carry angle brackets; the card must render them escaped
    // (&lt;/&gt;) — raw `<10s` is mrkdwn link syntax and would swallow the
    // bucket lines into a bogus link.
    assert!(
        text.contains("&lt;10s"),
        "renders bucket <10s escaped: {text}"
    );
    assert!(
        text.contains("&lt;60s"),
        "renders bucket <60s escaped: {text}"
    );
    assert!(
        text.contains("&lt;10m"),
        "renders bucket <10m escaped: {text}"
    );
    assert!(
        text.contains("&gt;=10m"),
        "renders bucket >=10m escaped: {text}"
    );
    assert!(
        !text.contains("<10s"),
        "no raw mrkdwn-link-syntax label survives: {text}"
    );
    assert!(text.contains('3'), "renders <10s count: {text}");
    assert!(text.contains('2'), "renders <60s count: {text}");
    assert!(text.contains("COST PER CHANGE"), "{text}");
    assert!(text.contains("$8.50"), "renders per-commit cost: {text}");
    assert!(text.contains("CYCLE TIME"), "{text}");
    assert!(text.contains("1.0h"), "renders mean cycle time: {text}");
    assert!(
        text.contains('1'),
        "renders escalation count or <10m count: {text}"
    );
}

#[test]
fn outcomes_slack_card_empty_history_renders_gracefully() {
    let outcomes = Outcomes {
        autonomy_ratio: AutonomyRatio {
            closed_missions: 0,
            total_interventions: 0,
            interventions_per_closed_mission: 0.0,
            zero_intervention_missions: 0,
            zero_intervention_share: 0.0,
        },
        grant_latency: GrantLatency {
            buckets: vec![
                LatencyBucket {
                    label: "<10s".into(),
                    count: 0,
                },
                LatencyBucket {
                    label: "<60s".into(),
                    count: 0,
                },
                LatencyBucket {
                    label: "<10m".into(),
                    count: 0,
                },
                LatencyBucket {
                    label: ">=10m".into(),
                    count: 0,
                },
            ],
            total_decided: 0,
        },
        escalations: vec![],
        cost_per_change: kranz_engine::outcomes::CostPerChange {
            total_cost_usd: 0.0,
            non_meta_commits: 0,
            usd_per_commit: None,
        },
        cycle_time: kranz_engine::outcomes::CycleTime {
            closed_missions: 0,
            total_ms: 0,
            mean_ms: None,
        },
        task_classes: vec![],
        context_reuse: vec![],
        rubber_stamp: kranz_engine::outcomes::RubberStampReport::default(),
        gate_score_flags: kranz_engine::gate_score_flags::GateScoreFlagsReport::default(),
        divergences: None,
        comparison: None,
    };

    let blocks = build_outcomes_summary(&outcomes);
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);
    assert!(text.contains('0'), "renders zero counts: {text}");
    assert!(
        text.contains("0 escalation"),
        "renders zero-escalation summary: {text}"
    );
}
