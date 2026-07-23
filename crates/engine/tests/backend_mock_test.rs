//! Integration tests for the mock agent backend (plan §8 "the seam").

use kranz_engine::auth_verify::AuthVerdict;
use kranz_engine::backend::{
    AgentBackend, AgentEvent, AgentSession, PromptMode, SessionExit, SessionSpec,
};
use kranz_engine::backend_mock::{
    mock_denied, mock_init, mock_result_text, mock_text, mock_tool_use, MockBackend, MockScript,
};
use kranz_engine::error::EngineError;
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::paths::MissionPaths;
use kranz_engine::runner::{run_validator, run_validator_in, run_worker};
use kranz_engine::types::{
    Assertion, AssertionCheck, Feature, FeatureOrigin, FeatureStatus, Milestone, MilestoneStatus,
    MissionConfig, Role,
};
use serde_json::json;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::timeout;

/// How long a `next_event` call must stay pending to count as "blocked".
const BLOCK_PROOF: Duration = Duration::from_millis(100);

fn spec(session_id: &str, prompt: PromptMode, writable: bool) -> SessionSpec {
    SessionSpec {
        cwd: std::env::temp_dir(),
        prompt,
        append_system_prompt: Some("role prompt".to_string()),
        model: "mock-model".to_string(),
        effort: "medium".to_string(),
        session_id: session_id.to_string(),
        resume: None,
        permission_mode: Some("acceptEdits".to_string()),
        allowed_tools: vec!["Bash(cargo test*)".to_string()],
        disallowed_tools: vec!["Bash(git push*)".to_string()],
        tools: vec![],
        writable,
        settings_json: None,
        json_schema: None,
        max_budget_usd: Some(1.0),
        max_turns: Some(10),
        env: HashMap::new(),
        sandbox: None,
    }
}

fn single_shot_spec(session_id: &str) -> SessionSpec {
    spec(
        session_id,
        PromptMode::SingleShot("do the thing".to_string()),
        true,
    )
}

fn streaming_spec(session_id: &str) -> SessionSpec {
    spec(
        session_id,
        PromptMode::Streaming("orchestrate".to_string()),
        false,
    )
}

async fn next(session: &mut Box<dyn AgentSession>) -> Option<AgentEvent> {
    session
        .next_event()
        .await
        .expect("next_event should not error")
}

/// Drain the stream to closure and return the text of the final Result event.
async fn final_result_text(session: &mut Box<dyn AgentSession>) -> String {
    let mut result = None;
    while let Some(event) = next(session).await {
        if let AgentEvent::Result { text, .. } = event {
            result = Some(text);
        }
    }
    result.expect("script should contain a Result event")
}

#[tokio::test]
async fn scripts_are_consumed_fifo_and_empty_queue_errors() {
    let backend = MockBackend::with_scripts(vec![
        MockScript::single_shot("first"),
        MockScript::single_shot("second"),
    ]);
    backend.push_script(MockScript::single_shot("third"));

    let mut s1 = backend.start(single_shot_spec("sid-1")).await.unwrap();
    let mut s2 = backend.start(single_shot_spec("sid-2")).await.unwrap();
    let mut s3 = backend.start(single_shot_spec("sid-3")).await.unwrap();

    assert_eq!(final_result_text(&mut s1).await, "first");
    assert_eq!(final_result_text(&mut s2).await, "second");
    assert_eq!(final_result_text(&mut s3).await, "third");

    let err = backend
        .start(single_shot_spec("sid-4"))
        .await
        .err()
        .expect("empty script queue must error");
    assert!(
        matches!(err, EngineError::Backend(_)),
        "unexpected error kind: {err:?}"
    );
    assert!(
        err.to_string().contains("no script queued"),
        "unexpected message: {err}"
    );
}

