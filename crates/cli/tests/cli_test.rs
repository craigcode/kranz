//! Integration tests for the `kranz` CLI.
//!
//! No real `claude` binary is ever spawned: parsing is exercised through
//! clap's `try_parse_from`, rendering through the pure `output` functions,
//! and the control-inbox commands directly against a tempdir.

use chrono::Utc;
use clap::{CommandFactory, Parser};
use kranz_cli::cli::{Cli, Command};
use kranz_cli::commands;
use kranz_cli::output;
use kranz_cli::tail::EventRenderer;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::types::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn event(seq: u64, mission_id: &str, kind: EventKind) -> Event {
    Event { seq, ts: Utc::now(), mission_id: mission_id.to_string(), kind }
}

fn created_kind(goal: &str, mission_id: &str) -> EventKind {
    EventKind::MissionCreated {
        goal: goal.to_string(),
        base_branch: "main".to_string(),
        mission_branch: format!("kranz/mission-{mission_id}"),
        config: MissionConfig::default(),
    }
}

fn sample_plan() -> Plan {
    Plan {
        goal: "ship it".to_string(),
        validation_contract: vec![Assertion {
            id: "a-1".to_string(),
            statement: "tests pass".to_string(),
            check: AssertionCheck::Command,
            command: Some("cargo test".to_string()),
        }],
        milestones: vec![PlanMilestone {
            title: "Milestone One".to_string(),
            features: vec![
                PlanFeature {
                    title: "Feature One".to_string(),
                    spec: "build the first thing".to_string(),
                    validation_criteria: vec!["it works".to_string()],
                },
                PlanFeature {
                    title: "Feature Two".to_string(),
                    spec: "build the second thing".to_string(),
                    validation_criteria: vec![],
                },
            ],
        }],
    }
}

/// Write a hand-built events.jsonl for `mission_id` under `repo`.
fn write_events(repo: &Path, mission_id: &str, kinds: Vec<EventKind>) -> PathBuf {
    let dir = repo.join(".kranz").join("missions").join(mission_id);
    fs::create_dir_all(&dir).unwrap();
    let mut lines = String::new();
    for (i, kind) in kinds.into_iter().enumerate() {
        let e = event((i + 1) as u64, mission_id, kind);
        lines.push_str(&serde_json::to_string(&e).unwrap());
        lines.push('\n');
    }
    let path = dir.join("events.jsonl");
    fs::write(&path, lines).unwrap();
    path
}

// ---------------------------------------------------------------------------
// Argument parsing (every subcommand, incl. flags)
// ---------------------------------------------------------------------------

#[test]
fn parses_plan() {
    let cli = Cli::try_parse_from(["kranz", "plan", "build the thing"]).unwrap();
    match cli.command {
        Command::Plan { goal } => assert_eq!(goal, "build the thing"),
        other => panic!("expected Plan, got {other:?}"),
    }
    // goal is required
    assert!(Cli::try_parse_from(["kranz", "plan"]).is_err());
}

#[test]
fn parses_run_with_global_flags() {
    let cli = Cli::try_parse_from([
        "kranz",
        "--repo",
        "some/repo",
        "--mission",
        "m-1",
        "--force-lock",
        "--dangerously-allow-all",
        "run",
    ])
    .unwrap();
    assert!(matches!(cli.command, Command::Run));
    assert_eq!(cli.repo.as_deref(), Some(Path::new("some/repo")));
    assert_eq!(cli.mission.as_deref(), Some("m-1"));
    assert!(cli.force_lock);
    assert!(cli.dangerously_allow_all);

    // Global flags also parse after the subcommand.
    let cli = Cli::try_parse_from(["kranz", "run", "--force-lock", "--mission", "m-2"]).unwrap();
    assert!(matches!(cli.command, Command::Run));
    assert!(cli.force_lock);
    assert_eq!(cli.mission.as_deref(), Some("m-2"));
}

#[test]
fn parses_status() {
    let cli = Cli::try_parse_from(["kranz", "status"]).unwrap();
    assert!(matches!(cli.command, Command::Status { json: false }));
    let cli = Cli::try_parse_from(["kranz", "status", "--json"]).unwrap();
    assert!(matches!(cli.command, Command::Status { json: true }));
}

