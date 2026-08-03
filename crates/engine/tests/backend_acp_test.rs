//! Integration tests for `backend_acp`: a mock ACP peer (POSIX shell script
//! speaking JSON-RPC 2.0 over NDJSON stdio) drives the real
//! [`AcpBackend`] through full sessions — spawn → initialize → session/new →
//! prompt → tool calls → updates → result → exit — plus the
//! kill-mid-session, cost/model-absent, and permission-refusal cases
//! (KRZ-301 acceptance).
//!
//! `cfg(unix)` throughout: the peer is a `/bin/sh` script, matching the
//! house stub idiom in `backend_claude_test.rs` (`write_script`).

#![cfg(unix)]

use kranz_engine::auth_verify::AuthVerdict;
use kranz_engine::backend::{
    AgentBackend, AgentEvent, AgentSession, PromptMode, SessionExit, SessionSpec,
};
use kranz_engine::backend_acp::AcpBackend;
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::paths::MissionPaths;
use kranz_engine::types::{
    Feature, FeatureOrigin, FeatureStatus, Milestone, MilestoneStatus, MissionConfig, Role,
    RunResult, TokenUsage,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Write an executable mock-peer script and return its path.
fn write_peer(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write peer script");
    let mut perms = std::fs::metadata(&path)
        .expect("script metadata")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).expect("chmod script");
    path
}

fn spec(dir: &Path, session_id: &str, writable: bool, disallowed: &[&str]) -> SessionSpec {
    SessionSpec {
        cwd: dir.to_path_buf(),
        prompt: PromptMode::SingleShot("do the thing".to_string()),
        append_system_prompt: None,
        model: "acp-configured-model".to_string(),
        effort: "high".to_string(),
        session_id: session_id.to_string(),
        resume: None,
        permission_mode: None,
        allowed_tools: vec![],
        disallowed_tools: disallowed.iter().map(|s| s.to_string()).collect(),
        tools: vec![],
        writable,
        settings_json: None,
        json_schema: None,
        max_budget_usd: None,
        max_turns: None,
        env: HashMap::new(),
        sandbox: None,
    }
}

async fn next(session: &mut Box<dyn AgentSession>) -> Option<AgentEvent> {
    session
        .next_event()
        .await
        .expect("next_event should not error")
}

/// The mock-peer preamble: a read loop that answers `initialize` and
/// `session/new`; the scenario body handles `session/prompt` (and any
/// permission-response line). `$id` is the client request id of the current
/// line; client ids are small integers serialized first by serde_json.
const PEER_PREAMBLE: &str = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":false},"authMethods":[],"agentInfo":{"name":"mock-acp-peer","version":"0.0.0-test"}}}'
      ;;
    *'"method":"session/new"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":'"$id"',"result":{"sessionId":"acp-mock-session-1"}}'
      ;;
"#;

const PEER_SUFFIX: &str = r#"  esac
done
"#;

fn notification(update: &str) -> String {
    format!(
        "      printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{{\"sessionId\":\"acp-mock-session-1\",\"update\":{update}}}}}'\n"
    )
}

/// Full happy-path session: two message chunks, one tool call with a
/// completed update, a USD usage_update, end_turn, exit 0.
fn full_session_peer() -> String {
    let mut body = String::from(PEER_PREAMBLE);
    body.push_str("    *'\"method\":\"session/prompt\"'*)\n");
    body.push_str(&notification("{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"Hello \"},\"messageId\":\"m1\"}"));
    body.push_str(&notification("{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"world\"},\"messageId\":\"m1\"}"));
    body.push_str(&notification("{\"sessionUpdate\":\"tool_call\",\"toolCallId\":\"tc-1\",\"title\":\"cargo test --workspace\",\"kind\":\"execute\",\"status\":\"in_progress\",\"rawInput\":{\"command\":\"cargo test --workspace\"}}"));
    body.push_str(&notification("{\"sessionUpdate\":\"tool_call_update\",\"toolCallId\":\"tc-1\",\"status\":\"completed\",\"content\":[{\"type\":\"content\",\"content\":{\"type\":\"text\",\"text\":\"test result: ok. 5 passed\"}}]}"));
    body.push_str(&notification("{\"sessionUpdate\":\"usage_update\",\"used\":12345,\"size\":200000,\"cost\":{\"amount\":0.042,\"currency\":\"USD\"}}"));
    body.push_str(
        "      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":'\"$id\"',\"result\":{\"stopReason\":\"end_turn\"}}'\n      exit 0\n      ;;\n",
    );
    body.push_str(PEER_SUFFIX);
    body
}

