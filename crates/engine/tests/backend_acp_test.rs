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
        hook_status: None,
    }
}

#[tokio::test]
async fn acp_containment_v1_uncertified_sandbox_is_refused_before_peer_spawn() {
    use kranz_engine::sandbox::{ResolvedSandbox, SandboxBackend, SandboxInputs};
    use kranz_engine::types::SandboxEnforce;
    let dir = tempfile::tempdir().unwrap();
    let peer = write_peer(
        dir.path(),
        "must-not-run.sh",
        "#!/bin/sh\ntouch spawned\nexit 1\n",
    );
    for backend in [
        SandboxBackend::Seatbelt,
        SandboxBackend::Bubblewrap,
        SandboxBackend::Container,
        SandboxBackend::AppContainer,
    ] {
        let mut spec = spec(dir.path(), "uncertified-containment", true, &[]);
        spec.sandbox = Some(ResolvedSandbox {
            backend,
            inputs: SandboxInputs {
                enforce: SandboxEnforce::FsNet,
                session_cwd: dir.path().into(),
                mission_dir: dir.path().join(".kranz/missions/m-test"),
                tmpdir: dir.path().join("scratch"),
                extra_write: vec![],
                egress: vec![],
                validator_read_deny_roots: vec![],
            },
            container: None,
        });
        let error = match AcpBackend::new(&peer, vec![]).start(spec).await {
            Ok(_) => panic!("an uncertified {backend:?} sandbox was admitted"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("refusing the supplied sandbox before spawn"),
            "{error}"
        );
        assert!(
            !dir.path().join("spawned").exists(),
            "{backend:?} executed outside its promised boundary"
        );
    }
}

// Transport-only fixtures act as an explicit test broker. Engine durability
// and operator authority are exercised separately by the live-consent tests.
async fn next(session: &mut Box<dyn AgentSession>) -> Option<AgentEvent> {
    let event = session
        .next_event()
        .await
        .expect("next_event should not error");
    if let Some(AgentEvent::PermissionRequested { proposal, .. }) = &event {
        session
            .permission_responder()
            .unwrap()
            .respond(proposal, proposal.prohibition.is_none())
            .unwrap();
    }
    event
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
    // Both the threaded second consumer and the real backend consume this
    // fixture. The original normalized-event assertions below stay unchanged.
    body.push_str("      cat <<'ACP_TURN'\n");
    body.push_str(kranz_acp::conformance::TURN);
    body.push_str("ACP_TURN\n      exit 0\n      ;;\n");
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

    // This peer reports no model. Configured attribution is not confirmation.
    match next(&mut session).await.expect("init") {
        AgentEvent::Init {
            session_id, model, ..
        } => {
            assert_eq!(session_id, "acp-mock-session-1");
            assert_eq!(model, "unreported");
        }
        other => panic!("expected Init, got {other:?}"),
    }
    assert_eq!(session.session_id(), "kranz-sess-1");

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
            AgentEvent::PermissionRequested { proposal, .. } => {
                if let Some(reason) = proposal.prohibition {
                    denial_summaries.push(reason);
                }
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
        denial_summaries[0].contains("Bash(git push*)"),
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
        if let AgentEvent::PermissionRequested { proposal, .. } = event {
            if let Some(reason) = proposal.prohibition {
                denial_summaries.push(reason);
            }
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
async fn backend_acp_torn_final_line_is_retained_as_a_failed_protocol_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let peer = write_peer(dir.path(), "mock-acp-die.sh", &torn_then_die_peer());
    let backend = AcpBackend::new(peer, vec![]);

    let mut session = backend
        .start(spec(dir.path(), "kranz-sess-6", true, &[]))
        .await
        .unwrap();
    let mut texts = Vec::new();
    let mut torn_receipt = false;
    // The malformed tail is retained, and the session fails closed.
    while let Some(event) = next(&mut session).await {
        match event {
            AgentEvent::Text { text, .. } => texts.push(text),
            AgentEvent::Other { raw } if raw.get("unparsed").is_some() => torn_receipt = true,
            _ => {}
        }
    }
    assert!(torn_receipt);
    assert_eq!(texts, vec!["partial work"]);
    match session.exit_status() {
        Some(SessionExit::Failed(msg)) => assert!(
            msg.contains("malformed JSON-RPC"),
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
        &[],
        None,
        None,
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

#[tokio::test]
async fn acp_compat_v1_reports_peer_model_and_preserves_role_prompt_and_engine_identity() {
    let dir = tempfile::tempdir().unwrap();
    let captured = dir.path().join("prompt.json");
    let body = full_session_peer()
        .replace(
            r#""result":{"sessionId":"acp-mock-session-1"}"#,
            r#""result":{"sessionId":"acp-mock-session-1","models":{"currentModelId":"legacy-label"},"configOptions":[{"id":"model","category":"model","currentValue":"peer-model"}]}"#,
        )
        .replace(
            "    *'\"method\":\"session/prompt\"'*)\n",
            "    *'\"method\":\"session/prompt\"'*)\n      printf '%s' \"$line\" > \"$KRANZ_ACP_PROMPT_CAPTURE\"\n",
        );
    let peer = write_peer(dir.path(), "model-and-prompt.sh", &body);
    let mut request = spec(dir.path(), "engine-identity", true, &[]);
    request.append_system_prompt = Some("Existing worker role instructions.".into());
    request.env.insert(
        "KRANZ_ACP_PROMPT_CAPTURE".into(),
        captured.display().to_string(),
    );
    let mut session = AcpBackend::new(peer, vec![]).start(request).await.unwrap();
    match next(&mut session).await.unwrap() {
        AgentEvent::Init {
            session_id,
            model,
            raw,
        } => {
            assert_eq!(session_id, "acp-mock-session-1");
            assert_eq!(model, "peer-model");
            assert_eq!(raw["engineSessionId"], "engine-identity");
            assert_eq!(raw["configuredModel"], "acp-configured-model");
            assert_eq!(raw["modelSource"], "peer");
            assert_eq!(raw["configuredModelSelectionApplied"], false);
        }
        event => panic!("expected Init, got {event:?}"),
    }
    assert_eq!(session.session_id(), "engine-identity");
    while next(&mut session).await.is_some() {}
    let sent: serde_json::Value =
        serde_json::from_slice(&std::fs::read(captured).unwrap()).unwrap();
    assert_eq!(
        sent["params"]["prompt"][0]["text"],
        "Existing worker role instructions.\n\ndo the thing"
    );
    assert_eq!(session.exit_status(), Some(SessionExit::Completed));
}

#[tokio::test]
async fn acp_compat_v1_foreign_updates_cannot_supply_the_worker_report() {
    let dir = tempfile::tempdir().unwrap();
    let body = full_session_peer().replace(
        r#""params":{"sessionId":"acp-mock-session-1""#,
        r#""params":{"sessionId":"another-session""#,
    );
    let peer = write_peer(dir.path(), "foreign-update.sh", &body);
    let mut session = AcpBackend::new(peer, vec![])
        .start(spec(dir.path(), "engine-foreign", true, &[]))
        .await
        .unwrap();
    while let Some(event) = next(&mut session).await {
        assert!(!matches!(
            event,
            AgentEvent::Text { .. } | AgentEvent::Result { .. }
        ));
    }
    assert!(
        matches!(session.exit_status(), Some(SessionExit::Failed(reason)) if reason.contains("sessionId"))
    );
}

#[tokio::test]
async fn acp_compat_v1_changed_permission_input_rechecks_the_current_command() {
    let dir = tempfile::tempdir().unwrap();
    let captured = dir.path().join("permission-outcome");
    // The announced call was harmless; the permission request changes its raw
    // command while reusing the same tool ID. Only the mock writes the receipt.
    let body = permission_peer("execute", "git push origin main").replacen(
        r#""rawInput":{"command":"git push origin main"}"#,
        r#""rawInput":{"command":"cargo test --workspace"}"#,
        1,
    );
    let peer = write_peer(dir.path(), "changed-permission.sh", &body);
    let mut request = spec(dir.path(), "engine-permission", true, &["Bash(git push*)"]);
    request.env.insert(
        "KRANZ_ACP_PEER_OUTCOME".into(),
        captured.display().to_string(),
    );
    let mut session = AcpBackend::new(peer, vec![]).start(request).await.unwrap();
    let mut denied = false;
    while let Some(event) = next(&mut session).await {
        if matches!(event, AgentEvent::PermissionRequested { proposal, .. } if proposal.prohibition.is_some())
        {
            denied = true;
        }
    }
    assert!(denied);
    assert_eq!(std::fs::read_to_string(captured).unwrap().trim(), "reject");
}

#[tokio::test]
async fn acp_compat_v1_stalled_prompt_stdin_has_a_bounded_failure() {
    let dir = tempfile::tempdir().unwrap();
    // Reply to session/new, then stop reading before the much larger prompt.
    let body = r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
IFS= read -r new_session
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"acp-mock-session-1"}}'
sleep 300
"#;
    let peer = write_peer(dir.path(), "stalled-stdin.sh", body);
    let mut request = spec(dir.path(), "engine-stalled", true, &[]);
    request.prompt = PromptMode::SingleShot("x".repeat(1024 * 1024));
    let result = tokio::time::timeout(
        Duration::from_secs(6),
        AcpBackend::new(peer, vec![]).start(request),
    )
    .await
    .expect("a stalled write must not park backend start indefinitely");
    assert!(matches!(result, Err(error) if error.to_string().contains("stdin write timed out")));
}

async fn drain_bounded(session: &mut Box<dyn AgentSession>) -> Vec<AgentEvent> {
    tokio::time::timeout(Duration::from_secs(6), async {
        let mut events = Vec::new();
        while let Some(event) = next(session).await {
            events.push(event);
        }
        events
    })
    .await
    .expect("ACP session must finish within its cleanup bound")
}

fn process_running(pid: i32) -> bool {
    #[cfg(target_os = "linux")]
    if let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) {
        if status
            .lines()
            .any(|line| line.starts_with("State:") && line.contains('Z'))
        {
            return false;
        }
    }
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test]
async fn acp_compat_v1_completion_and_peer_death_kill_same_group_descendants() {
    // Exercise all three lifecycle paths, including a descendant that keeps
    // stdout/stderr open after its parent has already exited.
    for scenario in ["daemon", "peer-death", "closed-stdout"] {
        let dir = tempfile::tempdir().unwrap();
        let mut body = no_usage_peer().replace(
            "    *'\"method\":\"session/prompt\"'*)\n",
            "    *'\"method\":\"session/prompt\"'*)\n      sleep 300 &\n      echo $! > descendant.pid\n",
        );
        body = match scenario {
            "daemon" => body.replace("      exit 0\n", "      sleep 300\n"),
            "peer-death" => body.replace(
                "      printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":'\"$id\"',\"result\":{\"stopReason\":\"end_turn\"}}'\n      exit 0\n",
                "      exit 3\n",
            ),
            _ => body.replace("      exit 0\n", "      exec 1>&-\n      sleep 300\n"),
        };
        let peer = write_peer(dir.path(), "descendants.sh", &body);
        let mut session = AcpBackend::new(peer, vec![])
            .start(spec(dir.path(), scenario, true, &[]))
            .await
            .unwrap();
        let events = drain_bounded(&mut session).await;
        let pid: i32 = std::fs::read_to_string(dir.path().join("descendant.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while process_running(pid) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("same-group descendant survived session completion");
        if scenario == "peer-death" {
            assert!(matches!(
                session.exit_status(),
                Some(SessionExit::Failed(_))
            ));
            assert!(!events
                .iter()
                .any(|event| matches!(event, AgentEvent::Result { .. })));
        } else {
            assert_eq!(session.exit_status(), Some(SessionExit::Completed));
            assert!(events.iter().any(|event| matches!(
                event,
                AgentEvent::Result {
                    is_error: false,
                    ..
                }
            )));
        }
    }
}

#[tokio::test]
async fn acp_compat_v1_nonzero_exit_after_report_is_not_hidden_by_completion() {
    let dir = tempfile::tempdir().unwrap();
    let peer = write_peer(
        dir.path(),
        "nonzero.sh",
        &no_usage_peer().replace("exit 0", "exit 23"),
    );
    let mut session = AcpBackend::new(peer, vec![])
        .start(spec(dir.path(), "nonzero", true, &[]))
        .await
        .unwrap();
    let events = drain_bounded(&mut session).await;
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::Result { .. })));
    assert!(
        matches!(session.exit_status(), Some(SessionExit::Failed(reason)) if reason.contains("23"))
    );
}

#[tokio::test]
async fn acp_compat_v1_malformed_frames_cannot_be_followed_by_a_successful_report() {
    for malformed in [
        r#"{"jsonrpc":"1.0","id":3,"result":{"stopReason":"end_turn"}}"#,
        r#"{"jsonrpc":"2.0","id":3,"result":{},"error":{}}"#,
        r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"cancelled","stopReason":"end_turn"}}"#,
        r#"{"jsonrpc":"2.0","id":3,"id":3,"result":{"stopReason":"end_turn"}}"#,
        "not JSON",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let body = no_usage_peer().replace(
            "    *'\"method\":\"session/prompt\"'*)\n",
            &format!(
                "    *'\"method\":\"session/prompt\"'*)\n      printf '%s\\n' '{malformed}'\n"
            ),
        );
        let peer = write_peer(dir.path(), "malformed.sh", &body);
        let mut session = AcpBackend::new(peer, vec![])
            .start(spec(dir.path(), "malformed", true, &[]))
            .await
            .unwrap();
        let events = drain_bounded(&mut session).await;
        assert!(
            matches!(session.exit_status(), Some(SessionExit::Failed(_))),
            "{malformed}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::Result { .. })),
            "{malformed}"
        );
        assert!(events
            .iter()
            .any(|event| matches!(event, AgentEvent::Other { .. })));
    }
}

