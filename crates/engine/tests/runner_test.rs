//! Integration tests for permission profiles (plan §4.7) and the session
//! runner (plan §4.6), driven entirely through the mock backend.

use kranz_engine::auth_verify::AuthVerdict;
use kranz_engine::backend::{AgentEvent, PromptMode, SessionExit, SessionSpec};
use kranz_engine::backend_claude::parse_stream_line;
use kranz_engine::backend_mock::{
    mock_denied, mock_init, mock_result_text, mock_text, mock_tool_use, MockBackend, MockScript,
};
use kranz_engine::control::{self, ControlWatcher};
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::events::{Event, EventKind};
use kranz_engine::paths::MissionPaths;
use kranz_engine::permissions::{self, PermissionProfile};
use kranz_engine::runner::{
    parse_validator_report, parse_worker_report, run_session, run_validator, run_worker,
    run_worker_in_buffered, RunMeta,
};
use kranz_engine::types::{
    Assertion, AssertionCheck, ControlCommand, Feature, FeatureOrigin, FeatureStatus, Milestone,
    MilestoneStatus, MissionConfig, Role, RunResult, TokenUsage, WorkerIsolation,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::timeout;

const MISSION: &str = "m-test";

/// Generous bound proving a cancelled run does not hang.
const HANG_PROOF: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn paths(dir: &std::path::Path) -> MissionPaths {
    MissionPaths::new(dir, MISSION)
}

/// Acquire the log (throttle 0 → deltas flush immediately) and seed it with
/// mission.created, as every real mission log starts.
fn seeded_log(p: &MissionPaths) -> EventLog {
    let mut log = EventLog::acquire(p, MISSION, Duration::from_millis(0), LockForce::No).unwrap();
    log.append(EventKind::MissionCreated {
        goal: "test the runner".to_string(),
        base_branch: "main".to_string(),
        mission_branch: format!("kranz/mission-{MISSION}"),
        config: MissionConfig::default(),
    })
    .unwrap();
    log
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

fn milestone() -> Milestone {
    Milestone {
        id: "ms-1".to_string(),
        title: "Auth".to_string(),
        features: vec![feature()],
        status: MilestoneStatus::Validating,
        fix_cycles: 0,
        start_sha: Some("abc123".to_string()),
        validator_guidance: None,
    }
}

fn assertion(id: &str, command: Option<&str>) -> Assertion {
    Assertion {
        id: id.to_string(),
        statement: format!("assertion {id} holds"),
        check: if command.is_some() {
            AssertionCheck::Command
        } else {
            AssertionCheck::AgentJudgement
        },
        command: command.map(str::to_string),
        pty_script: None,
    }
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

fn session_spec(prompt: PromptMode) -> SessionSpec {
    SessionSpec {
        cwd: std::env::temp_dir(),
        prompt,
        append_system_prompt: Some("role prompt".to_string()),
        model: "mock-model".to_string(),
        effort: "medium".to_string(),
        session_id: "11111111-1111-4111-8111-111111111111".to_string(),
        resume: None,
        permission_mode: Some("acceptEdits".to_string()),
        allowed_tools: vec!["Bash".to_string()],
        disallowed_tools: vec!["Bash(git push*)".to_string()],
        tools: vec![],
        writable: true,
        settings_json: None,
        json_schema: None,
        max_budget_usd: Some(1.0),
        max_turns: Some(10),
        env: HashMap::new(),
        sandbox: None,
        hook_status: None,
    }
}

fn worker_meta(run_id: &str) -> RunMeta {
    RunMeta {
        backend: None,
        run_id: run_id.to_string(),
        role: Role::Worker,
        feature_id: Some("f-1".to_string()),
        milestone_id: None,
        model: "mock-model".to_string(),
        prompt_hash: "deadbeef0000".to_string(),
        executor_route: None,
    }
}

/// Events of the log at `p`, in order (log must be dropped/flushed first).
fn read_log(p: &MissionPaths) -> Vec<Event> {
    EventLog::read_events(&p.events_file()).unwrap()
}

fn event_types(events: &[Event]) -> Vec<&'static str> {
    events.iter().map(|e| e.kind.type_name()).collect()
}

// ---------------------------------------------------------------------------
// Permissions (plan §4.7)
// ---------------------------------------------------------------------------

#[test]
fn worker_profile_denies_push_publish_network_and_config_extras() {
    let cfg = MissionConfig {
        deny_patterns: vec![
            "rm -rf /*".to_string(),    // bare command → wrapped
            "Bash(dd*)".to_string(),    // already a tool rule (has parens)
            "NotebookEdit".to_string(), // known tool name → kept verbatim
        ],
        worker_isolation: WorkerIsolation::Checkout,
        ..MissionConfig::default()
    };
    let profile = permissions::for_role(Role::Worker, &cfg, &[], &[], &[]);

    assert_eq!(profile.permission_mode.as_deref(), Some("acceptEdits"));
    assert_eq!(profile.allowed_tools, vec!["Bash".to_string()]);
    assert_eq!(profile.tools, None);

    for expected in [
        "Bash(git push*)",
        "Bash(git remote add*)",
        "Bash(npm publish*)",
        "Bash(yarn publish*)",
        "Bash(pnpm publish*)",
        "Bash(cargo publish*)",
        "Bash(twine*)",
        "Bash(gem push*)",
        "Bash(sudo*)",
        "Bash(curl*)",
        "Bash(wget*)",
        "WebFetch",
        "WebSearch",
        // config extras:
        "Bash(rm -rf /*)",
        "Bash(dd*)",
        "NotebookEdit",
    ] {
        assert!(
            profile.disallowed_tools.iter().any(|d| d == expected),
            "worker deny list missing {expected:?}: {:?}",
            profile.disallowed_tools
        );
    }
}

#[test]
fn orchestrator_profile_is_read_only() {
    let cfg = MissionConfig::default();
    let profile = permissions::for_role(Role::Orchestrator, &cfg, &[], &[], &[]);

    assert_eq!(profile.permission_mode.as_deref(), Some("default"));
    for expected in [
        "Read",
        "Glob",
        "Grep",
        "Bash(git log*)",
        "Bash(git diff*)",
        "Bash(git show*)",
        "Bash(git status*)",
        "Bash(git rev-parse*)",
        "Bash(git branch)",
        "Bash(git branch --list*)",
        "Bash(git branch --show-current)",
        "Bash(git branch -a)",
        "Bash(git branch -r)",
        "Bash(git branch --contains*)",
        "Bash(git tag)",
        "Bash(git tag --list*)",
        "Bash(git tag -l*)",
        "Bash(git tag --contains*)",
    ] {
        assert!(
            profile.allowed_tools.iter().any(|a| a == expected),
            "orchestrator allow list missing {expected:?}"
        );
    }
    // No write access anywhere in the allow list.
    assert!(!profile
        .allowed_tools
        .iter()
        .any(|a| a == "Write" || a == "Edit"));
    for expected in [
        "Write",
        "Edit",
        "NotebookEdit",
        "WebFetch",
        "WebSearch",
        "Bash(git push*)",
    ] {
        assert!(
            profile.disallowed_tools.iter().any(|d| d == expected),
            "orchestrator deny list missing {expected:?}"
        );
    }
}

/// Mirror of the CLI's `Bash(...)` tool-rule matching used by the allow
/// lists in this crate: a rule ending in `*` prefix-matches the command,
/// otherwise the command must match exactly. Non-Bash rules never match.
fn bash_rule_covers(rule: &str, command: &str) -> bool {
    let Some(inner) = rule.strip_prefix("Bash(").and_then(|r| r.strip_suffix(')')) else {
        return false;
    };
    match inner.strip_suffix('*') {
        Some(prefix) => command.starts_with(prefix),
        None => command == inner,
    }
}

/// Regression test: `Bash(git branch*)` / `Bash(git tag*)` used to be in the
/// read-only allow lists and prefix-matched ref-mutating commands like
/// `git branch -D main` and `git tag -d v1`. The allow lists must cover the
/// read-only listing forms without covering any mutating form.
#[test]
fn read_only_git_allows_cover_listing_but_not_ref_mutation() {
    let cfg = MissionConfig::default();
    for role in [
        Role::Orchestrator,
        Role::ValidatorScrutiny,
        Role::ValidatorFunctional,
    ] {
        let profile = permissions::for_role(role, &cfg, &[], &[], &[]);

        for mutating in [
            "git branch -D x",
            "git branch -d topic",
            "git branch -f main abc123",
            "git branch -m old new",
            "git branch --force main abc123",
            "git branch new-branch",
            "git tag -d v1",
            "git tag v2",
            "git tag -f v1 abc123",
            "git tag -a v3 -m msg",
        ] {
            assert!(
                !profile
                    .allowed_tools
                    .iter()
                    .any(|a| bash_rule_covers(a, mutating)),
                "{role:?} allow list covers mutating command {mutating:?}: {:?}",
                profile.allowed_tools
            );
        }

        for listing in [
            "git branch",
            "git branch --list",
            "git branch --list kranz/*",
            "git branch --show-current",
            "git branch -a",
            "git branch -r",
            "git branch --contains abc123",
            "git tag",
            "git tag --list",
            "git tag --list v1.*",
            "git tag -l v1.*",
            "git tag --contains abc123",
        ] {
            assert!(
                profile
                    .allowed_tools
                    .iter()
                    .any(|a| bash_rule_covers(a, listing)),
                "{role:?} allow list does not cover read-only command {listing:?}: {:?}",
                profile.allowed_tools
            );
        }
    }
}

#[test]
fn validator_profile_allows_contract_commands_as_bash_patterns() {
    let cfg = MissionConfig {
        allow_validator_commands: vec!["npm run lint".to_string()],
        worker_isolation: WorkerIsolation::Checkout,
        ..MissionConfig::default()
    };
    let commands = vec!["cargo test --all".to_string()];

    for role in [Role::ValidatorScrutiny, Role::ValidatorFunctional] {
        let profile = permissions::for_role(role, &cfg, &commands, &[], &[]);
        assert_eq!(profile.permission_mode.as_deref(), Some("default"));
        for expected in ["Read", "Bash(git diff*)"] {
            assert!(
                profile.allowed_tools.iter().any(|a| a == expected),
                "{role:?} allow list missing {expected:?}: {:?}",
                profile.allowed_tools
            );
        }
        for expected in [
            "Write",
            "Edit",
            "NotebookEdit",
            "WebFetch",
            "WebSearch",
            "Bash(git push*)",
        ] {
            assert!(
                profile.disallowed_tools.iter().any(|d| d == expected),
                "{role:?} deny list missing {expected:?}"
            );
        }
    }

    // Scrutiny/mechanical split: only the functional validator is permitted
    // the contract/config commands; scrutiny stays read-only + plain git.
    let functional = permissions::for_role(Role::ValidatorFunctional, &cfg, &commands, &[], &[]);
    for expected in ["Bash(cargo test --all*)", "Bash(npm run lint*)"] {
        assert!(
            functional.allowed_tools.iter().any(|a| a == expected),
            "functional allow list missing {expected:?}"
        );
    }
    let scrutiny = permissions::for_role(Role::ValidatorScrutiny, &cfg, &commands, &[], &[]);
    for forbidden in ["Bash(cargo test --all*)", "Bash(npm run lint*)"] {
        assert!(
            !scrutiny.allowed_tools.iter().any(|a| a == forbidden),
            "scrutiny must not permit {forbidden:?}: {:?}",
            scrutiny.allowed_tools
        );
    }
    // Operator grants still fold into both roles.
    for role in [Role::ValidatorScrutiny, Role::ValidatorFunctional] {
        let granted = permissions::for_role(role, &cfg, &[], &["cargo fmt".to_string()], &[]);
        assert!(
            granted
                .allowed_tools
                .iter()
                .any(|a| a == "Bash(cargo fmt*)"),
            "{role:?} lost an operator grant: {:?}",
            granted.allowed_tools
        );
    }
}

#[test]
fn dangerously_allow_all_bypasses_every_role() {
    let cfg = MissionConfig {
        dangerously_allow_all: true,
        worker_isolation: WorkerIsolation::Checkout,
        ..MissionConfig::default()
    };
    for role in [
        Role::Worker,
        Role::Orchestrator,
        Role::ValidatorScrutiny,
        Role::ValidatorFunctional,
    ] {
        let profile = permissions::for_role(role, &cfg, &["cargo test".to_string()], &[], &[]);
        assert_eq!(
            profile.permission_mode.as_deref(),
            Some("bypassPermissions")
        );
        assert!(profile.allowed_tools.is_empty());
        assert!(profile.disallowed_tools.is_empty());
    }
}

#[test]
fn apply_copies_profile_onto_spec() {
    let profile = PermissionProfile {
        permission_mode: Some("default".to_string()),
        tools: Some(vec!["Bash".to_string()]),
        allowed_tools: vec!["Read".to_string()],
        disallowed_tools: vec!["Write".to_string()],
    };
    let mut spec = session_spec(PromptMode::SingleShot("task".to_string()));
    permissions::apply(profile, &mut spec);

    assert_eq!(spec.permission_mode.as_deref(), Some("default"));
    assert_eq!(spec.allowed_tools, vec!["Read".to_string()]);
    assert_eq!(spec.disallowed_tools, vec!["Write".to_string()]);
}

// ---------------------------------------------------------------------------
// run_session
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_session_happy_path_pass_report_events_and_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);

    let backend =
        MockBackend::with_scripts(vec![MockScript::single_shot_json(&worker_report_json())]);
    let spec = session_spec(PromptMode::SingleShot("build it".to_string()));
    let outcome = run_session(&backend, spec, &mut log, &p, worker_meta("run-1"), None)
        .await
        .unwrap();

    assert_eq!(outcome.run_id, "run-1");
    assert_eq!(outcome.result, RunResult::Pass);
    assert_eq!(outcome.exit, SessionExit::Completed);
    assert_eq!(outcome.denied_count, 0);
    // Usage/cost from the mock's standard result event.
    assert_eq!(
        outcome.usage,
        TokenUsage {
            input: 1000,
            output: 200,
            cache_read: 0,
            cache_write: 0
        }
    );
    assert_eq!(outcome.cost_usd, Some(0.01));
    let report = outcome.report.expect("worker report should parse");
    assert_eq!(report.result, RunResult::Pass);
    assert_eq!(report.summary, "built the login endpoint");
    assert!(outcome.validator_report.is_none());

    // Event log: mission.created, worker.spawned, ≥1 worker.message, worker.completed.
    drop(log);
    let events = read_log(&p);
    let types = event_types(&events);
    assert_eq!(types[0], "mission.created");
    assert_eq!(types[1], "worker.spawned");
    assert_eq!(*types.last().unwrap(), "worker.completed");
    assert!(
        types.iter().filter(|t| **t == "worker.message").count() >= 1,
        "expected at least one worker.message: {types:?}"
    );

    match &events[1].kind {
        EventKind::WorkerSpawned {
            run_id,
            role,
            sdk_session_id,
            transcript_path,
            ..
        } => {
            assert_eq!(run_id, "run-1");
            assert_eq!(*role, Role::Worker);
            assert_eq!(sdk_session_id, "11111111-1111-4111-8111-111111111111");
            assert_eq!(transcript_path, "runs/run-1.jsonl");
        }
        other => panic!("expected worker.spawned, got {other:?}"),
    }
    match &events.last().unwrap().kind {
        EventKind::WorkerCompleted {
            run_id,
            result,
            tokens,
            cost_usd,
            report,
        } => {
            assert_eq!(run_id, "run-1");
            assert_eq!(*result, RunResult::Pass);
            assert_eq!(
                *tokens,
                TokenUsage {
                    input: 1000,
                    output: 200,
                    cache_read: 0,
                    cache_write: 0
                }
            );
            assert_eq!(*cost_usd, Some(0.01));
            assert!(report.is_some());
        }
        other => panic!("expected worker.completed, got {other:?}"),
    }

    // Transcript: file exists, one JSON line per scripted event.
    let transcript = std::fs::read_to_string(p.transcript_file("run-1")).unwrap();
    let lines: Vec<&str> = transcript.lines().collect();
    assert_eq!(lines.len(), 3, "init + text + result");
    for line in lines {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|e| panic!("transcript line is not JSON ({e}): {line}"));
    }
}