#[test]
fn parses_pause_and_resume() {
    assert!(matches!(Cli::try_parse_from(["kranz", "pause"]).unwrap().command, Command::Pause));
    assert!(matches!(Cli::try_parse_from(["kranz", "resume"]).unwrap().command, Command::Resume));
}

#[test]
fn parses_msg() {
    let cli = Cli::try_parse_from(["kranz", "msg", "hello there"]).unwrap();
    match cli.command {
        Command::Msg { text, interrupt } => {
            assert_eq!(text, "hello there");
            assert!(!interrupt);
        }
        other => panic!("expected Msg, got {other:?}"),
    }
    let cli = Cli::try_parse_from(["kranz", "msg", "stop it", "--interrupt"]).unwrap();
    match cli.command {
        Command::Msg { text, interrupt } => {
            assert_eq!(text, "stop it");
            assert!(interrupt);
        }
        other => panic!("expected Msg, got {other:?}"),
    }
}

#[test]
fn parses_missions() {
    assert!(matches!(
        Cli::try_parse_from(["kranz", "missions"]).unwrap().command,
        Command::Missions
    ));
}

#[test]
fn parses_serve() {
    let cli = Cli::try_parse_from(["kranz", "serve"]).unwrap();
    assert!(matches!(cli.command, Command::Serve { port: 4560, open: false, dashboard: None }));
    let cli = Cli::try_parse_from(["kranz", "serve", "--port", "5001", "--open"]).unwrap();
    assert!(matches!(cli.command, Command::Serve { port: 5001, open: true, .. }));
}

// ---------------------------------------------------------------------------
// msg --help documents the queue/interrupt semantics (plan §4.5)
// ---------------------------------------------------------------------------

#[test]
fn msg_help_documents_queue_and_interrupt() {
    let mut cmd = Cli::command();
    let msg = cmd.find_subcommand_mut("msg").expect("msg subcommand");
    let help = msg.render_long_help().to_string();
    // clap wraps help text; normalize whitespace before matching sentences.
    let normalized = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalized.contains("Messages queue and are processed between worker runs by default"),
        "missing queue sentence in: {normalized}"
    );
    assert!(
        normalized.contains(
            "aborts the current worker run (recorded as partial) before injecting the message"
        ),
        "missing interrupt sentence in: {normalized}"
    );
}

// ---------------------------------------------------------------------------
// Status rendering from a hand-written events.jsonl
// ---------------------------------------------------------------------------

#[test]
fn status_renders_tree_icons_totals_messages_and_decisions() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_events(
        repo,
        "m-test",
        vec![
            created_kind("ship it", "m-test"),
            EventKind::PlanApproved { plan: sample_plan() },
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".to_string(),
                start_sha: "abc123".to_string(),
            },
            EventKind::FeatureStarted { feature_id: "f-1-1".to_string() },
            EventKind::WorkerSpawned {
                run_id: "w-1".to_string(),
                role: Role::Worker,
                feature_id: Some("f-1-1".to_string()),
                milestone_id: None,
                sdk_session_id: "sess-1".to_string(),
                model: "sonnet".to_string(),
                prompt_hash: "hash".to_string(),
                transcript_path: "runs/w-1.jsonl".to_string(),
            },
            EventKind::WorkerCompleted {
                run_id: "w-1".to_string(),
                result: RunResult::Pass,
                tokens: TokenUsage { input: 1000, output: 200, cache_read: 50, cache_write: 25 },
                cost_usd: Some(0.5),
                report: None,
            },
            EventKind::FeatureCompleted {
                feature_id: "f-1-1".to_string(),
                commits: vec!["abc feature one".to_string()],
            },
            EventKind::OrchestratorDecision {
                summary: "looks good".to_string(),
                detail: None,
            },
            EventKind::UserMessage { text: "hurry up".to_string(), interrupt: false },
        ],
    );

    let state = commands::load_state(repo, "m-test").unwrap();
    let rendered = output::render_status(&state);

    assert!(rendered.contains("mission m-test  RUNNING  ship it"), "headline in:\n{rendered}");
    assert!(rendered.contains("branch  kranz/mission-m-test (base main)"), "branch in:\n{rendered}");
    // Milestone active ◐ with fixCycles; features complete ● and pending ○.
    assert!(rendered.contains("[◐] ms-1 Milestone One (fixCycles 0)"), "milestone in:\n{rendered}");
    assert!(
        rendered.contains("[●] f-1-1 Feature One (runs 1, respawns 0)"),
        "feature one in:\n{rendered}"
    );
    assert!(
        rendered.contains("[○] f-1-2 Feature Two (runs 0, respawns 0)"),
        "feature two in:\n{rendered}"
    );
    assert!(
        rendered.contains("totals: tokens 1000 in / 200 out, cache 50 r / 25 w, cost $0.50"),
        "totals in:\n{rendered}"
    );
    assert!(rendered.contains("pending user messages:"), "pending header in:\n{rendered}");
    assert!(rendered.contains("  - hurry up"), "pending message in:\n{rendered}");
    assert!(rendered.contains("  - looks good"), "decision in:\n{rendered}");
}