/// Same shape but no `usage_update` at all: cost/usage must stay absent.
fn no_usage_peer() -> String {
    let mut body = String::from(PEER_PREAMBLE);
    body.push_str("    *'\"method\":\"session/prompt\"'*)\n");
    body.push_str(&notification("{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"done\"},\"messageId\":\"m1\"}"));
    body.push_str(
        "      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":'\"$id\"',\"result\":{\"stopReason\":\"end_turn\"}}'\n      exit 0\n      ;;\n",
    );
    body.push_str(PEER_SUFFIX);
    body
}

/// Streams one chunk, then a TORN line (no newline) and sleeps: the
/// kill-mid-session scenario.
fn torn_then_hang_peer() -> String {
    let mut body = String::from(PEER_PREAMBLE);
    body.push_str("    *'\"method\":\"session/prompt\"'*)\n");
    body.push_str(&notification("{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"partial work\"},\"messageId\":\"m1\"}"));
    body.push_str(
        "      printf '%s' '{\"jsonrpc\":\"2.0\",\"method\":\"session/upda'\n      sleep 300\n      ;;\n",
    );
    body.push_str(PEER_SUFFIX);
    body
}

/// Streams one chunk and a torn line, then DIES mid-line (exit 3): the
/// peer-death variant of the torn-line case.
fn torn_then_die_peer() -> String {
    let mut body = String::from(PEER_PREAMBLE);
    body.push_str("    *'\"method\":\"session/prompt\"'*)\n");
    body.push_str(&notification("{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"partial work\"},\"messageId\":\"m1\"}"));
    body.push_str(
        "      printf '%s' '{\"jsonrpc\":\"2.0\",\"method\":\"session/upda'\n      exit 3\n      ;;\n",
    );
    body.push_str(PEER_SUFFIX);
    body
}

/// On prompt: report a tool call, ask permission, branch on the client's
/// answer (recorded to `$KRANZ_ACP_PEER_OUTCOME`), then end the turn.
/// `kind`/`title` parameterize the requested tool call.
fn permission_peer(kind: &str, title: &str) -> String {
    let mut body = String::from(PEER_PREAMBLE);
    body.push_str("    *'\"method\":\"session/prompt\"'*)\n");
    body.push_str("      prompt_id=$id\n");
    body.push_str(&notification(&format!(
        "{{\"sessionUpdate\":\"tool_call\",\"toolCallId\":\"tc-1\",\"title\":\"{title}\",\"kind\":\"{kind}\",\"status\":\"pending\",\"rawInput\":{{\"command\":\"{title}\"}}}}"
    )));
    body.push_str(&format!(
        "      printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":100,\"method\":\"session/request_permission\",\"params\":{{\"sessionId\":\"acp-mock-session-1\",\"toolCall\":{{\"toolCallId\":\"tc-1\",\"kind\":\"{kind}\",\"title\":\"{title}\",\"rawInput\":{{\"command\":\"{title}\"}}}},\"options\":[{{\"optionId\":\"allow-1\",\"name\":\"Allow once\",\"kind\":\"allow_once\"}},{{\"optionId\":\"reject-1\",\"name\":\"Reject once\",\"kind\":\"reject_once\"}}]}}}}'\n"
    ));
    body.push_str("      ;;\n");
    // The permission response (no "method") decides the branch.
    body.push_str("    *'\"optionId\"'*)\n");
    body.push_str("      case \"$line\" in\n");
    body.push_str("        *reject-1*) echo reject > \"$KRANZ_ACP_PEER_OUTCOME\" ;;\n");
    body.push_str("        *) echo allow > \"$KRANZ_ACP_PEER_OUTCOME\" ;;\n");
    body.push_str("      esac\n");
    body.push_str(&notification("{\"sessionUpdate\":\"tool_call_update\",\"toolCallId\":\"tc-1\",\"status\":\"failed\",\"title\":\"call did not run\"}"));
    body.push_str(
        "      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":'\"$prompt_id\"',\"result\":{\"stopReason\":\"end_turn\"}}'\n      exit 0\n      ;;\n",
    );
    body.push_str(PEER_SUFFIX);
    body
}