#[tokio::test]
async fn run_session_tags_denied_tool_results() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);

    let script = MockScript {
        events: vec![
            mock_init("mock-session"),
            mock_tool_use("Bash", "git push origin main"),
            mock_denied("Bash", "git push origin main"),
            mock_result_text("stopped"),
        ],
        ..Default::default()
    };
    let backend = MockBackend::with_scripts(vec![script]);
    let spec = session_spec(PromptMode::SingleShot("try to push".to_string()));
    let outcome = run_session(&backend, spec, &mut log, &p, worker_meta("run-d"), None)
        .await
        .unwrap();

    assert_eq!(outcome.denied_count, 1);

    drop(log);
    let events = read_log(&p);
    let denied: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::WorkerMessage { tag, .. } if tag == "denied"))
        .collect();
    assert_eq!(
        denied.len(),
        1,
        "exactly one denied worker.message: {events:?}"
    );
    match &denied[0].kind {
        EventKind::WorkerMessage {
            run_id, content, ..
        } => {
            assert_eq!(run_id, "run-d");
            assert!(content.contains("git push"), "denied content: {content}");
        }
        _ => unreachable!(),
    }
    // The tool-use itself is tagged tool-use, not denied.
    assert!(events.iter().any(
        |e| matches!(&e.kind, EventKind::WorkerMessage { tag, content, .. }
            if tag == "tool-use" && content.starts_with("Bash: "))
    ));
}

/// grant-request-decision-flow foundation: `denied_commands` must capture the
/// denied SHELL command from a REAL Claude stream — parsed through the actual
/// `parse_stream_line`, not the fabricated `mock_denied` shape. This is the
/// exact chain a prior attempt got wrong: a denied Claude `tool_result`
/// carries `tool: None` (the tool name is only on the preceding `tool_use`),
/// so the command must be correlated positionally from that `tool_use`.
#[tokio::test]
async fn denied_commands_captured_from_a_real_claude_stream() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);

    // A real assistant `tool_use` (Bash) and a real permission-denied
    // `tool_result`, both through the production parser.
    let tool_use_line = json!({
        "type": "assistant",
        "message": { "id": "m1", "content": [
            { "type": "tool_use", "name": "Bash", "input": { "command": "gc lint --strict" } }
        ] }
    })
    .to_string();
    let denied_line = json!({
        "type": "user",
        "message": { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "toolu_01",
              "content": "Permission denied: Bash(gc lint --strict) requires approval",
              "is_error": true }
        ] }
    })
    .to_string();

    // Sanity: the parser yields exactly the shapes the correlation relies on —
    // a Bash ToolUse whose summary is the command, and a denied ToolResult
    // whose `tool` is None.
    let parsed_use = parse_stream_line(&tool_use_line);
    assert!(matches!(&parsed_use[0],
        AgentEvent::ToolUse { tool, summary, .. } if tool == "Bash" && summary == "gc lint --strict"));
    let parsed_denied = parse_stream_line(&denied_line);
    assert!(matches!(
        &parsed_denied[0],
        AgentEvent::ToolResult {
            tool: None,
            denied: true,
            ..
        }
    ));

    let mut events = vec![mock_init("s-real")];
    events.extend(parsed_use);
    events.extend(parsed_denied);
    events.push(mock_result_text("stopped: needs approval"));
    let script = MockScript {
        events,
        ..Default::default()
    };
    let backend = MockBackend::with_scripts(vec![script]);
    let spec = session_spec(PromptMode::SingleShot("run the check".to_string()));
    let outcome = run_session(&backend, spec, &mut log, &p, worker_meta("run-real"), None)
        .await
        .unwrap();

    assert_eq!(outcome.denied_count, 1);
    assert_eq!(
        outcome.denied_commands,
        vec!["gc lint --strict".to_string()],
        "the denied Bash command must be correlated from the preceding tool_use"
    );
}

/// The capture must also fire for the Codex backend, whose shell ToolUse is
/// named `command_execution` (not `Bash`) but likewise carries the literal
/// command in its summary. A prior review found the runner's `bash`-only guard
/// silently dropped these — the command was right there but rejected.
#[tokio::test]
async fn denied_commands_captured_from_codex_command_execution() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);

    let script = MockScript {
        events: vec![
            mock_init("s-codex"),
            // Codex's shell tool: tool name "command_execution", summary = cmd.
            mock_tool_use("command_execution", "gc scan --all"),
            // The denied result (its own `tool`/`summary` are irrelevant — the
            // command is correlated from the preceding ToolUse).
            mock_denied("command_execution", "sandbox refused"),
            mock_result_text("stopped: needs approval"),
        ],
        ..Default::default()
    };
    let backend = MockBackend::with_scripts(vec![script]);
    let spec = session_spec(PromptMode::SingleShot("run the scan".to_string()));
    let outcome = run_session(&backend, spec, &mut log, &p, worker_meta("run-codex"), None)
        .await
        .unwrap();

    assert_eq!(outcome.denied_count, 1);
    assert_eq!(
        outcome.denied_commands,
        vec!["gc scan --all".to_string()],
        "a Codex command_execution denial must be captured, not dropped"
    );
}