#[test]
fn status_icons_cover_terminal_states() {
    // Blocked milestone ✖, failed feature ✗, skipped feature ⊘.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_events(
        repo,
        "m-icons",
        vec![
            created_kind("icon check", "m-icons"),
            EventKind::PlanApproved { plan: sample_plan() },
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".to_string(),
                start_sha: "abc".to_string(),
            },
            EventKind::FeatureFailed {
                feature_id: "f-1-1".to_string(),
                reason: "no good".to_string(),
            },
            EventKind::FeatureSkipped {
                feature_id: "f-1-2".to_string(),
                reason: "cut scope".to_string(),
            },
            EventKind::MilestoneBlocked {
                milestone_id: "ms-1".to_string(),
                reason: "cap reached".to_string(),
            },
        ],
    );
    let state = commands::load_state(repo, "m-icons").unwrap();
    let rendered = output::render_status(&state);
    assert!(rendered.contains("mission m-icons  BLOCKED"), "in:\n{rendered}");
    assert!(rendered.contains("[✖] ms-1"), "blocked milestone in:\n{rendered}");
    assert!(rendered.contains("[✗] f-1-1"), "failed feature in:\n{rendered}");
    assert!(rendered.contains("[⊘] f-1-2"), "skipped feature in:\n{rendered}");
}

// ---------------------------------------------------------------------------
// Control enqueueing (msg / pause / resume)
// ---------------------------------------------------------------------------

fn queued_json_files(repo: &Path, mission_id: &str) -> Vec<PathBuf> {
    let control = repo.join(".kranz").join("missions").join(mission_id).join("control");
    let mut files: Vec<PathBuf> = fs::read_dir(control)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    files
}

#[test]
fn msg_enqueues_control_command() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_events(repo, "m-1", vec![created_kind("goal", "m-1")]);

    let queued = commands::cmd_msg(repo, "m-1", "hello there", true).unwrap();
    assert!(queued.is_file());

    let files = queued_json_files(repo, "m-1");
    assert_eq!(files.len(), 1);
    let value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&files[0]).unwrap()).unwrap();
    assert_eq!(
        value,
        serde_json::json!({ "kind": "msg", "text": "hello there", "interrupt": true })
    );
}

#[test]
fn pause_and_resume_enqueue_control_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_events(repo, "m-1", vec![created_kind("goal", "m-1")]);

    commands::cmd_pause(repo, "m-1").unwrap();
    commands::cmd_resume(repo, "m-1").unwrap();

    let files = queued_json_files(repo, "m-1");
    assert_eq!(files.len(), 2);
    let first: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&files[0]).unwrap()).unwrap();
    let second: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&files[1]).unwrap()).unwrap();
    assert_eq!(first, serde_json::json!({ "kind": "pause" }));
    assert_eq!(second, serde_json::json!({ "kind": "resume" }));
}

#[test]
fn msg_rejects_unknown_mission() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(commands::cmd_msg(tmp.path(), "m-none", "x", false).is_err());
}

// ---------------------------------------------------------------------------
// Mission auto-selection
// ---------------------------------------------------------------------------

#[test]
fn mission_selection_none_and_single() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    assert!(commands::select_mission(repo, None).is_err());

    write_events(repo, "m-only", vec![created_kind("goal", "m-only")]);
    assert_eq!(commands::select_mission(repo, None).unwrap(), "m-only");
}

