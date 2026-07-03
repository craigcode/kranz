//! Block Kit formatting for each outbound event class, at the integration
//! boundary — asserts the produced blocks are valid Block Kit shapes and carry
//! the mission id / reason / branch / questions a reader needs.

use kranz_slack::format::{
    build_blocked, build_complete, build_needs_context, build_plan_ready, Blocked, Complete,
    NeedsContext, Outcome, PlanReady, APPROVE_ACTION_ID,
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
    let blocks = build_plan_ready(&PlanReady {
        mission_id: "m-42".into(),
        goal: "Rate-limit the notes API".into(),
        milestone_titles: vec!["Token bucket".into(), "429 responses".into()],
        assertion_count: 2,
    });
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);
    assert!(text.contains("m-42"));
    assert!(text.contains("Rate-limit the notes API"));
    assert!(text.contains("Token bucket") && text.contains("429 responses"));
    assert!(text.contains("2 validation assertions"));

    // The approve button carries the mission id under the shared action id, and
    // must be serializable (it goes straight into a chat.postMessage body).
    let actions = blocks.iter().find(|b| b["type"] == "actions").expect("actions block");
    let button = &actions["elements"][0];
    assert_eq!(button["action_id"], APPROVE_ACTION_ID);
    assert_eq!(button["value"], "m-42");
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
    let blocks = build_blocked(&Blocked {
        mission_id: "m-7".into(),
        milestone_id: "ms-2".into(),
        reason: "fix-cycle cap exceeded after 2 rounds".into(),
    });
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);
    assert!(text.contains("m-7"));
    assert!(text.contains("ms-2"));
    assert!(text.contains("fix-cycle cap exceeded"));
    assert!(text.to_lowercase().contains("reply in this thread to unblock"));
}

#[test]
fn complete_block_kit() {
    let blocks = build_complete(&Complete {
        mission_id: "m-9".into(),
        outcome: Outcome::Completed,
        summary: "Added rate limiting; all tests pass.".into(),
        branch: "kranz/mission-m-9".into(),
        cost_usd: Some(4.2),
    });
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
    let blocks = build_complete(&Complete {
        mission_id: "m-9".into(),
        outcome: Outcome::Failed,
        summary: "worker exhausted respawns".into(),
        branch: "kranz/mission-m-9".into(),
        cost_usd: None,
    });
    assert_valid_blocks(&blocks);
    let text = all_text(&blocks);
    assert!(text.contains("failed"));
    assert!(text.contains("worker exhausted respawns"));
}