#[tokio::test]
async fn run_session_without_parseable_report_is_partial() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);

    let backend = MockBackend::with_scripts(vec![MockScript::single_shot("no json here at all")]);
    let spec = session_spec(PromptMode::SingleShot("build it".to_string()));
    let outcome = run_session(&backend, spec, &mut log, &p, worker_meta("run-g"), None)
        .await
        .unwrap();

    assert_eq!(outcome.exit, SessionExit::Completed);
    assert!(outcome.report.is_none());
    assert_eq!(outcome.result, RunResult::Partial);
}

#[tokio::test]
async fn run_session_report_downgrades_provisional_pass() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);

    let report = json!({ "result": "partial", "summary": "ran out of budget" });
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot_json(&report)]);
    let spec = session_spec(PromptMode::SingleShot("build it".to_string()));
    let outcome = run_session(&backend, spec, &mut log, &p, worker_meta("run-p"), None)
        .await
        .unwrap();

    assert_eq!(outcome.exit, SessionExit::Completed);
    assert_eq!(outcome.result, RunResult::Partial);
    assert_eq!(outcome.report.unwrap().result, RunResult::Partial);
}

#[tokio::test]
async fn run_session_cancellation_aborts_and_reports_partial() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);

    // Streaming session that never ends on its own: after the initial events
    // it parks waiting for injected messages that never come.
    let script = MockScript::streaming(vec![mock_init("mock-session"), mock_text("working...")]);
    let backend = MockBackend::with_scripts(vec![script]);
    let spec = session_spec(PromptMode::Streaming("keep working".to_string()));

    // Pre-armed cancel: Notify stores the permit, so the runner sees it as
    // soon as it selects on the flag.
    let cancel = Arc::new(Notify::new());
    cancel.notify_one();

    let outcome = timeout(
        HANG_PROOF,
        run_session(
            &backend,
            spec,
            &mut log,
            &p,
            worker_meta("run-c"),
            Some(cancel),
        ),
    )
    .await
    .expect("cancelled run must not hang")
    .unwrap();

    assert_eq!(outcome.exit, SessionExit::Aborted);
    assert_eq!(outcome.result, RunResult::Partial);

    drop(log);
    let events = read_log(&p);
    match &events.last().unwrap().kind {
        EventKind::WorkerCompleted { result, .. } => assert_eq!(*result, RunResult::Partial),
        other => panic!("expected worker.completed, got {other:?}"),
    }
}

/// Regression (interrupt loss): the ControlWatcher fires BEFORE run_session
/// ever polls the cancel notify — modelling a fire while `backend.start()`
/// is still in flight. The `notify_one` permit must be stored so the run
/// still aborts; with `notify_waiters` the fire woke nobody and the run hung
/// forever on the never-ending stream.
#[tokio::test]
async fn interrupt_fired_before_first_poll_still_aborts_the_run() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);

    control::enqueue(
        &p,
        &ControlCommand::Msg {
            text: "stop".into(),
            interrupt: true,
        },
    )
    .unwrap();
    let cancel = Arc::new(Notify::new());
    // Run the watcher to completion first: it fires and returns while nobody
    // is registered on the notify.
    timeout(
        HANG_PROOF,
        ControlWatcher::wait_for_interrupt(
            p.clone(),
            Duration::from_millis(10),
            Arc::clone(&cancel),
        ),
    )
    .await
    .expect("watcher must fire and return");

    // Never-ending streaming session: only the stored permit can end it.
    let script = MockScript::streaming(vec![mock_init("mock-session"), mock_text("working...")]);
    let backend = MockBackend::with_scripts(vec![script]);
    let spec = session_spec(PromptMode::Streaming("keep working".to_string()));

    let outcome = timeout(
        HANG_PROOF,
        run_session(
            &backend,
            spec,
            &mut log,
            &p,
            worker_meta("run-pre"),
            Some(cancel),
        ),
    )
    .await
    .expect("a pre-fired interrupt must abort the run, not hang")
    .unwrap();

    assert_eq!(outcome.exit, SessionExit::Aborted);
    assert_eq!(outcome.result, RunResult::Partial);
}

#[tokio::test]
async fn run_session_scrubs_credentials_from_log_and_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);

    let token = "ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123";
    let script = MockScript {
        events: vec![
            mock_init("mock-session"),
            mock_text(&format!("found a token: {token} in .env")),
            mock_result_text("done"),
        ],
        ..Default::default()
    };
    let backend = MockBackend::with_scripts(vec![script]);
    let spec = session_spec(PromptMode::SingleShot("scan".to_string()));
    run_session(&backend, spec, &mut log, &p, worker_meta("run-s"), None)
        .await
        .unwrap();

    drop(log);
    let raw_log = std::fs::read_to_string(p.events_file()).unwrap();
    assert!(!raw_log.contains(token), "event log leaked the token");
    let events = read_log(&p);
    let text_content = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::WorkerMessage { tag, content, .. } if tag == "text" => Some(content.clone()),
            _ => None,
        })
        .expect("a text worker.message should exist");
    assert!(
        text_content.contains("[REDACTED]"),
        "content: {text_content}"
    );
    assert!(!text_content.contains(token));

    let transcript = std::fs::read_to_string(p.transcript_file("run-s")).unwrap();
    assert!(!transcript.contains(token), "transcript leaked the token");
    assert!(transcript.contains("[REDACTED]"));
}

