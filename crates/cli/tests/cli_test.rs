//! Integration tests for the `kranz` CLI.
//!
//! No real `claude` binary is ever spawned: parsing is exercised through
//! clap's `try_parse_from`, rendering through the pure `output` functions,
//! and the control-inbox commands directly against a tempdir.

use chrono::Utc;
use clap::{CommandFactory, Parser};
use kranz_cli::cli::{Cli, Command, RevisionCommand};
use kranz_cli::commands;
use kranz_cli::output;
use kranz_cli::tail::EventRenderer;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::types::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[test]
fn licenses_travel_with_the_binary_outside_a_source_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir
        .path()
        .join(if cfg!(windows) { "kranz.exe" } else { "kranz" });
    fs::copy(env!("CARGO_BIN_EXE_kranz"), &binary).unwrap();
    let output = std::process::Command::new(&binary)
        .arg("licenses")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with(include_str!("../LICENSE")));
    assert!(text.contains(include_str!("../assets/THIRD_PARTY_NOTICES.txt")));
    assert!(text.ends_with(include_str!(
        "../assets/dashboard/dist/THIRD_PARTY_NOTICES.txt"
    )));
    assert!(text.contains("Microsoft Corporation"));
    assert!(text.contains("Meta Platforms"));
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        1,
        "licenses need no repository or sidecar files"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn event(seq: u64, mission_id: &str, kind: EventKind) -> Event {
    Event {
        seq,
        ts: Utc::now(),
        mission_id: mission_id.to_string(),
        kind,
    }
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
            negative_control: None,
            pty_script: None,
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
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec![],
        standards_manifest: None,
        reviewer_independence: None,
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

fn git(repo: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git")
}

// ---------------------------------------------------------------------------
// Argument parsing (every subcommand, incl. flags)
// ---------------------------------------------------------------------------

#[test]
fn parses_plan() {
    let cli = Cli::try_parse_from(["kranz", "plan", "build the thing"]).unwrap();
    match cli.command {
        Command::Plan { goal } => assert_eq!(goal.as_deref(), Some("build the thing")),
        other => panic!("expected Plan, got {other:?}"),
    }
    // No goal = resume the latest in-planning mission.
    let cli = Cli::try_parse_from(["kranz", "plan"]).unwrap();
    assert!(matches!(cli.command, Command::Plan { goal: None }));
}

#[test]
fn kranz_init_parses_detection_and_registration_answers() {
    let cli = Cli::try_parse_from([
        "kranz",
        "init",
        "--gate",
        "make test",
        "--gate",
        "make lint",
        "--register",
        "--id",
        "alpha",
        "--display-name",
        "Alpha App",
    ])
    .unwrap();
    match cli.command {
        Command::Init {
            gates,
            register,
            id,
            display_name,
        } => {
            assert_eq!(gates, ["make test", "make lint"]);
            assert!(register);
            assert_eq!(id.as_deref(), Some("alpha"));
            assert_eq!(display_name.as_deref(), Some("Alpha App"));
        }
        other => panic!("expected init, got {other:?}"),
    }
    assert!(Cli::try_parse_from(["kranz", "init", "--id", "alpha"]).is_err());
}

/// Exit code only; the `--json` payload shape is pinned by
/// `knowledge_refresh_report_serializes_verdicts_for_json_output` in the engine.
#[test]
fn knowledge_refresh_cli_returns_one_for_unverified_note() {
    let repo = tempfile::tempdir().unwrap();
    let vault = repo.path().join("docs/knowledge");
    fs::create_dir_all(&vault).unwrap();
    fs::write(
        vault.join("unverified.md"),
        "---\ntitle: Unverified\nowner: agent\nfreshness: check-on-touch\nlast_verified: 2026-08-18\nverified_against:\n---\n",
    )
    .unwrap();

    assert_eq!(
        commands::cmd_knowledge_refresh(repo.path(), false).unwrap(),
        1
    );
    assert_eq!(
        commands::cmd_knowledge_refresh(repo.path(), true).unwrap(),
        1
    );
}

#[test]
fn knowledge_refresh_cli_returns_zero_for_verified_note() {
    let repo = tempfile::tempdir().unwrap();
    if !git(repo.path(), &["init", "-b", "main"]).status.success() {
        assert!(git(repo.path(), &["init"]).status.success());
    }
    assert!(git(repo.path(), &["config", "user.name", "kranz-test"])
        .status
        .success());
    assert!(
        git(repo.path(), &["config", "user.email", "test@kranz.local"])
            .status
            .success()
    );
    fs::write(repo.path().join("AGENTS.md"), "rules\n").unwrap();
    let vault = repo.path().join("docs/knowledge");
    fs::create_dir_all(&vault).unwrap();
    fs::write(
        vault.join("verified.md"),
        "---\ntitle: Verified\nowner: agent\nfreshness: check-on-touch\nlast_verified: 2099-01-01\nverified_against:\n  - AGENTS.md\n---\n",
    )
    .unwrap();
    assert!(git(repo.path(), &["add", "-A"]).status.success());
    assert!(git(
        repo.path(),
        &["-c", "commit.gpgsign=false", "commit", "-m", "seed"]
    )
    .status
    .success());

    assert_eq!(
        commands::cmd_knowledge_refresh(repo.path(), false).unwrap(),
        0
    );
    assert_eq!(
        commands::cmd_knowledge_refresh(repo.path(), true).unwrap(),
        0
    );
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

/// The two lock flags route to the engine's three-tier [`LockForce`]:
/// nothing → No, --force-lock → IfNotLive, --dangerously-steal-live-lock →
/// EvenIfLive (implies force; passing both keeps the strongest).
#[test]
fn lock_flags_route_to_lock_force_tiers() {
    use kranz_engine::event_log::LockForce;

    let cli = Cli::try_parse_from(["kranz", "run"]).unwrap();
    assert_eq!(cli.lock_force(), LockForce::No);

    let cli = Cli::try_parse_from(["kranz", "run", "--force-lock"]).unwrap();
    assert_eq!(cli.lock_force(), LockForce::IfNotLive);

    let cli = Cli::try_parse_from(["kranz", "run", "--dangerously-steal-live-lock"]).unwrap();
    assert_eq!(cli.lock_force(), LockForce::EvenIfLive);

    // Both at once is legal; the stronger tier wins.
    let cli = Cli::try_parse_from([
        "kranz",
        "--force-lock",
        "--dangerously-steal-live-lock",
        "abandon",
        "m-1",
    ])
    .unwrap();
    assert!(cli.force_lock);
    assert!(cli.dangerously_steal_live_lock);
    assert_eq!(cli.lock_force(), LockForce::EvenIfLive);
}

#[test]
fn parses_status() {
    let cli = Cli::try_parse_from(["kranz", "status"]).unwrap();
    assert!(matches!(cli.command, Command::Status { json: false }));
    let cli = Cli::try_parse_from(["kranz", "status", "--json"]).unwrap();
    assert!(matches!(cli.command, Command::Status { json: true }));
}

#[test]
fn parses_sandbox_probe() {
    let cli = Cli::try_parse_from(["kranz", "sandbox-probe"]).unwrap();
    assert!(matches!(cli.command, Command::SandboxProbe { json: false }));
    let cli = Cli::try_parse_from(["kranz", "sandbox-probe", "--json"]).unwrap();
    assert!(matches!(cli.command, Command::SandboxProbe { json: true }));
}

#[test]
fn parses_sandbox_prepare_targets() {
    assert!(Cli::try_parse_from(["kranz", "sandbox-prepare"]).is_err());
    let cli = Cli::try_parse_from([
        "kranz",
        "sandbox-prepare",
        "--target",
        "C:\\",
        "--target",
        "D:\\",
    ])
    .unwrap();
    assert!(matches!(
        cli.command,
        Command::SandboxPrepare { targets }
            if targets == vec![PathBuf::from("C:\\"), PathBuf::from("D:\\")]
    ));
}

#[test]
fn parses_export_traces() {
    let cli = Cli::try_parse_from(["kranz", "export-traces"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::ExportTraces {
            mission_id: None,
            all: false,
            out: None,
        }
    ));

    let cli = Cli::try_parse_from(["kranz", "export-traces", "m-1"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::ExportTraces {
            mission_id: Some(ref id),
            all: false,
            out: None,
        } if id == "m-1"
    ));

    let cli =
        Cli::try_parse_from(["kranz", "export-traces", "--all", "--out", "out.jsonl"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::ExportTraces {
            mission_id: None,
            all: true,
            out: Some(ref path),
        } if path == std::path::Path::new("out.jsonl")
    ));
}

#[test]
fn corpus_export_cli_parses() {
    let cli = Cli::try_parse_from(["kranz", "export-corpus"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::ExportCorpus {
            mission_id: None,
            all: false,
            out: None,
        }
    ));

    let cli = Cli::try_parse_from(["kranz", "export-corpus", "m-1"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::ExportCorpus {
            mission_id: Some(ref id),
            all: false,
            out: None,
        } if id == "m-1"
    ));

    let cli =
        Cli::try_parse_from(["kranz", "export-corpus", "--all", "--out", "corpus.jsonl"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::ExportCorpus {
            mission_id: None,
            all: true,
            out: Some(ref path),
        } if path == std::path::Path::new("corpus.jsonl")
    ));
}

#[test]
fn parses_pause_and_resume() {
    assert!(matches!(
        Cli::try_parse_from(["kranz", "pause"]).unwrap().command,
        Command::Pause
    ));
    assert!(matches!(
        Cli::try_parse_from(["kranz", "resume"]).unwrap().command,
        Command::Resume
    ));
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
fn parses_revision_commands() {
    let cli = Cli::try_parse_from(["kranz", "revise", "m-1", "drop", "scope"]).unwrap();
    match cli.command {
        Command::Revise { id, instructions } => {
            assert_eq!(id, "m-1");
            assert_eq!(instructions, vec!["drop", "scope"]);
        }
        other => panic!("expected Revise, got {other:?}"),
    }

    let cli = Cli::try_parse_from(["kranz", "revision", "approve", "m-1", "2"]).unwrap();
    match cli.command {
        Command::Revision {
            command: RevisionCommand::Approve { id, revision },
        } => {
            assert_eq!(id, "m-1");
            assert_eq!(revision, 2);
        }
        other => panic!("expected Revision approve, got {other:?}"),
    }

    let cli = Cli::try_parse_from(["kranz", "revision", "reject", "m-1", "3"]).unwrap();
    match cli.command {
        Command::Revision {
            command: RevisionCommand::Reject { id, revision },
        } => {
            assert_eq!(id, "m-1");
            assert_eq!(revision, 3);
        }
        other => panic!("expected Revision reject, got {other:?}"),
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
fn parses_scan_modes() {
    assert!(matches!(
        Cli::try_parse_from(["kranz", "scan"]).unwrap().command,
        Command::Scan {
            staged: false,
            range: None
        }
    ));

    assert!(matches!(
        Cli::try_parse_from(["kranz", "scan", "--staged"])
            .unwrap()
            .command,
        Command::Scan {
            staged: true,
            range: None
        }
    ));

    let cli = Cli::try_parse_from(["kranz", "scan", "--range", "main..HEAD"]).unwrap();
    match cli.command {
        Command::Scan { staged, range } => {
            assert!(!staged);
            assert_eq!(range.as_deref(), Some("main..HEAD"));
        }
        other => panic!("expected Scan, got {other:?}"),
    }
}

#[test]
fn parses_ready() {
    assert!(matches!(
        Cli::try_parse_from(["kranz", "ready"]).unwrap().command,
        Command::Ready {
            json: false,
            all: false
        }
    ));
    assert!(matches!(
        Cli::try_parse_from(["kranz", "ready", "--json"])
            .unwrap()
            .command,
        Command::Ready {
            json: true,
            all: false
        }
    ));
    assert!(matches!(
        Cli::try_parse_from(["kranz", "ready", "--all"])
            .unwrap()
            .command,
        Command::Ready {
            json: false,
            all: true
        }
    ));
}

#[test]
fn parses_serve() {
    let cli = Cli::try_parse_from(["kranz", "serve"]).unwrap();
    match cli.command {
        Command::Serve {
            port,
            ref host,
            insecure_lan,
            open,
            ref dashboard,
            ref token,
            slack,
            ..
        } => {
            assert_eq!(port, 4560);
            assert_eq!(host, "127.0.0.1", "default bind stays loopback");
            assert!(!insecure_lan);
            assert!(!open && !slack);
            assert!(dashboard.is_none() && token.is_none());
        }
        other => panic!("expected Serve, got {other:?}"),
    }
    let cli = Cli::try_parse_from(["kranz", "serve", "--port", "5001", "--open"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Serve {
            port: 5001,
            open: true,
            ..
        }
    ));
    // --host widens the bind (glasses/LAN clients); --insecure-lan acknowledges it.
    let cli =
        Cli::try_parse_from(["kranz", "serve", "--host", "0.0.0.0", "--insecure-lan"]).unwrap();
    match cli.command {
        Command::Serve {
            ref host,
            insecure_lan,
            ..
        } => {
            assert_eq!(host, "0.0.0.0");
            assert!(insecure_lan);
        }
        other => panic!("expected Serve, got {other:?}"),
    }
    // --token pins the mutation token (scripting).
    let cli = Cli::try_parse_from(["kranz", "serve", "--token", "sesame"]).unwrap();
    match cli.command {
        Command::Serve { token, .. } => assert_eq!(token.as_deref(), Some("sesame")),
        other => panic!("expected Serve, got {other:?}"),
    }
}

#[test]
fn parses_release() {
    let cli = Cli::try_parse_from(["kranz", "release"]).unwrap();
    match cli.command {
        Command::Release { ref url, ref token } => {
            assert_eq!(url, "http://127.0.0.1:4560");
            assert!(token.is_none());
        }
        other => panic!("expected Release, got {other:?}"),
    }

    // The global --mission is unaffected by (and available to) release.
    let cli = Cli::try_parse_from(["kranz", "--mission", "m-7", "release"]).unwrap();
    assert_eq!(cli.mission.as_deref(), Some("m-7"));
    assert!(matches!(cli.command, Command::Release { .. }));

    let cli = Cli::try_parse_from([
        "kranz",
        "release",
        "--url",
        "http://127.0.0.1:5001",
        "--token",
        "sesame",
    ])
    .unwrap();
    match cli.command {
        Command::Release { url, token } => {
            assert_eq!(url, "http://127.0.0.1:5001");
            assert_eq!(token.as_deref(), Some("sesame"));
        }
        other => panic!("expected Release, got {other:?}"),
    }
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
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".to_string(),
                start_sha: "abc123".to_string(),
            },
            EventKind::FeatureStarted {
                feature_id: "f-1-1".to_string(),
            },
            EventKind::WorkerSpawned {
                backend: None,
                run_id: "w-1".to_string(),
                role: Role::Worker,
                feature_id: Some("f-1-1".to_string()),
                milestone_id: None,
                candidate: None,
                executor_route: None,
                sdk_session_id: "sess-1".to_string(),
                model: "sonnet".to_string(),
                quant: "n/a".to_string(),
                weight_hash: None,
                prompt_hash: "hash".to_string(),
                transcript_path: "runs/w-1.jsonl".to_string(),
            },
            EventKind::WorkerCompleted {
                run_id: "w-1".to_string(),
                result: RunResult::Pass,
                tokens: TokenUsage {
                    input: 1000,
                    output: 200,
                    cache_read: 50,
                    cache_write: 25,
                },
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
            EventKind::UserMessage {
                text: "hurry up".to_string(),
                interrupt: false,
            },
        ],
    );

    let state = commands::load_state(repo, "m-test").unwrap();
    let rendered = output::render_status(&state);

    assert!(
        rendered.contains("mission m-test  RUNNING  ship it"),
        "headline in:\n{rendered}"
    );
    assert!(
        rendered.contains("branch  kranz/mission-m-test (base main)"),
        "branch in:\n{rendered}"
    );
    // Milestone active ◐ with fixCycles; features complete ● and pending ○.
    assert!(
        rendered.contains("[◐] ms-1 Milestone One (fixCycles 0)"),
        "milestone in:\n{rendered}"
    );
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
    assert!(
        rendered.contains("pending user messages:"),
        "pending header in:\n{rendered}"
    );
    assert!(
        rendered.contains("  - hurry up"),
        "pending message in:\n{rendered}"
    );
    assert!(
        rendered.contains("  - looks good"),
        "decision in:\n{rendered}"
    );
}

#[test]
fn export_traces_is_regenerable_and_filters_to_validated_passes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_events(
        repo,
        "m-export",
        vec![
            created_kind("ship it", "m-export"),
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".to_string(),
                start_sha: "abc123".to_string(),
            },
            EventKind::FeatureStarted {
                feature_id: "f-1-1".to_string(),
            },
            EventKind::WorkerSpawned {
                backend: None,
                run_id: "w-pass".to_string(),
                role: Role::Worker,
                feature_id: Some("f-1-1".to_string()),
                milestone_id: None,
                candidate: None,
                executor_route: None,
                sdk_session_id: "sess-pass".to_string(),
                model: "sonnet".to_string(),
                quant: "n/a".to_string(),
                weight_hash: None,
                prompt_hash: "hash".to_string(),
                transcript_path: "runs/w-pass.jsonl".to_string(),
            },
            EventKind::WorkerCompleted {
                run_id: "w-pass".to_string(),
                result: RunResult::Pass,
                tokens: TokenUsage::default(),
                cost_usd: None,
                report: Some(WorkerReport {
                    result: RunResult::Pass,
                    summary: "built feature one".to_string(),
                    files_touched: vec![],
                    tests_added: vec![],
                    test_evidence: "cargo test: ok".to_string(),
                    dependencies_added: vec![],
                    known_gaps: vec![],
                    commits: vec!["abc feature one".to_string()],
                    commands_run: vec![],
                    escalation: None,
                    questions: None,
                }),
            },
            EventKind::FeatureCompleted {
                feature_id: "f-1-1".to_string(),
                commits: vec!["abc feature one".to_string()],
            },
            EventKind::FeatureStarted {
                feature_id: "f-1-2".to_string(),
            },
            EventKind::WorkerSpawned {
                backend: None,
                run_id: "w-fail".to_string(),
                role: Role::Worker,
                feature_id: Some("f-1-2".to_string()),
                milestone_id: None,
                candidate: None,
                executor_route: None,
                sdk_session_id: "sess-fail".to_string(),
                model: "sonnet".to_string(),
                quant: "n/a".to_string(),
                weight_hash: None,
                prompt_hash: "hash".to_string(),
                transcript_path: "runs/w-fail.jsonl".to_string(),
            },
            EventKind::WorkerCompleted {
                run_id: "w-fail".to_string(),
                result: RunResult::Fail,
                tokens: TokenUsage::default(),
                cost_usd: None,
                report: Some(WorkerReport {
                    result: RunResult::Fail,
                    summary: "could not build feature two".to_string(),
                    files_touched: vec![],
                    tests_added: vec![],
                    test_evidence: String::new(),
                    dependencies_added: vec![],
                    known_gaps: vec![],
                    commits: vec![],
                    commands_run: vec![],
                    escalation: None,
                    questions: None,
                }),
            },
            EventKind::FeatureFailed {
                feature_id: "f-1-2".to_string(),
                reason: "gave up".to_string(),
                commits: Vec::new(),
            },
            EventKind::MilestoneCompleted {
                milestone_id: "ms-1".to_string(),
                tag: None,
            },
        ],
    );

    let first = commands::cmd_export_traces(repo, "m-export").unwrap();
    let second = commands::cmd_export_traces(repo, "m-export").unwrap();
    assert_eq!(
        first, second,
        "export-traces must be byte-identical across consecutive invocations"
    );

    let lines: Vec<&str> = first.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "only the validation-PASSED run should be exported: {first}"
    );
    let value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(value["runId"], "w-pass");
    assert_eq!(value["model"], "sonnet");
    assert!(
        !first.contains("w-fail"),
        "failed feature's run must not appear: {first}"
    );
}