#[test]
fn mission_selection_picks_newest_events_log() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let old_events = write_events(repo, "m-old", vec![created_kind("old goal", "m-old")]);
    write_events(repo, "m-new", vec![created_kind("new goal", "m-new")]);

    // Push m-old's events.jsonl an hour into the past so mtimes differ
    // regardless of filesystem timestamp granularity.
    let file = fs::OpenOptions::new().write(true).open(&old_events).unwrap();
    file.set_times(
        fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(3600)),
    )
    .unwrap();

    assert_eq!(commands::select_mission(repo, None).unwrap(), "m-new");
    // --mission overrides auto-selection.
    assert_eq!(commands::select_mission(repo, Some("m-old")).unwrap(), "m-old");
    // ... but a bogus explicit id is an error.
    assert!(commands::select_mission(repo, Some("m-missing")).is_err());
}

// ---------------------------------------------------------------------------
// missions listing
// ---------------------------------------------------------------------------

#[test]
fn missions_lists_ids_status_and_goal() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_events(repo, "m-a", vec![created_kind("goal a", "m-a")]);
    write_events(
        repo,
        "m-b",
        vec![created_kind("goal b", "m-b"), EventKind::PlanApproved { plan: sample_plan() }],
    );

    let listing = commands::cmd_missions(repo).unwrap();
    assert!(listing.contains("m-a"), "in: {listing}");
    assert!(listing.contains("PLANNING"), "in: {listing}");
    assert!(listing.contains("goal a"), "in: {listing}");
    assert!(listing.contains("m-b"), "in: {listing}");
    assert!(listing.contains("RUNNING"), "in: {listing}");
}

// ---------------------------------------------------------------------------
// Live-printer event rendering
// ---------------------------------------------------------------------------

#[test]
fn renderer_tags_worker_lines_and_truncates() {
    let mut renderer = EventRenderer::new(false);

    let spawned = event(
        1,
        "m-1",
        EventKind::WorkerSpawned {
            run_id: "w-1".to_string(),
            role: Role::Worker,
            feature_id: Some("f-1-2".to_string()),
            milestone_id: None,
            sdk_session_id: "sess".to_string(),
            model: "sonnet".to_string(),
            prompt_hash: "hash".to_string(),
            transcript_path: "runs/w-1.jsonl".to_string(),
        },
    );
    assert_eq!(renderer.render(&spawned), "[worker f-1-2] spawned (sonnet)");

    let tool_use = event(
        2,
        "m-1",
        EventKind::WorkerMessage {
            run_id: "w-1".to_string(),
            tag: "tool-use".to_string(),
            content: "Bash: cargo test".to_string(),
        },
    );
    assert_eq!(renderer.render(&tool_use), "[worker f-1-2] tool-use: Bash: cargo test");

    let denied = event(
        3,
        "m-1",
        EventKind::WorkerMessage {
            run_id: "w-1".to_string(),
            tag: "denied".to_string(),
            content: "git push".to_string(),
        },
    );
    assert_eq!(renderer.render(&denied), "[worker f-1-2] DENIED: git push");

    let decision = event(
        4,
        "m-1",
        EventKind::OrchestratorDecision { summary: "carry on".to_string(), detail: None },
    );
    assert_eq!(renderer.render(&decision), "[orch] decision: carry on");

    let paused = event(5, "m-1", EventKind::MissionPaused {});
    assert_eq!(renderer.render(&paused), "[mission] paused");

    let validating =
        event(6, "m-1", EventKind::MilestoneValidating { milestone_id: "ms-1".to_string() });
    assert_eq!(renderer.render(&validating), "[milestone ms-1] validating");

    // Long, multi-line content collapses to one line capped at 160 chars.
    let long = event(
        7,
        "m-1",
        EventKind::WorkerMessage {
            run_id: "w-1".to_string(),
            tag: "text".to_string(),
            content: format!("line one\nline two {}", "x".repeat(500)),
        },
    );
    let line = renderer.render(&long);
    assert!(!line.contains('\n'));
    assert!(line.chars().count() <= 160, "len {} in: {line}", line.chars().count());
    assert!(line.ends_with('…'));
}