/// Regression (structured-field leak): a credential inside a WorkerReport
/// field (testEvidence etc.) must be redacted BEFORE the report is parsed —
/// otherwise it lands verbatim in the `worker.completed` event on
/// events.jsonl, bypassing the transcript/message scrubbing.
#[tokio::test]
async fn run_session_scrubs_worker_report_structured_fields() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);

    let token = "ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123";
    let report = json!({
        "result": "pass",
        "summary": format!("done; used {token} for the API"),
        "testEvidence": format!("curl with {token} returned 200"),
    });
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot_json(&report)]);
    let spec = session_spec(PromptMode::SingleShot("build".to_string()));
    let outcome = run_session(&backend, spec, &mut log, &p, worker_meta("run-r"), None)
        .await
        .unwrap();

    // The parsed report is redacted (it feeds worker.completed and the
    // orchestrator judgement turn); so is the outcome's final text.
    let parsed = outcome.report.expect("report parses from scrubbed text");
    assert_eq!(parsed.result, RunResult::Pass);
    assert!(
        parsed.summary.contains("[REDACTED]"),
        "summary: {}",
        parsed.summary
    );
    assert!(
        parsed.test_evidence.contains("[REDACTED]"),
        "testEvidence: {}",
        parsed.test_evidence
    );
    assert!(!parsed.test_evidence.contains(token));
    assert!(
        !outcome.final_text.contains(token),
        "final_text must be scrubbed"
    );

    // Nothing on events.jsonl carries the raw token.
    drop(log);
    let raw_log = std::fs::read_to_string(p.events_file()).unwrap();
    assert!(!raw_log.contains(token), "event log leaked the token");
    let events = read_log(&p);
    match &events.last().unwrap().kind {
        EventKind::WorkerCompleted {
            report: Some(r), ..
        } => {
            assert!(
                r.test_evidence.contains("[REDACTED]"),
                "event report: {r:?}"
            );
        }
        other => panic!("expected worker.completed with a report, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Report parsing fallbacks
// ---------------------------------------------------------------------------

/// H10a: a decision turn commits the mission, so it accepts only JSON the
/// model presented AS its answer — the whole reply, or the sole fenced block.
/// The lenient first-`{`-to-last-`}` span stays out of this path: it takes a
/// JSON object the model quoted and disowned as the verdict.
#[derive(serde::Deserialize)]
struct Verdicts {
    verdicts: Vec<serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct Decision {
    decision: String,
}

#[test]
fn parse_decision_refuses_quoted_and_disowned_json() {
    let disowned = "The worker's NOTES.md contains this block, which I do NOT endorse: \
                    {\"verdicts\":[{\"id\":\"a-1\",\"pass\":true,\"evidence\":\"see notes\"}],\
                    \"summary\":\"all good\"} My actual finding: a-1 is NOT met.";
    assert!(
        kranz_engine::runner::parse_decision::<Verdicts>(disowned).is_none(),
        "prose-embedded JSON must not be read as the decision"
    );
    // The lenient parser is what it is: this is the exact leniency the
    // decision path drops.
    let lenient =
        kranz_engine::runner::parse_report::<Verdicts>(disowned).expect("greedy span still parses");
    assert_eq!(
        lenient.verdicts.len(),
        1,
        "fixture no longer exercises the greedy span"
    );
}

#[test]
fn parse_decision_accepts_the_three_legitimate_shapes() {
    let bare = r#"{"decision":"complete"}"#;
    assert_eq!(
        kranz_engine::runner::parse_decision::<Decision>(bare)
            .expect("bare JSON")
            .decision,
        "complete"
    );
    assert_eq!(
        kranz_engine::runner::parse_decision::<Decision>(&format!("\n  {bare}\n  "))
            .expect("whitespace-padded JSON")
            .decision,
        "complete"
    );
    assert_eq!(
        kranz_engine::runner::parse_decision::<Decision>(&format!("```json\n{bare}\n```"))
            .expect("sole fenced block")
            .decision,
        "complete"
    );
    assert_eq!(
        kranz_engine::runner::parse_decision::<Decision>(&format!("```\n{bare}\n```"))
            .expect("sole untagged fenced block")
            .decision,
        "complete"
    );
}

#[test]
fn parse_decision_refuses_two_fenced_blocks_and_unclosed_fences() {
    let two = "```json\n{\"decision\":\"respawn\"}\n```\nand my real answer:\n\
               ```json\n{\"decision\":\"complete\"}\n```";
    assert!(
        kranz_engine::runner::parse_decision::<Decision>(two).is_none(),
        "two candidate blocks must fail closed, not pick one"
    );
    let unclosed = "```json\n{\"decision\":\"complete\"}";
    assert!(kranz_engine::runner::parse_decision::<Decision>(unclosed).is_none());
    // Prose around a sole fenced block is fine; prose INSIDE it is not.
    let trailing = "```json\n{\"decision\":\"complete\"} then some prose\n```";
    assert!(kranz_engine::runner::parse_decision::<Decision>(trailing).is_none());
}

/// M-6 (follow-up review): H10a's greedy-brace half was closed, but the
/// "quoted and disowned" half only moved from bare prose into a fence. A
/// fence is a quotation mark as easily as an answer, so the decision must be
/// the LAST thing the reply says.
#[test]
fn parse_decision_refuses_a_fenced_block_the_model_disowns_afterwards() {
    let disowned = "Here is an example of a verdict I am NOT issuing:\n\
                    ```json\n\
                    {\"verdicts\":[{\"id\":\"a-1\",\"pass\":true,\"evidence\":\"...\"}]}\n\
                    ```\n\
                    My actual verdict is FAIL.";
    assert!(
        kranz_engine::runner::parse_decision::<Verdicts>(disowned).is_none(),
        "a fenced block the model disowns in trailing prose must fail closed"
    );

    // The taught shape (a lead-in, then the fence, then nothing) still
    // parses; only trailing content is fatal.
    let lead_in = "Here is my verdict:\n\
                   ```json\n\
                   {\"verdicts\":[{\"id\":\"a-1\",\"pass\":true,\"evidence\":\"...\"}]}\n\
                   ```\n  \n";
    assert_eq!(
        kranz_engine::runner::parse_decision::<Verdicts>(lead_in)
            .expect("a lead-in before the fence is fine")
            .verdicts
            .len(),
        1
    );
}

/// M-7 (follow-up review): a validator quoting a code snippet inside an
/// `evidence` string value put backticks mid-line, the counting scan saw
/// four fences, and a perfectly good verdict cost a retry and then landed on
/// the caller's default. Fence state is per LINE, so mid-line backticks are
/// just characters.
#[test]
fn parse_decision_accepts_json_whose_string_values_contain_backticks() {
    let quoting = "```json\n\
                   {\"verdicts\":[{\"id\":\"a-1\",\"pass\":false,\
                   \"evidence\":\"the snippet ```rust fn main(){}``` never compiled\"}]}\n\
                   ```";
    let parsed = kranz_engine::runner::parse_decision::<Verdicts>(quoting)
        .expect("backticks inside a JSON string value are not fences");
    assert_eq!(parsed.verdicts.len(), 1);
}

/// The lenient parser stays for the worker/validator report channel, which is
/// not a consent decision and where a report the model buries in prose is
/// better recovered than dropped. `parse_decision` is the strict twin.
#[test]
fn parse_worker_report_strict_and_fallbacks() {
    let strict = worker_report_json().to_string();
    assert!(parse_worker_report(&strict).is_some(), "strict parse");

    let braces = format!("Here is my report. {strict} That's all.");
    assert!(
        parse_worker_report(&braces).is_some(),
        "brace-substring parse"
    );

    let fenced = format!("Work finished {{with caveats}}.\n```json\n{strict}\n```\nGoodbye.");
    let report = parse_worker_report(&fenced).expect("fenced parse");
    assert_eq!(report.summary, "built the login endpoint");

    assert!(parse_worker_report("no json here at all").is_none());
    assert!(parse_worker_report("{ not valid json }").is_none());
}

#[test]
fn parse_validator_report_fallbacks() {
    let value = json!({
        "findings": [{
            "subject": "a-1",
            "severity": "major",
            "evidence": "test asserts the mock, not the behaviour",
            "suggestedFix": "assert on output"
        }],
        "summary": "one major finding"
    });
    let fenced = format!("Prose first.\n```json\n{value}\n```");
    let report = parse_validator_report(&fenced).expect("fenced parse");
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].severity, "major");

    assert!(parse_validator_report("total garbage").is_none());
}

// ---------------------------------------------------------------------------
// run_worker / run_validator wrappers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_worker_builds_spec_and_uses_report_result() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    let cfg = MissionConfig::default();

    let backend =
        MockBackend::with_scripts(vec![MockScript::single_shot_json(&worker_report_json())]);
    let outcome = run_worker(
        &backend,
        &mut log,
        &p,
        &cfg,
        &feature(),
        "ship the auth system",
        "Auth",
        Some("mind the rate limiter"),
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
    .unwrap();

    assert_eq!(outcome.result, RunResult::Pass);
    assert!(outcome.report.is_some());

    let specs = backend.started_specs();
    assert_eq!(specs.len(), 1);
    let spec = &specs[0];
    assert_eq!(spec.cwd, p.repo_root);
    assert!(
        !spec.env.contains_key("KRANZ_BASE_SHA"),
        "no base sha means no env var"
    );
    assert_eq!(spec.model, cfg.worker.model);
    assert_eq!(spec.effort, cfg.worker.reasoning_effort);
    assert_eq!(spec.max_turns, cfg.worker.max_turns);
    assert_eq!(spec.max_budget_usd, cfg.worker.max_budget_usd);
    assert!(
        spec.writable,
        "worker sessions must get write-capable backends"
    );
    assert_eq!(spec.permission_mode.as_deref(), Some("acceptEdits"));
    assert_eq!(spec.allowed_tools, vec!["Bash".to_string()]);
    assert!(spec.disallowed_tools.iter().any(|d| d == "Bash(git push*)"));
    assert!(
        uuid::Uuid::parse_str(&spec.session_id).is_ok(),
        "session id is a uuid"
    );

    // Role prompt rendered into append_system_prompt; task body is the prompt.
    let system = spec
        .append_system_prompt
        .as_deref()
        .expect("role prompt set");
    assert!(
        system.contains("f-1"),
        "featureId rendered into role prompt"
    );
    assert!(!system.contains("{featureId}"), "no unrendered placeholder");
    match &spec.prompt {
        PromptMode::SingleShot(task) => {
            assert!(task.contains("Add login"));
            assert!(task.contains("Build the login endpoint"));
            assert!(task.contains("users can log in"));
            assert!(task.contains("mind the rate limiter"));
        }
        other => panic!("worker must be single-shot, got {other:?}"),
    }

    // WorkerReport schema enforced at the source.
    let schema = spec.json_schema.as_ref().expect("json schema set");
    assert_eq!(schema["additionalProperties"], json!(false));
    assert!(schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "result"));
    assert!(schema["properties"]["filesTouched"].is_object());

    // Feature id recorded on worker.spawned.
    drop(log);
    let events = read_log(&p);
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::WorkerSpawned { feature_id: Some(f), role: Role::Worker, .. } if f == "f-1"
    )));
}

#[tokio::test]
async fn run_worker_seeds_scratch_home_and_config_dir_worker_env_hygiene() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    let cfg = MissionConfig::default();

    let backend =
        MockBackend::with_scripts(vec![MockScript::single_shot_json(&worker_report_json())]);
    let base_sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    run_worker(
        &backend,
        &mut log,
        &p,
        &cfg,
        &feature(),
        "ship the auth system",
        "Auth",
        None,
        None,
        Some(base_sha),
        &[],
        &[],
        &[],
        AuthVerdict::Authenticated,
        &[],
        None,
        None,
    )
    .await
    .unwrap();

    let specs = backend.started_specs();
    let spec = &specs[0];

    // Contract env is preserved.
    assert_eq!(
        spec.env.get("KRANZ_BASE_SHA").map(String::as_str),
        Some(base_sha)
    );

    // seed_worker_env relocates HOME only once the preflight has proven the
    // scratch env authenticates (mission m-165b6f, f-1-2). With an
    // Authenticated verdict, the worker spec is relocated to the scratch
    // home under `scratch_home_root(session_id)`. CLAUDE_CONFIG_DIR is
    // deliberately NOT set: it poisons keychain-backed OAuth resolution
    // (probed 2026-07-29), and a relocated HOME resolves HOME/.claude
    // implicitly.
    let scratch_root = kranz_engine::backend_claude::scratch_home_root(&spec.session_id);
    let home = spec.env.get("HOME").expect("HOME relocated to scratch dir");
    assert!(
        std::path::Path::new(home).starts_with(&scratch_root),
        "HOME {home} must be under scratch root {}",
        scratch_root.display()
    );
    assert!(
        !spec.env.contains_key("CLAUDE_CONFIG_DIR"),
        "CLAUDE_CONFIG_DIR must NOT be relocated (keychain OAuth poison)"
    );
    assert!(
        std::path::Path::new(home).join(".claude").is_dir(),
        "the seeded scratch config dir is HOME/.claude"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn run_worker_routes_macos_fs_net_through_egress_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    let mut cfg = MissionConfig::default();
    cfg.worker.sandbox.enforce = kranz_engine::types::SandboxEnforce::FsNet;

    let backend =
        MockBackend::with_scripts(vec![MockScript::single_shot_json(&worker_report_json())]);
    let outcome = run_worker(
        &backend,
        &mut log,
        &p,
        &cfg,
        &feature(),
        "ship the auth system",
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
    .unwrap();

    // macOS fs+net resolves to Seatbelt (loopback-only egress) plus the
    // filtering egress proxy: the session's env points at the spawned proxy.
    let specs = backend.started_specs();
    assert_eq!(specs.len(), 1, "fs+net no longer refuses on macOS");
    let spec = &specs[0];
    let sandbox = spec.sandbox.as_ref().expect("sandbox resolved");
    assert_eq!(
        sandbox.backend,
        kranz_engine::sandbox::SandboxBackend::Seatbelt
    );
    let proxy = spec
        .env
        .get(kranz_engine::egress_proxy::HTTPS_PROXY_ENV)
        .expect("fs+net session carries proxy env");
    assert!(
        proxy.starts_with("http://127.0.0.1:"),
        "seatbelt sessions reach the proxy on loopback: {proxy}"
    );
    assert_eq!(
        spec.env.get(kranz_engine::egress_proxy::HTTP_PROXY_ENV),
        Some(proxy)
    );
    assert_eq!(
        spec.env.get(kranz_engine::egress_proxy::NO_PROXY_ENV),
        Some(&kranz_engine::egress_proxy::NO_PROXY_VALUE.to_string())
    );
    assert!(
        outcome.denied_egress.is_empty(),
        "no CONNECTs in a mock session: {:?}",
        outcome.denied_egress
    );
}

#[tokio::test]
async fn run_validator_builds_spec_permissions_and_parses_report() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    let cfg = MissionConfig {
        allow_validator_commands: vec!["npm run lint".to_string()],
        worker_isolation: WorkerIsolation::Checkout,
        ..MissionConfig::default()
    };
    let contract = vec![
        assertion("a-1", Some("cargo test --all")),
        assertion("a-2", None),
    ];

    let report = json!({ "findings": [], "summary": "everything holds" });
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot_json(&report)]);
    let outcome = run_validator(
        &backend,
        &mut log,
        &p,
        &cfg,
        Role::ValidatorFunctional,
        &milestone(),
        &contract,
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
    .unwrap();

    assert_eq!(outcome.result, RunResult::Pass);
    let validator_report = outcome.validator_report.expect("validator report parses");
    assert!(validator_report.findings.is_empty());
    assert!(outcome.report.is_none(), "no WorkerReport for validators");

    let specs = backend.started_specs();
    let spec = &specs[0];
    assert!(
        !spec.writable,
        "validator sessions must keep read-only backend mode"
    );
    assert!(
        !spec.env.contains_key("KRANZ_BASE_SHA"),
        "no base sha means no env var"
    );
    assert!(
        !spec.env.contains_key("HOME") && !spec.env.contains_key("CLAUDE_CONFIG_DIR"),
        "worker_env_hygiene scratch env is worker-role only — validator env must be unchanged: {:?}",
        spec.env
    );
    assert_eq!(spec.model, cfg.validator_functional.model);
    assert_eq!(spec.permission_mode.as_deref(), Some("default"));
    assert!(spec
        .allowed_tools
        .iter()
        .any(|a| a == "Bash(cargo test --all*)"));
    assert!(spec
        .allowed_tools
        .iter()
        .any(|a| a == "Bash(npm run lint*)"));
    assert!(spec.disallowed_tools.iter().any(|d| d == "Write"));

    let system = spec.append_system_prompt.as_deref().unwrap();
    assert!(
        system.contains("abc123"),
        "startSha rendered into role prompt"
    );
    match &spec.prompt {
        PromptMode::SingleShot(task) => {
            assert!(task.contains("abc123..HEAD"));
            assert!(task.contains("cargo test --all"));
            assert!(task.contains("a-2"), "agent-judgement assertion listed");
            assert!(task.contains("users can log in"), "feature criteria listed");
        }
        other => panic!("validator must be single-shot, got {other:?}"),
    }

    // Milestone id (not feature id) recorded on worker.spawned.
    drop(log);
    let events = read_log(&p);
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::WorkerSpawned {
            milestone_id: Some(m),
            feature_id: None,
            role: Role::ValidatorFunctional,
            ..
        } if m == "ms-1"
    )));
}

