//! Integration tests for `backend_cursor`: a mock `agent` binary (POSIX
//! shell script emitting the committed fixture's `stream-json` shapes)
//! drives the real [`CursorBackend`] through full sessions — spawn → tool
//! calls → result → exit with ordered events — plus absent-usage,
//! permission-refusal, pre-billing model rejection, kill-mid-session, and
//! CLI-death resumability cases (the ticket's test gate).
//!
//! `cfg(unix)` throughout: the mock is a `/bin/sh` script, matching the
//! house stub idiom in `backend_acp_test.rs` / `backend_claude_test.rs`.

#![cfg(unix)]

use kranz_engine::auth_verify::AuthVerdict;
use kranz_engine::backend::{
    AgentBackend, AgentEvent, AgentSession, PromptMode, SessionExit, SessionSpec,
};
use kranz_engine::backend_cursor::CursorBackend;
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::paths::MissionPaths;
use kranz_engine::types::{Feature, FeatureOrigin, FeatureStatus, MissionConfig, TokenUsage};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Write an executable mock-`agent` script and return its path.
fn write_mock(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write mock script");
    let mut perms = std::fs::metadata(&path)
        .expect("script metadata")
        .permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).expect("chmod script");
    path
}

/// The session spec for one mock-driven run. `HOME` is pinned into the spec
/// env so the session uses the relocated-HOME env branch verbatim — session
/// tests never read the operator's real `~/.cursor` (scratch-home seeding
/// has its own unit tests in `backend_cursor.rs`).
fn spec(dir: &Path, session_id: &str, writable: bool) -> SessionSpec {
    let mut env = HashMap::new();
    env.insert("HOME".to_string(), dir.display().to_string());
    SessionSpec {
        cwd: dir.to_path_buf(),
        prompt: PromptMode::SingleShot("do the thing".to_string()),
        append_system_prompt: None,
        model: "gpt-5".to_string(),
        effort: "high".to_string(),
        session_id: session_id.to_string(),
        resume: None,
        permission_mode: None,
        allowed_tools: vec![],
        disallowed_tools: vec![],
        tools: vec![],
        writable,
        settings_json: None,
        json_schema: None,
        max_budget_usd: None,
        max_turns: None,
        env,
        sandbox: None,
        hook_status: None,
    }
}

async fn next(session: &mut Box<dyn AgentSession>) -> Option<AgentEvent> {
    session
        .next_event()
        .await
        .expect("next_event should not error")
}

/// Full happy-path session for a WRITABLE role: asserts `--force` reached
/// the child argv (the writable permission mapping, probe item 6), then
/// emits init → user echo → tool_call started/completed → assistant →
/// result-with-usage and exits 0.
const FULL_SESSION_MOCK: &str = r#"#!/bin/sh
case " $* " in
  *" --force "*) ;;
  *) echo "mock: expected --force in argv, got: $*" >&2; exit 2 ;;
esac
cat <<'MOCK_EOF'
{"type":"system","subtype":"init","apiKeySource":"login","cwd":"/tmp/mock","session_id":"cursor-mock-sess-1","model":"GPT-5.6 Luna 272K Low","permissionMode":"default"}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"do the thing"}]},"session_id":"cursor-mock-sess-1"}
{"type":"tool_call","subtype":"started","call_id":"c1","tool_call":{"shellToolCall":{"args":{"command":"ls"}}},"session_id":"cursor-mock-sess-1"}
{"type":"tool_call","subtype":"completed","call_id":"c1","tool_call":{"shellToolCall":{"args":{"command":"ls"},"result":{"success":{"command":"ls","exitCode":0,"stdout":"README.md\n","stderr":""}}}},"session_id":"cursor-mock-sess-1"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Created `hello.txt`."}]},"session_id":"cursor-mock-sess-1"}
{"type":"result","subtype":"success","duration_ms":10,"duration_api_ms":10,"is_error":false,"result":"Created `hello.txt`.","session_id":"cursor-mock-sess-1","usage":{"inputTokens":100,"outputTokens":20,"cacheReadTokens":300,"cacheWriteTokens":5}}
MOCK_EOF
exit 0
"#;

/// Same shape but the terminal result carries NO usage object: usage and
/// cost must both stay absent (never fabricated).
const NO_USAGE_MOCK: &str = r#"#!/bin/sh
case " $* " in
  *" --force "*) ;;
  *) echo "mock: expected --force in argv, got: $*" >&2; exit 2 ;;
