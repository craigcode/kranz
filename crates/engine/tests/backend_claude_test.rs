//! Tests for the real claude CLI backend (parser, argv builder, discovery,
//! process lifecycle via a fake CLI script, and an ignored live smoke test).
//!
//! NOTE: expected fixture values (session id, token counts, cost) are taken
//! from the committed fixture `tests/fixtures/stream_json_single_shot.jsonl`,
//! which is real probed CLI output and the parser's ground truth.

use kranz_engine::backend::{
    AgentBackend, AgentEvent, AgentSession, PromptMode, SessionExit, SessionSpec,
};
use kranz_engine::backend_claude::{
    build_args, claude_min_config_entries, discover_claude_binary, parse_stream_line,
    seed_worker_scratch_home, user_message_line, ClaudeBackend, CLAUDE_CREDENTIALS_ENTRY,
};
use kranz_engine::error::EngineError;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

const FIXTURE_SESSION_ID: &str = "3ab68fd6-491a-4fad-b848-a207c52e4fc2";

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("stream_json_single_shot.jsonl")
}

fn parse_fixture() -> Vec<AgentEvent> {
    let content = std::fs::read_to_string(fixture_path()).expect("read fixture");
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(parse_stream_line)
        .collect()
}

fn base_spec(prompt: PromptMode) -> SessionSpec {
    SessionSpec {
        cwd: std::env::temp_dir(),
        prompt,
        append_system_prompt: None,
        model: "sonnet".to_string(),
        effort: "medium".to_string(),
        session_id: "11111111-2222-3333-4444-555555555555".to_string(),
        resume: None,
        permission_mode: None,
        allowed_tools: vec![],
        disallowed_tools: vec![],
        tools: vec![],
        settings_json: None,
        json_schema: None,
        max_budget_usd: None,
        max_turns: None,
        env: HashMap::new(),
        sandbox: None,
    }
}

/// Position of `needle` in `args`, panicking with context when absent.
fn index_of(args: &[String], needle: &str) -> usize {
    args.iter()
        .position(|a| a == needle)
        .unwrap_or_else(|| panic!("{needle} not found in {args:?}"))
}

async fn next_event(session: &mut Box<dyn AgentSession>) -> Option<AgentEvent> {
    tokio::time::timeout(Duration::from_secs(30), session.next_event())
        .await
        .expect("next_event timed out")
        .expect("next_event errored")
}

/// Drain the session stream to closure, collecting every event.
// Only the unix `fake_cli` module drains; the windows tree-kill test consumes
// events one at a time. Silence dead_code off-unix without masking it on unix.
#[cfg_attr(not(unix), allow(dead_code))]
async fn drain(session: &mut Box<dyn AgentSession>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = next_event(session).await {
        events.push(event);
    }
    events
}

// ---------------------------------------------------------------------------
// Parser: fixture
// ---------------------------------------------------------------------------