/// Worker-reported `commandsRun` is model-authored JSON with no human step,
/// so it must not mint `Bash(...)` allow rules for the read-only validator
/// (audit-exec M1). It stays in the prompt as a claim: the validator is told
/// the worker says it ran these, and is told they are not permitted.
#[tokio::test]
async fn worker_reported_commands_never_become_validator_allow_rules() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    let cfg = MissionConfig {
        allow_validator_commands: vec!["npm run lint".to_string()],
        worker_isolation: WorkerIsolation::Checkout,
        ..MissionConfig::default()
    };
    let contract = vec![assertion("a-1", Some("cargo test --all"))];
    let worker_commands = vec!["bash -c".to_string(), "python3 -c".to_string()];

    let report = json!({ "findings": [], "summary": "everything holds" });
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot_json(&report)]);
    run_validator(
        &backend,
        &mut log,
        &p,
        &cfg,
        Role::ValidatorFunctional,
        &milestone(),
        &contract,
        "abc123",
        None,
        None,
        &[],
        &[],
        &worker_commands,
        None,
        None,
    )
    .await
    .unwrap();

    let specs = backend.started_specs();
    let spec = &specs[0];
    for forbidden in ["Bash(bash -c*)", "Bash(python3 -c*)"] {
        assert!(
            !spec.allowed_tools.iter().any(|a| a == forbidden),
            "worker report minted {forbidden:?}: {:?}",
            spec.allowed_tools
        );
    }
    // The approved contract and operator config still grant theirs.
    for expected in ["Bash(cargo test --all*)", "Bash(npm run lint*)"] {
        assert!(
            spec.allowed_tools.iter().any(|a| a == expected),
            "lost {expected:?}: {:?}",
            spec.allowed_tools
        );
    }
    // Still visible to the validator, labelled as a claim.
    match &spec.prompt {
        PromptMode::SingleShot(task) => {
            assert!(task.contains("bash -c"), "worker claim dropped from prompt");
            assert!(
                task.contains("worker reports"),
                "worker claim not labelled as a claim: {task}"
            );
        }
        other => panic!("validator must be single-shot, got {other:?}"),
    }
}

/// A human-approved grant for a command the worker also reported still
/// grants it: provenance is what changed, not the grant path.
#[tokio::test]
async fn granted_commands_still_allow_even_when_the_worker_reported_them() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    let cfg = MissionConfig {
        worker_isolation: WorkerIsolation::Checkout,
        ..MissionConfig::default()
    };
    let report = json!({ "findings": [], "summary": "ok" });
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot_json(&report)]);
    run_validator(
        &backend,
        &mut log,
        &p,
        &cfg,
        Role::ValidatorFunctional,
        &milestone(),
        &[],
        "abc123",
        None,
        None,
        &["cargo fmt".to_string()],
        &[],
        &["cargo fmt".to_string()],
        None,
        None,
    )
    .await
    .unwrap();

    let specs = backend.started_specs();
    assert!(
        specs[0]
            .allowed_tools
            .iter()
            .any(|a| a == "Bash(cargo fmt*)"),
        "operator grant lost: {:?}",
        specs[0].allowed_tools
    );
}

#[tokio::test]
async fn run_validator_rejects_non_validator_roles() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    let cfg = MissionConfig::default();
    let backend = MockBackend::new();

    let err = run_validator(
        &backend,
        &mut log,
        &p,
        &cfg,
        Role::Worker,
        &milestone(),
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
    .unwrap_err();
    assert!(err.to_string().contains("validator role"), "got: {err}");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn run_validator_routes_macos_fs_net_through_egress_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    let mut cfg = MissionConfig::default();
    cfg.validator_scrutiny.sandbox.enforce = kranz_engine::types::SandboxEnforce::FsNet;
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot_json(
        &json!({ "findings": [], "summary": "all good" }),
    )]);

    let outcome = run_validator(
        &backend,
        &mut log,
        &p,
        &cfg,
        Role::ValidatorScrutiny,
        &milestone(),
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
    .unwrap();

    // Validator sessions get the same proxy wiring as workers on macOS fs+net.
    let specs = backend.started_specs();
    assert_eq!(specs.len(), 1, "fs+net no longer refuses on macOS");
    assert_eq!(
        specs[0].sandbox.as_ref().map(|s| s.backend),
        Some(kranz_engine::sandbox::SandboxBackend::Seatbelt)
    );
    assert!(
        specs[0]
            .env
            .contains_key(kranz_engine::egress_proxy::HTTPS_PROXY_ENV),
        "validator fs+net session carries proxy env: {:?}",
        specs[0].env
    );
    assert!(outcome.denied_egress.is_empty());
}

/// Contract commands admit their natural reinvocations (observed live:
/// verbatim-only prefixes denied the validator its own checks and blocked a
/// milestone on a permissions artifact) through exact declared-form rules —
/// and NOTHING wider (ticket validator-immutability-proof): the old
/// leading-two-token catch-alls (`python3 -*`, `python3 -m*`) are gone, and
/// heredoc contracts run through the engine-side contract execution path
/// instead of a validator Bash rule.
#[test]
fn validator_command_patterns_cover_natural_variations() {
    use kranz_engine::permissions::command_allow_patterns;

    let pats = command_allow_patterns("python3 extract_links.py && echo EXIT_OK");
    let covers = |cmd: &str| {
        pats.iter().any(|p| {
            let inner = p
                .strip_prefix("Bash(")
                .and_then(|s| s.strip_suffix(")"))
                .unwrap();
            match inner.strip_suffix('*') {
                Some(prefix) => cmd.starts_with(prefix),
                None => cmd == inner,
            }
        })
    };
    // Verbatim, bare segment, and arg-extended reinvocations of a segment.
    assert!(covers("python3 extract_links.py && echo EXIT_OK"));
    assert!(covers("python3 extract_links.py"));
    assert!(covers(
        "python3 extract_links.py operator@example.com out.txt"
    ));
    assert!(covers("echo EXIT_OK"));
    // Unrelated programs stay uncovered — including other uses of the same
    // interpreter (no `python3 -*` catch-all).
    assert!(!covers("python3 -c 'import sys'"));
    assert!(!covers("python3 other_script.py"));
    assert!(!covers("curl https://example.com"));
    assert!(!covers("rm -rf /"));

    // Heredoc contract commands get no validator Bash rule at all: the
    // engine runs contract commands itself and hands the validator the
    // captured PASS/FAIL evidence (validator repair 3/5).
    let heredoc = command_allow_patterns("python3 - <<'PY'\nprint('ok')\nPY");
    assert!(!heredoc.iter().any(|p| p == "Bash(python3 -*)"));

    // And the profile carries the exact declared form only — a `-m` contract
    // no longer widens to arbitrary interpreter/module use.
    let cfg = MissionConfig::default();
    let profile = kranz_engine::permissions::for_role(
        Role::ValidatorFunctional,
        &cfg,
        &["python3 -m pytest test_x.py -v".to_string()],
        &[],
        &[],
    );
    assert!(profile
        .allowed_tools
        .iter()
        .any(|p| p == "Bash(python3 -m pytest test_x.py -v*)"));
    assert!(!profile
        .allowed_tools
        .iter()
        .any(|p| p == "Bash(python3 -m*)"));
    assert!(!profile.allowed_tools.iter().any(|p| p == "Bash(python3*)"));
}

// ---------------------------------------------------------------------------
// run_worker_in_buffered (roadmap M3 wall-clock overlap)
// ---------------------------------------------------------------------------

