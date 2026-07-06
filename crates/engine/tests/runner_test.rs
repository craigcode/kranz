//! Integration tests for permission profiles (plan §4.7) and the session
//! runner (plan §4.6), driven entirely through the mock backend.

use kranz_engine::backend::{PromptMode, SessionExit, SessionSpec};
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
    MilestoneStatus, MissionConfig, Role, RunResult, TokenUsage,
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
        settings_json: None,
        json_schema: None,
        max_budget_usd: Some(1.0),
        max_turns: Some(10),
        env: HashMap::new(),
    }
}

fn worker_meta(run_id: &str) -> RunMeta {
    RunMeta {
        run_id: run_id.to_string(),
        role: Role::Worker,
        feature_id: Some("f-1".to_string()),
        milestone_id: None,
        model: "mock-model".to_string(),
        prompt_hash: "deadbeef0000".to_string(),
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
        ..MissionConfig::default()
    };
    let profile = permissions::for_role(Role::Worker, &cfg, &[], &[]);

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
    let profile = permissions::for_role(Role::Orchestrator, &cfg, &[], &[]);

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
        let profile = permissions::for_role(role, &cfg, &[], &[]);

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
        ..MissionConfig::default()
    };
    let commands = vec!["cargo test --all".to_string()];

    for role in [Role::ValidatorScrutiny, Role::ValidatorFunctional] {
        let profile = permissions::for_role(role, &cfg, &commands, &[]);
        assert_eq!(profile.permission_mode.as_deref(), Some("default"));
        for expected in [
            "Bash(cargo test --all*)", // contract command
            "Bash(npm run lint*)",     // config extra
            "Read",
            "Bash(git diff*)",
        ] {
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
}

#[test]
fn dangerously_allow_all_bypasses_every_role() {
    let cfg = MissionConfig {
        dangerously_allow_all: true,
        ..MissionConfig::default()
    };
    for role in [
        Role::Worker,
        Role::Orchestrator,
        Role::ValidatorScrutiny,
        Role::ValidatorFunctional,
    ] {
        let profile = permissions::for_role(role, &cfg, &["cargo test".to_string()], &[]);
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
async fn run_validator_builds_spec_permissions_and_parses_report() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = seeded_log(&p);
    let cfg = MissionConfig {
        allow_validator_commands: vec!["npm run lint".to_string()],
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
        Role::ValidatorScrutiny,
        &milestone(),
        &contract,
        "abc123",
        None,
        None,
        &[],
        &[],
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
        !spec.env.contains_key("KRANZ_BASE_SHA"),
        "no base sha means no env var"
    );
    assert_eq!(spec.model, cfg.validator_scrutiny.model);
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
            role: Role::ValidatorScrutiny,
            ..
        } if m == "ms-1"
    )));
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
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("validator role"), "got: {err}");
}

/// Contract commands must admit their natural reinvocations (observed live:
/// verbatim-only prefixes denied the validator its own checks and blocked a
/// milestone on a permissions artifact).
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
    // Verbatim, bare segment, and heredoc-ish/arg-extended reinvocations.
    assert!(covers("python3 extract_links.py && echo EXIT_OK"));
    assert!(covers("python3 extract_links.py"));
    assert!(covers(
        "python3 extract_links.py operator@example.com out.txt"
    ));
    assert!(covers("echo EXIT_OK"));
    // Heredoc contract command: leading-two-token rule admits `python3 -`.
    let heredoc = command_allow_patterns("python3 - <<'PY'\nprint('ok')\nPY");
    assert!(heredoc.iter().any(|p| p == "Bash(python3 -*)"));
    // Unrelated programs stay uncovered.
    assert!(!covers("curl https://example.com"));
    assert!(!covers("rm -rf /"));

    // And the profile carries them through for validators.
    let cfg = MissionConfig::default();
    let profile = kranz_engine::permissions::for_role(
        Role::ValidatorFunctional,
        &cfg,
        &["python3 -m pytest test_x.py -v".to_string()],
        &[],
    );
    assert!(profile
        .allowed_tools
        .iter()
        .any(|p| p == "Bash(python3 -m*)"));
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