#[test]
fn export_traces_rejects_mission_ids_outside_the_repo_namespace() {
    let repo = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write_events(
        outside.path(),
        "m-external",
        vec![created_kind("external", "m-external")],
    );
    let external_mission = outside.path().join(".kranz/missions/m-external");

    let err = commands::cmd_export_traces(repo.path(), external_mission.to_str().unwrap())
        .expect_err("an absolute path must not be accepted as a mission id");
    assert!(err.to_string().contains("invalid mission id"), "got: {err}");
}

#[test]
fn export_traces_all_aggregates_and_skips_unreadable_missions() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_events(
        repo,
        "m-a",
        vec![
            created_kind("ship a", "m-a"),
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".to_string(),
                start_sha: "abc123".to_string(),
            },
            EventKind::FeatureStarted {
                feature_id: "f-1-1".to_string(),
            },
            EventKind::WorkerSpawned {
                backend: None,
                run_id: "w-a-pass".to_string(),
                role: Role::Worker,
                feature_id: Some("f-1-1".to_string()),
                milestone_id: None,
                candidate: None,
                executor_route: None,
                sdk_session_id: "sess-a".to_string(),
                model: "sonnet".to_string(),
                quant: "n/a".to_string(),
                weight_hash: None,
                prompt_hash: "hash".to_string(),
                transcript_path: "runs/w-a-pass.jsonl".to_string(),
            },
            EventKind::WorkerCompleted {
                run_id: "w-a-pass".to_string(),
                result: RunResult::Pass,
                tokens: TokenUsage::default(),
                cost_usd: None,
                report: Some(WorkerReport {
                    result: RunResult::Pass,
                    summary: "built feature one".to_string(),
                    files_touched: vec![],
                    tests_added: vec![],
                    test_evidence: "cargo test: ok".to_string(),
                    dependencies_added: vec![],
                    known_gaps: vec![],
                    commits: vec!["abc feature one".to_string()],
                    commands_run: vec![],
                    escalation: None,
                    questions: None,
                }),
            },
            EventKind::FeatureCompleted {
                feature_id: "f-1-1".to_string(),
                commits: vec!["abc feature one".to_string()],
            },
            EventKind::MilestoneCompleted {
                milestone_id: "ms-1".to_string(),
                tag: None,
            },
        ],
    );
    // A second mission directory whose event log is missing entirely — must
    // be skipped, not fatal, for --all.
    fs::create_dir_all(repo.join(".kranz/missions/m-broken")).unwrap();

    let jsonl = commands::cmd_export_traces_all(repo);
    let lines: Vec<&str> = jsonl.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly one passed trace across missions, m-broken skipped: {jsonl}"
    );
    let value: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(value["runId"], "w-a-pass");
    assert_eq!(value["missionId"], "m-a");
}