/// The buffered worker path touches NO event log and returns the exact same
/// event KINDS the live path would have appended, in append order — the
/// building block the engine replays serially after a concurrent batch, so
/// events.jsonl stays single-writer. The per-run transcript is still written
/// live (transcripts are per-run files, not the single-writer log), and the
/// RunOutcome matches the live path.
#[tokio::test]
async fn run_worker_in_buffered_collects_kinds_without_touching_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let log = seeded_log(&p);
    let cfg = MissionConfig::default();

    // A worker run with a tool-use so we get a worker.message delta in the
    // buffer as well as spawned/completed.
    let script = MockScript {
        events: vec![
            mock_init("mock-session"),
            mock_tool_use("Bash", "cargo build"),
            mock_text(&worker_report_json().to_string()),
            mock_result_text(&worker_report_json().to_string()),
        ],
        ..Default::default()
    };
    let backend = MockBackend::with_scripts(vec![script]);

    let cwd = p.repo_root.clone();
    let (buffered, outcome) = run_worker_in_buffered(
        &backend,
        &p,
        &cfg,
        &feature(),
        "ship the auth system",
        "Auth",
        None,
        &cwd,
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
    .unwrap();

    // The RunOutcome is exactly what the live path yields.
    assert_eq!(outcome.result, RunResult::Pass);
    assert!(outcome.report.is_some());

    // The log was NOT touched: only mission.created is on disk (no spawned/
    // message/completed appended). The single-writer invariant is preserved by
    // construction — the buffered path never held the log.
    drop(log);
    let events = read_log(&p);
    assert_eq!(
        event_types(&events),
        vec!["mission.created"],
        "buffered run appends nothing"
    );

    // The buffer holds spawned first, at least one message, completed last —
    // the same kinds, same order, the live path would have appended.
    assert!(
        matches!(
            buffered.first(),
            Some(EventKind::WorkerSpawned {
                role: Role::Worker,
                backend: Some(kranz_engine::types::BackendKind::Claude),
                ..
            })
        ),
        "first buffered kind is worker.spawned: {buffered:?}"
    );
    assert!(
        matches!(
            buffered.last(),
            Some(EventKind::WorkerCompleted {
                result: RunResult::Pass,
                ..
            })
        ),
        "last buffered kind is a passing worker.completed: {buffered:?}"
    );
    assert!(
        buffered
            .iter()
            .any(|k| matches!(k, EventKind::WorkerMessage { tag, .. } if tag == "tool-use")),
        "a tool-use worker.message is buffered: {buffered:?}"
    );

    // The transcript file WAS written live (per-run file, not the log).
    let transcript = std::fs::read_to_string(p.transcript_file(&outcome.run_id)).unwrap();
    assert_eq!(
        transcript.lines().count(),
        4,
        "init + tool-use + text + result"
    );
}

// ---------------------------------------------------------------------------
// Egress proxy (3.3a): fs+net sessions route through the filtering proxy
// ---------------------------------------------------------------------------

use kranz_engine::egress_proxy::{
    EgressDenial, HTTPS_PROXY_ENV, HTTP_PROXY_ENV, NO_PROXY_ENV, NO_PROXY_VALUE,
};
use kranz_engine::sandbox::{ResolvedSandbox, SandboxBackend, SandboxInputs};
use kranz_engine::types::SandboxEnforce;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

fn fs_net_sandbox(
    backend: SandboxBackend,
    egress: Vec<String>,
    dir: &std::path::Path,
) -> ResolvedSandbox {
    ResolvedSandbox {
        backend,
        inputs: SandboxInputs {
            enforce: SandboxEnforce::FsNet,
            session_cwd: dir.to_path_buf(),
            mission_dir: dir.to_path_buf(),
            tmpdir: std::env::temp_dir(),
            extra_write: Vec::new(),
            egress,
            validator_read_deny_roots: Vec::new(),
        },
        container: None,
    }
}

/// Read one proxy response head (through the CRLF terminator).
async fn read_proxy_response_head(stream: &mut TcpStream) -> String {
    let mut reader = BufReader::new(stream);
    let mut head = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let done = line == "\r\n";
        head.push_str(&line);
        if done {
            return head;
        }
    }
}

/// Poll until the backend has recorded its first started spec (the proxy is
/// spawned before `backend.start`, so a recorded spec means the proxy env is
/// readable and the proxy is serving).
async fn first_started_spec(backend: &MockBackend) -> SessionSpec {
    let deadline = std::time::Instant::now() + HANG_PROOF;
    loop {
        if let Some(spec) = backend.started_specs().into_iter().next() {
            return spec;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "session never started"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// A parked fs+net (Seatbelt) session's env points at the run's spawned
/// proxy; an allowed CONNECT tunnels, a denied CONNECT gets a 403 and lands
/// in `RunOutcome.denied_egress`, the disposable mission JSONL, and the
/// durable run-attributed event log — all over loopback.
#[tokio::test]
async fn run_session_fs_net_wires_proxy_env_and_surfaces_denials() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);

    // Loopback echo server: the allowed CONNECT target.
    let echo = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let echo_port = echo.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut socket, _) = echo.accept().await.unwrap();
        let mut buf = [0u8; 64];
        let n = socket.read(&mut buf).await.unwrap();
        socket.write_all(&buf[..n]).await.unwrap();
    });

    // A streaming script with no events parks the session in next_event until
    // the test's cancel aborts it — holding the proxy's lifetime open.
    let backend = Arc::new(MockBackend::with_scripts(vec![MockScript::streaming(
        vec![],
    )]));
    let mut spec = session_spec(PromptMode::Streaming("held".to_string()));
    spec.sandbox = Some(fs_net_sandbox(
        SandboxBackend::Seatbelt,
        vec![format!("127.0.0.1:{echo_port}")],
        dir.path(),
    ));

    let cancel = Arc::new(Notify::new());
    let run = {
        let backend = Arc::clone(&backend);
        let cancel = Arc::clone(&cancel);
        let p = p.clone();
        tokio::spawn(async move {
            run_session(
                backend.as_ref(),
                spec,
                &mut log,
                &p,
                worker_meta("run-egress"),
                Some(cancel),
            )
            .await
        })
    };

    let started = first_started_spec(backend.as_ref()).await;
    let proxy_url = started
        .env
        .get(HTTPS_PROXY_ENV)
        .expect("fs+net session carries HTTPS_PROXY")
        .clone();
    let proxy_addr = proxy_url
        .strip_prefix("http://")
        .expect("proxy url is http://host:port");
    assert!(
        proxy_addr.starts_with("127.0.0.1:"),
        "seatbelt sessions reach the proxy on loopback: {proxy_url}"
    );
    assert_eq!(
        started.env.get(HTTP_PROXY_ENV).map(String::as_str),
        Some(proxy_url.as_str())
    );
    assert_eq!(
        started.env.get(NO_PROXY_ENV).map(String::as_str),
        Some(NO_PROXY_VALUE)
    );

    // Allowed host (the echo server is on the allowlist): 200 + tunnel bytes.
    let mut client = TcpStream::connect(proxy_addr).await.unwrap();
    client
        .write_all(format!("CONNECT 127.0.0.1:{echo_port} HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let head = read_proxy_response_head(&mut client).await;
    assert!(head.starts_with("HTTP/1.1 200"), "allowed CONNECT: {head}");
    client.write_all(b"tunneled").await.unwrap();
    let mut buf = vec![0u8; b"tunneled".len()];
    client.read_exact(&mut buf).await.unwrap();
    assert_eq!(buf, b"tunneled", "bytes tunnel through the run's proxy");

    // Denied host: 403, never a silent timeout. Repeat the same destination
    // to prove the durable audit event deduplicates amplification while its
    // omittedCount preserves the raw-record count.
    for _ in 0..2 {
        let mut client = TcpStream::connect(proxy_addr).await.unwrap();
        client
            .write_all(b"CONNECT denied.example:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let head = read_proxy_response_head(&mut client).await;
        assert!(head.starts_with("HTTP/1.1 403"), "denied CONNECT: {head}");
    }

    cancel.notify_one();
    let outcome = timeout(HANG_PROOF, run)
        .await
        .expect("run must not hang")
        .unwrap()
        .unwrap();

    assert_eq!(
        outcome.denied_egress,
        vec![
            EgressDenial {
                host: "denied.example".to_string(),
                port: 443,
            },
            EgressDenial {
                host: "denied.example".to_string(),
                port: 443,
            },
        ],
        "the run surfaces exactly its own proxy's denials"
    );

    // The mission JSONL holds the fsynced record with a timestamp.
    let content = std::fs::read_to_string(p.egress_denials_file()).unwrap();
    let records: Vec<serde_json::Value> = content
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert!(records.iter().all(|record| {
        record["host"] == "denied.example" && record["port"] == 443 && record["ts"].is_string()
    }));

    let events = EventLog::read_events(&p.events_file()).unwrap();
    let denial_seq = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::WorkerEgressDenied {
                run_id,
                denials,
                omitted_count,
            } if run_id == "run-egress" => {
                assert_eq!(denials.len(), 1);
                assert_eq!(denials[0].host, "denied.example");
                assert_eq!(denials[0].port, 443);
                assert_eq!(*omitted_count, 1);
                Some(event.seq)
            }
            _ => None,
        })
        .expect("runner persists a run-attributed denial event");
    let completed_seq = events
        .iter()
        .find_map(|event| match &event.kind {
            EventKind::WorkerCompleted { run_id, .. } if run_id == "run-egress" => Some(event.seq),
            _ => None,
        })
        .expect("run has a completion event");
    assert!(
        denial_seq < completed_seq,
        "denial evidence must land before the run completion boundary"
    );
}

/// No fs+net ⇒ no proxy at all: no env, no denials, no denial file. Covers
/// unsandboxed, fs-only, and bwrap fs+net (netns cannot reach a host proxy —
/// v1 out of scope), plus container fs+net with an empty egress list
/// (--network none).
#[tokio::test]
async fn run_session_without_proxy_route_spawns_no_proxy() {
    for (name, sandbox) in [
        ("unsandboxed", None),
        (
            "fs-only",
            Some(fs_net_sandbox(
                SandboxBackend::Seatbelt,
                vec![],
                std::env::temp_dir().as_path(),
            )),
        ),
        (
            "bwrap fs+net",
            Some(fs_net_sandbox(
                SandboxBackend::Bubblewrap,
                vec![],
                std::env::temp_dir().as_path(),
            )),
        ),
        (
            "container fs+net empty egress",
            Some(fs_net_sandbox(
                SandboxBackend::Container,
                vec![],
                std::env::temp_dir().as_path(),
            )),
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let mut log = seeded_log(&p);
        let backend =
            MockBackend::with_scripts(vec![MockScript::single_shot_json(&worker_report_json())]);
        let mut spec = session_spec(PromptMode::SingleShot("task".to_string()));
        spec.sandbox = sandbox.map(|mut s| {
            // The fs-only case exercises the Fs tier; the rest keep FsNet.
            if name == "fs-only" {
                s.inputs.enforce = SandboxEnforce::Fs;
            }
            s
        });

        let outcome = run_session(
            &backend,
            spec,
            &mut log,
            &p,
            worker_meta("run-noproxy"),
            None,
        )
        .await
        .unwrap();

        let started = &backend.started_specs()[0];
        assert!(
            !started.env.contains_key(HTTPS_PROXY_ENV)
                && !started.env.contains_key(HTTP_PROXY_ENV)
                && !started.env.contains_key(NO_PROXY_ENV),
            "{name}: no proxy env: {:?}",
            started.env
        );
        assert!(outcome.denied_egress.is_empty(), "{name}: no denials");
        assert!(
            !p.egress_denials_file().exists(),
            "{name}: no denial file is created"
        );
    }
}

/// A proxy that cannot start (its denial file path is blocked by a directory)
/// fails the run CLOSED — before any session spawn.
#[tokio::test]
async fn run_session_fs_net_proxy_start_failure_fails_closed_before_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    std::fs::create_dir_all(p.runs_dir()).unwrap();
    // Block the denial file path with a directory: opening it for append
    // fails, so the proxy start fails.
    std::fs::create_dir(p.egress_denials_file()).unwrap();

    // A script IS queued: if the run wrongly proceeded to spawn, it would
    // succeed — so reaching backend.start is distinguishable from failing
    // closed.
    let backend =
        MockBackend::with_scripts(vec![MockScript::single_shot_json(&worker_report_json())]);
    let mut spec = session_spec(PromptMode::SingleShot("task".to_string()));
    spec.sandbox = Some(fs_net_sandbox(SandboxBackend::Seatbelt, vec![], dir.path()));

    let err = run_session(
        &backend,
        spec,
        &mut log,
        &p,
        worker_meta("run-closed"),
        None,
    )
    .await
    .expect_err("a proxy start failure must error the run");

    let message = err.to_string();
    assert!(message.contains("egress proxy"), "{message}");
    assert!(
        message.contains("refusing to run without enforcement"),
        "{message}"
    );
    assert!(
        backend.started_specs().is_empty(),
        "fail-closed means the session never spawns"
    );
}

