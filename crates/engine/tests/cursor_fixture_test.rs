//! Drives the committed Cursor CLI `stream-json` fixture
//! (`docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl` — captured
//! from a real write-capable run, the decided `direct-parser` route's wire
//! evidence) through the real [`kranz_engine::backend_cursor`] parser, and
//! pins the additive `BackendKind::Cursor` config wiring. Offline: no
//! `agent` binary, no network.

use kranz_engine::backend::AgentEvent;
use kranz_engine::backend_cursor::parse_cursor_line;
use kranz_engine::config::{effective_model, model_tier, parse_backend, validate, ModelTier};
use kranz_engine::cost::DEFAULT_CURSOR_MODEL;
use kranz_engine::types::{BackendKind, MissionConfig, Role, SandboxEnforce, TokenUsage};
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("scoping")
        .join("cursor-probe-evidence")
        .join("fixture-stream-json.jsonl")
}

fn fixture_lines() -> Vec<String> {
    std::fs::read_to_string(fixture_path())
        .expect("read fixture")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.to_string())
        .collect()
}

fn parse_fixture() -> Vec<AgentEvent> {
    let mut events = Vec::new();
    for line in fixture_lines() {
        events.extend(parse_cursor_line(&line, DEFAULT_CURSOR_MODEL));
    }
    events
}

/// Acceptance item 1: the terminal result event carries the full result text
/// (stream-json splits the same text across the final `assistant` event and
/// the terminal `result` event — no stitching needed).
#[test]
fn backend_cursor_fixture_terminal_text_matches_the_final_assistant_event() {
    let events = parse_fixture();

    let last_assistant = events
        .iter()
        .rev()
        .find_map(|e| match e {
            AgentEvent::Text { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("expected at least one assistant Text event");
    let terminal = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Result { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("expected a terminal Result event");

    assert!(!terminal.is_empty());
    assert_eq!(
        terminal, last_assistant,
        "the terminal result text IS the final assistant text on this wire"
    );
    assert_eq!(
        terminal,
        "Created `hello.txt` containing:\n\n```text\nhi\n```"
    );
}

/// Acceptance item 2: tool use and tool results are first-class, ordered
/// events — the fixture's started/completed pairs for all three observed
/// tool kinds, in wire order.
#[test]
fn backend_cursor_fixture_tool_events_are_first_class_and_ordered() {
    let events = parse_fixture();
    let sequence: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolUse { tool, summary, .. } => Some(format!("use:{tool}:{summary}")),
            AgentEvent::ToolResult {
                tool,
                denied,
                summary,
                ..
            } => Some(format!(
                "result:{}:{denied}:{summary}",
                tool.as_deref().unwrap_or("?")
            )),
            _ => None,
        })
        .collect();

    assert_eq!(
        sequence.len(),
        6,
        "three started/completed pairs in wire order: {sequence:?}"
    );
    assert!(sequence[0].starts_with("use:shellToolCall:ls"));
    assert!(
        sequence[1].starts_with("result:shellToolCall:false:README.md"),
        "the shell result summary is the captured stdout: {}",
        sequence[1]
    );
    assert!(sequence[2].starts_with("use:editToolCall:"));
    assert!(sequence[2].contains("hello.txt"));
    assert!(sequence[3].starts_with("result:editToolCall:false:"));
    assert!(sequence[4].starts_with("use:readToolCall:"));
    assert!(sequence[5].starts_with("result:readToolCall:false:"));
}

/// Acceptance items 3: usage rides the terminal result event verbatim; the
/// wire carries no dollar cost, so cost is computed client-side from those
/// real tokens (never read off the wire, never fabricated).
#[test]
fn backend_cursor_fixture_usage_is_verbatim_and_cost_client_side() {
    let events = parse_fixture();
    let (usage, cost_usd, num_turns, is_error) = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Result {
                usage,
                cost_usd,
                num_turns,
                is_error,
                ..
            } => Some((usage.clone(), *cost_usd, *num_turns, *is_error)),
            _ => None,
        })
        .expect("expected a terminal Result event");

    assert!(!is_error);
    assert_eq!(
        usage,
        TokenUsage {
            input: 32473,
            output: 305,
            cache_read: 96675,
            cache_write: 0,
        }
    );
    let cost = cost_usd.expect("usage on the wire means a computed cost");
    assert!(cost > 0.0, "a real usage computes a real cost: {cost}");
    assert_eq!(num_turns, Some(1));
}