#[tokio::test]
async fn single_shot_yields_init_text_result_then_none_completed() {
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot("all done")]);
    let mut session = backend.start(single_shot_spec("sid-1")).await.unwrap();

    assert_eq!(session.session_id(), "sid-1");
    assert!(
        session.exit_status().is_none(),
        "exit must be unavailable before close"
    );

    match next(&mut session).await.expect("init event") {
        AgentEvent::Init {
            session_id,
            model,
            raw,
        } => {
            assert_eq!(session_id, "mock-session");
            assert_eq!(model, "mock-model");
            assert_eq!(raw["mock"], true);
        }
        other => panic!("expected Init, got {other:?}"),
    }
    match next(&mut session).await.expect("text event") {
        AgentEvent::Text { text, raw } => {
            assert_eq!(text, "all done");
            assert_eq!(raw["mock"], true);
        }
        other => panic!("expected Text, got {other:?}"),
    }
    match next(&mut session).await.expect("result event") {
        AgentEvent::Result {
            text,
            is_error,
            usage,
            cost_usd,
            num_turns,
            raw,
        } => {
            assert_eq!(text, "all done");
            assert!(!is_error);
            assert_eq!(usage.input, 1000);
            assert_eq!(usage.output, 200);
            assert_eq!(usage.cache_read, 0);
            assert_eq!(usage.cache_write, 0);
            assert_eq!(cost_usd, Some(0.01));
            assert_eq!(num_turns, Some(1));
            assert_eq!(raw["mock"], true);
        }
        other => panic!("expected Result, got {other:?}"),
    }

    assert!(next(&mut session).await.is_none());
    // Idempotent after close.
    assert!(next(&mut session).await.is_none());
    assert_eq!(session.exit_status(), Some(SessionExit::Completed));
}

#[tokio::test]
async fn single_shot_json_result_text_round_trips() {
    let report = serde_json::json!({
        "result": "pass",
        "summary": "implemented the widget",
        "filesTouched": ["src/widget.rs"],
    });
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot_json(&report)]);
    let mut session = backend.start(single_shot_spec("sid-json")).await.unwrap();

    let text = final_result_text(&mut session).await;
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("result text is JSON");
    assert_eq!(parsed, report);
    assert_eq!(session.exit_status(), Some(SessionExit::Completed));
}

#[tokio::test]
async fn streaming_blocks_until_message_then_yields_batch() {
    let script = MockScript::streaming(vec![mock_init("mock-session"), mock_text("intro")])
        .responding(vec![
            vec![mock_text("reply-1")],
            vec![
                mock_tool_use("Bash", "cargo test"),
                mock_denied("Bash", "git push"),
                mock_result_text("turn done"),
            ],
        ]);
    let backend = MockBackend::with_scripts(vec![script]);
    let mut session = backend.start(streaming_spec("sid-stream")).await.unwrap();

    assert!(matches!(
        next(&mut session).await,
        Some(AgentEvent::Init { .. })
    ));
    assert!(matches!(
        next(&mut session).await,
        Some(AgentEvent::Text { .. })
    ));

    // Queue exhausted: a streaming session must NOT return None early.
    let blocked = timeout(BLOCK_PROOF, session.next_event()).await;
    assert!(
        blocked.is_err(),
        "streaming next_event must block on an empty queue"
    );
    assert!(session.exit_status().is_none());

    session.send_user_message("continue please").await.unwrap();
    match next(&mut session).await.expect("first batch") {
        AgentEvent::Text { text, .. } => assert_eq!(text, "reply-1"),
        other => panic!("expected Text, got {other:?}"),
    }

    // First batch drained: blocks again until the next message.
    let blocked = timeout(BLOCK_PROOF, session.next_event()).await;
    assert!(blocked.is_err(), "must block between batches");

    session.send_user_message("and then?").await.unwrap();
    match next(&mut session).await.expect("tool use") {
        AgentEvent::ToolUse { tool, summary, .. } => {
            assert_eq!(tool, "Bash");
            assert_eq!(summary, "cargo test");
        }
        other => panic!("expected ToolUse, got {other:?}"),
    }
    match next(&mut session).await.expect("denied tool result") {
        AgentEvent::ToolResult { tool, denied, .. } => {
            assert_eq!(tool.as_deref(), Some("Bash"));
            assert!(denied);
        }
        other => panic!("expected ToolResult, got {other:?}"),
    }
    assert!(matches!(
        next(&mut session).await,
        Some(AgentEvent::Result { .. })
    ));

    assert_eq!(
        backend.injected_messages(),
        vec![vec!["continue please".to_string(), "and then?".to_string(),]]
    );
}