/// Seed a mission exercising all three corpus sources: a validation-PASSED
/// worker run, a divergence noted+resolved (its second candidate stream
/// spawned but never completed — an unvalidated session), and an approved
/// grant park.
fn write_corpus_mission(repo: &Path, mission_id: &str) {
    write_events(
        repo,
        mission_id,
        vec![
            created_kind("ship it", mission_id),
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".to_string(),
                start_sha: "abc123".to_string(),
            },
            EventKind::FeatureStarted {
                feature_id: "f-1-1".to_string(),
            },
            EventKind::WorkerSpawned {
                backend: None,
                run_id: "w-pass".to_string(),
                role: Role::Worker,
                feature_id: Some("f-1-1".to_string()),
                milestone_id: None,
                candidate: None,
                executor_route: None,
                sdk_session_id: "sess-pass".to_string(),
                model: "sonnet".to_string(),
                quant: "n/a".to_string(),
                weight_hash: None,
                prompt_hash: "hash".to_string(),
                transcript_path: "runs/w-pass.jsonl".to_string(),
            },
            EventKind::WorkerCompleted {
                run_id: "w-pass".to_string(),
                result: RunResult::Pass,
                tokens: TokenUsage::default(),
                cost_usd: None,
                report: Some(WorkerReport {
                    result: RunResult::Pass,
                    summary: "built feature one".to_string(),
                    files_touched: vec![],
                    tests_added: vec![],
                    test_evidence: "cargo test: ok".to_string(),
                    dependencies_added: vec![],
                    known_gaps: vec![],
                    commits: vec!["abc feature one".to_string()],
                    commands_run: vec![],
                    escalation: None,
                    questions: None,
                }),
            },
            EventKind::FeatureCompleted {
                feature_id: "f-1-1".to_string(),
                commits: vec!["abc feature one".to_string()],
            },
            EventKind::WorkerSpawned {
                backend: None,
                run_id: "w-cand".to_string(),
                role: Role::Worker,
                feature_id: Some("f-1-1".to_string()),
                milestone_id: None,
                candidate: None,
                executor_route: None,
                sdk_session_id: "sess-cand".to_string(),
                model: "gpt-5".to_string(),
                quant: "n/a".to_string(),
                weight_hash: None,
                prompt_hash: "hash".to_string(),
                transcript_path: "runs/w-cand.jsonl".to_string(),
            },
            EventKind::DivergenceNoted {
                unit: "f-1-1".to_string(),
                candidates: vec![
                    DivergenceCandidate {
                        run_id: "w-pass".to_string(),
                        branch: format!("kranz/pool/{mission_id}/f-1-1-c0"),
                        backend: "claude".to_string(),
                        tree: "aaa".to_string(),
                    },
                    DivergenceCandidate {
                        run_id: "w-cand".to_string(),
                        branch: format!("kranz/pool/{mission_id}/f-1-1-c1"),
                        backend: "codex".to_string(),
                        tree: "bbb".to_string(),
                    },
                ],
                diverged: true,
            },
            EventKind::DivergenceResolved {
                unit: "f-1-1".to_string(),
                selected: Some(0),
                reason: "kept the first candidate".to_string(),
                decided_by: "operator".to_string(),
            },
            EventKind::GrantRequested {
                milestone_id: "ms-1".to_string(),
                kind: GrantKind::Command,
                command: "cargo test".to_string(),
            },
            EventKind::GrantApproved {
                kind: GrantKind::Command,
                command: "cargo test".to_string(),
            },
            EventKind::MilestoneCompleted {
                milestone_id: "ms-1".to_string(),
                tag: None,
            },
        ],
    );
}