#[tokio::test]
async fn acp_compat_v1_permission_requires_identity_and_a_valid_once_option() {
    use serde_json::json;
    let cases = [
        (
            "tc-1",
            json!([{"optionId":"persist","kind":"allow_always"}]),
            "cancelled",
        ),
        (
            "tc-1",
            json!([{"optionId":"unknown","kind":"surprise"}]),
            "cancelled",
        ),
        (
            "tc-1",
            json!([{"optionId":"","kind":"allow_once"}]),
            "cancelled",
        ),
        (
            "tc-1",
            json!([{"optionId":"same","kind":"allow_once"},{"optionId":"same","kind":"reject_once"}]),
            "cancelled",
        ),
        (
            "",
            json!([{"optionId":"opaque-allow","kind":"allow_once"}]),
            "cancelled",
        ),
        (
            "tc-1",
            json!([{"optionId":"opaque-adapter-id","kind":"allow_once"}]),
            "selected",
        ),
    ];
    for (action_id, options, expected) in cases {
        let dir = tempfile::tempdir().unwrap();
        let permission = json!({"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{
            "sessionId":"acp-mock-session-1","toolCall":{"toolCallId":action_id,"kind":"execute","rawInput":{"command":"cargo test"}},"options":options
        }});
        let body = format!("{PEER_PREAMBLE}    *'\"method\":\"session/prompt\"'*)\n      printf '%s\\n' '{permission}'\n      IFS= read -r answer\n      printf '%s' \"$answer\" > permission.json\n      printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{{\"stopReason\":\"end_turn\"}}}}'\n      exit 0\n      ;;\n{PEER_SUFFIX}");
        let peer = write_peer(dir.path(), "permission-options.sh", &body);
        let mut session = AcpBackend::new(peer, vec![])
            .start(spec(dir.path(), "options", true, &[]))
            .await
            .unwrap();
        let events = drain_bounded(&mut session).await;
        let answer: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("permission.json")).unwrap())
                .unwrap();
        assert_eq!(answer["result"]["outcome"]["outcome"], expected);
        assert_eq!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::PermissionRequested { proposal, .. } if proposal.prohibition.is_some())),
            expected == "cancelled"
        );
        if expected == "selected" {
            assert_eq!(answer["result"]["outcome"]["optionId"], "opaque-adapter-id");
        }
    }
}