#[tokio::test]
async fn abort_closes_stream_with_aborted_exit() {
    let script = MockScript::streaming(vec![mock_text("hello")]);
    let backend = MockBackend::with_scripts(vec![script]);
    let mut session = backend.start(streaming_spec("sid-abort")).await.unwrap();

    assert!(matches!(
        next(&mut session).await,
        Some(AgentEvent::Text { .. })
    ));

    // Parked on the empty queue; timeout cancels the pending future.
    let blocked = timeout(BLOCK_PROOF, session.next_event()).await;
    assert!(
        blocked.is_err(),
        "streaming next_event must block before abort"
    );

    session.abort().await.unwrap();
    // Wakes/returns promptly with None instead of blocking again.
    let after = timeout(BLOCK_PROOF, session.next_event())
        .await
        .expect("next_event must not block after abort")
        .unwrap();
    assert!(after.is_none());
    assert_eq!(session.exit_status(), Some(SessionExit::Aborted));
}

#[tokio::test]
async fn abort_drops_pending_events_like_a_killed_process() {
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot("never seen")]);
    let mut session = backend.start(single_shot_spec("sid-kill")).await.unwrap();

    session.abort().await.unwrap();
    assert_eq!(session.exit_status(), Some(SessionExit::Aborted));
    assert!(
        next(&mut session).await.is_none(),
        "aborted session must not drain events"
    );
    // Abort after close must not overwrite a finished exit.
    let backend2 = MockBackend::with_scripts(vec![MockScript::single_shot("done")]);
    let mut finished = backend2.start(single_shot_spec("sid-done")).await.unwrap();
    final_result_text(&mut finished).await;
    assert_eq!(finished.exit_status(), Some(SessionExit::Completed));
    finished.abort().await.unwrap();
    assert_eq!(finished.exit_status(), Some(SessionExit::Completed));
}

#[tokio::test]
async fn send_user_message_errors_on_non_streaming_session() {
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot("ok")]);
    let mut session = backend.start(single_shot_spec("sid-1")).await.unwrap();

    let err = session.send_user_message("hello?").await.unwrap_err();
    assert!(
        matches!(err, EngineError::Backend(_)),
        "unexpected error kind: {err:?}"
    );
    assert!(
        err.to_string().contains("non-streaming"),
        "unexpected message: {err}"
    );
    // Rejected messages are not recorded.
    assert_eq!(backend.injected_messages(), vec![Vec::<String>::new()]);
}

#[tokio::test]
async fn started_specs_record_what_the_engine_asked_for() {
    let backend = MockBackend::with_scripts(vec![
        MockScript::single_shot("a"),
        MockScript::single_shot("b").with_session_id("override-sid"),
    ]);

    let mut spec1 = single_shot_spec("sid-first");
    spec1.model = "opus".to_string();
    spec1.effort = "high".to_string();
    let spec2 = streaming_spec("sid-second");

    let s1 = backend.start(spec1).await.unwrap();
    let s2 = backend.start(spec2).await.unwrap();

    let specs = backend.started_specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(specs[0].session_id, "sid-first");
    assert_eq!(specs[0].model, "opus");
    assert_eq!(specs[0].effort, "high");
    assert!(matches!(specs[0].prompt, PromptMode::SingleShot(ref p) if p == "do the thing"));
    assert_eq!(
        specs[0].allowed_tools,
        vec!["Bash(cargo test*)".to_string()]
    );
    assert_eq!(specs[1].session_id, "sid-second");
    assert!(matches!(specs[1].prompt, PromptMode::Streaming(ref p) if p == "orchestrate"));

    // Default session id comes from the spec; the script may override it.
    assert_eq!(s1.session_id(), "sid-first");
    assert_eq!(s2.session_id(), "override-sid");
}