esac
cat <<'MOCK_EOF'
{"type":"system","subtype":"init","session_id":"cursor-mock-sess-2","model":"GPT-5.6 Luna 272K Low"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]},"session_id":"cursor-mock-sess-2"}
{"type":"result","subtype":"success","duration_ms":4,"is_error":false,"result":"done","session_id":"cursor-mock-sess-2"}
MOCK_EOF
exit 0
"#;

/// The permission-refusal case for a READ-ONLY role: asserts `--mode ask`
/// reached the child argv (the read-only mapping), then reports a push
/// attempt that failed — on this wire a refused/failed command surfaces as
/// `result.failure` with a real exit code, observable for transcripts and
/// denial reporting, but NOT a kranz guardrail denial (kranz's no-push
/// invariant for cursor is enforced externally: the read-only turn mode
/// plus scoped credentials).
const PERMISSION_MOCK: &str = r#"#!/bin/sh
case " $* " in
  *" --mode ask "*) ;;
  *) echo "mock: expected --mode ask in argv, got: $*" >&2; exit 2 ;;
esac
cat <<'MOCK_EOF'
{"type":"system","subtype":"init","session_id":"cursor-mock-sess-ask","model":"GPT-5.6 Luna 272K Low","permissionMode":"ask"}
{"type":"tool_call","subtype":"started","call_id":"c1","tool_call":{"shellToolCall":{"args":{"command":"git push origin main"}}},"session_id":"cursor-mock-sess-ask"}
{"type":"tool_call","subtype":"completed","call_id":"c1","tool_call":{"shellToolCall":{"args":{"command":"git push origin main"},"result":{"failure":{"command":"git push origin main","exitCode":1,"signal":"","stdout":"","stderr":"denied by policy: pushes are not permitted","aborted":false}}}},"session_id":"cursor-mock-sess-ask"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"I cannot run that: ask mode is read-only."}]},"session_id":"cursor-mock-sess-ask"}
{"type":"result","subtype":"success","is_error":false,"result":"I cannot run that: ask mode is read-only.","session_id":"cursor-mock-sess-ask","usage":{"inputTokens":10,"outputTokens":5,"cacheReadTokens":0,"cacheWriteTokens":0}}
MOCK_EOF
exit 0
"#;

/// Probe item 5: an invalid/unentitled `--model` id fails deterministically
/// and user-readably BEFORE any billed turn — exit 1, plain-text
/// `Cannot use this model: ...` (not JSON), no result event.
const MODEL_REJECTION_MOCK: &str = r#"#!/bin/sh
echo "Cannot use this model: bogus-id. Available models: gpt-5"
exit 1
"#;

/// Streams init + one assistant line, then a TORN line (no newline) and
/// sleeps: the kill-mid-session scenario.
const TORN_THEN_HANG_MOCK: &str = r#"#!/bin/sh
printf '%s\n' '{"type":"system","subtype":"init","session_id":"cursor-mock-sess-hang","model":"m"}'
printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"partial work"}]},"session_id":"cursor-mock-sess-hang"}'
printf '%s' '{"type":"resu'
sleep 300
"#;

/// A COMPLETED turn whose tool output happens to contain a pre-billing
/// phrase on stderr: the detector must NOT re-label the session — the phrase
/// belongs to the tool, not to the CLI's own rejection path.
const NOISY_STDERR_MOCK: &str = r#"#!/bin/sh
echo "remote: Authentication required" >&2
cat <<'MOCK_EOF'
{"type":"system","subtype":"init","session_id":"cursor-mock-sess-noise","model":"m"}
{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"cursor-mock-sess-noise","usage":{"inputTokens":1,"outputTokens":1,"cacheReadTokens":0,"cacheWriteTokens":0}}
MOCK_EOF
exit 0
"#;