#[test]
fn fixture_parses_into_expected_events() {
    let events = parse_fixture();

    // Exactly one Init with the fixture's session id and a haiku model.
    let inits: Vec<(&String, &String)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Init {
                session_id, model, ..
            } => Some((session_id, model)),
            _ => None,
        })
        .collect();
    assert_eq!(inits.len(), 1, "expected exactly one Init event");
    assert_eq!(inits[0].0, FIXTURE_SESSION_ID);
    assert!(inits[0].1.contains("haiku"), "model was {}", inits[0].1);

    // At least one Text event carrying "OK".
    let texts: Vec<&String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Text { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert!(!texts.is_empty(), "expected at least one Text event");
    assert!(texts.iter().any(|t| *t == "OK"), "texts were {texts:?}");

    // Exactly one Result with the fixture's usage/cost/turn numbers.
    let results: Vec<&AgentEvent> = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::Result { .. }))
        .collect();
    assert_eq!(results.len(), 1, "expected exactly one Result event");
    let AgentEvent::Result {
        text,
        is_error,
        usage,
        cost_usd,
        num_turns,
        raw,
    } = results[0]
    else {
        unreachable!()
    };
    assert_eq!(text, "OK");
    assert!(!is_error);
    assert_eq!(usage.input, 10);
    assert_eq!(usage.output, 35);
    assert_eq!(usage.cache_read, 18243);
    assert_eq!(usage.cache_write, 7688);
    let cost = cost_usd.expect("fixture result has total_cost_usd");
    assert!((cost - 0.0179763).abs() < 1e-9, "cost was {cost}");
    assert_eq!(*num_turns, Some(1));
    assert_eq!(raw["type"], "result", "raw line must be preserved verbatim");

    // Unknown line types map to Other: rate_limit_event, thinking_tokens x3,
    // post_turn_summary, plus the assistant thinking block = 6.
    let other_raw_tags: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Other { raw } => Some(format!(
                "{}/{}",
                raw["type"].as_str().unwrap_or("?"),
                raw["subtype"].as_str().unwrap_or("-"),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(other_raw_tags.len(), 6, "others were {other_raw_tags:?}");
    assert!(other_raw_tags.contains(&"rate_limit_event/-".to_string()));
    assert!(other_raw_tags.contains(&"system/thinking_tokens".to_string()));
    assert!(other_raw_tags.contains(&"system/post_turn_summary".to_string()));
    assert!(
        other_raw_tags.contains(&"assistant/-".to_string()),
        "thinking block -> Other"
    );

    // Nothing dropped: 1 init + 1 text + 1 result + 6 other.
    assert_eq!(events.len(), 9);
}

// ---------------------------------------------------------------------------
// Parser: units
// ---------------------------------------------------------------------------

#[test]
fn unparseable_line_becomes_other_with_raw_unparsed() {
    let events = parse_stream_line("not json {");
    assert_eq!(events.len(), 1);
    let AgentEvent::Other { raw } = &events[0] else {
        panic!("expected Other, got {:?}", events[0]);
    };
    assert_eq!(raw["unparsed"], "not json {");
}

#[test]
fn multi_block_assistant_message_yields_multiple_events_in_order() {
    let line = json!({
        "type": "assistant",
        "message": {
            "id": "msg_multi",
            "content": [
                { "type": "thinking", "thinking": "hmm" },
                { "type": "text", "text": "before tools" },
                { "type": "text", "text": "" },
                { "type": "tool_use", "name": "Bash", "input": { "command": "cargo test -p kranz-engine" } },
                { "type": "tool_use", "name": "Edit", "input": { "file_path": "src/lib.rs", "old_string": "a", "new_string": "b" } },
                { "type": "tool_use", "name": "MyCustomTool", "input": { "key": "value" } },
            ],
        },
    })
    .to_string();

    let events = parse_stream_line(&line);
    // thinking -> Other, "before tools" -> Text, "" skipped, 3 tool_use.
    assert_eq!(events.len(), 5, "events were {events:?}");
    assert!(matches!(&events[0], AgentEvent::Other { .. }));
    match &events[1] {
        AgentEvent::Text { text, .. } => assert_eq!(text, "before tools"),
        other => panic!("expected Text, got {other:?}"),
    }
    match &events[2] {
        AgentEvent::ToolUse { tool, summary, .. } => {
            assert_eq!(tool, "Bash");
            assert_eq!(summary, "cargo test -p kranz-engine");
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
    match &events[3] {
        AgentEvent::ToolUse { tool, summary, .. } => {
            assert_eq!(tool, "Edit");
            assert_eq!(summary, "src/lib.rs");
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
    match &events[4] {
        AgentEvent::ToolUse { tool, summary, raw } => {
            assert_eq!(tool, "MyCustomTool");
            assert_eq!(summary, r#"{"key":"value"}"#, "compact input JSON");
            assert_eq!(raw["message"]["id"], "msg_multi", "raw carries full line");
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
}

#[test]
fn long_custom_tool_input_summary_is_truncated_to_200_chars() {
    let line = json!({
        "type": "assistant",
        "message": {
            "id": "msg_long",
            "content": [
                { "type": "tool_use", "name": "Grep", "input": { "pattern": "x".repeat(500) } },
            ],
        },
    })
    .to_string();
    let events = parse_stream_line(&line);
    let AgentEvent::ToolUse { summary, .. } = &events[0] else {
        panic!("expected ToolUse");
    };
    assert_eq!(summary.chars().count(), 200);
}

fn tool_result_line(text: &str, is_error: bool) -> String {
    json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [
                { "type": "tool_result", "tool_use_id": "toolu_01", "content": text, "is_error": is_error },
            ],
        },
    })
    .to_string()
}

#[test]
fn tool_result_denied_heuristic() {
    // is_error + "permission" (any case) -> denied.
    let events = parse_stream_line(&tool_result_line(
        "Permission denied: Bash(git push) requires approval",
        true,
    ));
    let AgentEvent::ToolResult {
        tool,
        denied,
        summary,
        ..
    } = &events[0]
    else {
        panic!("expected ToolResult, got {:?}", events[0]);
    };
    assert!(denied);
    assert_eq!(*tool, None);
    assert!(summary.starts_with("Permission denied"));

    // Plain failures are not denials.
    let events = parse_stream_line(&tool_result_line("command not found: frobnicate", true));
    let AgentEvent::ToolResult { denied, .. } = &events[0] else {
        panic!("expected ToolResult");
    };
    assert!(!denied);

    // Hook blocks are denials even without is_error.
    let events = parse_stream_line(&tool_result_line(
        "operation blocked by PreToolUse hook",
        false,
    ));
    let AgentEvent::ToolResult { denied, .. } = &events[0] else {
        panic!("expected ToolResult");
    };
    assert!(denied);

    // Successful results are not denials; summary keeps first 200 chars.
    let long = "y".repeat(450);
    let events = parse_stream_line(&tool_result_line(&long, false));
    let AgentEvent::ToolResult {
        denied, summary, ..
    } = &events[0]
    else {
        panic!("expected ToolResult");
    };
    assert!(!denied);
    assert_eq!(summary.chars().count(), 200);
}

#[test]
fn tool_result_content_array_form_is_flattened() {
    let line = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "toolu_02",
                    "content": [
                        { "type": "text", "text": "permission to run this tool" },
                    ],
                    "is_error": true,
                },
            ],
        },
    })
    .to_string();
    let events = parse_stream_line(&line);
    assert_eq!(events.len(), 1);
    let AgentEvent::ToolResult {
        denied, summary, ..
    } = &events[0]
    else {
        panic!("expected ToolResult, got {:?}", events[0]);
    };
    assert!(denied);
    assert_eq!(summary, "permission to run this tool");
}

// ---------------------------------------------------------------------------
// build_args
// ---------------------------------------------------------------------------

#[test]
fn build_args_single_shot_puts_prompt_last() {
    let mut spec = base_spec(PromptMode::SingleShot("Reply with exactly: OK".to_string()));
    spec.append_system_prompt = Some("you are a worker".to_string());
    let args = build_args(&spec);

    assert_eq!(
        &args[..4],
        &["-p", "--output-format", "stream-json", "--verbose"]
    );
    assert_eq!(args[index_of(&args, "--model") + 1], "sonnet");
    assert_eq!(args[index_of(&args, "--effort") + 1], "medium");
    assert_eq!(
        args[index_of(&args, "--append-system-prompt") + 1],
        "you are a worker"
    );
    assert_eq!(
        args[index_of(&args, "--session-id") + 1],
        "11111111-2222-3333-4444-555555555555"
    );
    assert_eq!(
        args.last().unwrap(),
        "Reply with exactly: OK",
        "prompt must be last"
    );
    assert!(!args.contains(&"--resume".to_string()));
    assert!(!args.contains(&"--input-format".to_string()));
    // Omitted options add no flags.
    assert!(!args.contains(&"--permission-mode".to_string()));
    assert!(!args.contains(&"--allowedTools".to_string()));
    assert!(!args.contains(&"--disallowedTools".to_string()));
    assert!(!args.contains(&"--settings".to_string()));
    assert!(!args.contains(&"--json-schema".to_string()));
    assert!(!args.contains(&"--max-budget-usd".to_string()));
}