#[test]
fn corpus_export_cli_is_regenerable_and_covers_all_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_corpus_mission(repo, "m-corpus");

    let first = commands::cmd_export_corpus(repo, "m-corpus").unwrap();
    let second = commands::cmd_export_corpus(repo, "m-corpus").unwrap();
    assert_eq!(
        first, second,
        "export-corpus must be byte-identical across consecutive invocations"
    );

    let lines: Vec<&str> = first.lines().collect();
    assert_eq!(lines.len(), 3, "one record per source: {first}");
    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();

    // worker-trace: provenance-tagged (backend derived, gate chain present
    // though empty on this gateless fixture); the unvalidated w-cand stream
    // never enters the corpus as a trace.
    assert_eq!(records[0]["source"], "worker-trace");
    assert_eq!(records[0]["runId"], "w-pass");
    assert_eq!(records[0]["missionId"], "m-corpus");
    assert_eq!(records[0]["backend"], "claude");
    assert!(records[0]["gateChain"].is_array());
    // divergence: both candidates and the resolution.
    assert_eq!(records[1]["source"], "divergence");
    assert_eq!(records[1]["candidates"][0]["runId"], "w-pass");
    assert_eq!(records[1]["candidates"][1]["runId"], "w-cand");
    assert_eq!(records[1]["resolution"]["selected"], 0);
    assert_eq!(records[1]["resolution"]["decidedBy"], "operator");
    // escalation: the approved grant park as a labeled judgment.
    assert_eq!(records[2]["source"], "escalation");
    assert_eq!(records[2]["kind"], "grant");
    assert_eq!(records[2]["ask"], "command: cargo test");
    assert_eq!(records[2]["decision"], "approved");
    assert!(records[2]["askSeq"].is_number());
}

#[test]
fn corpus_export_all_aggregates_and_skips_unreadable_missions() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_corpus_mission(repo, "m-a");
    // A second mission directory whose event log is missing entirely — must
    // be skipped, not fatal, for --all.
    fs::create_dir_all(repo.join(".kranz/missions/m-broken")).unwrap();

    let jsonl = commands::cmd_export_corpus_all(repo);
    let lines: Vec<&str> = jsonl.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "m-a's three records, m-broken skipped: {jsonl}"
    );
    for line in &lines {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(value["missionId"], "m-a");
    }
    // Aggregation is deterministic too: same tree in, same bytes out.
    assert_eq!(jsonl, commands::cmd_export_corpus_all(repo));
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
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".to_string(),
                start_sha: "abc".to_string(),
            },
            EventKind::FeatureFailed {
                feature_id: "f-1-1".to_string(),
                reason: "no good".to_string(),
                commits: Vec::new(),
            },
            EventKind::FeatureSkipped {
                feature_id: "f-1-2".to_string(),
                reason: "cut scope".to_string(),
            },
            EventKind::MilestoneBlocked {
                block_context: None,
                milestone_id: "ms-1".to_string(),
                reason: "cap reached".to_string(),
            },
        ],
    );
    let state = commands::load_state(repo, "m-icons").unwrap();
    let rendered = output::render_status(&state);
    assert!(
        rendered.contains("mission m-icons  BLOCKED"),
        "in:\n{rendered}"
    );
    assert!(
        rendered.contains("[✖] ms-1"),
        "blocked milestone in:\n{rendered}"
    );
    assert!(
        rendered.contains("[✗] f-1-1"),
        "failed feature in:\n{rendered}"
    );
    assert!(
        rendered.contains("[⊘] f-1-2"),
        "skipped feature in:\n{rendered}"
    );
}

// ---------------------------------------------------------------------------
// Cost estimate rendering (shared by /plan line mode and the planning TUI)
// ---------------------------------------------------------------------------

#[test]
fn cost_estimate_renders_range_and_calibration_provenance() {
    let estimate = kranz_engine::cost::CostEstimate {
        worker_runs: 8.4,
        validator_runs: 6.0,
        low_usd: 9.175,
        expected_usd: 18.35,
        high_usd: 45.875,
        shape: kranz_engine::cost::MissionShape::Unknown,
        confidence: kranz_engine::cost::Confidence::High,
    };

    // A few completed missions but below the corpus-fit threshold: reports the
    // count and flags that the range is not yet fitted.
    let rendered = output::render_cost_estimate(&estimate, 2);
    assert!(
        rendered.contains("estimated $9.18-$45.88"),
        "range in: {rendered}"
    );
    assert!(
        rendered.contains("expected ~$18.35"),
        "expected in: {rendered}"
    );
    assert!(
        rendered.contains("2 completed mission(s)")
            && rendered.contains("too few to fit the range"),
        "below-threshold provenance in: {rendered}"
    );

    // Enough completed missions to fit the range.
    let rendered = output::render_cost_estimate(&estimate, 12);
    assert!(
        rendered.contains("range fit to 12 completed missions"),
        "fitted provenance in: {rendered}"
    );

    // No completed missions yet: says the params are the built-in defaults.
    let rendered = output::render_cost_estimate(&estimate, 0);
    assert!(
        rendered.contains("built-in defaults — no completed missions yet"),
        "default provenance in: {rendered}"
    );
    assert!(
        !rendered.contains("range fit") && !rendered.contains("too few"),
        "no calibration provenance without missions: {rendered}"
    );
}

#[test]
fn cost_estimate_low_confidence_names_shape() {
    let low_confidence = kranz_engine::cost::CostEstimate {
        worker_runs: 8.4,
        validator_runs: 6.0,
        low_usd: 9.175,
        expected_usd: 18.35,
        high_usd: 275.25,
        shape: kranz_engine::cost::MissionShape::DocHeavy,
        confidence: kranz_engine::cost::Confidence::Low,
    };
    let rendered = output::render_cost_estimate(&low_confidence, 2);
    assert!(
        !rendered.contains('\n'),
        "rendered line must be single-line: {rendered}"
    );
    let lower = rendered.to_ascii_lowercase();
    assert!(
        lower.contains("doc") || lower.contains("judgement"),
        "shape named in: {rendered}"
    );
    assert!(
        lower.contains("low confidence")
            || lower.contains("corpus lacks")
            || lower.contains("rough"),
        "low-confidence phrase in: {rendered}"
    );

    let high_confidence = kranz_engine::cost::CostEstimate {
        worker_runs: 8.4,
        validator_runs: 6.0,
        low_usd: 9.175,
        expected_usd: 18.35,
        high_usd: 45.875,
        shape: kranz_engine::cost::MissionShape::CodeChange,
        confidence: kranz_engine::cost::Confidence::High,
    };
    let rendered_high = output::render_cost_estimate(&high_confidence, 2);
    assert!(
        !rendered_high
            .to_ascii_lowercase()
            .contains("low confidence"),
        "high-confidence line must not carry the low-confidence phrase: {rendered_high}"
    );
}

// ---------------------------------------------------------------------------
// Control enqueueing (msg / pause / resume)
// ---------------------------------------------------------------------------

fn queued_json_files(repo: &Path, mission_id: &str) -> Vec<PathBuf> {
    let control = repo
        .join(".kranz")
        .join("missions")
        .join(mission_id)
        .join("control");
    let mut files: Vec<PathBuf> = fs::read_dir(control)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    files
}