/// Streams init + one assistant line and a torn line, then DIES mid-line
/// (exit 3): the CLI-death variant of the torn-line case.
const TORN_THEN_DIE_MOCK: &str = r#"#!/bin/sh
printf '%s\n' '{"type":"system","subtype":"init","session_id":"cursor-mock-sess-die","model":"m"}'
printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"partial work"}]},"session_id":"cursor-mock-sess-die"}'
printf '%s' '{"type":"resu'
exit 3
"#;

#[tokio::test]
async fn backend_cursor_full_session_streams_ordered_events() {
    let dir = tempfile::tempdir().unwrap();
    let mock = write_mock(dir.path(), "mock-agent.sh", FULL_SESSION_MOCK);
    let backend = CursorBackend::new(mock);

    let mut session = backend
        .start(spec(dir.path(), "kranz-sess-1", true))
        .await
        .expect("spawn should succeed");

    match next(&mut session).await.expect("init") {
        AgentEvent::Init {
            session_id, model, ..
        } => {
            assert_eq!(session_id, "cursor-mock-sess-1");
            assert_eq!(model, "GPT-5.6 Luna 272K Low");
        }
        other => panic!("expected Init, got {other:?}"),
    }
    assert_eq!(session.session_id(), "cursor-mock-sess-1");

    // The user-prompt echo rides the transcript as Other.
    assert!(matches!(
        next(&mut session).await.expect("user echo"),
        AgentEvent::Other { .. }
    ));
    // Tool calls are first-class events, not transcript text.
    match next(&mut session).await.expect("tool use") {
        AgentEvent::ToolUse { tool, summary, .. } => {
            assert_eq!(tool, "shellToolCall");
            assert_eq!(summary, "ls");
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
            assert_eq!(tool.as_deref(), Some("shellToolCall"));
            assert!(!denied, "a completed tool call is not a denial");
            assert_eq!(summary, "README.md\n");
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
    match next(&mut session).await.expect("assistant text") {
        AgentEvent::Text { text, .. } => assert_eq!(text, "Created `hello.txt`."),
        other => panic!("expected Text, got {other:?}"),
    }
    match next(&mut session).await.expect("terminal result") {
        AgentEvent::Result {
            text,
            is_error,
            usage,
            cost_usd,
            num_turns,
            ..
        } => {
            assert_eq!(text, "Created `hello.txt`.");
            assert!(!is_error);
            assert_eq!(
                usage,
                TokenUsage {
                    input: 100,
                    output: 20,
                    cache_read: 300,
                    cache_write: 5,
                },
                "the wire usage maps verbatim onto TokenUsage"
            );
            let cost = cost_usd.expect("usage on the wire means a computed cost");
            assert!(cost > 0.0, "cost is computed client-side from real usage");
            assert_eq!(num_turns, Some(1));
        }
        other => panic!("expected Result, got {other:?}"),
    }

    assert!(next(&mut session).await.is_none());
    assert_eq!(session.exit_status(), Some(SessionExit::Completed));
}

#[tokio::test]
async fn backend_cursor_absent_usage_and_cost_stay_absent() {
    let dir = tempfile::tempdir().unwrap();
    let mock = write_mock(dir.path(), "mock-agent-no-usage.sh", NO_USAGE_MOCK);
    let backend = CursorBackend::new(mock);

    let mut session = backend
        .start(spec(dir.path(), "kranz-sess-2", true))
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
                "unreported usage stays absent — no client-side guess from nothing"
            );
        }
        other => panic!("expected Result, got {other:?}"),
    }
    assert_eq!(session.exit_status(), Some(SessionExit::Completed));
}

