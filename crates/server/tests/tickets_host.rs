//! `MissionHost::draft` (roadmap f-1-2): drafting a backlog ticket
//! end-to-end through the host, driven entirely by the engine's mock backend
//! — no real `claude` binary. Mirrors the mock-wiring conventions of
//! `crates/engine/tests/draft_test.rs` and the git-isolation discipline of
//! `crates/server/tests/host_test.rs`.

use kranz_engine::backend::AgentBackend;
use kranz_engine::backend_mock::{mock_init, mock_result_text, mock_text, MockBackend, MockScript};
use kranz_engine::ticket::{Ticket, TicketState};
use kranz_server::MissionHost;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Once};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Git fixtures (same isolation discipline as host_test.rs / draft_test.rs)
// ---------------------------------------------------------------------------

static ENV_ISOLATION: Once = Once::new();

fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing = std::env::temp_dir().join(format!(
            "kranz-tickets-host-test-no-config-{}",
            std::process::id()
        ));
        std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
        std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
        if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
            std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
        }
        let home = std::env::temp_dir().join(format!(
            "kranz-tickets-host-test-home-{}",
            std::process::id()
        ));
        let _ = std::fs::create_dir_all(&home);
        std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, &home);
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

/// The orchestrator streaming script: session-start seed turn ("ready"), then
/// one entry of `replies` per subsequent engine turn (planning_turn, then
/// request_plan).
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

fn write_ticket(repo: &Path, slug: &str, body: &str) {
    let dir = Ticket::tickets_dir(repo);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{slug}.md"));
    std::fs::write(&path, body).unwrap();
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
// draft() end-to-end
// ---------------------------------------------------------------------------

#[tokio::test]
async fn draft_parks_ticket_for_review_with_mission_recorded() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    write_ticket(&root, "rl", TICKET_BODY);

    // The goal seeded to the orchestrator folds the ticket's Goal section —
    // matches `Ticket::mission_goal()` for a ticket with no scoping answers,
    // acceptance hints, or context.
    let goal = "Add rate limiting.";
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            plan_json(goal),
        ])]));
    let host = MissionHost::with_backend(root.clone(), backend);

    let outcome = host.draft("rl", false).await.expect("draft must succeed");

    let mission_id = match &outcome {
        kranz_engine::draft::DraftOutcome::ParkedForReview {
            mission_id,
            mission_branch,
        } => {
            assert_eq!(mission_branch, &format!("kranz/mission-{mission_id}"));
            mission_id.clone()
        }
        other => panic!("expected ParkedForReview, got {other:?}"),
    };

    assert_eq!(Ticket::read_state(&root, "rl"), TicketState::Review);
    assert_eq!(
        Ticket::mission_for(&root, "rl").as_deref(),
        Some(mission_id.as_str())
    );

    // The mission the draft created is observable/resumable afterward: its
    // engine was released back out of the registry (an idempotent `release`
    // returns `true` for a mission already free), and its event log exists
    // on disk for a WS tail to observe.
    assert!(host.release(&mission_id).expect("release must not error"));
    assert!(kranz_engine::paths::MissionPaths::new(&root, &mission_id)
        .events_file()
        .is_file());
}

#[tokio::test]
async fn draft_rejects_an_invalid_slug_as_bad_request() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
    let host = MissionHost::with_backend(root, backend);

    let err = host
        .draft("../escape", false)
        .await
        .expect_err("must reject traversal slug");
    assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn draft_of_a_missing_ticket_is_not_found() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
    let host = MissionHost::with_backend(root, backend);

    let err = host
        .draft("does-not-exist", false)
        .await
        .expect_err("must 404");
    assert_eq!(err.status, axum::http::StatusCode::NOT_FOUND);
}