#[tokio::test]
async fn injected_messages_align_with_start_order() {
    let backend = MockBackend::with_scripts(vec![
        MockScript::streaming(vec![mock_text("one")])
            .responding(vec![vec![mock_text("r1")], vec![mock_text("r2")]]),
        MockScript::streaming(vec![mock_text("two")]),
    ]);

    let mut s1 = backend.start(streaming_spec("sid-1")).await.unwrap();
    let mut s2 = backend.start(streaming_spec("sid-2")).await.unwrap();

    s1.send_user_message("alpha").await.unwrap();
    s2.send_user_message("gamma").await.unwrap();
    s1.send_user_message("beta").await.unwrap();
    // A message with no scripted batch is still recorded and yields nothing.
    let blocked = timeout(BLOCK_PROOF, s2.next_event()).await;
    // s2 has its initial event pending, drain it, then it should block.
    if let Ok(ev) = blocked {
        assert!(matches!(ev.unwrap(), Some(AgentEvent::Text { .. })));
        let blocked = timeout(BLOCK_PROOF, s2.next_event()).await;
        assert!(blocked.is_err(), "no batch scripted: s2 must keep blocking");
    }

    assert_eq!(
        backend.injected_messages(),
        vec![
            vec!["alpha".to_string(), "beta".to_string()],
            vec!["gamma".to_string()],
        ]
    );

    // s1 drains initial + both batches in order.
    let mut texts = Vec::new();
    for _ in 0..3 {
        match next(&mut s1).await.expect("scripted event") {
            AgentEvent::Text { text, .. } => texts.push(text),
            other => panic!("expected Text, got {other:?}"),
        }
    }
    assert_eq!(texts, vec!["one", "r1", "r2"]);
}

fn worker_report_json() -> serde_json::Value {
    json!({
        "result": "pass",
        "summary": "built the login endpoint",
        "filesTouched": ["src/login.rs"],
        "testsAdded": ["login_works"],
        "testEvidence": "test login_works ... ok",
        "dependenciesAdded": [],
        "knownGaps": [],
        "commits": ["abc123 [f-1] add login"]
    })
}

fn feature() -> Feature {
    Feature {
        id: "f-1".to_string(),
        title: "Add login".to_string(),
        spec: "Build the login endpoint".to_string(),
        validation_criteria: vec!["users can log in".to_string()],
        origin: FeatureOrigin::Plan,
        status: FeatureStatus::Pending,
        worker_runs: vec![],
        commits: vec![],
        respawns: 0,
    }
}

#[tokio::test]
async fn worker_session_carries_configured_tools_onto_the_spec() {
    let dir = tempfile::tempdir().unwrap();
    let p = MissionPaths::new(dir.path(), "m-test");
    let mut log = EventLog::acquire(&p, "m-test", Duration::from_millis(0), LockForce::No).unwrap();

    let mut cfg = MissionConfig::default();
    cfg.worker.tools = vec!["Bash".to_string(), "Read".to_string()];

    let backend =
        MockBackend::with_scripts(vec![MockScript::single_shot_json(&worker_report_json())]);
    run_worker(
        &backend,
        &mut log,
        &p,
        &cfg,
        &feature(),
        "ship auth",
        "Auth",
        None,
        None,
        None,
        &[],
        &[],
        AuthVerdict::Inconclusive,
    )
    .await
    .unwrap();

    let specs = backend.started_specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].tools, cfg.worker.tools);
}

fn validator_report_json() -> serde_json::Value {
    json!({ "findings": [], "summary": "all good" })
}

fn milestone() -> Milestone {
    Milestone {
        id: "ms-1".to_string(),
        title: "Ship auth".to_string(),
        features: vec![feature()],
        status: MilestoneStatus::Validating,
        fix_cycles: 0,
        start_sha: None,
        validator_guidance: None,
    }
}