/// The permission-refusal case: a read-only session runs under `--mode ask`
/// (asserted by the mock at the argv level), and the refused push surfaces
/// as a ToolResult with the refusal's stderr in the summary — observable for
/// transcripts and denial reporting — while `denied` stays false (a failure
/// with a real exit code is not a kranz guardrail denial; the no-push
/// invariant holds via the read-only turn mode and scoped credentials).
#[tokio::test]
async fn backend_cursor_permission_refusal_surfaces_as_failure_not_denial() {
    let dir = tempfile::tempdir().unwrap();
    let mock = write_mock(dir.path(), "mock-agent-ask.sh", PERMISSION_MOCK);
    let backend = CursorBackend::new(mock);

    let mut session = backend
        .start(spec(dir.path(), "kranz-sess-3", false))
        .await
        .unwrap();

    let mut saw_push_attempt = false;
    let mut refusal_summaries = Vec::new();
    while let Some(event) = next(&mut session).await {
        match event {
            AgentEvent::ToolUse { tool, summary, .. } => {
                assert_eq!(tool, "shellToolCall");
                assert_eq!(summary, "git push origin main");
                saw_push_attempt = true;
            }
            AgentEvent::ToolResult {
                tool,
                denied,
                summary,
                ..
            } => {
                assert_eq!(tool.as_deref(), Some("shellToolCall"));
                assert!(
                    !denied,
                    "cursor has no in-band denial frame; a real exit code is a normal failure"
                );
                refusal_summaries.push(summary);
            }
            _ => {}
        }
    }
    assert!(
        saw_push_attempt,
        "the push attempt must surface as a ToolUse"
    );
    assert_eq!(
        refusal_summaries,
        vec!["denied by policy: pushes are not permitted".to_string()],
        "the refusal's stderr is the recorded summary (denial reporting)"
    );
    assert_eq!(session.exit_status(), Some(SessionExit::Completed));
}