/// The Init event carries the wire's own session id and model DISPLAY string
/// (acceptance table row 1); the user-prompt echo stays transcript-only.
#[test]
fn backend_cursor_fixture_init_and_user_echo() {
    let events = parse_fixture();
    match &events[0] {
        AgentEvent::Init {
            session_id, model, ..
        } => {
            assert!(!session_id.is_empty());
            assert_eq!(model, "GPT-5.6 Luna 272K Low");
        }
        other => panic!("expected Init first, got {other:?}"),
    }
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AgentEvent::Other { raw } if raw["type"] == "user")),
        "the user prompt echo rides the transcript as Other"
    );
}

/// The additive config wiring: parse/kind/flags/model rules all agree, the
/// enforced-sandbox pair fails closed, and the worker floor gates cursor
/// behind the below-default opt-in (validator-first posture).
#[test]
fn backend_cursor_config_wiring_is_additive_and_fails_closed() {
    assert_eq!(parse_backend(Some("cursor")), Ok(BackendKind::Cursor));
    assert_eq!(BackendKind::Cursor.as_str(), "cursor");
    assert!(
        !BackendKind::Cursor.supports_sandbox_enforcement(),
        "cursor's --sandbox is observed to be no isolation boundary; enforced \
         kranz sandboxes fail closed at validation"
    );
    // The committed fixture carries cacheReadTokens AND cacheWriteTokens on
    // the terminal result event, so both cache reporters are honest `true`
    // (unlike kimi/local/acp, where a zero would be fabricated).
    assert!(BackendKind::Cursor.reports_cache_read_tokens());
    assert!(BackendKind::Cursor.reports_cache_write_tokens());
    // Account-specific ~190-model catalogs cannot be allowlisted client-side.
    assert_eq!(
        model_tier(BackendKind::Cursor, DEFAULT_CURSOR_MODEL),
        Some(ModelTier::BelowDefault)
    );
    // A role that only set `backend = "cursor"` gets the backend default.
    assert_eq!(
        effective_model(Role::ValidatorScrutiny, BackendKind::Cursor, "opus"),
        DEFAULT_CURSOR_MODEL
    );

    let mut cfg = MissionConfig::default();
    cfg.validator_scrutiny.backend = Some("cursor".into());
    assert_eq!(
        cfg.backend_kind(Role::ValidatorScrutiny),
        BackendKind::Cursor
    );
    assert!(
        validate(&cfg).is_ok(),
        "a cursor validator validates (validator-first deployment)"
    );

    // The fail-closed sandbox pair: an enforced sandbox on a backend that
    // cannot honor it must never run.
    cfg.validator_scrutiny.sandbox.enforce = SandboxEnforce::Fs;
    let error = validate(&cfg).expect_err("cursor + sandbox.enforce=fs must fail closed");
    assert!(
        error.to_string().contains("cursor"),
        "the error names the backend: {error}"
    );
    cfg.validator_scrutiny.sandbox.enforce = SandboxEnforce::Off;

    // The worker floor: cursor's uniform below-default classification makes
    // worker use an explicit opt-in (worker use waits for live soak).
    cfg.worker.backend = Some("cursor".into());
    let error = validate(&cfg).expect_err("a cursor worker needs the below-default opt-in");
    assert!(
        error.to_string().contains("allowBelowDefaultWorkerModel"),
        "the floor error names the opt-in: {error}"
    );
    cfg.allow_below_default_worker_model = true;
    assert!(validate(&cfg).is_ok(), "the opt-in admits a cursor worker");
}