#[tokio::test]
async fn acp_compat_v1_actual_worker_runner_parses_report_from_a_persistent_adapter() {
    let dir = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(dir.path(), "m-acp-report");
    let mut log = EventLog::acquire(&paths, "m-acp-report", Duration::ZERO, LockForce::No).unwrap();
    let feature = Feature {
        id: "f-1".into(),
        title: "Protocol fixture".into(),
        spec: "Return the protocol fixture".into(),
        validation_criteria: vec!["report parses".into()],
        origin: FeatureOrigin::Plan,
        status: FeatureStatus::Pending,
        worker_runs: vec![],
        commits: vec![],
        respawns: 0,
    };
    let update = serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":r#"{"result":"partial","summary":"kranz-acp-live-fixture-v1","knownGaps":["Protocol fixture only"],"escalation":null}"#}});
    let body = format!("{PEER_PREAMBLE}    *'\"method\":\"session/prompt\"'*)\n      printf '%s' \"$line\" > prompt.json\n{}      printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{{\"stopReason\":\"end_turn\"}}}}'\n      sleep 300\n      ;;\n{PEER_SUFFIX}", notification(&update.to_string()));
    let peer = write_peer(dir.path(), "runner-report.sh", &body);
    let outcome = tokio::time::timeout(
        Duration::from_secs(6),
        kranz_engine::runner::run_worker(
            &AcpBackend::new(peer, vec![]),
            &mut log,
            &paths,
            &MissionConfig::default(),
            &feature,
            "Protocol fixture",
            "Compatibility",
            None,
            None,
            None,
            &[],
            &[],
            &[],
            AuthVerdict::Inconclusive,
            &[],
            None,
            None,
        ),
    )
    .await
    .expect("worker runner must not wait for the adapter daemon to exit")
    .unwrap();
    assert_eq!(outcome.exit, SessionExit::Completed);
    assert_eq!(outcome.result, RunResult::Partial);
    assert_eq!(outcome.report.unwrap().summary, "kranz-acp-live-fixture-v1");
    assert_eq!(outcome.cost_usd, None);
    assert_ne!(outcome.session_id, "acp-mock-session-1");
    let prompt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("prompt.json")).unwrap()).unwrap();
    let text = prompt["params"]["prompt"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("filesTouched"),
        "the existing worker report instructions must reach ACP"
    );
}