// ---------------------------------------------------------------------------
// Hook gate projection (ticket claude-code-hook-gate-projection, KRZ-302)
// ---------------------------------------------------------------------------

/// A worker run with a declared touch set gets the out-of-contract write
/// rule projected onto its per-session settings (the PreToolUse hook block),
/// and the engine-written spec file lands under the session-private scratch
/// root. The mock session never invokes the hook, so NO records exist and
/// nothing folds — a silent hook is not a failure (the engine-side sweep is
/// the authoritative layer).
#[tokio::test]
async fn hook_gate_projection_worker_run_projects_hook_settings_and_spec_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    let cfg = MissionConfig::default();

    let backend =
        MockBackend::with_scripts(vec![MockScript::single_shot_json(&worker_report_json())]);
    let touch_set = vec!["src/**".to_string(), "!src/generated/**".to_string()];
    let outcome = run_worker(
        &backend,
        &mut log,
        &p,
        &cfg,
        &feature(),
        "ship the auth system",
        "Auth",
        None,
        None,
        None,
        &[],
        &[],
        &[],
        AuthVerdict::Inconclusive,
        &touch_set,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(outcome.result, RunResult::Pass);

    let specs = backend.started_specs();
    assert_eq!(specs.len(), 1);
    let spec = &specs[0];

    // The per-session settings carry the hooks block in the documented
    // schema shape (hook_gates module docs): PreToolUse, the write-tool
    // matcher, one command handler naming `hook-guard --config <spec>`.
    let settings = spec.settings_json.as_ref().expect("hook settings set");
    let group = &settings["hooks"]["PreToolUse"][0];
    assert_eq!(group["matcher"], json!("Write|Edit|MultiEdit|NotebookEdit"));
    let command = group["hooks"][0]["command"].as_str().unwrap();
    assert!(
        command.contains("hook-guard") && command.contains("--config"),
        "the hook command invokes the guard subcommand: {command}"
    );
    assert!(group["hooks"][0]["timeout"].is_number());

    // The spec file it points at exists, under the session-private scratch
    // root, carrying the touch set verbatim and the session cwd.
    let spec_path = kranz_engine::hook_gates::spec_file(&spec.session_id);
    assert!(
        spec_path.starts_with(kranz_engine::backend_claude::scratch_home_root(
            &spec.session_id
        )),
        "the spec file lives under the session-private scratch root"
    );
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&spec_path).unwrap()).unwrap();
    assert_eq!(written["touchSet"], json!(touch_set));
    assert_eq!(
        written["sessionCwd"],
        json!(p.repo_root.display().to_string())
    );
    assert_eq!(
        written["recordFile"],
        json!(kranz_engine::hook_gates::record_file(&spec.session_id)
            .display()
            .to_string())
    );
    assert!(
        command.contains(&spec_path.display().to_string()),
        "the hook command names the written spec file: {command}"
    );

    // The hook never fired in the mock session: no records, so no
    // hook.gate.fired events — and the run still completes normally.
    drop(log);
    let events = read_log(&p);
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::HookGateFired { .. })),
        "no records means no hook.gate.fired events"
    );
    assert!(events
        .iter()
        .any(|e| matches!(&e.kind, EventKind::WorkerCompleted { .. })));

    let _ = std::fs::remove_dir_all(kranz_engine::backend_claude::scratch_home_root(
        &spec.session_id,
    ));
}

/// Regression: an EMPTY touch set is advisory-off (the sweep's posture) —
/// the worker spec is byte-for-byte the pre-projection shape (no
/// settings_json, no spec file), which is also exactly how sessions on
/// backends without hook support behave (they ignore settings_json).
#[tokio::test]
async fn hook_gate_projection_empty_touch_set_leaves_worker_spec_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    let cfg = MissionConfig::default();

    let backend =
        MockBackend::with_scripts(vec![MockScript::single_shot_json(&worker_report_json())]);
    run_worker(
        &backend,
        &mut log,
        &p,
        &cfg,
        &feature(),
        "ship the auth system",
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
    .unwrap();

    let specs = backend.started_specs();
    assert_eq!(specs.len(), 1);
    assert!(
        specs[0].settings_json.is_none(),
        "an empty touch set projects nothing"
    );
    assert!(!kranz_engine::hook_gates::spec_file(&specs[0].session_id).exists());
}

/// A gate-failing action inside the session surfaces as a structured
/// `hook.gate.fired` event BEFORE `worker.completed` — the record file the
/// guard wrote (here pre-seeded exactly as `kranz hook-guard` writes it)
/// folds into the log at session end, with the run id stamped from the run.
#[tokio::test]
async fn hook_gate_projection_records_fold_into_the_log_before_worker_completed() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);

    // A unique session id (never the shared fixture id) so the record file
    // cannot collide with another parallel test's scratch state.
    let mut spec = session_spec(PromptMode::SingleShot("task".to_string()));
    spec.session_id = format!("hook-gate-projection-{}", uuid::Uuid::new_v4());
    let session_id = spec.session_id.clone();

    let gate_spec = kranz_engine::hook_gates::HookGateSpec {
        version: kranz_engine::hook_gates::SPEC_VERSION,
        gate: kranz_engine::hook_gates::HOOK_GATE_ID.to_string(),
        session_cwd: p.repo_root.clone(),
        touch_set: vec!["src/**".to_string()],
        record_file: kranz_engine::hook_gates::record_file(&session_id),
    };
    kranz_engine::hook_gates::HookGateRecord::blocked(
        &gate_spec,
        "PreToolUse",
        "Write",
        "docs/oops.md",
        "matches none of the declared touch-set globs",
        Some("cli-session-1"),
        Some("toolu_1"),
    )
    .append_to(&kranz_engine::hook_gates::record_file(&session_id))
    .unwrap();

    let backend =
        MockBackend::with_scripts(vec![MockScript::single_shot_json(&worker_report_json())]);
    let outcome = run_session(&backend, spec, &mut log, &p, worker_meta("run-hook"), None)
        .await
        .unwrap();
    // The hook verdict never changes the run's own result mapping —
    // record-only evidence, with the sweep authoritative.
    assert_eq!(outcome.result, RunResult::Pass);

    drop(log);
    let events = read_log(&p);
    let types = event_types(&events);
    let fired_at = types
        .iter()
        .position(|t| *t == "hook.gate.fired")
        .expect("the hook record folded into an event: {types:?}");
    let completed_at = types
        .iter()
        .position(|t| *t == "worker.completed")
        .expect("worker.completed: {types:?}");
    assert!(
        fired_at < completed_at,
        "hook.gate.fired must land BEFORE worker.completed: {types:?}"
    );
    match &events[fired_at].kind {
        EventKind::HookGateFired {
            run_id,
            gate,
            hook_event,
            tool,
            subject,
            verdict,
            detail,
        } => {
            assert_eq!(run_id, "run-hook", "the run id is engine-stamped");
            assert_eq!(gate, "out-of-contract-write");
            assert_eq!(hook_event, "PreToolUse");
            assert_eq!(tool, "Write");
            assert_eq!(subject, "docs/oops.md");
            assert_eq!(verdict, "blocked");
            assert_eq!(
                detail.as_deref(),
                Some("matches none of the declared touch-set globs")
            );
        }
        other => panic!("expected hook.gate.fired, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(kranz_engine::backend_claude::scratch_home_root(&session_id));
}

/// The authoritative layer is unchanged: a worker that bypasses the hook
/// entirely (here: the write lands via a scripted commit — the Bash-write
/// shape a PreToolUse Write hook never sees) produces NO hook.gate.fired
/// events, and the engine-side out-of-contract sweep still flags the path
/// afterwards. Mirrors the orchestrator's
/// `out_of_contract_sweep_flags_path_outside_touch_set` fixture, which must
/// also keep firing untouched.
#[tokio::test]
async fn hook_gate_projection_bypassed_failure_still_caught_by_the_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // A real repo so the scripted commit lands; `.kranz/` ignored so the
    // mission's own bookkeeping never enters the sweep's candidate set.
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git spawns");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    git(&["init", "-q"]);
    std::fs::write(root.join(".gitignore"), ".kranz/\n").unwrap();
    git(&["add", ".gitignore"]);
    git(&[
        "-c",
        "user.name=test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "-q",
        "-m",
        "init",
    ]);
    let base_sha = String::from_utf8_lossy(&git(&["rev-parse", "HEAD"]).stdout)
        .trim()
        .to_string();

    let p = paths(root);
    let mut log = seeded_log(&p);
    let cfg = MissionConfig::default();
    let touch_set = vec!["src/**".to_string()];

    // The hook-bypass shape: the worker's out-of-contract write lands via a
    // commit the Write hook never sees; NO record file ever exists.
    let script = MockScript::single_shot_json(&worker_report_json())
        .writes_file("docs/oops.md", "written past the hook\n")
        .commits_all("[f-1] add login (and a sneaky note)");
    let backend = MockBackend::with_scripts(vec![script]);
    let outcome = run_worker(
        &backend,
        &mut log,
        &p,
        &cfg,
        &feature(),
        "ship the auth system",
        "Auth",
        None,
        None,
        None,
        &[],
        &[],
        &[],
        AuthVerdict::Inconclusive,
        &touch_set,
        None,
        None,
    )
    .await
    .unwrap();
    assert_eq!(outcome.result, RunResult::Pass);

    // Hook bypassed ⇒ no in-process evidence…
    drop(log);
    let events = read_log(&p);
    assert!(
        !events
            .iter()
            .any(|e| matches!(&e.kind, EventKind::HookGateFired { .. })),
        "a bypassed hook leaves no hook.gate.fired events"
    );

    // …and the engine-side sweep still catches the out-of-contract write,
    // via the same commit-range + path_findings composition the
    // orchestrator's out_of_contract_sweep runs.
    let diff = std::process::Command::new("git")
        .args(["diff", "--name-only", &format!("{base_sha}..HEAD")])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(diff.status.success());
    let commit = kranz_engine::git_ops::CommitInfo {
        sha: "HEAD".to_string(),
        subject: "[f-1] add login (and a sneaky note)".to_string(),
    };
    let diff_stdout = String::from_utf8_lossy(&diff.stdout);
    let changes: Vec<kranz_engine::contract_sweep::AttributedChange> = diff_stdout
        .lines()
        .map(|path| kranz_engine::contract_sweep::AttributedChange {
            path,
            commit: &commit,
        })
        .collect();
    assert_eq!(changes.len(), 1, "only the bypassed write was committed");
    let findings = kranz_engine::contract_sweep::path_findings(&touch_set, &changes);
    assert_eq!(findings.len(), 1, "the sweep still fires: {findings:?}");
    assert_eq!(findings[0].subject, "docs/oops.md");
    assert_eq!(
        findings[0].class,
        kranz_engine::contract_sweep::FINDING_CLASS
    );
}