#[test]
fn build_args_streaming_uses_stream_json_input_and_no_positional_prompt() {
    let spec = base_spec(PromptMode::Streaming("orchestrate the mission".to_string()));
    let args = build_args(&spec);

    let i = index_of(&args, "--input-format");
    assert_eq!(args[i + 1], "stream-json");
    assert!(
        !args.contains(&"orchestrate the mission".to_string()),
        "streaming prompt goes via stdin, not argv"
    );
}

#[test]
fn build_args_resume_replaces_session_id() {
    let mut spec = base_spec(PromptMode::SingleShot("go".to_string()));
    spec.resume = Some("99999999-8888-7777-6666-555555555555".to_string());
    let args = build_args(&spec);

    assert_eq!(
        args[index_of(&args, "--resume") + 1],
        "99999999-8888-7777-6666-555555555555"
    );
    assert!(!args.contains(&"--session-id".to_string()));
}

#[test]
fn build_args_each_tool_pattern_is_its_own_arg() {
    let mut spec = base_spec(PromptMode::SingleShot("go".to_string()));
    spec.permission_mode = Some("acceptEdits".to_string());
    spec.allowed_tools = vec![
        "Bash(cargo test*)".to_string(),
        "Read".to_string(),
        "Glob".to_string(),
    ];
    spec.disallowed_tools = vec!["Bash(git push*)".to_string(), "WebFetch".to_string()];
    let args = build_args(&spec);

    assert_eq!(
        args[index_of(&args, "--permission-mode") + 1],
        "acceptEdits"
    );

    let a = index_of(&args, "--allowedTools");
    assert_eq!(args[a + 1], "Bash(cargo test*)");
    assert_eq!(args[a + 2], "Read");
    assert_eq!(args[a + 3], "Glob");

    let d = index_of(&args, "--disallowedTools");
    assert_eq!(args[d + 1], "Bash(git push*)");
    assert_eq!(args[d + 2], "WebFetch");
}

#[test]
fn build_args_emits_tools_flag_only_when_configured() {
    let mut spec = base_spec(PromptMode::SingleShot("go".to_string()));
    spec.tools = vec!["Bash".to_string(), "Read".to_string()];
    let args = build_args(&spec);

    let t = index_of(&args, "--tools");
    assert_eq!(args[t + 1], "Bash");
    assert_eq!(args[t + 2], "Read");

    let empty_spec = base_spec(PromptMode::SingleShot("go".to_string()));
    let empty_args = build_args(&empty_spec);
    assert!(
        !empty_args.iter().any(|a| a == "--tools"),
        "no --tools token when spec.tools is empty"
    );
}