#[tokio::test]
async fn functional_validator_tools_are_carried_and_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let p = MissionPaths::new(dir.path(), "m-test");
    let mut log = EventLog::acquire(&p, "m-test", Duration::from_millis(0), LockForce::No).unwrap();

    let mut cfg = MissionConfig::default();
    cfg.validator_functional.tools = vec![
        "Bash".to_string(),
        "Read".to_string(),
        "Glob".to_string(),
        "Grep".to_string(),
        "FakeBrowserTool".to_string(),
    ];

    let backend =
        MockBackend::with_scripts(vec![MockScript::single_shot_json(&validator_report_json())]);
    run_validator(
        &backend,
        &mut log,
        &p,
        &cfg,
        Role::ValidatorFunctional,
        &milestone(),
        &[],
        "start-sha",
        None,
        None,
        &[],
        &[],
        None,
    )
    .await
    .unwrap();

    let specs = backend.started_specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].tools, cfg.validator_functional.tools);
    assert!(specs[0]
        .allowed_tools
        .contains(&"FakeBrowserTool".to_string()));
    assert!(!specs[0].allowed_tools.contains(&"Bash".to_string()));
    assert!(specs[0].disallowed_tools.contains(&"Write".to_string()));
    assert!(specs[0].disallowed_tools.contains(&"Edit".to_string()));
}

#[tokio::test]
async fn scrutiny_validator_does_not_get_extra_tools_folded_into_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let p = MissionPaths::new(dir.path(), "m-test");
    let mut log = EventLog::acquire(&p, "m-test", Duration::from_millis(0), LockForce::No).unwrap();

    let mut cfg = MissionConfig::default();
    cfg.validator_scrutiny.tools = vec![
        "Bash".to_string(),
        "Read".to_string(),
        "Glob".to_string(),
        "Grep".to_string(),
        "FakeBrowserTool".to_string(),
    ];

    let backend =
        MockBackend::with_scripts(vec![MockScript::single_shot_json(&validator_report_json())]);
    run_validator(
        &backend,
        &mut log,
        &p,
        &cfg,
        Role::ValidatorScrutiny,
        &milestone(),
        &[],
        "start-sha",
        None,
        None,
        &[],
        &[],
        None,
    )
    .await
    .unwrap();

    let specs = backend.started_specs();
    assert_eq!(specs.len(), 1);
    assert!(!specs[0]
        .allowed_tools
        .contains(&"FakeBrowserTool".to_string()));
}

#[tokio::test]
async fn scrutiny_task_and_permissions_carry_no_contract_commands() {
    // Scrutiny/mechanical split: the scrutiny validator inspects the range
    // read-only — its task advertises no commands and its permission
    // profile permits none; the functional validator keeps both.
    let dir = tempfile::tempdir().unwrap();
    let p = MissionPaths::new(dir.path(), "m-test");
    let mut log = EventLog::acquire(&p, "m-test", Duration::from_millis(0), LockForce::No).unwrap();
    let cfg = MissionConfig::default();
    let contract = vec![Assertion {
        id: "a-1".into(),
        statement: "the build succeeds".into(),
        check: AssertionCheck::Command,
        command: Some("cargo test --workspace".into()),
    }];

    for role in [Role::ValidatorScrutiny, Role::ValidatorFunctional] {
        let backend =
            MockBackend::with_scripts(vec![MockScript::single_shot_json(&validator_report_json())]);
        run_validator(
            &backend,
            &mut log,
            &p,
            &cfg,
            role,
            &milestone(),
            &contract,
            "start-sha",
            None,
            None,
            &[],
            &[],
            None,
        )
        .await
        .unwrap();

        let specs = backend.started_specs();
        assert_eq!(specs.len(), 1);
        let PromptMode::SingleShot(task) = &specs[0].prompt else {
            panic!("validator task must be single-shot: {:?}", specs[0].prompt);
        };
        if role == Role::ValidatorScrutiny {
            assert!(
                !task.contains("Allowed commands"),
                "scrutiny task must not advertise commands: {task}"
            );
            assert!(
                !specs[0]
                    .allowed_tools
                    .iter()
                    .any(|a| a.starts_with("Bash(cargo")),
                "scrutiny must not be permitted the cargo gate: {:?}",
                specs[0].allowed_tools
            );
        } else {
            assert!(
                task.contains("Allowed commands"),
                "functional task must advertise commands: {task}"
            );
            assert!(
                specs[0]
                    .allowed_tools
                    .iter()
                    .any(|a| a == "Bash(cargo test --workspace*)"),
                "functional must be permitted the cargo gate: {:?}",
                specs[0].allowed_tools
            );
        }
        // Both roles keep plain-git inspect access.
        assert!(
            specs[0]
                .allowed_tools
                .iter()
                .any(|a| a == "Bash(git diff*)"),
            "{role:?} lost git-inspect access: {:?}",
            specs[0].allowed_tools
        );
    }
}