/// One queued control file's command JSON, with the authenticity tag lifted
/// off and checked. `control::enqueue` signs every file (audit 2026-09-01
/// C1), so the shape assertions below stay about the command while the
/// signature's presence is asserted once, here.
fn queued_command(path: &Path) -> serde_json::Value {
    let mut value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    let sig = value
        .as_object_mut()
        .expect("a control file is a JSON object")
        .remove("sig")
        .expect("every enqueued control file carries a signature");
    assert_eq!(sig.as_str().unwrap().len(), 64, "hex HMAC-SHA256: {sig}");
    value
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
    let value: serde_json::Value = queued_command(&files[0]);
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
    let first: serde_json::Value = queued_command(&files[0]);
    let second: serde_json::Value = queued_command(&files[1]);
    assert_eq!(first, serde_json::json!({ "kind": "pause" }));
    assert_eq!(second, serde_json::json!({ "kind": "resume" }));
}

#[test]
fn msg_rejects_unknown_mission() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(commands::cmd_msg(tmp.path(), "m-none", "x", false).is_err());
}

#[test]
fn revision_commands_enqueue_for_a_revisable_mission() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // An approved mission (past Planning) accepts a revision request.
    write_events(
        repo,
        "m-1",
        vec![
            created_kind("goal", "m-1"),
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
        ],
    );
    commands::cmd_request_revision(repo, "m-1", "drop feature two").unwrap();
    let files = queued_json_files(repo, "m-1");
    assert_eq!(files.len(), 1);
    let value: serde_json::Value = queued_command(&files[0]);
    assert_eq!(
        value,
        serde_json::json!({ "kind": "request-revision", "instructions": "drop feature two" })
    );

    // A mission with a pending revision 1 accepts approve and reject at that
    // number (separate missions so each control inbox holds exactly one).
    for (mission, cmd, expected) in [
        (
            "m-2",
            "approve",
            serde_json::json!({ "kind": "approve-revision", "revision": 1 }),
        ),
        (
            "m-3",
            "reject",
            serde_json::json!({ "kind": "reject-revision", "revision": 1 }),
        ),
    ] {
        write_events(
            repo,
            mission,
            vec![
                created_kind("goal", mission),
                EventKind::PlanApproved {
                    plan: sample_plan(),
                    base_sha: None,
                },
                EventKind::PlanRevisionProposed {
                    revision: 1,
                    plan: sample_plan(),
                    instructions: "seed".into(),
                },
            ],
        );
        if cmd == "approve" {
            commands::cmd_approve_revision(repo, mission, 1).unwrap();
        } else {
            commands::cmd_reject_revision(repo, mission, 1).unwrap();
        }
        let files = queued_json_files(repo, mission);
        assert_eq!(files.len(), 1, "{mission}");
        let value: serde_json::Value = queued_command(&files[0]);
        assert_eq!(value, expected, "{mission}");
    }
}

#[test]
fn revision_commands_reject_bad_state() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // Planning-only mission: no approved plan to revise yet.
    write_events(repo, "m-plan", vec![created_kind("goal", "m-plan")]);
    assert!(commands::cmd_request_revision(repo, "m-plan", "x").is_err());

    // Approved mission with no pending revision.
    write_events(
        repo,
        "m-1",
        vec![
            created_kind("goal", "m-1"),
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
        ],
    );
    assert!(
        commands::cmd_request_revision(repo, "m-1", "   ").is_err(),
        "empty instructions are refused"
    );
    assert!(
        commands::cmd_approve_revision(repo, "m-1", 1).is_err(),
        "no pending revision to approve"
    );
    assert!(
        commands::cmd_reject_revision(repo, "m-1", 1).is_err(),
        "no pending revision to reject"
    );

    // Pending revision 1, but the operator names the wrong number.
    write_events(
        repo,
        "m-2",
        vec![
            created_kind("goal", "m-2"),
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
            EventKind::PlanRevisionProposed {
                revision: 1,
                plan: sample_plan(),
                instructions: "seed".into(),
            },
        ],
    );
    assert!(
        commands::cmd_approve_revision(repo, "m-2", 2).is_err(),
        "revision number must match the pending one"
    );

    // Unknown mission.
    assert!(commands::cmd_request_revision(repo, "m-none", "x").is_err());
}

fn seed_pending_grant(repo: &Path, mission: &str, command: &str) {
    write_events(
        repo,
        mission,
        vec![
            created_kind("goal", mission),
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
            EventKind::GrantRequested {
                milestone_id: "ms-1".into(),
                kind: kranz_engine::types::GrantKind::Command,
                command: command.into(),
            },
        ],
    );
}

#[test]
fn grant_commands_enqueue_the_right_control_command() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    seed_pending_grant(repo, "m-a", "gc audit --deep");
    commands::cmd_approve_grant(repo, "m-a", "gc audit --deep").unwrap();
    let files = queued_json_files(repo, "m-a");
    assert_eq!(files.len(), 1);
    let value: serde_json::Value = queued_command(&files[0]);
    assert_eq!(
        value,
        serde_json::json!({ "kind": "approve-grant", "command": "gc audit --deep" })
    );

    seed_pending_grant(repo, "m-b", "gc audit --deep");
    commands::cmd_deny_grant(repo, "m-b", "gc audit --deep", "not this run").unwrap();
    let files = queued_json_files(repo, "m-b");
    assert_eq!(files.len(), 1);
    let value: serde_json::Value = queued_command(&files[0]);
    assert_eq!(
        value,
        serde_json::json!({
            "kind": "deny-grant",
            "command": "gc audit --deep",
            "reason": "not this run"
        })
    );
}

#[test]
fn grant_commands_reject_bad_state() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // Approved mission with no pending grant.
    write_events(
        repo,
        "m-1",
        vec![
            created_kind("goal", "m-1"),
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
        ],
    );
    assert!(
        commands::cmd_approve_grant(repo, "m-1", "gc audit").is_err(),
        "no pending grant to approve"
    );
    assert!(
        commands::cmd_deny_grant(repo, "m-1", "gc audit", "x").is_err(),
        "no pending grant to deny"
    );

    // Pending grant for one command, but the operator names a different one.
    seed_pending_grant(repo, "m-2", "gc audit --deep");
    assert!(
        commands::cmd_approve_grant(repo, "m-2", "rm -rf /").is_err(),
        "the command must match the parked request"
    );

    // Unknown mission.
    assert!(commands::cmd_approve_grant(repo, "m-none", "gc audit").is_err());
}

/// Seed an open structured question q-1 (options sqlite/in-memory) on an
/// active mission (ticket structured-human-question-events).
fn seed_open_question(repo: &Path, mission: &str) {
    write_events(
        repo,
        mission,
        vec![
            created_kind("goal", mission),
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".into(),
                start_sha: "abc1234".into(),
            },
            EventKind::WorkerSpawned {
                backend: None,
                run_id: "r-1".into(),
                role: Role::Worker,
                feature_id: Some("f-1-1".into()),
                milestone_id: None,
                candidate: None,
                executor_route: None,
                sdk_session_id: "sess".into(),
                model: "sonnet".into(),
                quant: "n/a".into(),
                weight_hash: None,
                prompt_hash: "hash".into(),
                transcript_path: "runs/r-1.jsonl".into(),
            },
            EventKind::QuestionOpened {
                question_id: "q-1".into(),
                role: Role::Worker,
                text: "Which storage engine?".into(),
                options: vec!["sqlite".into(), "in-memory".into()],
                run_id: Some("r-1".into()),
                feature_id: Some("f-1-1".into()),
                milestone_id: Some("ms-1".into()),
            },
        ],
    );
}

#[test]
fn question_events_commands_list_and_answer() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    seed_open_question(repo, "m-q");

    // The projection lists: id, ask, indexed options.
    let listing = commands::cmd_list_questions(repo, "m-q").unwrap();
    assert!(listing.contains("q-1"), "id listed: {listing}");
    assert!(
        listing.contains("Which storage engine?"),
        "ask listed: {listing}"
    );
    assert!(
        listing.contains("[0] sqlite") && listing.contains("[1] in-memory"),
        "options indexed: {listing}"
    );

    // An option answer enqueues the dedicated control kind, camelCase id.
    commands::cmd_answer_question(repo, "m-q", "q-1", "sqlite", Some(0)).unwrap();
    let files = queued_json_files(repo, "m-q");
    assert_eq!(files.len(), 1);
    let value: serde_json::Value = queued_command(&files[0]);
    assert_eq!(
        value,
        serde_json::json!({
            "kind": "answer-question",
            "questionId": "q-1",
            "answer": "sqlite",
            "option": 0
        })
    );
}