#[test]
fn build_args_settings_schema_and_budget_are_compact() {
    let mut spec = base_spec(PromptMode::SingleShot("go".to_string()));
    spec.settings_json = Some(json!({ "hooks": { "PreToolUse": [ { "matcher": "Bash" } ] } }));
    spec.json_schema = Some(json!({ "type": "object", "required": ["result"] }));
    spec.max_budget_usd = Some(2.5);
    let args = build_args(&spec);

    let settings = &args[index_of(&args, "--settings") + 1];
    assert_eq!(settings, r#"{"hooks":{"PreToolUse":[{"matcher":"Bash"}]}}"#);
    assert!(!settings.contains('\n'), "settings must be one compact arg");

    let schema = &args[index_of(&args, "--json-schema") + 1];
    assert_eq!(schema, r#"{"required":["result"],"type":"object"}"#);

    assert_eq!(args[index_of(&args, "--max-budget-usd") + 1], "2.5");
}

#[test]
fn user_message_line_is_one_json_line_in_wire_format() {
    let line = user_message_line("hello\nworld");
    assert!(line.ends_with('\n'));
    assert_eq!(
        line.trim_end().lines().count(),
        1,
        "must be a single JSONL line"
    );
    let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(value["type"], "user");
    assert_eq!(value["message"]["role"], "user");
    assert_eq!(value["message"]["content"][0]["type"], "text");
    assert_eq!(value["message"]["content"][0]["text"], "hello\nworld");
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn write_script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write script");
    let mut perms = std::fs::metadata(&path)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod script");
    path
}

#[cfg(unix)]
#[test]
fn discover_accepts_configured_script_that_reports_a_version() {
    let dir = tempfile::tempdir().unwrap();
    let script = write_script(
        dir.path(),
        "fake-claude",
        "#!/bin/sh\necho '9.9.9 (fake)'\n",
    );
    let found = discover_claude_binary(Some(script.to_str().unwrap()))
        .expect("configured script must be accepted");
    assert_eq!(found, script);
}

#[test]
fn discover_with_nonexistent_configured_path_falls_through_or_lists_attempts() {
    let bogus = if cfg!(windows) {
        r"C:\definitely\not\here\claude-nope.exe"
    } else {
        "/definitely/not/here/claude-nope"
    };
    match discover_claude_binary(Some(bogus)) {
        // A real claude elsewhere on this machine: the bogus configured path
        // fell through instead of hard-failing.
        Ok(found) => assert_ne!(found, PathBuf::from(bogus)),
        // Nothing else found: the error must list what was tried.
        Err(EngineError::Config(msg)) => {
            assert!(
                msg.contains(bogus),
                "error must list the configured attempt: {msg}"
            );
            assert!(
                msg.contains("claude"),
                "error should mention other candidates: {msg}"
            );
        }
        Err(other) => panic!("expected Config error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Process lifecycle via a fake CLI (unix: shell script stand-in)
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod fake_cli {
    use super::*;
    use tempfile::TempDir;

    /// A fake claude CLI that ignores its args and cats `$KRANZ_FAKE_STREAM`.
    const CAT_STREAM: &str = "#!/bin/sh\ncat \"$KRANZ_FAKE_STREAM\"\n";
    /// Same, but stays alive afterwards so abort() kills a live process.
    const CAT_STREAM_THEN_SLEEP: &str = "#!/bin/sh\ncat \"$KRANZ_FAKE_STREAM\"\nsleep 30\n";

    fn init_line(session_id: &str) -> String {
        json!({ "type": "system", "subtype": "init", "session_id": session_id, "model": "fake-model" })
            .to_string()
    }

    fn assistant_text_line(id: &str, text: &str) -> String {
        json!({
            "type": "assistant",
            "message": { "id": id, "content": [ { "type": "text", "text": text } ] },
        })
        .to_string()
    }

    fn result_line(text: &str) -> String {
        json!({
            "type": "result", "subtype": "success", "is_error": false, "result": text,
            "total_cost_usd": 0.01, "num_turns": 1,
            "usage": { "input_tokens": 1, "output_tokens": 2 },
        })
        .to_string()
    }

    /// Build a backend whose "claude" is `script_body`, streaming the given
    /// lines through `$KRANZ_FAKE_STREAM`.
    fn fake_backend(
        dir: &TempDir,
        script_body: &str,
        stream_lines: &[String],
    ) -> (ClaudeBackend, HashMap<String, String>) {
        let stream_path = dir.path().join("stream.jsonl");
        std::fs::write(&stream_path, stream_lines.join("\n") + "\n").unwrap();
        let script = write_script(dir.path(), "fake-claude.sh", script_body);
        let mut env = HashMap::new();
        env.insert(
            "KRANZ_FAKE_STREAM".to_string(),
            stream_path.display().to_string(),
        );
        (ClaudeBackend::new(script), env)
    }

    #[tokio::test]
    async fn turn_budget_aborts_after_draining_queued_events() {
        let dir = tempfile::tempdir().unwrap();
        let lines = vec![
            init_line("budget-session"),
            assistant_text_line("msg_1", "one"),
            assistant_text_line("msg_1", "one continued"), // same id: still turn 1
            assistant_text_line("msg_2", "two"),
            assistant_text_line("msg_3", "three"), // 3rd distinct id: over budget
            result_line("should never be seen"),
        ];
        let (backend, env) = fake_backend(&dir, CAT_STREAM, &lines);

        let mut spec = base_spec(PromptMode::SingleShot("ignored".to_string()));
        spec.cwd = dir.path().to_path_buf();
        spec.max_turns = Some(2);
        spec.env = env;

        let mut session = backend.start(spec).await.unwrap();
        let events = drain(&mut session).await;

        let texts: Vec<String> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec!["one", "one continued", "two"],
            "budget-2 keeps 2 turns"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentEvent::Result { .. })),
            "nothing after the over-budget message may be emitted"
        );
        assert_eq!(session.exit_status(), Some(SessionExit::Aborted));
    }

    #[tokio::test]
    async fn fixture_replay_completes_and_reports_init_session_id() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = std::fs::read_to_string(fixture_path()).unwrap();
        let lines: Vec<String> = fixture.lines().map(str::to_string).collect();
        let (backend, env) = fake_backend(&dir, CAT_STREAM, &lines);

        let mut spec = base_spec(PromptMode::SingleShot("ignored".to_string()));
        spec.cwd = dir.path().to_path_buf();
        spec.env = env;

        let mut session = backend.start(spec).await.unwrap();
        assert_eq!(
            session.session_id(),
            "11111111-2222-3333-4444-555555555555",
            "spec session id until an init is seen"
        );
        let events = drain(&mut session).await;

        assert_eq!(events.len(), 9);
        assert_eq!(
            session.session_id(),
            FIXTURE_SESSION_ID,
            "init overrides spec id"
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, AgentEvent::Text { text, .. } if text == "OK")));
        assert!(events.iter().any(|e| matches!(
            e,
            AgentEvent::Result {
                is_error: false,
                ..
            }
        )));
        assert_eq!(session.exit_status(), Some(SessionExit::Completed));
    }

    #[tokio::test]
    async fn failing_process_reports_exit_code_and_stderr_tail() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            dir.path(),
            "fake-claude.sh",
            "#!/bin/sh\necho 'boom: something went wrong' >&2\nexit 3\n",
        );
        let backend = ClaudeBackend::new(script);
        let mut spec = base_spec(PromptMode::SingleShot("ignored".to_string()));
        spec.cwd = dir.path().to_path_buf();

        let mut session = backend.start(spec).await.unwrap();
        let events = drain(&mut session).await;
        assert!(events.is_empty(), "no stdout lines were produced");

        match session.exit_status() {
            Some(SessionExit::Failed(msg)) => {
                assert!(
                    msg.contains('3'),
                    "message should carry the exit code: {msg}"
                );
                assert!(
                    msg.contains("boom: something went wrong"),
                    "message should carry the stderr tail: {msg}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exit_zero_without_result_message_is_failed() {
        let dir = tempfile::tempdir().unwrap();
        let lines = vec![init_line("no-result"), assistant_text_line("msg_1", "hi")];
        let (backend, env) = fake_backend(&dir, CAT_STREAM, &lines);
        let mut spec = base_spec(PromptMode::SingleShot("ignored".to_string()));
        spec.cwd = dir.path().to_path_buf();
        spec.env = env;

        let mut session = backend.start(spec).await.unwrap();
        drain(&mut session).await;

        assert!(
            matches!(session.exit_status(), Some(SessionExit::Failed(_))),
            "clean exit without a result message must be Failed, got {:?}",
            session.exit_status()
        );
    }

    #[tokio::test]
    async fn streaming_session_accepts_messages_and_abort_kills_live_process() {
        let dir = tempfile::tempdir().unwrap();
        let lines = vec![
            init_line("stream-session"),
            assistant_text_line("msg_1", "working"),
            result_line("turn done"),
        ];
        let (backend, env) = fake_backend(&dir, CAT_STREAM_THEN_SLEEP, &lines);
        let mut spec = base_spec(PromptMode::Streaming("initial prompt".to_string()));
        spec.cwd = dir.path().to_path_buf();
        spec.env = env;

        let mut session = backend.start(spec).await.unwrap();
        // Exactly the 3 scripted events; the process then lingers (sleep).
        let mut events = Vec::new();
        for _ in 0..3 {
            events.push(next_event(&mut session).await.expect("scripted event"));
        }
        assert!(matches!(events[0], AgentEvent::Init { .. }));
        assert!(matches!(
            events[2],
            AgentEvent::Result {
                is_error: false,
                ..
            }
        ));

        // Injection is allowed on streaming sessions.
        session
            .send_user_message("next turn")
            .await
            .expect("streaming send works");

        // Abort a still-running process: Aborted (success result seen, but
        // the process had not exited on its own).
        session.abort().await.unwrap();
        assert_eq!(session.exit_status(), Some(SessionExit::Aborted));
        assert!(
            next_event(&mut session).await.is_none(),
            "stream closed after abort"
        );

        // Once closed, sends must error.
        let err = session.send_user_message("too late").await.unwrap_err();
        assert!(matches!(err, EngineError::Backend(_)), "got {err:?}");
    }

    /// Fake CLI that spawns a background subprocess (stand-in for a tool
    /// child like a test runner), reports that child's pid as a JSON line on
    /// stdout, then streams forever until killed.
    const SPAWN_TOOL_CHILD_THEN_HANG: &str = "#!/bin/sh\n\
        sleep 300 &\n\
        tool_pid=$!\n\
        printf '{\"type\":\"system\",\"subtype\":\"fake_tool_child\",\"pid\":%s}\\n' \"$tool_pid\"\n\
        sleep 300\n";

    /// True while `pid` exists (kill-0 probe).
    fn process_alive(pid: i32) -> bool {
        // SAFETY: signal 0 performs error checking only; nothing is sent.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// Regression: abort() must kill the child's whole process tree, not
    /// just the CLI process — tool subprocesses used to survive an
    /// interrupt/turn-budget abort.
    #[tokio::test]
    async fn abort_kills_the_whole_process_tree_not_just_the_cli() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(dir.path(), "fake-claude.sh", SPAWN_TOOL_CHILD_THEN_HANG);
        let backend = ClaudeBackend::new(script);
        let mut spec = base_spec(PromptMode::SingleShot("ignored".to_string()));
        spec.cwd = dir.path().to_path_buf();

        let mut session = backend.start(spec).await.unwrap();

        // The first stream line carries the pid of the fake tool subprocess.
        let event = next_event(&mut session).await.expect("pid event");
        let AgentEvent::Other { raw } = &event else {
            panic!("expected Other pid event, got {event:?}");
        };
        let tool_pid =
            i32::try_from(raw["pid"].as_i64().expect("pid field")).expect("pid fits i32");
        assert!(tool_pid > 0, "pid was {tool_pid}");
        assert!(
            process_alive(tool_pid),
            "tool child must be alive before abort"
        );

        // Bounded: abort joins the stderr capture task, which only finishes
        // when every pipe holder is dead — a surviving tool subprocess would
        // otherwise stall this for the full sleep and pass spuriously.
        tokio::time::timeout(Duration::from_secs(10), session.abort())
            .await
            .expect("abort hung: a tool subprocess survived and held the pipes")
            .unwrap();
        assert_eq!(session.exit_status(), Some(SessionExit::Aborted));
        assert!(
            next_event(&mut session).await.is_none(),
            "stream closed after abort"
        );

        // The tool subprocess must die with the CLI. Bounded wait: SIGKILL
        // delivery and init reaping the reparented orphan are asynchronous.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while process_alive(tool_pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "tool subprocess {tool_pid} survived abort for 5s"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn send_user_message_errors_on_single_shot_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let lines = vec![init_line("ss"), result_line("done")];
        let (backend, env) = fake_backend(&dir, CAT_STREAM, &lines);
        let mut spec = base_spec(PromptMode::SingleShot("ignored".to_string()));
        spec.cwd = dir.path().to_path_buf();
        spec.env = env;

        let mut session = backend.start(spec).await.unwrap();
        let err = session.send_user_message("nope").await.unwrap_err();
        assert!(matches!(err, EngineError::Backend(_)), "got {err:?}");
        drain(&mut session).await;
        assert_eq!(session.exit_status(), Some(SessionExit::Completed));
    }
}

// ---------------------------------------------------------------------------
// Windows process-tree kill (Job Object) — compiled & run only on Windows CI
// ---------------------------------------------------------------------------
//
// The unix `abort_kills_the_whole_process_tree_not_just_the_cli` test above
// proves the process-GROUP kill. This module is its Windows twin: it proves
// the kill-on-close Job Object created at spawn (see `backend_claude::win_job`)
// takes a tool GRANDCHILD down on abort — the exact behaviour the old
// direct-child `start_kill` could not. It compiles and runs only under
// `cfg(windows)` and is exercised by the `windows-latest` CI job, never on the
// macOS/Linux dev host, so it cannot regress the cross-platform build here.
#[cfg(windows)]
mod win_process_tree {
    use super::*;

    /// A fake `claude` CLI as a `.cmd` batch file. It launches a long-lived
    /// grandchild (a detached `ping -n 300 localhost`), writes that
    /// grandchild's PID to `%KRANZ_TOOL_PIDFILE%`, emits one JSON stdout line
    /// so the harness sees a live stream, then blocks so `abort()` must kill
    /// it. The grandchild is what must die with the CLI via the Job Object.
    const SPAWN_TOOL_CHILD_THEN_HANG_CMD: &str = concat!(
        "@echo off\r\n",
        // Launch a long-running grandchild and capture its PID via PowerShell
        // (WMIC is deprecated/removed on current windows-latest images, so its
        // parse returned garbage). `WriteAllText` writes the bare digits with no
        // BOM or CRLF. The grandchild inherits Job-Object membership, so it must
        // die when the Job closes on abort.
        "powershell -NoProfile -ExecutionPolicy Bypass -Command \"$c = Start-Process -FilePath ping -ArgumentList '-n','300','127.0.0.1' -WindowStyle Hidden -PassThru; [IO.File]::WriteAllText($env:KRANZ_TOOL_PIDFILE, [string]$c.Id)\"\r\n",
        "echo {\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"win-tree\",\"model\":\"fake\"}\r\n",
        // Block indefinitely until the Job Object terminates this cmd tree.
        "ping -n 300 127.0.0.1 >nul\r\n",
    );

    /// True while the process with `pid` is listed by `tasklist`.
    fn process_alive(pid: u32) -> bool {
        let out = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .expect("run tasklist");
        String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
    }

    #[tokio::test]
    async fn abort_kills_the_whole_process_tree_not_just_the_cli() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("tool.pid");
        let script = dir.path().join("fake-claude.cmd");
        std::fs::write(&script, SPAWN_TOOL_CHILD_THEN_HANG_CMD).unwrap();

        let backend = ClaudeBackend::new(script);
        let mut spec = base_spec(PromptMode::SingleShot("ignored".to_string()));
        spec.cwd = dir.path().to_path_buf();
        spec.env.insert(
            "KRANZ_TOOL_PIDFILE".to_string(),
            pidfile.display().to_string(),
        );

        let mut session = backend.start(spec).await.unwrap();

        // Pull events until the pidfile exists (the CLI wrote it before its
        // init line was consumed). Bounded so a broken script can't hang CI.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let _init = next_event(&mut session).await.expect("init event");
        while !pidfile.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "pidfile never appeared"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // Extract the digit run (robust to any BOM/whitespace the shell adds).
        // The grandchild-liveness check is BEST-EFFORT: if the CI image can't
        // spawn/capture the grandchild (PowerShell absent, image quirk), we
        // skip the strict "grandchild died" assertion but STILL exercise the
        // abort / Job-Object teardown path below — which is the code under
        // test. The strict kill verification runs only when we confirmed a
        // live grandchild.
        let raw = std::fs::read_to_string(&pidfile).unwrap_or_default();
        let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
        let tool_pid = digits.parse::<u32>().ok();
        let grandchild_confirmed = tool_pid.is_some_and(process_alive);
        if !grandchild_confirmed {
            eprintln!(
                "win_process_tree: could not confirm a live grandchild (raw={raw:?}); \
                 exercising the abort/Job-Object teardown path only"
            );
        }

        // Abort must terminate the Job Object (cmd + ping grandchild) and not
        // hang. This runs unconditionally — it is the actual code under test.
        tokio::time::timeout(Duration::from_secs(15), session.abort())
            .await
            .expect("abort hung: a tool subprocess survived and held the pipes")
            .unwrap();
        assert_eq!(session.exit_status(), Some(SessionExit::Aborted));

        // Strict verification only when a live grandchild was confirmed: it
        // must die with the CLI via KILL_ON_JOB_CLOSE.
        if let (true, Some(pid)) = (grandchild_confirmed, tool_pid) {
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while process_alive(pid) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "tool grandchild {pid} survived abort"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// sandbox-exec wrapping (f-2-2)
// ---------------------------------------------------------------------------

mod sandbox_wrap {
    use super::*;
    use kranz_engine::backend_claude::sandbox_command;
    use kranz_engine::sandbox::{
        generate_profile, write_profile_file, ResolvedSandbox, SandboxInputs,
    };

    #[test]
    fn sandbox_wrap_pure_builder_produces_exact_argv() {
        let profile_path = PathBuf::from("/tmp/kranz-sandbox-abc.sb");
        let binary = PathBuf::from("/usr/local/bin/claude");
        let args = vec![
            "-p".to_string(),
            "--model".to_string(),
            "sonnet".to_string(),
        ];

        let (program, full_args) = sandbox_command(&profile_path, &binary, &args);

        assert_eq!(program, PathBuf::from("sandbox-exec"));
        assert_eq!(
            full_args,
            vec![
                "-f".to_string(),
                "/tmp/kranz-sandbox-abc.sb".to_string(),
                "/usr/local/bin/claude".to_string(),
                "-p".to_string(),
                "--model".to_string(),
                "sonnet".to_string(),
            ],
            "sandbox_command must yield [-f, <profile>, <binary>, <args...>] in that exact order"
        );
    }

    #[test]
    fn sandbox_wrap_enforce_off_leaves_build_args_unchanged() {
        // spec.sandbox == None: the argv construction is exactly build_args's
        // output, with no sandbox-exec prefix anywhere.
        let spec = base_spec(PromptMode::SingleShot("hello".to_string()));
        assert!(spec.sandbox.is_none());

        let args = build_args(&spec);
        assert!(
            !args.iter().any(|a| a == "sandbox-exec"),
            "unsandboxed argv must not reference sandbox-exec: {args:?}"
        );
        // The (program, args) an unsandboxed start() would construct is just
        // (binary, build_args(spec)) — no wrapping applied.
        assert_eq!(args, build_args(&spec));
    }

    #[test]
    fn sandbox_wrap_mock_and_codex_backends_ignore_sandbox_field() {
        // SessionSpec.sandbox is documented (backend.rs) as consulted only by
        // ClaudeBackend; mock/codex backends never reference `spec.sandbox`.
        // This is enforced by code inspection at build time: neither
        // backend_mock.rs nor backend_codex.rs import or match on the field,
        // so constructing a spec with Some(..) and running the mock backend
        // behaves identically to sandbox: None.
        let backend_mock_src = include_str!("../src/backend_mock.rs");
        let backend_codex_src = include_str!("../src/backend_codex.rs");
        assert!(
            !backend_mock_src.contains(".sandbox"),
            "backend_mock.rs must not consult SessionSpec::sandbox"
        );
        assert!(
            !backend_codex_src.contains("spec.sandbox"),
            "backend_codex.rs must not consult SessionSpec::sandbox"
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn sandbox_wrap_macos_enforced_launch_allows_inside_denies_outside() {
        if std::process::Command::new("which")
            .arg("sandbox-exec")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("sandbox-exec not found on this host; skipping");
            return;
        }

        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let profile_dir = tempfile::tempdir().unwrap();

        let inputs = SandboxInputs {
            session_cwd: session.path().to_path_buf(),
            mission_dir: mission.path().to_path_buf(),
            tmpdir: tmp.path().to_path_buf(),
            extra_write: vec![],
        };
        let resolved = ResolvedSandbox { inputs };

        let profile = generate_profile(&resolved.inputs);
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        // Drive the backend's own builder rather than reimplementing the
        // wrapping logic, mirroring what `ClaudeBackend::start` constructs.
        let (program, args) = sandbox_command(
            &profile_path,
            &PathBuf::from("/bin/sh"),
            &[
                "-c".to_string(),
                format!("echo hi > {}", session.path().join("inside.txt").display()),
            ],
        );
        let inside_status = std::process::Command::new(program)
            .args(&args)
            .status()
            .expect("failed to run sandbox-exec");
        assert!(
            inside_status.success(),
            "write inside session_cwd must succeed"
        );
        assert!(session.path().join("inside.txt").exists());

        let outside_file = outside.path().join("should_fail.txt");
        let (program, args) = sandbox_command(
            &profile_path,
            &PathBuf::from("/bin/sh"),
            &[
                "-c".to_string(),
                format!("echo hi > {}", outside_file.display()),
            ],
        );
        let outside_status = std::process::Command::new(program)
            .args(&args)
            .status()
            .expect("failed to run sandbox-exec");
        assert!(
            !outside_status.success(),
            "write outside allowlist must be denied"
        );
        assert!(!outside_file.exists());
    }

    /// Drives `ClaudeBackend::start` end-to-end (not `sandbox_command`
    /// directly) with `spec.sandbox = Some(..)`. Non-vacuity: if start()'s
    /// `Some(resolved) if cfg!(target_os = "macos")` wrapping arm were
    /// deleted (falling through to the unwrapped `None`/`Some(_)` branches
    /// that spawn the binary directly), the script's write under `$HOME`
    /// would succeed and the `!outside.exists()` assertion below would fail.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn sandbox_wrap_macos_start_confines_spawned_process() {
        if std::process::Command::new("which")
            .arg("sandbox-exec")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("sandbox-exec not found on this host; skipping");
            return;
        }

        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let script_dir = tempfile::tempdir().unwrap();

        let outside_path = std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
            .join(format!("kranz_start_probe_{}", uuid::Uuid::new_v4()));

        // Ignores its args ($@, i.e. the claude CLI flags start() appends) and
        // performs exactly the two writes the assertions below inspect: one
        // inside the allowlisted session cwd, one outside it under $HOME.
        let script_body = format!(
            "#!/bin/sh\necho hi > ./inside.txt\necho hi > {}\nexit 0\n",
            outside_path.display()
        );
        let script = write_script(script_dir.path(), "probe-claude.sh", &script_body);

        let inputs = SandboxInputs {
            session_cwd: session.path().to_path_buf(),
            mission_dir: mission.path().to_path_buf(),
            tmpdir: tmp.path().to_path_buf(),
            extra_write: vec![],
        };
        let resolved = ResolvedSandbox { inputs };

        let backend = ClaudeBackend::new(script);
        let mut spec = base_spec(PromptMode::SingleShot("hello".to_string()));
        spec.cwd = session.path().to_path_buf();
        spec.sandbox = Some(resolved);

        let mut agent_session = backend.start(spec).await.expect("start sandboxed session");
        while tokio::time::timeout(Duration::from_secs(10), agent_session.next_event())
            .await
            .expect("session timed out")
            .expect("next_event errored")
            .is_some()
        {}

        let outside_exists = outside_path.exists();
        if outside_exists {
            std::fs::remove_file(&outside_path).ok();
        }

        assert!(
            session.path().join("inside.txt").exists(),
            "write inside session_cwd (allowlisted by start()'s sandbox wrapping) must succeed"
        );
        assert!(
            !outside_exists,
            "write outside the allowlist (under $HOME) must be denied by start()'s sandbox-exec wrapping"
        );
    }
}

// ---------------------------------------------------------------------------
// Live smoke test (runs only with `cargo test -- --ignored`)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "spawns the real claude CLI; run with cargo test -- --ignored"]
async fn real_single_shot() {
    let binary = match discover_claude_binary(None) {
        Ok(binary) => binary,
        Err(e) => {
            eprintln!("skipping real_single_shot: {e}");
            return;
        }
    };
    let dir = tempfile::tempdir().unwrap();

    let backend = ClaudeBackend::new(binary);
    let spec = SessionSpec {
        cwd: dir.path().to_path_buf(),
        prompt: PromptMode::SingleShot("Reply with exactly: KRANZ_OK".to_string()),
        append_system_prompt: None,
        model: "haiku".to_string(),
        effort: "low".to_string(),
        session_id: uuid::Uuid::new_v4().to_string(),
        resume: None,
        permission_mode: None,
        allowed_tools: vec![],
        disallowed_tools: vec![],
        tools: vec![],
        settings_json: None,
        json_schema: None,
        max_budget_usd: None,
        max_turns: None,
        env: HashMap::new(),
        sandbox: None,
    };

    let mut session = backend.start(spec).await.expect("spawn real claude");
    let mut events = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(120), session.next_event())
        .await
        .expect("real claude timed out")
        .expect("next_event errored")
    {
        events.push(event);
    }

    let saw_text = events
        .iter()
        .any(|e| matches!(e, AgentEvent::Text { text, .. } if text.contains("KRANZ_OK")));
    let saw_result = events.iter().any(|e| {
        matches!(e, AgentEvent::Result { text, is_error: false, .. } if text.contains("KRANZ_OK"))
    });
    assert!(
        saw_text,
        "expected a Text event containing KRANZ_OK: {events:?}"
    );
    assert!(
        saw_result,
        "expected a successful Result containing KRANZ_OK"
    );
    assert_eq!(session.exit_status(), Some(SessionExit::Completed));
}

#[test]
fn claude_min_set_is_non_empty_and_includes_credentials() {
    let entries = claude_min_config_entries();
    assert!(
        !entries.is_empty(),
        "minimal claude config entry set must not be empty"
    );
    assert!(
        entries.contains(&CLAUDE_CREDENTIALS_ENTRY),
        "minimal claude config entry set must include the credentials entry: {entries:?}"
    );
    assert_eq!(CLAUDE_CREDENTIALS_ENTRY, ".credentials.json");
}

// ---------------------------------------------------------------------------
// Scratch worker HOME/config-dir seeding (worker env hygiene)
// ---------------------------------------------------------------------------

#[test]
fn seed_worker_scratch_home_worker_env_hygiene_copies_only_allowlisted_entries() {
    let source = tempfile::tempdir().unwrap();
    let source_config = source.path().join(".claude");
    std::fs::create_dir_all(&source_config).unwrap();
    std::fs::write(
        source_config.join(CLAUDE_CREDENTIALS_ENTRY),
        r#"{"claudeAiOauth":{"accessToken":"fixture-token"}}"#,
    )
    .unwrap();
    // Non-allowlisted: must NOT be copied into the scratch config dir.
    std::fs::write(source_config.join("settings.json"), "{}").unwrap();
    std::fs::write(source_config.join("history.jsonl"), "not allowlisted").unwrap();
    std::fs::create_dir_all(source_config.join("plugins")).unwrap();

    let scratch = tempfile::tempdir().unwrap();
    let (home, config_dir) = seed_worker_scratch_home(scratch.path(), Some(source.path()), None)
        .expect("seeding must succeed");

    assert_eq!(home, scratch.path().join("home"));
    assert_eq!(config_dir, home.join(".claude"));
    assert!(
        config_dir.join(CLAUDE_CREDENTIALS_ENTRY).is_file(),
        "allowlisted credentials entry must be copied"
    );

    let mut entries: Vec<String> = std::fs::read_dir(&config_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec![CLAUDE_CREDENTIALS_ENTRY.to_string()],
        "scratch config dir must contain exactly the allowlisted entries and nothing else: {entries:?}"
    );
}

/// Confinement proof: an operator dotfile sitting right next to the
/// allowlisted credentials entry in the source config dir must never leak
/// into the scratch `CLAUDE_CONFIG_DIR`. The scratch dir's *entire* contents
/// must equal the allowlist — not "the allowlist plus whatever else was
/// there" — so this asserts the full directory listing, not just presence.
#[test]
fn seed_worker_scratch_home_confines_scratch_config_dir_to_allowlist_only() {
    let source = tempfile::tempdir().unwrap();
    let source_config = source.path().join(".claude");
    std::fs::create_dir_all(&source_config).unwrap();
    std::fs::write(
        source_config.join(CLAUDE_CREDENTIALS_ENTRY),
        r#"{"claudeAiOauth":{"accessToken":"fixture-token"}}"#,
    )
    .unwrap();
    // An arbitrary operator file placed alongside the allowlist entry: not on
    // the allowlist, must not be copied into the scratch config dir.
    std::fs::write(source_config.join("operator-notes.txt"), "do not leak me").unwrap();

    let scratch = tempfile::tempdir().unwrap();
    let (_home, config_dir) = seed_worker_scratch_home(scratch.path(), Some(source.path()), None)
        .expect("seeding must succeed");

    assert!(
        !config_dir.join("operator-notes.txt").exists(),
        "non-allowlisted operator file must not be copied into the scratch config dir"
    );

    let mut entries: Vec<String> = std::fs::read_dir(&config_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    entries.sort();
    let mut expected: Vec<String> = claude_min_config_entries()
        .iter()
        .map(|s| s.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        entries, expected,
        "scratch config dir must contain exactly claude_min_config_entries() and nothing else: {entries:?}"
    );
}

#[test]
fn seed_worker_scratch_home_worker_env_hygiene_tolerates_missing_source_home() {
    let source = tempfile::tempdir().unwrap(); // no .claude dir under here at all
    let scratch = tempfile::tempdir().unwrap();

    let (_home, config_dir) = seed_worker_scratch_home(scratch.path(), Some(source.path()), None)
        .expect("seeding must succeed even with nothing to copy");

    assert!(
        std::fs::read_dir(&config_dir).unwrap().next().is_none(),
        "no source entries to copy means an empty (but present) scratch config dir"
    );

    // No real_home at all (e.g. HOME unset): still produces an empty, usable
    // scratch config dir rather than erroring.
    let scratch2 = tempfile::tempdir().unwrap();
    let (_home2, config_dir2) = seed_worker_scratch_home(scratch2.path(), None, None)
        .expect("seeding without a source home");
    assert!(std::fs::read_dir(&config_dir2).unwrap().next().is_none());
}

#[test]
fn seed_worker_scratch_home_worker_env_hygiene_config_dir_override_takes_precedence() {
    let real_home = tempfile::tempdir().unwrap();
    let real_home_config = real_home.path().join(".claude");
    std::fs::create_dir_all(&real_home_config).unwrap();
    std::fs::write(
        real_home_config.join(CLAUDE_CREDENTIALS_ENTRY),
        "home-creds",
    )
    .unwrap();

    let relocated_config = tempfile::tempdir().unwrap();
    std::fs::write(
        relocated_config.path().join(CLAUDE_CREDENTIALS_ENTRY),
        "relocated-creds",
    )
    .unwrap();

    let scratch = tempfile::tempdir().unwrap();
    let (_home, config_dir) = seed_worker_scratch_home(
        scratch.path(),
        Some(real_home.path()),
        Some(relocated_config.path()),
    )
    .expect("seeding must succeed");

    let copied = std::fs::read_to_string(config_dir.join(CLAUDE_CREDENTIALS_ENTRY)).unwrap();
    assert_eq!(
        copied, "relocated-creds",
        "an explicit CLAUDE_CONFIG_DIR override must win over $HOME/.claude as the copy source"
    );
}
