//! Validates the committed `droid exec -o json` fixtures used to ground the
//! next feature's droid backend parser. This crate has no droid parser yet,
//! so these tests check the fixture's raw JSON shape directly rather than
//! through an `AgentEvent` adapter.

use kranz_engine::runner::parse_validator_report;
use std::path::PathBuf;

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

fn parse_fixture(name: &str) -> serde_json::Value {
    let content = std::fs::read_to_string(fixture_path(name)).expect("read fixture");
    serde_json::from_str(&content).expect("valid JSON")
}

#[test]
fn fixture_is_valid_result_object() {
    let value = parse_fixture("droid_exec_scrutiny.json");
    assert_eq!(value["type"], "result");
    assert!(value["session_id"].is_string());
}

#[test]
fn fixture_has_token_usage() {
    let value = parse_fixture("droid_exec_scrutiny.json");
    assert!(value["usage"]["input_tokens"].is_number());
    assert!(value["usage"]["output_tokens"].is_number());
    assert!(value["usage"]["cache_read_input_tokens"].is_number());
}

#[test]
fn droid_fixture_result_is_a_valid_validator_report() {
    let value = parse_fixture("droid_exec_scrutiny.json");
    let result_text = value["result"].as_str().expect("result must be a string");

    let report =
        parse_validator_report(result_text).expect("result string must parse as a ValidatorReport");
    assert!(!report.findings.is_empty());
    assert!(!report.summary.is_empty());
    for finding in &report.findings {
        assert!(!finding.subject.is_empty());
        assert!(["critical", "major", "minor"].contains(&finding.severity.as_str()));
        assert!(!finding.evidence.is_empty());
    }
}

#[test]
fn no_report_fixture_is_valid_result_object() {
    let value = parse_fixture("droid_exec_scrutiny_no_report.json");
    assert_eq!(value["type"], "result");
    assert!(value["session_id"].is_string());
    assert!(value["usage"]["input_tokens"].is_number());
    assert!(value["usage"]["output_tokens"].is_number());
    assert!(value["usage"]["cache_read_input_tokens"].is_number());
}

#[test]
fn no_report_fixture_result_does_not_parse_as_validator_report() {
    let value = parse_fixture("droid_exec_scrutiny_no_report.json");
    let result_text = value["result"].as_str().expect("result must be a string");
    assert_eq!(result_text, "");
    assert!(parse_validator_report(result_text).is_none());
}