#[tokio::test]
async fn backend_acp_full_session_streams_ordered_events() {
    let dir = tempfile::tempdir().unwrap();
    let peer = write_peer(dir.path(), "mock-acp.sh", &full_session_peer());
    let backend = AcpBackend::new(peer, vec![]);

    let mut session = backend
        .start(spec(dir.path(), "kranz-sess-1", true, &[]))
        .await
        .expect("handshake should succeed");

    // Init is synthesized from the handshake with the PEER's session id and
    // the CONFIGURED model (ACP v1 reports no model on the wire).
    match next(&mut session).await.expect("init") {
        AgentEvent::Init {
            session_id, model, ..
        } => {
            assert_eq!(session_id, "acp-mock-session-1");
            assert_eq!(model, "acp-configured-model");
        }
        other => panic!("expected Init, got {other:?}"),
    }
    assert_eq!(session.session_id(), "acp-mock-session-1");

    match next(&mut session).await.expect("chunk 1") {
        AgentEvent::Text { text, .. } => assert_eq!(text, "Hello "),
        other => panic!("expected Text, got {other:?}"),
    }
    match next(&mut session).await.expect("chunk 2") {
        AgentEvent::Text { text, .. } => assert_eq!(text, "world"),
        other => panic!("expected Text, got {other:?}"),
    }
    // Tool calls are first-class events, not transcript text.
    match next(&mut session).await.expect("tool use") {
        AgentEvent::ToolUse { tool, summary, .. } => {
            assert_eq!(tool, "execute");
            assert_eq!(summary, "cargo test --workspace");
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
    match next(&mut session).await.expect("tool result") {
        AgentEvent::ToolResult {
            tool,
            denied,
            summary,
            ..
        } => {
            assert_eq!(tool.as_deref(), Some("execute"));
            assert!(!denied, "a completed tool call is not a denial");
            assert_eq!(summary, "test result: ok. 5 passed");
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
    // usage_update rides the transcript as Other (no mid-stream usage event).
    assert!(matches!(
        next(&mut session).await.expect("usage transcript"),
        AgentEvent::Other { .. }
    ));
    match next(&mut session).await.expect("terminal result") {
        AgentEvent::Result {
            text,
            is_error,
            usage,
            cost_usd,
            num_turns,
            ..
        } => {
            assert_eq!(text, "Hello world", "chunks of one message stitch");
            assert!(!is_error);
            // ACP v1 reports no input/output split: absent stays absent.
            assert_eq!(usage, TokenUsage::default());
            // The peer's USD cost is captured verbatim.
            assert_eq!(cost_usd, Some(0.042));
            assert_eq!(num_turns, Some(1));
        }
        other => panic!("expected Result, got {other:?}"),
    }

    assert!(next(&mut session).await.is_none());
    assert_eq!(session.exit_status(), Some(SessionExit::Completed));
}

#[tokio::test]
async fn backend_acp_absent_cost_and_usage_stay_absent() {
    let dir = tempfile::tempdir().unwrap();
    let peer = write_peer(dir.path(), "mock-acp-no-usage.sh", &no_usage_peer());
    let backend = AcpBackend::new(peer, vec![]);

    let mut session = backend
        .start(spec(dir.path(), "kranz-sess-2", true, &[]))
        .await
        .unwrap();
    let mut result = None;
    while let Some(event) = next(&mut session).await {
        if matches!(event, AgentEvent::Result { .. }) {
            result = Some(event);
        }
    }
    match result.expect("terminal result") {
        AgentEvent::Result {
            usage, cost_usd, ..
        } => {
            assert_eq!(usage, TokenUsage::default(), "usage is never fabricated");
            assert_eq!(
                cost_usd, None,
                "unreported cost stays absent — no client-side price-table guess"
            );
        }
        other => panic!("expected Result, got {other:?}"),
    }
    assert_eq!(session.exit_status(), Some(SessionExit::Completed));
}

#[tokio::test]
async fn backend_acp_disallowed_tool_is_refused_at_the_seam() {
    let dir = tempfile::tempdir().unwrap();
    let outcome_path = dir.path().join("peer-outcome.txt");
    let peer = write_peer(
        dir.path(),
        "mock-acp-permission.sh",
        &permission_peer("execute", "git push origin main"),
    );
    let backend = AcpBackend::new(peer, vec![]);

    let mut spec = spec(dir.path(), "kranz-sess-3", true, &["Bash(git push*)"]);
    // spec.env entries cross into the (cleared) child env verbatim — that is
    // how the test reads back which branch the peer took.
    spec.env.insert(
        "KRANZ_ACP_PEER_OUTCOME".to_string(),
        outcome_path.display().to_string(),
    );
    let mut session = backend.start(spec).await.unwrap();

    let mut saw_tool_use = false;
    let mut denial_summaries = Vec::new();
    while let Some(event) = next(&mut session).await {
        match event {
            AgentEvent::ToolUse { tool, .. } => {
                assert_eq!(tool, "execute");
                saw_tool_use = true;
            }
            AgentEvent::ToolResult {
                denied: true,
                summary,
                ..
            } => {
                denial_summaries.push(summary);
            }
            _ => {}
        }
    }
    assert!(
        saw_tool_use,
        "the tool call must surface as a ToolUse event"
    );
    assert_eq!(
        denial_summaries.len(),
        1,
        "exactly one synthesized denial event is expected: {denial_summaries:?}"
    );
    assert!(
        denial_summaries[0].contains("refused by kranz permission seam")
            && denial_summaries[0].contains("Bash(git push*)"),
        "the denial event names the seam and the rule: {}",
        denial_summaries[0]
    );
    assert_eq!(
        std::fs::read_to_string(&outcome_path).unwrap().trim(),
        "reject",
        "the peer must have been answered with the reject option"
    );
    assert_eq!(session.exit_status(), Some(SessionExit::Completed));
}

#[tokio::test]
async fn backend_acp_read_only_session_refuses_mutating_kinds() {
    let dir = tempfile::tempdir().unwrap();
    let outcome_path = dir.path().join("peer-outcome.txt");
    let peer = write_peer(
        dir.path(),
        "mock-acp-permission.sh",
        &permission_peer("edit", "write src/main.rs"),
    );
    let backend = AcpBackend::new(peer, vec![]);

    let mut spec = spec(dir.path(), "kranz-sess-4", false, &[]);
    spec.env.insert(
        "KRANZ_ACP_PEER_OUTCOME".to_string(),
        outcome_path.display().to_string(),
    );
    let mut session = backend.start(spec).await.unwrap();

    let mut denial_summaries = Vec::new();
    while let Some(event) = next(&mut session).await {
        if let AgentEvent::ToolResult {
            denied: true,
            summary,
            ..
        } = event
        {
            denial_summaries.push(summary);
        }
    }
    assert_eq!(denial_summaries.len(), 1, "{denial_summaries:?}");
    assert!(
        denial_summaries[0].contains("writable: false"),
        "the read-only posture is the recorded reason: {}",
        denial_summaries[0]
    );
    assert_eq!(
        std::fs::read_to_string(&outcome_path).unwrap().trim(),
        "reject"
    );
}

#[tokio::test]
async fn backend_acp_kill_mid_session_leaves_a_clean_aborted_stream() {
    let dir = tempfile::tempdir().unwrap();
    let peer = write_peer(dir.path(), "mock-acp-hang.sh", &torn_then_hang_peer());
    let backend = AcpBackend::new(peer, vec![]);

    let mut session = backend
        .start(spec(dir.path(), "kranz-sess-5", true, &[]))
        .await
        .unwrap();
    assert!(matches!(
        next(&mut session).await,
        Some(AgentEvent::Init { .. })
    ));
    assert!(matches!(
        next(&mut session).await,
        Some(AgentEvent::Text { ref text, .. }) if text == "partial work"
    ));

    // Kill the peer mid-session (its torn partial line is still unflushed).
    // Bounded: abort joins the stderr task, which only finishes once every
    // pipe holder is dead.
    tokio::time::timeout(Duration::from_secs(10), session.abort())
        .await
        .expect("abort hung")
        .unwrap();
    assert_eq!(session.exit_status(), Some(SessionExit::Aborted));
    assert!(
        next(&mut session).await.is_none(),
        "stream closes after abort; the torn line is never a parse failure"
    );
}

#[tokio::test]
async fn backend_acp_torn_final_line_is_not_a_parse_failure() {
    let dir = tempfile::tempdir().unwrap();
    let peer = write_peer(dir.path(), "mock-acp-die.sh", &torn_then_die_peer());
    let backend = AcpBackend::new(peer, vec![]);

    let mut session = backend
        .start(spec(dir.path(), "kranz-sess-6", true, &[]))
        .await
        .unwrap();
    let mut texts = Vec::new();
    // Drain: next_event must never error on the torn tail — it surfaces at
    // most as an Other transcript entry.
    while let Some(event) = next(&mut session).await {
        if let AgentEvent::Text { text, .. } = event {
            texts.push(text);
        }
    }
    assert_eq!(texts, vec!["partial work"]);
    match session.exit_status() {
        Some(SessionExit::Failed(msg)) => assert!(
            msg.contains("without answering session/prompt"),
            "peer death mid-turn is an honest failure: {msg}"
        ),
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// Engine-level resumability: a worker run whose ACP peer dies mid-line
/// fails honestly, and the mission event log it was appending to re-acquires
/// with contiguous seqs (the torn-line repair path in `event_log.rs` is what
/// makes the NEXT acquire clean).
#[tokio::test]
async fn backend_acp_peer_death_mid_run_leaves_a_resumable_event_log() {
    let dir = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(dir.path(), "m-acp-test");
    let mut log = EventLog::acquire(
        &paths,
        "m-acp-test",
        Duration::from_millis(0),
        LockForce::No,
    )
    .unwrap();
    let cfg = MissionConfig::default();
    let feature = Feature {
        id: "f-1".to_string(),
        title: "Add login".to_string(),
        spec: "Build the login endpoint".to_string(),
        validation_criteria: vec!["users can log in".to_string()],
        origin: FeatureOrigin::Plan,
        status: FeatureStatus::Pending,
        worker_runs: vec![],
        commits: vec![],
        respawns: 0,
    };

    let peer = write_peer(dir.path(), "mock-acp-die.sh", &torn_then_die_peer());
    let backend = AcpBackend::new(peer, vec![]);
    let outcome = kranz_engine::runner::run_worker(
        &backend,
        &mut log,
        &paths,
        &cfg,
        &feature,
        "ship auth",
        "Auth",
        None,
        None,
        None,
        &[],
        &[],
        &[],
        AuthVerdict::Inconclusive,
    )
    .await
    .expect("run_worker returns the recorded outcome even for a failed run");
    assert!(
        matches!(outcome.result, kranz_engine::types::RunResult::Fail)
            && matches!(outcome.exit, kranz_engine::backend::SessionExit::Failed(_)),
        "a peer dying mid-turn must record an honest failure, got {:?} / {:?}",
        outcome.result,
        outcome.exit
    );
    drop(log);

    // Re-acquire (the resume path): seqs are contiguous from 1 and the log
    // folds without corruption.
    let log = EventLog::acquire(
        &paths,
        "m-acp-test",
        Duration::from_millis(0),
        LockForce::No,
    )
    .expect("log must re-acquire after a peer-death crash");
    let events = EventLog::read_events(log.events_path()).expect("log must read back");
    assert!(
        !events.is_empty(),
        "the failed run still left its audit trail"
    );
    for (idx, event) in events.iter().enumerate() {
        assert_eq!(
            event.seq as usize,
            idx + 1,
            "seqs must be contiguous after re-acquire"
        );
    }
}

/// Streams one chunk, then answers `session/prompt` with the given
/// `stopReason` (`None` = the field is omitted entirely) and exits 0: the
/// non-completing-turn scenarios (12th-pass review).
fn stop_reason_peer(reason: Option<&str>) -> String {
    let mut body = String::from(PEER_PREAMBLE);
    body.push_str("    *'\"method\":\"session/prompt\"'*)\n");
    body.push_str(&notification("{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"partial answer\"},\"messageId\":\"m1\"}"));
    let result = match reason {
        Some(reason) => format!("{{\"stopReason\":\"{reason}\"}}"),
        None => "{}".to_string(),
    };
    body.push_str(&format!(
        "      printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":'\"$id\"',\"result\":{result}}}'\n      exit 0\n      ;;\n"
    ));
    body.push_str(PEER_SUFFIX);
    body
}

/// Streams a COMPLETE validator report as one chunk, then ends the turn at
/// `max_tokens`: the truncated-turn validator scenario (12th-pass review).
fn truncated_validator_peer() -> String {
    let mut body = String::from(PEER_PREAMBLE);
    body.push_str("    *'\"method\":\"session/prompt\"'*)\n");
    body.push_str(&notification("{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"{\\\"findings\\\": [], \\\"summary\\\": \\\"everything holds\\\"}\"},\"messageId\":\"m1\"}"));
    body.push_str(
        "      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":'\"$id\"',\"result\":{\"stopReason\":\"max_tokens\"}}'\n      exit 0\n      ;;\n",
    );
    body.push_str(PEER_SUFFIX);
    body
}

/// 12th-pass review: ACP v1 names `end_turn` as the only natural completion.
/// `refusal`, the truncation reasons, `cancelled`, and any UNKNOWN reason
/// must all synthesize an error result — never a success — with the reason
/// carried in the raw payload so the failure is diagnosable.
#[tokio::test]
async fn backend_acp_acp_stop_reason_non_end_turn_reasons_fail_honestly() {
    for reason in [
        "refusal",
        "max_tokens",
        "max_turn_requests",
        "cancelled",
        "mystery-reason",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let peer = write_peer(
            dir.path(),
            "mock-acp-stop.sh",
            &stop_reason_peer(Some(reason)),
        );
        let backend = AcpBackend::new(peer, vec![]);

        let mut session = backend
            .start(spec(dir.path(), "kranz-sess-stop", true, &[]))
            .await
            .unwrap();
        let mut result = None;
        while let Some(event) = next(&mut session).await {
            if matches!(event, AgentEvent::Result { .. }) {
                result = Some(event);
            }
        }
        match result.expect("terminal result") {
            AgentEvent::Result {
                text,
                is_error,
                cost_usd,
                raw,
                ..
            } => {
                assert!(is_error, "stopReason {reason:?} must fail honestly");
                assert_eq!(text, "partial answer", "streamed text is still captured");
                assert_eq!(
                    raw["stopReason"], reason,
                    "the raw payload names the reason: {raw}"
                );
                assert_eq!(cost_usd, None, "cost capture is unchanged");
            }
            other => panic!("expected Result, got {other:?}"),
        }
        // The peer exited 0 after answering: a clean PROCESS exit whose
        // error RESULT the runner maps to an honest Fail.
        assert_eq!(session.exit_status(), Some(SessionExit::Completed));
    }
}

/// A missing `stopReason` is not a completion either: the result fails and
/// the raw payload shows the field was absent (`null`), not silently
/// treated as natural completion.
#[tokio::test]
async fn backend_acp_acp_stop_reason_absent_fails_honestly() {
    let dir = tempfile::tempdir().unwrap();
    let peer = write_peer(dir.path(), "mock-acp-no-reason.sh", &stop_reason_peer(None));
    let backend = AcpBackend::new(peer, vec![]);

    let mut session = backend
        .start(spec(dir.path(), "kranz-sess-noreason", true, &[]))
        .await
        .unwrap();
    let mut result = None;
    while let Some(event) = next(&mut session).await {
        if matches!(event, AgentEvent::Result { .. }) {
            result = Some(event);
        }
    }
    match result.expect("terminal result") {
        AgentEvent::Result { is_error, raw, .. } => {
            assert!(is_error, "a missing stopReason must fail honestly");
            assert!(
                raw["stopReason"].is_null(),
                "absent stays visibly absent in the raw payload: {raw}"
            );
        }
        other => panic!("expected Result, got {other:?}"),
    }
}

/// The regression the mapping fixes: a validator turn cut at `max_tokens`
/// AFTER streaming a parseable report must NOT pass validation — the report
/// parses, yet the run records an honest failure because the turn did not
/// complete naturally.
#[tokio::test]
async fn backend_acp_acp_stop_reason_truncated_turn_validator_cannot_pass() {
    let dir = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(dir.path(), "m-acp-stop");
    let mut log = EventLog::acquire(
        &paths,
        "m-acp-stop",
        Duration::from_millis(0),
        LockForce::No,
    )
    .unwrap();
    let cfg = MissionConfig::default();
    let milestone = Milestone {
        id: "ms-1".to_string(),
        title: "Auth".to_string(),
        features: vec![Feature {
            id: "f-1".to_string(),
            title: "Add login".to_string(),
            spec: "Build the login endpoint".to_string(),
            validation_criteria: vec!["users can log in".to_string()],
            origin: FeatureOrigin::Plan,
            status: FeatureStatus::Pending,
            worker_runs: vec![],
            commits: vec![],
            respawns: 0,
        }],
        status: MilestoneStatus::Validating,
        fix_cycles: 0,
        start_sha: Some("abc123".to_string()),
        validator_guidance: None,
    };

    let peer = write_peer(
        dir.path(),
        "mock-acp-truncated-validator.sh",
        &truncated_validator_peer(),
    );
    let backend = AcpBackend::new(peer, vec![]);
    let outcome = kranz_engine::runner::run_validator(
        &backend,
        &mut log,
        &paths,
        &cfg,
        Role::ValidatorScrutiny,
        &milestone,
        &[],
        "abc123",
        None,
        None,
        &[],
        &[],
        &[],
        None,
    )
    .await
    .expect("run_validator returns the recorded outcome even for a failed run");
    assert!(
        outcome.validator_report.is_some(),
        "the streamed report parses — the failure must come from the stop reason"
    );
    assert!(
        matches!(outcome.result, RunResult::Fail),
        "a max_tokens validator turn must fail honestly, got {:?}",
        outcome.result
    );
}