#[test]
fn question_events_commands_reject_stale_answers() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    seed_open_question(repo, "m-q");

    assert!(
        commands::cmd_answer_question(repo, "m-q", "q-nope", "sqlite", None).is_err(),
        "unknown question"
    );
    assert!(
        commands::cmd_answer_question(repo, "m-q", "q-1", "sqlite", Some(9)).is_err(),
        "option index out of range"
    );
    assert!(
        commands::cmd_answer_question(repo, "m-q", "q-1", "in-memory", Some(0)).is_err(),
        "option text must match the parked question"
    );

    // The rejections above enqueued nothing: the first valid answer is the
    // only file in the inbox.
    commands::cmd_answer_question(repo, "m-q", "q-1", "sqlite", Some(0)).unwrap();
    assert_eq!(
        queued_json_files(repo, "m-q").len(),
        1,
        "stale answers never enqueue"
    );

    // A mission with no open questions rejects the answer.
    write_events(
        repo,
        "m-1",
        vec![
            created_kind("goal", "m-1"),
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
        ],
    );
    assert!(commands::cmd_answer_question(repo, "m-1", "q-1", "x", None).is_err());
}

// ---------------------------------------------------------------------------
// Control-command targeting (pause/resume/msg refuse terminal missions —
// their inbox is never drained, so "success" there would be a silent no-op)
// ---------------------------------------------------------------------------

#[test]
fn control_targeting_refuses_an_explicit_terminal_mission_naming_its_status() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_events(
        repo,
        "m-done",
        vec![
            created_kind("goal", "m-done"),
            EventKind::MissionCompleted {},
        ],
    );

    let err = commands::select_control_mission(repo, Some("m-done"))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("Complete"),
        "the actual status is named: {err}"
    );

    // Unknown explicit ids stay errors.
    let err = commands::select_control_mission(repo, Some("m-nope"))
        .unwrap_err()
        .to_string();
    assert!(err.contains("m-nope"), "{err}");
}

#[test]
fn control_targeting_bare_refuses_a_terminal_pick() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_events(
        repo,
        "m-done",
        vec![
            created_kind("goal", "m-done"),
            EventKind::MissionCompleted {},
        ],
    );

    let err = commands::select_control_mission(repo, None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("Complete"), "status named: {err}");
    assert!(err.contains("active"), "{err}");
}

#[test]
fn control_targeting_bare_keeps_the_newest_by_mtime_defaulting_for_active_missions() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let old_events = write_events(repo, "m-old", vec![created_kind("old", "m-old")]);
    write_events(repo, "m-new", vec![created_kind("new", "m-new")]);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&old_events)
        .unwrap();
    file.set_times(
        fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(3600)),
    )
    .unwrap();

    // Same UX as select_mission for active picks…
    assert_eq!(
        commands::select_control_mission(repo, None).unwrap(),
        "m-new"
    );
    // …and an explicit ACTIVE id passes through the shared resolver.
    assert_eq!(
        commands::select_control_mission(repo, Some("m-old")).unwrap(),
        "m-old"
    );
}

#[test]
fn control_queue_hint_appears_only_when_no_engine_is_running() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_events(repo, "m-1", vec![created_kind("goal", "m-1")]);

    // No lock holder: the command would just sit in the inbox — say so.
    let hint = commands::control_queue_hint(repo, "m-1").expect("no live engine, so a hint");
    assert!(hint.contains("next runs"), "{hint}");

    // A live lock (our own pid) means an engine will drain the inbox: no hint.
    let paths = kranz_engine::paths::MissionPaths::new(repo, "m-1");
    fs::write(paths.lock_file(), format!("{}\n", std::process::id())).unwrap();
    assert_eq!(commands::control_queue_hint(repo, "m-1"), None);
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
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&old_events)
        .unwrap();
    file.set_times(
        fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(3600)),
    )
    .unwrap();

    assert_eq!(commands::select_mission(repo, None).unwrap(), "m-new");
    // --mission overrides auto-selection.
    assert_eq!(
        commands::select_mission(repo, Some("m-old")).unwrap(),
        "m-old"
    );
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
        vec![
            created_kind("goal b", "m-b"),
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
        ],
    );

    let listing = commands::cmd_missions(repo).unwrap();
    assert!(listing.contains("m-a"), "in: {listing}");
    assert!(listing.contains("PLANNING"), "in: {listing}");
    assert!(listing.contains("goal a"), "in: {listing}");
    assert!(listing.contains("m-b"), "in: {listing}");
    assert!(listing.contains("APPROVED"), "in: {listing}");

    let m_b_line = listing
        .lines()
        .find(|line| line.contains("m-b"))
        .unwrap_or_else(|| panic!("no m-b line in: {listing}"));
    assert!(
        !m_b_line.contains("RUNNING"),
        "m-b line should not be RUNNING: {m_b_line}"
    );
}

#[test]
fn missions_forged_orphan_renders_placeholder() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_events(repo, "m-real", vec![created_kind("goal real", "m-real")]);

    let missions_dir = repo.join(".kranz").join("missions");
    fs::create_dir_all(&missions_dir).unwrap();
    fs::write(
        missions_dir.join("index.md"),
        "# Kranz missions\n\n- [m-ghost](m-ghost/plan.md)\n",
    )
    .unwrap();

    let listing = commands::cmd_missions(repo).unwrap();
    let ghost_line = listing
        .lines()
        .find(|line| line.contains("m-ghost"))
        .unwrap_or_else(|| panic!("no m-ghost line in: {listing}"));
    assert!(
        ghost_line.contains("deleted mission (no data recorded)"),
        "in: {ghost_line}"
    );
    assert!(!ghost_line.contains("unreadable"), "in: {ghost_line}");
    assert!(!ghost_line.contains("not found"), "in: {ghost_line}");
}

#[test]
fn corrupt_log_stays_error() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let dir = repo.join(".kranz").join("missions").join("m-corrupt");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl"), "not valid json\n").unwrap();

    let listing = commands::cmd_missions(repo).unwrap();
    let corrupt_line = listing
        .lines()
        .find(|line| line.contains("m-corrupt"))
        .unwrap_or_else(|| panic!("no m-corrupt line in: {listing}"));
    assert!(corrupt_line.contains("unreadable"), "in: {corrupt_line}");
    assert!(
        !corrupt_line.contains("deleted mission (no data recorded)"),
        "in: {corrupt_line}"
    );
}

// ---------------------------------------------------------------------------
// Live-printer event rendering
// ---------------------------------------------------------------------------

/// The question events render one line each (ticket
/// structured-human-question-events): opened names the ask and the option
/// count (or the free-text expectation), answered names the answer, cleared
/// names why.
#[test]
fn question_events_renderer_renders_question_lines() {
    let mut renderer = EventRenderer::new(false);

    let opened = event(
        1,
        "m-1",
        EventKind::QuestionOpened {
            question_id: "q-1".to_string(),
            role: Role::Worker,
            text: "Which storage engine?".to_string(),
            options: vec!["sqlite".to_string(), "in-memory".to_string()],
            run_id: Some("r-1".to_string()),
            feature_id: Some("f-1-1".to_string()),
            milestone_id: Some("ms-1".to_string()),
        },
    );
    assert_eq!(
        renderer.render(&opened),
        "[mission] question q-1 opened: Which storage engine? (2 option(s)); awaiting an answer"
    );

    let free_text = event(
        2,
        "m-1",
        EventKind::QuestionOpened {
            question_id: "q-2".to_string(),
            role: Role::Worker,
            text: "What should the flag be called?".to_string(),
            options: vec![],
            run_id: Some("r-1".to_string()),
            feature_id: Some("f-1-1".to_string()),
            milestone_id: Some("ms-1".to_string()),
        },
    );
    assert_eq!(
        renderer.render(&free_text),
        "[mission] question q-2 opened: What should the flag be called? (free-text answer)"
    );

    let answered = event(
        3,
        "m-1",
        EventKind::QuestionAnswered {
            question_id: "q-1".to_string(),
            answer: "sqlite".to_string(),
            via: "answer-question".to_string(),
            option: Some(0),
        },
    );
    assert_eq!(
        renderer.render(&answered),
        "[mission] question q-1 answered: sqlite"
    );

    let cleared = event(
        4,
        "m-1",
        EventKind::QuestionCleared {
            question_id: "q-2".to_string(),
            why: "milestone completed".to_string(),
        },
    );
    assert_eq!(
        renderer.render(&cleared),
        "[mission] question q-2 cleared (milestone completed)"
    );
}

