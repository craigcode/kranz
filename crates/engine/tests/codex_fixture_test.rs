//! Validates the committed `codex exec --json` fixture used to ground the
//! next feature's codex backend parser. This crate has no codex parser yet
//! (see `docs/scoping/codex-backend.md`), so these tests check the fixture's
//! raw JSONL shape directly rather than through an `AgentEvent` adapter.

use kranz_engine::runner::parse_validator_report;
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("codex_exec_scrutiny.jsonl")
}

fn parse_lines() -> Vec<serde_json::Value> {
    let content = std::fs::read_to_string(fixture_path()).expect("read fixture");
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("valid JSONL line"))
        .collect()
}

#[test]
fn fixture_is_valid_jsonl() {
    let lines = parse_lines();
    assert!(!lines.is_empty());
}

#[test]
fn fixture_has_init_style_event() {
    let lines = parse_lines();
    assert!(
        lines
            .iter()
            .any(|v| v["type"] == "thread.started" && v["thread_id"].is_string()),
        "expected a thread.started init-style event with a thread_id"
    );
}

#[test]
fn fixture_has_at_least_one_text_event() {
    let lines = parse_lines();
    let text_events = lines
        .iter()
        .filter(|v| v["type"] == "item.completed" && v["item"]["type"] == "agent_message")
        .count();
    assert!(
        text_events >= 1,
        "expected at least one agent_message text event"
    );
}

#[test]
fn fixture_has_tool_call_and_result() {
    let lines = parse_lines();
    let has_call = lines
        .iter()
        .any(|v| v["type"] == "item.started" && v["item"]["type"] == "command_execution");
    let has_result = lines.iter().any(|v| {
        v["type"] == "item.completed"
            && v["item"]["type"] == "command_execution"
            && v["item"]["exit_code"].is_number()
    });
    assert!(
        has_call,
        "expected a command_execution tool call (item.started)"
    );
    assert!(
        has_result,
        "expected a command_execution tool result (item.completed with exit_code)"
    );
}

#[test]
fn fixture_has_terminal_event_with_token_usage() {
    let lines = parse_lines();
    let terminal = lines
        .iter()
        .find(|v| v["type"] == "turn.completed")
        .expect("expected a turn.completed terminal event");
    assert!(terminal["usage"]["input_tokens"].is_number());
    assert!(terminal["usage"]["output_tokens"].is_number());
    assert!(terminal["usage"]["cached_input_tokens"].is_number());
}

#[test]
fn fixture_final_assistant_text_is_a_valid_validator_report() {
    let lines = parse_lines();
    let final_text = lines
        .iter()
        .rev()
        .find_map(|v| {
            if v["type"] == "item.completed" && v["item"]["type"] == "agent_message" {
                v["item"]["text"].as_str().map(str::to_string)
            } else {
                None
            }
        })
        .expect("expected a final agent_message before the terminal event");

    let report = parse_validator_report(&final_text)
        .expect("final assistant text must parse as a ValidatorReport");
    assert!(!report.findings.is_empty());
    assert!(!report.summary.is_empty());
    for finding in &report.findings {
        assert!(!finding.subject.is_empty());
        assert!(["critical", "major", "minor"].contains(&finding.severity.as_str()));
        assert!(!finding.evidence.is_empty());
    }
}