#[tokio::test]
async fn engine_run_contract_results_reach_functional_task_only() {
    // Validator repair 3/5: captured contract results ride the functional
    // validator's task; scrutiny's split task stays diff+criteria only.
    let dir = tempfile::tempdir().unwrap();
    let p = MissionPaths::new(dir.path(), "m-test");
    let mut log = EventLog::acquire(&p, "m-test", Duration::from_millis(0), LockForce::No).unwrap();
    let cfg = MissionConfig::default();
    let results = "- [a-1] `cargo test` → FAIL\nerror[E0308]: mismatched types\n";

    for role in [Role::ValidatorScrutiny, Role::ValidatorFunctional] {
        let backend =
            MockBackend::with_scripts(vec![MockScript::single_shot_json(&validator_report_json())]);
        run_validator_in(
            &backend,
            &mut log,
            &p,
            &cfg,
            role,
            &milestone(),
            &[],
            "start-sha",
            None,
            dir.path(),
            None,
            &[],
            &[],
            None,
            Some(results),
        )
        .await
        .unwrap();

        let specs = backend.started_specs();
        assert_eq!(specs.len(), 1);
        let PromptMode::SingleShot(task) = &specs[0].prompt else {
            panic!("validator task must be single-shot");
        };
        if role == Role::ValidatorScrutiny {
            assert!(
                !task.contains("Contract command results"),
                "scrutiny must not see engine-run results: {task}"
            );
        } else {
            assert!(
                task.contains("Contract command results"),
                "functional must see engine-run results: {task}"
            );
            assert!(task.contains("[a-1] `cargo test` → FAIL"), "{task}");
            assert!(task.contains("do NOT re-run"), "{task}");
        }
    }
}

// ---------------------------------------------------------------------------
// MockScript file-write seam (mock workers can deliver real commits)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn writes_file_creates_the_file_in_the_session_cwd() {
    let cwd = tempfile::tempdir().unwrap();
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot("done")
        .writes_file("nested/dir/output.txt", "hello from the mock worker\n")]);

    let mut session = backend
        .start(SessionSpec {
            cwd: cwd.path().to_path_buf(),
            ..single_shot_spec("write-session")
        })
        .await
        .expect("start session");

    // Drain the scripted stream — the write happens at start(), before any
    // event is pumped, but draining proves the write doesn't interfere with
    // ordinary scripted-event behaviour.
    final_result_text(&mut session).await;

    let written = std::fs::read_to_string(cwd.path().join("nested/dir/output.txt"))
        .expect("mock write should have created the file");
    assert_eq!(written, "hello from the mock worker\n");
}

#[tokio::test]
async fn default_script_writes_nothing() {
    let cwd = tempfile::tempdir().unwrap();
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot("done")]);

    backend
        .start(SessionSpec {
            cwd: cwd.path().to_path_buf(),
            ..single_shot_spec("no-write-session")
        })
        .await
        .expect("start session");

    let entries: Vec<_> = std::fs::read_dir(cwd.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert!(
        entries.is_empty(),
        "no script writes means no files should appear: {entries:?}"
    );
}