/// H9: the tail is the default `kranz run` surface, so agent-authored event
/// text must reach the terminal with no escape sequence and no OSC payload
/// left behind as bare text.
///
/// Each stripped sequence leaves a U+FFFD in its place (follow-up review
/// H-3): silent deletion on an operator-facing surface hides the fact that
/// the field was tampered with at all.
#[test]
fn renderer_strips_control_sequences_from_body_and_tag() {
    let mut renderer = EventRenderer::new(false);

    let blocked = event(
        1,
        "m-1",
        EventKind::MilestoneBlocked {
            block_context: None,
            milestone_id: "ms-1\u{1b}[2A".to_string(),
            reason: "stuck\u{1b}]52;c;Y3VybCBldmlsfHNo\u{7} on the build".to_string(),
        },
    );
    let line = renderer.render(&blocked);
    assert!(!line.contains('\u{1b}'), "ESC survived the tail: {line:?}");
    assert!(!line.contains("52;c;"), "OSC payload survived: {line:?}");
    assert_eq!(
        line,
        "[milestone ms-1\u{fffd}] BLOCKED: stuck\u{fffd} on the build"
    );
}

#[test]
fn renderer_tags_worker_lines_and_truncates() {
    let mut renderer = EventRenderer::new(false);

    let spawned = event(
        1,
        "m-1",
        EventKind::WorkerSpawned {
            backend: None,
            run_id: "w-1".to_string(),
            role: Role::Worker,
            feature_id: Some("f-1-2".to_string()),
            milestone_id: None,
            candidate: None,
            executor_route: None,
            sdk_session_id: "sess".to_string(),
            model: "sonnet".to_string(),
            quant: "n/a".to_string(),
            weight_hash: None,
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
    assert_eq!(
        renderer.render(&tool_use),
        "[worker f-1-2] tool-use: Bash: cargo test"
    );

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

    let egress_denied = event(
        4,
        "m-1",
        EventKind::WorkerEgressDenied {
            run_id: "w-1".to_string(),
            denials: vec![
                kranz_engine::egress_proxy::EgressDenial {
                    host: "example.com".to_string(),
                    port: 443,
                },
                kranz_engine::egress_proxy::EgressDenial {
                    host: "other.example".to_string(),
                    port: 8443,
                },
            ],
            omitted_count: 3,
        },
    );
    assert_eq!(
        renderer.render(&egress_denied),
        "[worker f-1-2] EGRESS DENIED: example.com:443 (+4 additional record(s))"
    );

    let decision = event(
        5,
        "m-1",
        EventKind::OrchestratorDecision {
            summary: "carry on".to_string(),
            detail: None,
        },
    );
    assert_eq!(renderer.render(&decision), "[orch] decision: carry on");

    let paused = event(5, "m-1", EventKind::MissionPaused {});
    assert_eq!(renderer.render(&paused), "[mission] paused");

    let redacted = event(
        6,
        "m-1",
        EventKind::SecretRedacted {
            rule_id: "openai-api-key".to_string(),
            fingerprint: "abc123def456".to_string(),
            location: "event/payload/text".to_string(),
        },
    );
    assert_eq!(
        renderer.render(&redacted),
        "[secret] redacted openai-api-key abc123def456 at event/payload/text"
    );

    let validating = event(
        7,
        "m-1",
        EventKind::MilestoneValidating {
            milestone_id: "ms-1".to_string(),
        },
    );
    assert_eq!(renderer.render(&validating), "[milestone ms-1] validating");

    // Long, multi-line content collapses to one line capped at 160 chars.
    let long = event(
        8,
        "m-1",
        EventKind::WorkerMessage {
            run_id: "w-1".to_string(),
            tag: "text".to_string(),
            content: format!("line one\nline two {}", "x".repeat(500)),
        },
    );
    let line = renderer.render(&long);
    assert!(!line.contains('\n'));
    assert!(
        line.chars().count() <= 160,
        "len {} in: {line}",
        line.chars().count()
    );
    assert!(line.ends_with('…'));
}

/// `kranz plan` (no goal) resumes only missions still in planning, newest
/// first; approved/running missions are never picked; explicit --mission wins.
#[test]
fn select_planning_mission_prefers_newest_planning_only() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // m-old: planning. m-run: past planning. m-new: planning, newest.
    write_events(repo, "m-old", vec![created_kind("old goal", "m-old")]);
    write_events(
        repo,
        "m-run",
        vec![
            created_kind("running goal", "m-run"),
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
        ],
    );
    let newest = write_events(repo, "m-new", vec![created_kind("new goal", "m-new")]);
    let future = SystemTime::now() + Duration::from_secs(60);
    let times = fs::FileTimes::new().set_modified(future);
    fs::File::options()
        .append(true)
        .open(&newest)
        .unwrap()
        .set_times(times)
        .unwrap();

    assert_eq!(
        commands::select_planning_mission(repo, None).unwrap(),
        "m-new"
    );
    assert_eq!(
        commands::select_planning_mission(repo, Some("m-old")).unwrap(),
        "m-old"
    );

    // With only non-planning missions, there is nothing to resume.
    let tmp2 = tempfile::tempdir().unwrap();
    write_events(
        tmp2.path(),
        "m-run",
        vec![
            created_kind("g", "m-run"),
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
        ],
    );
    let err = commands::select_planning_mission(tmp2.path(), None).unwrap_err();
    assert!(err
        .to_string()
        .contains("no mission is currently in planning"));
}

// ---------------------------------------------------------------------------
// Line-mode plan approval → run-now decision
// ---------------------------------------------------------------------------

/// The interactive line-mode "start execution now? [Y/n]" decision after a
/// plan approval: empty input takes the default (yes), n/no decline, and EOF
/// (Ctrl-D / closed stdin) declines — execution spend never starts without a
/// live answer. Piped stdin never reaches this prompt at all: the scripted
/// planning path keeps its historical approve-then-exit behavior.
#[test]
fn run_now_answer_default_yes_explicit_no_eof_no() {
    assert!(commands::run_now_answer(Some("")));
    assert!(commands::run_now_answer(Some("   ")));
    assert!(commands::run_now_answer(Some("y")));
    assert!(commands::run_now_answer(Some("Yes")));
    assert!(commands::run_now_answer(Some(" y ")));

    assert!(!commands::run_now_answer(Some("n")));
    assert!(!commands::run_now_answer(Some("N")));
    assert!(!commands::run_now_answer(Some("no")));
    assert!(!commands::run_now_answer(Some("NO")));
    assert!(!commands::run_now_answer(Some(" no ")));
    assert!(!commands::run_now_answer(None));
}

/// Usage-limit backend errors get an actionable hint; other errors pass through.
#[test]
fn limit_errors_gain_resume_hint() {
    let limit = anyhow::anyhow!(
        "orchestrator turn returned an error result: You've hit your session limit \
         · resets 6pm (America/Los_Angeles)"
    );
    let msg = format!("{:#}", commands::augment_limit_hint(limit));
    assert!(msg.contains("usage window, not a Kranz failure"), "{msg}");
    assert!(
        msg.contains("`kranz plan` (no goal) resumes planning"),
        "{msg}"
    );

    let other = anyhow::anyhow!("git operation failed: nothing to commit");
    let msg = format!("{:#}", commands::augment_limit_hint(other));
    assert!(!msg.contains("usage window"), "{msg}");
}

// ---------------------------------------------------------------------------
// Mission hygiene: abandon / clean parsing + cleanable classifier (roadmap M2)
// ---------------------------------------------------------------------------

#[test]
fn parses_abandon() {
    // Bare abandon: id via the global --mission, default reason.
    let cli = Cli::try_parse_from(["kranz", "abandon"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Abandon {
            id: None,
            reason: None
        }
    ));

    // Positional id + explicit reason.
    let cli =
        Cli::try_parse_from(["kranz", "abandon", "m-42", "--reason", "cut from scope"]).unwrap();
    match cli.command {
        Command::Abandon { id, reason } => {
            assert_eq!(id.as_deref(), Some("m-42"));
            assert_eq!(reason.as_deref(), Some("cut from scope"));
        }
        other => panic!("expected Abandon, got {other:?}"),
    }

    // The global --mission also feeds abandon (id positional stays None).
    let cli = Cli::try_parse_from(["kranz", "--mission", "m-7", "abandon"]).unwrap();
    assert_eq!(cli.mission.as_deref(), Some("m-7"));
    assert!(matches!(cli.command, Command::Abandon { id: None, .. }));
}

#[test]
fn parses_clean() {
    let cli = Cli::try_parse_from(["kranz", "clean"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Clean {
            yes: false,
            all: false
        }
    ));

    let cli = Cli::try_parse_from(["kranz", "clean", "--yes", "--all"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Clean {
            yes: true,
            all: true
        }
    ));

    let cli = Cli::try_parse_from(["kranz", "clean", "--yes"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Clean {
            yes: true,
            all: false
        }
    ));
}

/// The pure status→class classifier (in the engine) selects exactly the right
/// set for each combination of folded status and plan.json presence.
#[test]
fn cleanable_class_selects_the_right_missions() {
    use kranz_engine::mission_catalog::{cleanable_class, CleanClass};

    // Terminal-not-complete is always stale.
    assert_eq!(
        cleanable_class(MissionStatus::Failed, true),
        CleanClass::Stale
    );
    assert_eq!(
        cleanable_class(MissionStatus::Abandoned, false),
        CleanClass::Stale
    );

    // Planning: a husk (no plan) is stale; with a plan it is live work.
    assert_eq!(
        cleanable_class(MissionStatus::Planning, false),
        CleanClass::Stale
    );
    assert_eq!(
        cleanable_class(MissionStatus::Planning, true),
        CleanClass::Keep
    );

    // Complete is kept by default, removed only with --all.
    assert_eq!(
        cleanable_class(MissionStatus::Complete, true),
        CleanClass::CompleteKeepByDefault
    );
    assert!(!cleanable_class(MissionStatus::Complete, true).is_cleaned(false));
    assert!(cleanable_class(MissionStatus::Complete, true).is_cleaned(true));

    // Live non-terminal states are always kept.
    for status in [
        MissionStatus::Running,
        MissionStatus::Paused,
        MissionStatus::Blocked,
        MissionStatus::Validating,
    ] {
        assert_eq!(
            cleanable_class(status, true),
            CleanClass::Keep,
            "{status:?}"
        );
        assert!(
            !cleanable_class(status, false).is_cleaned(true),
            "{status:?} with --all"
        );
    }
}

/// Write a plan.json for a mission (marks it "past planning" for the classifier).
fn write_plan_json(repo: &Path, mission_id: &str) {
    let dir = repo.join(".kranz").join("missions").join(mission_id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("plan.json"),
        serde_json::to_string(&sample_plan()).unwrap(),
    )
    .unwrap();
}

/// Write a lock file recording `pid` for a mission (a live current-process pid
/// makes the mission read as "running").
fn write_lock(repo: &Path, mission_id: &str, pid: u32) {
    let dir = repo.join(".kranz").join("missions").join(mission_id);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("events.jsonl.lock"), pid.to_string()).unwrap();
}