#[tokio::test]
async fn acp_compat_v1_oversized_and_invalid_utf8_frames_fail_before_a_report() {
    for emit in [
        "dd if=/dev/zero bs=1048576 count=9 2>/dev/null | tr '\\000' x\nsleep 300",
        "printf '{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"acp-mock-session-1\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"\\377\"}}}}\\n'",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let body = no_usage_peer().replace("    *'\"method\":\"session/prompt\"'*)\n", &format!("    *'\"method\":\"session/prompt\"'*)\n{emit}\n"));
        let peer = write_peer(dir.path(), "bad-bytes.sh", &body);
        let mut session = AcpBackend::new(peer, vec![]).start(spec(dir.path(), "bad-bytes", true, &[])).await.unwrap();
        let events = drain_bounded(&mut session).await;
        assert!(matches!(session.exit_status(), Some(SessionExit::Failed(_))));
        assert!(!events.iter().any(|event| matches!(event, AgentEvent::Text { .. } | AgentEvent::Result { .. })));
    }
}

#[tokio::test]
async fn acp_compat_v1_handshake_rejects_version_and_missing_session_identity() {
    for (body, expected) in [
        (
            no_usage_peer().replace("\"protocolVersion\":1", "\"protocolVersion\":999"),
            "negotiated protocol version 999",
        ),
        (
            no_usage_peer().replace("\"sessionId\":\"acp-mock-session-1\"", "\"sessionId\":\"\""),
            "session/new response carried no sessionId",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let body = body.replace("#!/bin/sh\n", "#!/bin/sh\necho $$ > peer.pid\n");
        let peer = write_peer(dir.path(), "bad-handshake.sh", &body);
        // start() also prepares the isolated toolchain home and reaps the
        // rejected peer. The semantic rejection, not a three-second host I/O
        // benchmark, is this test's contract. A handshake timeout cannot pass.
        let result = tokio::time::timeout(
            Duration::from_secs(35),
            AcpBackend::new(peer, vec![]).start(spec(dir.path(), "handshake", true, &[])),
        )
        .await
        .expect("invalid handshake must reject and reap within its outer budget");
        let error = match result {
            Ok(_) => panic!("invalid handshake was accepted: {expected}"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains(expected), "wrong rejection: {error}");
        assert!(!error.contains("cleanup unconfirmed"), "{error}");
        let pid = std::fs::read_to_string(dir.path().join("peer.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "peer still exists");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH),
            "rejected peer must be reaped"
        );
    }
}

#[tokio::test]
async fn acp_compat_v1_handshake_timeout_reaps_the_unresponsive_peer() {
    let dir = tempfile::tempdir().unwrap();
    let peer = write_peer(
        dir.path(),
        "handshake-timeout.sh",
        "#!/bin/sh\necho $$ > peer.pid\nexec sleep 300\n",
    );
    let result = tokio::time::timeout(
        Duration::from_secs(35),
        AcpBackend::new(peer, vec![]).start(spec(dir.path(), "timeout", true, &[])),
    )
    .await
    .expect("handshake timeout must be bounded");
    assert!(matches!(result, Err(error) if error.to_string().contains("handshake timeout")));
    let pid = std::fs::read_to_string(dir.path().join("peer.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!process_running(pid));
}

#[tokio::test]
async fn acp_compat_v1_streaming_cost_uses_deltas_and_preserves_telemetry_gaps() {
    let dir = tempfile::tempdir().unwrap();
    let update = |amount| {
        notification(&format!(
            r#"{{"sessionUpdate":"usage_update","used":100,"size":200000,"cost":{{"currency":"USD","amount":{amount}}}}}"#
        ))
    };
    let body = format!("turn=0\n{PEER_PREAMBLE}").replace("turn=0\n#!/bin/sh", "#!/bin/sh\nturn=0") + &format!(
        "    *'\"method\":\"session/prompt\"'*)\n      turn=$((turn + 1))\n      case $turn in\n      1)\n{}      ;;\n      2)\n{}      ;;\n      4)\n{}      ;;\n      esac\n      printf '%s\\n' '{{\"jsonrpc\":\"2.0\",\"id\":'\"$id\"',\"result\":{{\"stopReason\":\"end_turn\"}}}}'\n      ;;\n{PEER_SUFFIX}", update(0.5), update(0.75), update(1.5));
    let peer = write_peer(dir.path(), "cost-deltas.sh", &body);
    let mut request = spec(dir.path(), "costs", true, &[]);
    request.prompt = PromptMode::Streaming("first".into());
    let mut session = AcpBackend::new(peer, vec![]).start(request).await.unwrap();
    for (turn, expected) in [Some(0.5), Some(0.25), None, None].into_iter().enumerate() {
        if turn > 0 {
            session.send_user_message("next").await.unwrap();
        }
        let cost = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(AgentEvent::Result { cost_usd, .. }) = next(&mut session).await {
                    break cost_usd;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(cost, expected);
    }
    session.abort().await.unwrap();
}

#[tokio::test]
async fn live_permission_pumps_output_before_consent_and_sends_once() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = dir.path().join("effect");
    let mut body = permission_peer("execute", "printf fixture");
    // A progress notification after the request must be readable before consent.
    let progress = notification(
        r#"{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"waiting for consent"}}"#,
    );
    // Insert directly after the permission request line, before the shell reads.
    let start = body.find("session/request_permission").unwrap();
    let needle = start + body[start..].find('\n').unwrap();
    body.insert_str(needle + 1, &progress);
    let peer = write_peer(dir.path(), "live-consent.sh", &body);
    let mut request = spec(dir.path(), "live-permission-session", true, &[]);
    request.env.insert(
        "KRANZ_ACP_PEER_OUTCOME".into(),
        outcome.display().to_string(),
    );
    let mut session = AcpBackend::new(peer, vec![]).start(request).await.unwrap();
    let mut proposal = None;
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(event) = session.next_event().await.unwrap() {
            match event {
                AgentEvent::PermissionRequested { proposal: p, .. } => proposal = Some(p),
                AgentEvent::Text { text, .. } if text == "waiting for consent" => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(!outcome.exists(), "no effect before consent");
    let proposal = proposal.expect("live request delivered");
    session
        .permission_responder()
        .unwrap()
        .respond(&proposal, true)
        .unwrap();
    let events = drain_bounded(&mut session).await;
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::PermissionResponded {
                    delivery: kranz_engine::live_permission::Delivery::Sent,
                    ..
                }
            ))
            .count(),
        1
    );
    assert_eq!(std::fs::read_to_string(outcome).unwrap().trim(), "allow");
}

#[tokio::test]
async fn live_permission_changed_action_cannot_use_a_queued_approval() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = dir.path().join("effect");
    let ready = dir.path().join("fragment-written");
    let release = dir.path().join("finish-frame");
    let mut body = permission_peer("execute", "printf fixture");
    let changed = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-mock-session-1","update":{"sessionUpdate":"tool_call_update","toolCallId":"tc-1","rawInput":{"command":"git push origin main"}}}}"#;
    let (prefix, suffix) = changed.split_at(changed.len() / 2);
    let fragmented = format!(
        r#"      printf '%s' '{prefix}'
      printf ready > "$KRANZ_ACP_PEER_READY"
      while [ ! -f "$KRANZ_ACP_PEER_RELEASE" ]; do sleep 0.01; done
      printf '%s\n' '{suffix}'
"#
    );
    let start = body.find("session/request_permission").unwrap();
    let needle = start + body[start..].find('\n').unwrap();
    body.insert_str(needle + 1, &fragmented);
    let peer = write_peer(dir.path(), "changed-live-consent.sh", &body);
    let mut request = spec(dir.path(), "live-permission-changed", true, &[]);
    for (name, path) in [
        ("KRANZ_ACP_PEER_OUTCOME", &outcome),
        ("KRANZ_ACP_PEER_READY", &ready),
        ("KRANZ_ACP_PEER_RELEASE", &release),
    ] {
        request.env.insert(name.into(), path.display().to_string());
    }
    let mut session = AcpBackend::new(peer, vec![]).start(request).await.unwrap();
    let proposal = loop {
        if let Some(AgentEvent::PermissionRequested { proposal, .. }) =
            session.next_event().await.unwrap()
        {
            break proposal;
        }
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // No complete notification exists yet. Cancellation must preserve the
    // fragment, and consent cannot overtake it while the peer holds the rest.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), session.next_event())
            .await
            .is_err()
    );
    session
        .permission_responder()
        .unwrap()
        .respond(&proposal, true)
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), session.next_event())
            .await
            .is_err()
    );
    assert!(!outcome.exists());
    std::fs::write(release, "continue").unwrap();
    drain_bounded(&mut session).await;
    assert!(matches!(
        session.exit_status(),
        Some(SessionExit::Failed(_))
    ));
    assert!(!outcome.exists());
}