// ---------------------------------------------------------------------------
// Flight Rules stage projections (ticket flight-rules-workflow-projection,
// KRZ-345, design D-G): worker/validator sessions receive only their stage's
// rules from the approved pin, inside the marked untrusted boundary, and the
// recorded prompt hash covers the exact projection. Anti-vacuity prefix
// `flight_rules_projection_` (grep-verified unique to this ticket's tests).
// ---------------------------------------------------------------------------

/// A hand-built approved pin spanning every stage: implementation
/// (ZZ-IMPL-001, enforced must), validation (ZZ-VAL-001, approved should),
/// planning-only (ZZ-PLAN-001) and merge-only (ZZ-MERGE-001) rules that must
/// never reach a worker/validator session.
fn standards_pin() -> kranz_engine::types::StandardsPin {
    fn rule(
        id: &str,
        rfc: &str,
        revision: u64,
        level: &str,
        status: &str,
        stages: &[&str],
        checker: Option<&str>,
    ) -> kranz_engine::types::PinnedRule {
        kranz_engine::types::PinnedRule {
            id: id.to_string(),
            revision,
            rfc: rfc.to_string(),
            level: level.to_string(),
            effective_status: status.to_string(),
            statement: format!("zz statement for {id}."),
            domains: vec!["zz".to_string()],
            stages: stages.iter().map(|s| s.to_string()).collect(),
            when_paths: vec![],
            task_classes: vec![],
            checker: checker.map(str::to_string),
            waivable: false,
        }
    }
    kranz_engine::types::StandardsPin {
        pack_name: "zz-pack".to_string(),
        pack_dir: "vendor/pack".to_string(),
        standards_root: "standards".to_string(),
        digest: "ab".repeat(32),
        source: kranz_engine::types::StandardsPinSource::RepoTracked,
        task_class: None,
        touch_set: vec!["crates/**".to_string()],
        context_paths: Vec::new(),
        gates: Vec::new(),
        rules: vec![
            rule(
                "ZZ-IMPL-001",
                "RFC-002",
                2,
                "must",
                "enforced",
                &["implementation", "validation"],
                Some("gate:zz-gate"),
            ),
            rule(
                "ZZ-MERGE-001",
                "RFC-002",
                1,
                "must",
                "enforced",
                &["merge"],
                Some("gate:zz-gate"),
            ),
            rule(
                "ZZ-PLAN-001",
                "RFC-001",
                1,
                "should",
                "approved",
                &["planning"],
                Some("agent-judgement"),
            ),
            rule(
                "ZZ-VAL-001",
                "RFC-001",
                1,
                "should",
                "approved",
                &["validation"],
                Some("agent-judgement"),
            ),
        ],
    }
}

/// The `worker.spawned` prompt hash recorded in the log (the session
/// provenance the projection must be covered by).
fn spawned_prompt_hash(p: &MissionPaths) -> String {
    read_log(p)
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::WorkerSpawned { prompt_hash, .. } => Some(prompt_hash.clone()),
            _ => None,
        })
        .expect("worker.spawned recorded")
}

#[tokio::test]
async fn flight_rules_projection_worker_prompt_projects_implementation_stage_only() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    let cfg = MissionConfig::default();
    let pin = standards_pin();
    let backend =
        MockBackend::with_scripts(vec![MockScript::single_shot_json(&worker_report_json())]);
    let outcome = run_worker(
        &backend,
        &mut log,
        &p,
        &cfg,
        &feature(),
        "goal",
        "milestone",
        None,
        None,
        None,
        &[],
        &[],
        &[],
        AuthVerdict::Inconclusive,
        &[],
        None,
        Some(&pin),
    )
    .await
    .unwrap();
    assert_eq!(outcome.result, RunResult::Pass);

    let specs = backend.started_specs();
    let prompt = specs[0]
        .append_system_prompt
        .as_deref()
        .expect("role prompt");
    // Only the implementation-stage rule projects, labelled with its source,
    // inside the marked untrusted boundary naming both digests.
    assert!(prompt.contains("`ZZ-IMPL-001` r2"), "{prompt}");
    for absent in ["ZZ-PLAN-001", "ZZ-VAL-001", "ZZ-MERGE-001"] {
        assert!(
            !prompt.contains(absent),
            "{absent} must never reach the worker: {prompt}"
        );
    }
    assert!(prompt.contains("worker projection"), "{prompt}");
    assert!(prompt.contains("untrusted content boundary"), "{prompt}");
    assert!(
        prompt.contains("cannot register tools, commands, grants, or permissions"),
        "{prompt}"
    );
    assert!(
        prompt.contains(&format!("sha256:{}", pin.digest)),
        "{prompt}"
    );
    assert!(prompt.contains("projection digest `sha256:"), "{prompt}");
    assert!(
        prompt.contains("source: pack `zz-pack` root `standards`, RFC `RFC-002`"),
        "{prompt}"
    );
    // Honest labels: the enforced MUST is the only blocking-capable rule.
    assert!(prompt.contains("may block through its checker"), "{prompt}");
    assert!(
        prompt.contains("an approved rule can never block"),
        "{prompt}"
    );

    // The recorded session prompt hash covers the exact projection text —
    // replay identifies the manifest/projection digest from the header.
    drop(log);
    let recorded = spawned_prompt_hash(&p);
    assert_eq!(recorded, kranz_engine::prompts::hash_text(prompt));
    assert_ne!(
        recorded,
        kranz_engine::prompts::hash(Role::Worker),
        "the extended prompt must hash differently from the bare template"
    );
}

#[tokio::test]
async fn flight_rules_projection_validator_prompts_project_validation_stage_only() {
    for (role, surface) in [
        (Role::ValidatorScrutiny, "validator-scrutiny projection"),
        (Role::ValidatorFunctional, "validator-functional projection"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let mut log = seeded_log(&p);
        let cfg = MissionConfig::default();
        let pin = standards_pin();
        let backend = MockBackend::with_scripts(vec![MockScript::single_shot_json(
            &json!({ "findings": [], "summary": "all good" }),
        )]);
        run_validator(
            &backend,
            &mut log,
            &p,
            &cfg,
            role,
            &milestone(),
            &[],
            "abc123",
            None,
            None,
            &[],
            &[],
            &[],
            None,
            Some(&pin),
        )
        .await
        .unwrap();

        let specs = backend.started_specs();
        let prompt = specs[0]
            .append_system_prompt
            .as_deref()
            .expect("role prompt");
        // Validation-stage rules only, in stable id order.
        assert!(prompt.contains("`ZZ-IMPL-001` r2"), "{role:?}: {prompt}");
        assert!(prompt.contains("`ZZ-VAL-001` r1"), "{role:?}: {prompt}");
        for absent in ["ZZ-PLAN-001", "ZZ-MERGE-001"] {
            assert!(
                !prompt.contains(absent),
                "{absent} must never reach a validator: {prompt}"
            );
        }
        assert!(
            prompt.find("ZZ-IMPL-001").unwrap() < prompt.find("ZZ-VAL-001").unwrap(),
            "stable id order: {prompt}"
        );
        assert!(prompt.contains(surface), "{role:?}: {prompt}");
        // The approved SHOULD is labelled advisory, never blocking.
        let val_line = prompt
            .lines()
            .find(|l| l.contains("ZZ-VAL-001"))
            .expect("the approved rule line");
        assert!(val_line.contains("approved should"), "{val_line}");
        assert!(val_line.contains("advisory — cannot block"), "{val_line}");

        drop(log);
        let recorded = spawned_prompt_hash(&p);
        assert_eq!(
            recorded,
            kranz_engine::prompts::hash_text(prompt),
            "{role:?}: the recorded hash must cover the exact projection"
        );
    }
}

#[tokio::test]
async fn flight_rules_projection_no_pin_keeps_prompt_and_hash_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    let cfg = MissionConfig::default();
    let backend =
        MockBackend::with_scripts(vec![MockScript::single_shot_json(&worker_report_json())]);
    run_worker(
        &backend,
        &mut log,
        &p,
        &cfg,
        &feature(),
        "goal",
        "milestone",
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
    .unwrap();

    let specs = backend.started_specs();
    let prompt = specs[0]
        .append_system_prompt
        .as_deref()
        .expect("role prompt");
    assert!(
        !prompt.contains("Flight Rules"),
        "no pin ⇒ nothing appended: {prompt}"
    );
    drop(log);
    assert_eq!(
        spawned_prompt_hash(&p),
        kranz_engine::prompts::hash(Role::Worker),
        "no pin ⇒ the recorded hash is the bare template hash, as before"
    );
}