/// `select_cleanable` over a tempdir of hand-written missions: it picks the
/// terminal + planning-husk missions, never the planning-with-plan mission,
/// and never a mission whose lock is held by a live pid.
#[test]
fn select_cleanable_over_mixed_missions() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // Failed: terminal → stale.
    write_events(
        repo,
        "m-failed",
        vec![
            created_kind("g", "m-failed"),
            EventKind::MissionFailed { reason: "x".into() },
        ],
    );
    // Abandoned: terminal → stale.
    write_events(
        repo,
        "m-abandoned",
        vec![
            created_kind("g", "m-abandoned"),
            EventKind::MissionAbandoned { reason: "x".into() },
        ],
    );
    // Planning husk: still Planning, NO plan.json → stale.
    write_events(repo, "m-husk", vec![created_kind("g", "m-husk")]);
    // Planning WITH a plan.json → live work, keep.
    write_events(repo, "m-planned", vec![created_kind("g", "m-planned")]);
    write_plan_json(repo, "m-planned");
    // Complete: kept by default, removed only with --all.
    write_events(
        repo,
        "m-complete",
        vec![
            created_kind("g", "m-complete"),
            EventKind::MissionCompleted {},
        ],
    );
    // Failed but its lock is held by THIS live process → never cleaned.
    write_events(
        repo,
        "m-running",
        vec![
            created_kind("g", "m-running"),
            EventKind::MissionFailed { reason: "x".into() },
        ],
    );
    write_lock(repo, "m-running", std::process::id());

    // Default (no --all): failed + abandoned + husk, but not planned/complete/running.
    let ids: Vec<String> = commands::select_cleanable(repo, false)
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert!(ids.contains(&"m-failed".to_string()), "{ids:?}");
    assert!(ids.contains(&"m-abandoned".to_string()), "{ids:?}");
    assert!(ids.contains(&"m-husk".to_string()), "{ids:?}");
    assert!(
        !ids.contains(&"m-planned".to_string()),
        "planning-with-plan kept: {ids:?}"
    );
    assert!(
        !ids.contains(&"m-complete".to_string()),
        "complete kept by default: {ids:?}"
    );
    assert!(
        !ids.contains(&"m-running".to_string()),
        "live-locked never cleaned: {ids:?}"
    );

    // --all additionally includes the Complete mission (still never running).
    let ids_all: Vec<String> = commands::select_cleanable(repo, true)
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert!(
        ids_all.contains(&"m-complete".to_string()),
        "--all includes complete: {ids_all:?}"
    );
    assert!(
        !ids_all.contains(&"m-running".to_string()),
        "live-locked still safe: {ids_all:?}"
    );
    assert!(
        !ids_all.contains(&"m-planned".to_string()),
        "planning-with-plan still kept: {ids_all:?}"
    );
}

/// The removal path deletes exactly the selected mission directories and leaves
/// everything else — the kept missions and the missions/index.md — in place.
#[test]
fn remove_missions_deletes_selected_and_keeps_index_and_others() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    write_events(
        repo,
        "m-failed",
        vec![
            created_kind("g", "m-failed"),
            EventKind::MissionFailed { reason: "x".into() },
        ],
    );
    write_events(repo, "m-planned", vec![created_kind("g", "m-planned")]);
    write_plan_json(repo, "m-planned");

    // A missions index.md must survive cleaning (never removed).
    let missions_dir = repo.join(".kranz").join("missions");
    fs::write(missions_dir.join("index.md"), "# Kranz missions\n").unwrap();

    let entries = commands::select_cleanable(repo, false);
    let removed = commands::remove_missions(repo, &entries, false);
    assert_eq!(
        removed,
        vec!["m-failed".to_string()],
        "only the failed mission removed"
    );

    assert!(
        !missions_dir.join("m-failed").exists(),
        "failed mission dir gone"
    );
    assert!(
        missions_dir.join("m-planned").exists(),
        "planning-with-plan mission kept"
    );
    assert!(
        missions_dir.join("index.md").is_file(),
        "missions index.md untouched"
    );
    // Nothing else lingering: exactly the kept mission dir + the index remain.
    let mut remaining: Vec<String> = fs::read_dir(&missions_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    remaining.sort();
    assert_eq!(
        remaining,
        vec!["index.md".to_string(), "m-planned".to_string()]
    );
}

#[test]
fn clean_prunes_missions_index() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    write_events(
        repo,
        "m-failed",
        vec![
            created_kind("g", "m-failed"),
            EventKind::MissionFailed { reason: "x".into() },
        ],
    );
    write_events(repo, "m-planned", vec![created_kind("g", "m-planned")]);
    write_plan_json(repo, "m-planned");

    let missions_dir = repo.join(".kranz").join("missions");
    fs::write(
        missions_dir.join("index.md"),
        "# Kranz missions\n\
         - 2026-01-01 · [m-failed](m-failed/plan.md) — goal one\n\
         - 2026-01-02 · [m-planned](m-planned/plan.md) — goal two\n",
    )
    .unwrap();

    let entries = commands::select_cleanable(repo, false);
    let removed = commands::remove_missions(repo, &entries, false);
    assert_eq!(removed, vec!["m-failed".to_string()]);

    let index = fs::read_to_string(missions_dir.join("index.md")).unwrap();
    assert!(
        !index.contains("m-failed"),
        "removed mission's line pruned from index: {index}"
    );
    assert!(
        index.contains("[m-planned](m-planned/plan.md) — goal two"),
        "unrelated mission's line kept: {index}"
    );
    assert!(index.contains("# Kranz missions"), "header kept: {index}");
}