#[tokio::test]
async fn live_permission_cancel_with_an_unanswered_request_does_not_deadlock() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = dir.path().join("effect");
    let peer = write_peer(
        dir.path(),
        "cancel-live-consent.sh",
        &permission_peer("execute", "printf fixture"),
    );
    let mut request = spec(dir.path(), "live-permission-cancel", true, &[]);
    request.env.insert(
        "KRANZ_ACP_PEER_OUTCOME".into(),
        outcome.display().to_string(),
    );
    let mut session = AcpBackend::new(peer, vec![]).start(request).await.unwrap();
    while let Some(event) = session.next_event().await.unwrap() {
        if matches!(event, AgentEvent::PermissionRequested { .. }) {
            break;
        }
    }
    tokio::time::timeout(Duration::from_secs(3), session.abort())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.exit_status(), Some(SessionExit::Aborted));
    assert!(!outcome.exists());
}

#[tokio::test]
async fn live_permission_unanswered_cannot_complete_without_a_terminal_provider() {
    let dir = tempfile::tempdir().unwrap();
    let body = format!(
        r#"{PEER_PREAMBLE}
    *'"method":"session/prompt"'*)
      printf '%s\n' '{{"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{{"sessionId":"acp-mock-session-1","toolCall":{{"toolCallId":"tc-1","kind":"execute","rawInput":{{"command":"npm test"}}}},"options":[{{"optionId":"allow","kind":"allow_once"}}]}}}}'
      printf '%s\n' '{{"jsonrpc":"2.0","id":3,"result":{{"stopReason":"end_turn"}}}}'
      exit 0
      ;;
{PEER_SUFFIX}"#
    );
    let peer = write_peer(dir.path(), "unanswered.sh", &body);
    let mut session = AcpBackend::new(peer, vec![])
        .start(spec(dir.path(), "unanswered", true, &[]))
        .await
        .unwrap();
    let mut requests = 0;
    while let Some(event) = session.next_event().await.unwrap() {
        if matches!(event, AgentEvent::PermissionRequested { .. }) {
            requests += 1;
        }
    }
    assert_eq!(requests, 1);
    assert!(
        matches!(session.exit_status(), Some(SessionExit::Failed(reason)) if reason.contains("unanswered"))
    );
}