/// Probe item 5: a pre-billing model rejection fails as a configuration
/// error — deterministic, user-readable, and never mistaken for a retryable
/// transport failure.
#[tokio::test]
async fn backend_cursor_model_rejection_fails_as_a_config_error() {
    let dir = tempfile::tempdir().unwrap();
    let mock = write_mock(
        dir.path(),
        "mock-agent-bogus-model.sh",
        MODEL_REJECTION_MOCK,
    );
    let backend = CursorBackend::new(mock);

    let mut session = backend
        .start(spec(dir.path(), "kranz-sess-4", false))
        .await
        .unwrap();
    // The plain-text rejection line is never a parse failure: it rides the
    // transcript as an Other entry.
    while let Some(event) = next(&mut session).await {
        assert!(
            matches!(event, AgentEvent::Other { .. }),
            "a pre-billing rejection emits no structured events: {event:?}"
        );
    }
    match session.exit_status() {
        Some(SessionExit::Failed(msg)) => {
            assert!(
                msg.contains("Cannot use this model: bogus-id"),
                "the rejection text is surfaced: {msg}"
            );
            assert!(
                msg.contains("before any billed turn"),
                "the failure is worded as the pre-billing rejection it is: {msg}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn backend_cursor_kill_mid_session_leaves_a_clean_aborted_stream() {
    let dir = tempfile::tempdir().unwrap();
    let mock = write_mock(dir.path(), "mock-agent-hang.sh", TORN_THEN_HANG_MOCK);
    let backend = CursorBackend::new(mock);

    let mut session = backend
        .start(spec(dir.path(), "kranz-sess-5", true))
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

    // Kill the CLI mid-session (its torn partial line is still unflushed).
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

/// A CLI dying mid-line fails honestly (exit 3, no terminal event) — the
/// torn tail is at most an Other transcript entry, never a parse error.
#[tokio::test]
async fn backend_cursor_torn_final_line_is_not_a_parse_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mock = write_mock(dir.path(), "mock-agent-die.sh", TORN_THEN_DIE_MOCK);
    let backend = CursorBackend::new(mock);

    let mut session = backend
        .start(spec(dir.path(), "kranz-sess-6", true))
        .await
        .unwrap();
    let mut texts = Vec::new();
    while let Some(event) = next(&mut session).await {
        if let AgentEvent::Text { text, .. } = event {
            texts.push(text);
        }
    }
    assert_eq!(texts, vec!["partial work"]);
    match session.exit_status() {
        Some(SessionExit::Failed(msg)) => assert!(
            msg.contains("without emitting a terminal event"),
            "CLI death mid-turn is an honest failure: {msg}"
        ),
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// The guard on the pre-billing detector: a COMPLETED turn whose stderr
/// happens to contain a rejection phrase (a tool's own output, e.g. a
/// remote's auth error) stays Completed — only genuinely rejected sessions
/// get the configuration-error wording.
#[tokio::test]
async fn backend_cursor_completed_turn_with_phrase_on_stderr_stays_completed() {
    let dir = tempfile::tempdir().unwrap();
    let mock = write_mock(dir.path(), "mock-agent-noisy.sh", NOISY_STDERR_MOCK);
    let backend = CursorBackend::new(mock);

    let mut session = backend
        .start(spec(dir.path(), "kranz-sess-noise", true))
        .await
        .unwrap();
    while next(&mut session).await.is_some() {}
    assert_eq!(
        session.exit_status(),
        Some(SessionExit::Completed),
        "a completed turn is never re-labeled by stderr noise"
    );
}

/// Emits a one-line result and exits 0 after CAPTURING the child env's
/// `$HOME` into `home-capture.txt` (the harness reads it back to find the
/// session-private home the hook-status install must have landed in).
const HOME_CAPTURE_MOCK: &str = r#"#!/bin/sh
echo "$HOME" > "__CAPTURE_DIR__/home-capture.txt"
cat <<'MOCK_EOF'
{"type":"system","subtype":"init","session_id":"cursor-mock-sess-home","model":"m"}
{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"cursor-mock-sess-home"}
MOCK_EOF
exit 0
"#;

/// Drive one worker run through [`kranz_engine::runner::run_worker`] with a
/// HOME-capturing mock and return the session-private HOME the child saw.
async fn run_home_capture(
    dir: &Path,
    mission: &str,
    cfg: &MissionConfig,
) -> (PathBuf, kranz_engine::runner::RunOutcome) {
    let paths = MissionPaths::new(dir, mission);
    let mut log =
        EventLog::acquire(&paths, mission, Duration::from_millis(0), LockForce::No).unwrap();
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
    let mock_body = HOME_CAPTURE_MOCK.replace("__CAPTURE_DIR__", &dir.display().to_string());
    let mock = write_mock(dir, "mock-agent-home.sh", &mock_body);
    let backend = CursorBackend::new(mock);
    let outcome = kranz_engine::runner::run_worker(
        &backend,
        &mut log,
        &paths,
        cfg,
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
    .expect("run_worker should complete");
    drop(log);
    let home =
        std::fs::read_to_string(dir.join("home-capture.txt")).expect("the mock captured its HOME");
    (PathBuf::from(home.trim()), outcome)
}

/// The opt-in lane end to end at the runner+backend seam: `hookStatus`
/// enabled AND a cursor worker ⇒ the run REGISTERS the per-run token in
/// the gitignored projection and the session-private HOME carries the
/// user-level hooks.json + spec file — while the tracked repo tree gets
/// NO `.cursor` writes (install hygiene at the spawn seam).
#[tokio::test]
async fn hook_status_signal_cursor_run_installs_the_lane_into_the_session_home() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = MissionConfig {
        worker: kranz_engine::types::RoleConfig {
            backend: Some("cursor".to_string()),
            ..MissionConfig::default().worker
        },
        hook_status: Some(kranz_engine::types::HookStatusConfig {
            enabled: true,
            endpoint: "http://127.0.0.1:9/api/hook-status".to_string(),
        }),
        ..MissionConfig::default()
    };

    let (home, outcome) = run_home_capture(dir.path(), "m-hook-lane", &cfg).await;
    assert!(
        matches!(outcome.exit, SessionExit::Completed),
        "{outcome:?}"
    );

    // The session-private HOME carries the lane install.
    let hooks_text = std::fs::read_to_string(home.join(".cursor").join("hooks.json"))
        .expect("hooks.json installed into the session HOME");
    let hooks: serde_json::Value = serde_json::from_str(&hooks_text).unwrap();
    assert_eq!(hooks["version"], 1);
    let command = hooks["hooks"]["sessionStart"][0]["command"]
        .as_str()
        .unwrap();
    assert!(command.contains("hook-status"), "{command}");

    // The registration landed in the gitignored projection, keyed by the
    // run id the spec file names.
    let proj_dir = kranz_engine::hook_status::hook_status_dir(dir.path()).join("m-hook-lane");
    let entries: Vec<_> = std::fs::read_dir(&proj_dir)
        .expect("registration written")
        .flatten()
        .collect();
    assert_eq!(entries.len(), 1, "exactly one run registered: {entries:?}");
    let registered: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(entries[0].path()).unwrap()).unwrap();
    let run_id = entries[0]
        .path()
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let views = kranz_engine::hook_status::read_mission_signals(dir.path(), "m-hook-lane");
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].run_id, run_id);
    assert!(views[0].signal.is_none(), "no hooks fired in the mock");
    assert!(registered["tokenHash"].as_str().is_some());

    // Install hygiene: the tracked tree got nothing (no `.cursor`, no
    // hooks.json outside the gitignored projection).
    assert!(
        !dir.path().join(".cursor").exists(),
        "spawning must never write hook config into the tracked tree"
    );

    let _ = std::fs::remove_dir_all(home.parent().unwrap_or(&home));
}

/// Hooks-disabled regression (the acceptance hint's headline): a default
/// config runs the SAME cursor worker with NO lane — no registration, no
/// hooks.json, a byte-identical pre-lane session.
#[tokio::test]
async fn hook_status_signal_cursor_run_without_opt_in_installs_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = MissionConfig::default();

    let (home, outcome) = run_home_capture(dir.path(), "m-hook-off", &cfg).await;
    assert!(
        matches!(outcome.exit, SessionExit::Completed),
        "{outcome:?}"
    );

    assert!(
        !home.join(".cursor").join("hooks.json").exists(),
        "hooks disabled is the byte-identical default: no hooks.json"
    );
    assert!(
        !kranz_engine::hook_status::hook_status_dir(dir.path()).exists(),
        "no projection dir is even created"
    );

    let _ = std::fs::remove_dir_all(home.parent().unwrap_or(&home));
}

/// Per-backend opt-in: an ENABLED lane on a non-hook-capable configured
/// backend (the claude default) seeds nothing — no registration, no
/// session install. (The claude backend would ignore the seed anyway;
/// gating at the runner avoids a registration no POST can ever arrive
/// for.)
#[tokio::test]
async fn hook_status_signal_enabled_lane_ignores_non_hook_capable_backends() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = MissionConfig {
        hook_status: Some(kranz_engine::types::HookStatusConfig {
            enabled: true,
            endpoint: "http://127.0.0.1:9/api/hook-status".to_string(),
        }),
        ..MissionConfig::default()
    };
    // worker.backend stays the claude default (None).

    let (home, outcome) = run_home_capture(dir.path(), "m-hook-claude", &cfg).await;
    assert!(
        matches!(outcome.exit, SessionExit::Completed),
        "{outcome:?}"
    );

    assert!(
        !kranz_engine::hook_status::hook_status_dir(dir.path()).exists(),
        "a non-hook-capable backend gets no registration"
    );
    assert!(
        !home.join(".cursor").join("hooks.json").exists(),
        "and no session install"
    );

    let _ = std::fs::remove_dir_all(home.parent().unwrap_or(&home));
}

/// Engine-level resumability: a worker run whose `agent` dies mid-line
/// fails honestly, and the mission event log it was appending to re-acquires
/// with contiguous seqs (the torn-line repair path in `event_log.rs` is what
/// makes the NEXT acquire clean) — the kill-mid-session resumable-log case.
#[tokio::test]
async fn backend_cursor_cli_death_mid_run_leaves_a_resumable_event_log() {
    let dir = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(dir.path(), "m-cursor-test");
    let mut log = EventLog::acquire(
        &paths,
        "m-cursor-test",
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

    let mock = write_mock(dir.path(), "mock-agent-die.sh", TORN_THEN_DIE_MOCK);
    let backend = CursorBackend::new(mock);
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
            && matches!(outcome.exit, SessionExit::Failed(_)),
        "a CLI dying mid-turn must record an honest failure, got {:?} / {:?}",
        outcome.result,
        outcome.exit
    );
    drop(log);

    // Re-acquire (the resume path): seqs are contiguous from 1 and the log
    // folds without corruption.
    let log = EventLog::acquire(
        &paths,
        "m-cursor-test",
        Duration::from_millis(0),
        LockForce::No,
    )
    .expect("log must re-acquire after a CLI-death crash");
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
