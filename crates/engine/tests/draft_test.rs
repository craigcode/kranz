//! Engine tests for the non-interactive draft core ([`kranz_engine::draft`],
//! roadmap f-1-1), driven entirely through [`MockBackend`] — no real `claude`
//! binary. Mirrors the mock-wiring conventions of `mission_test.rs`
//! (`orch_script`/`MockBackend::with_scripts`/`MissionEngine::create`).

use kranz_engine::backend::AgentBackend;
use kranz_engine::backend_mock::{mock_init, mock_result_text, mock_text, MockScript};
use kranz_engine::draft::{drive_draft, DraftOutcome};
use kranz_engine::orchestrator::MissionEngine;
use kranz_engine::queue;
use kranz_engine::ticket::{Ticket, TicketState};
use kranz_engine::types::MissionConfig;
use kranz_engine::MockBackend;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Once};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Git fixtures (same isolation discipline as mission_test.rs)
// ---------------------------------------------------------------------------

static ENV_ISOLATION: Once = Once::new();

fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing =
            std::env::temp_dir().join(format!("kranz-draft-test-no-config-{}", std::process::id()));
        std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
        std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
        if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
            std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
        }
    });
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn setup() -> bool {
    isolate_git_env();
    if git_available() {
        true
    } else {
        eprintln!("skipping test: git is not on PATH");
        false
    }
}

fn raw_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let init = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .expect("spawn git init");
    if !init.status.success() {
        raw_git(dir.path(), &["init"]);
        raw_git(dir.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);
    }
    raw_git(dir.path(), &["config", "user.name", "test"]);
    raw_git(dir.path(), &["config", "user.email", "test@example.com"]);
    std::fs::write(dir.path().join("README.md"), "seed\n").unwrap();
    raw_git(dir.path(), &["add", "-A"]);
    raw_git(dir.path(), &["commit", "-m", "seed"]);
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize repo root");
    (dir, root)
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn test_cfg() -> MissionConfig {
    MissionConfig {
        skip_scrutiny: true,
        skip_functional: true,
        ..MissionConfig::default()
    }
}

/// The orchestrator streaming script: session-start seed turn ("ready"),
/// then one entry of `replies` per subsequent engine turn (planning_turn,
/// then request_plan).
fn orch_script(replies: Vec<String>) -> MockScript {
    MockScript::streaming(vec![mock_init("orch-session"), mock_result_text("ready")]).responding(
        replies
            .iter()
            .map(|reply| vec![mock_text(reply), mock_result_text(reply)])
            .collect(),
    )
}

fn plan_json(goal: &str) -> String {
    json!({
        "goal": goal,
        "validationContract": [],
        "milestones": [{
            "title": "M1",
            "features": [{
                "title": "F1",
                "spec": "do it",
                "validationCriteria": ["works"]
            }]
        }]
    })
    .to_string()
}

fn write_ticket(repo: &Path, slug: &str, body: &str) -> Ticket {
    let dir = Ticket::tickets_dir(repo);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{slug}.md"));
    std::fs::write(&path, body).unwrap();
    Ticket::load(&path).unwrap()
}

const TICKET_BODY: &str = "\
---
title: Rate limit
priority: 2
---

## Goal
Add rate limiting.
";

// ---------------------------------------------------------------------------
// Ready plan → parked in Review
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ready_plan_parks_in_review_with_mission_recorded() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "rl", TICKET_BODY);

    let goal = ticket.mission_goal();
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            plan_json(&goal),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    let mission_id = engine.mission_id().to_string();

    let outcome = drive_draft(&mut engine, &root, &ticket, false)
        .await
        .unwrap();

    match outcome {
        DraftOutcome::ParkedForReview {
            mission_id: out_id,
            mission_branch,
        } => {
            assert_eq!(out_id, mission_id);
            assert_eq!(mission_branch, format!("kranz/mission-{mission_id}"));
        }
        other => panic!("expected ParkedForReview, got {other:?}"),
    }
    assert_eq!(Ticket::read_state(&root, "rl"), TicketState::Review);
    assert_eq!(
        Ticket::mission_for(&root, "rl").as_deref(),
        Some(mission_id.as_str())
    );
    assert!(!queue::contains(&root, &mission_id));
}

// ---------------------------------------------------------------------------
// Ready plan + then_enqueue=true → Queued + queue entry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ready_plan_with_then_enqueue_queues_the_mission() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "rl2", TICKET_BODY);

    let goal = ticket.mission_goal();
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            plan_json(&goal),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    let mission_id = engine.mission_id().to_string();

    let outcome = drive_draft(&mut engine, &root, &ticket, true)
        .await
        .unwrap();

    match outcome {
        DraftOutcome::Enqueued { mission_id: out_id } => assert_eq!(out_id, mission_id),
        other => panic!("expected Enqueued, got {other:?}"),
    }
    assert_eq!(Ticket::read_state(&root, "rl2"), TicketState::Queued);
    assert!(queue::contains(&root, &mission_id));
}

// ---------------------------------------------------------------------------
// NotReady reply → NeedsContext, questions appended
// ---------------------------------------------------------------------------

#[tokio::test]
async fn not_ready_reply_appends_needs_context_and_flips_state() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "rl3", TICKET_BODY);

    let goal = ticket.mission_goal();
    // request_plan retries once on unparseable JSON before giving up; both the
    // first turn and the retry come back as prose here.
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            "let me think about this some more".to_string(),
            "- which auth backend?\n- is postgres available?".to_string(),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    let mission_id = engine.mission_id().to_string();

    let outcome = drive_draft(&mut engine, &root, &ticket, false)
        .await
        .unwrap();

    match outcome {
        DraftOutcome::NeedsContext {
            mission_id: out_id,
            questions,
        } => {
            assert_eq!(out_id, mission_id);
            assert_eq!(
                questions,
                vec![
                    "which auth backend?".to_string(),
                    "is postgres available?".to_string(),
                ]
            );
        }
        other => panic!("expected NeedsContext, got {other:?}"),
    }
    assert_eq!(Ticket::read_state(&root, "rl3"), TicketState::NeedsContext);

    let path = Ticket::tickets_dir(&root).join("rl3.md");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("## Needs context (from orchestrator)"));
    assert!(body.contains("- which auth backend?"));
    assert!(body.contains("- is postgres available?"));
}
