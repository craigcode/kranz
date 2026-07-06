//! `GET /api/tickets` and `GET /api/tickets/:slug` (roadmap f-3-1) driven
//! over the tokenless test router (`kranz_server::router`), against a plain
//! tempdir repo with hand-written ticket `.md` + `.status` fixtures — no
//! git, no engine, no mock backend needed since these are pure reads.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kranz_engine::ticket::Ticket;
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::sync::Once;
use tempfile::TempDir;
use tower::ServiceExt;

static ENV_ISOLATION: Once = Once::new();

/// Same git-isolation discipline as `tickets_host.rs`: a fresh repo per test,
/// masked global/system git config and `$HOME` so `MissionEngine::create`
/// never touches the developer's real git identity.
fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing = std::env::temp_dir().join(format!(
            "kranz-tickets-rest-test-no-config-{}",
            std::process::id()
        ));
        std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
        std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
        if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
            std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
        }
        let home = std::env::temp_dir().join(format!(
            "kranz-tickets-rest-test-home-{}",
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

/// Throwaway git repo with one commit; `None` (skip) when git is missing.
fn init_git_repo() -> Option<TempDir> {
    isolate_git_env();
    if !git_available() {
        eprintln!("skipping test: git is not on PATH");
        return None;
    }
    let dir = tempfile::tempdir().expect("tempdir");
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
    Some(dir)
}

fn write_ticket(repo: &Path, slug: &str, body: &str) {
    let dir = Ticket::tickets_dir(repo);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{slug}.md")), body).unwrap();
}

fn write_status(repo: &Path, slug: &str, state: &str) {
    let dir = Ticket::tickets_dir(repo);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{slug}.status")),
        format!(r#"{{"state":"{state}"}}"#),
    )
    .unwrap();
}

const RATE_LIMIT_BODY: &str = "\
---
title: Rate limit
priority: 1
blocked-by: [auth]
---

## Goal
Add rate limiting.
";

const AUTH_BODY: &str = "\
---
title: Auth overhaul
priority: 2
---

## Goal
Rework auth.

## Needs context (from orchestrator)
- Which providers must stay supported?
- Should sessions survive the migration?
";

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn list_tickets_returns_summary_rows() {
    let tmp = TempDir::new().unwrap();
    write_ticket(tmp.path(), "rate-limit", RATE_LIMIT_BODY);
    write_status(tmp.path(), "rate-limit", "review");
    write_ticket(tmp.path(), "auth", AUTH_BODY);

    let app = kranz_server::router(tmp.path().to_path_buf(), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/tickets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let json = body_json(response).await;
    let rows = json.as_array().expect("array response");
    assert_eq!(rows.len(), 2);

    // Sorted by (priority, slug): rate-limit (priority 1) before auth (2).
    let rate_limit = &rows[0];
    assert_eq!(rate_limit["slug"], "rate-limit");
    assert_eq!(rate_limit["priority"], 1);
    assert_eq!(rate_limit["state"], "review");
    assert_eq!(rate_limit["title"], "Rate limit");
    assert_eq!(rate_limit["blockedBy"], serde_json::json!(["auth"]));

    let auth = &rows[1];
    assert_eq!(auth["slug"], "auth");
    assert_eq!(auth["priority"], 2);
    assert_eq!(auth["state"], "new");
    assert_eq!(auth["title"], "Auth overhaul");
    assert_eq!(auth["blockedBy"], serde_json::json!([]));
}

#[tokio::test]
async fn get_ticket_returns_full_ticket_with_needs_context() {
    let tmp = TempDir::new().unwrap();
    write_ticket(tmp.path(), "auth", AUTH_BODY);

    let app = kranz_server::router(tmp.path().to_path_buf(), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/tickets/auth")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let json = body_json(response).await;
    assert_eq!(json["slug"], "auth");
    assert_eq!(json["title"], "Auth overhaul");
    assert_eq!(json["priority"], 2);
    assert_eq!(json["schedule"], "once");
    assert_eq!(json["blockedBy"], serde_json::json!([]));
    assert_eq!(json["goal"], "Rework auth.");
    assert_eq!(json["state"], "new");
    assert_eq!(
        json["needsContext"],
        serde_json::json!([
            "Which providers must stay supported?",
            "Should sessions survive the migration?"
        ])
    );
}

#[tokio::test]
async fn get_ticket_404s_for_a_missing_ticket() {
    let tmp = TempDir::new().unwrap();

    let app = kranz_server::router(tmp.path().to_path_buf(), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/tickets/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// POST /api/tickets/:slug/draft and /approve (roadmap f-3-2)
// ---------------------------------------------------------------------------

const DOWNSTREAM_BODY: &str = "\
---
title: Downstream
priority: 2
blocked-by: [upstream]
---

## Goal
Ship the downstream feature.
";

const NO_BLOCKER_BODY: &str = "\
---
title: Standalone
priority: 2
---

## Goal
Ship it.
";

const CYCLE_A_BODY: &str = "\
---
title: A
priority: 2
blocked-by: [cycle-b]
---

## Goal
A.
";

const CYCLE_B_BODY: &str = "\
---
title: B
priority: 2
blocked-by: [cycle-a]
---

## Goal
B.
";

fn hosted_app(repo: &Path, token: &str) -> axum::Router {
    let backend: std::sync::Arc<dyn kranz_engine::backend::AgentBackend> =
        std::sync::Arc::new(kranz_engine::backend_mock::MockBackend::new());
    let host = kranz_server::MissionHost::with_backend(repo.to_path_buf(), backend);
    kranz_server::router_with_host(host, None, Some(token.to_string()))
}

#[tokio::test]
async fn rest_ticket_mutations_draft_400s_for_a_bad_slug() {
    let tmp = TempDir::new().unwrap();
    let app = hosted_app(tmp.path(), "tok");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tickets/..%2fescape/draft")
                .header("x-kranz-token", "tok")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rest_ticket_mutations_draft_404s_for_a_missing_ticket() {
    let tmp = TempDir::new().unwrap();
    let app = hosted_app(tmp.path(), "tok");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tickets/does-not-exist/draft")
                .header("x-kranz-token", "tok")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rest_ticket_mutations_draft_returns_202_with_the_real_mission_id() {
    let Some(dir) = init_git_repo() else {
        return;
    };
    write_ticket(dir.path(), "standalone", NO_BLOCKER_BODY);

    let app = hosted_app(dir.path(), "tok");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tickets/standalone/draft")
                .header("x-kranz-token", "tok")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let json = body_json(response).await;
    let mission_id = json["missionId"].as_str().expect("missionId string");
    assert!(mission_id.starts_with("m-"), "{mission_id}");
    // Observable immediately: the planning mission was registered
    // synchronously, before the draft turns were spawned in the background.
    assert!(
        kranz_engine::paths::MissionPaths::new(dir.path(), mission_id)
            .events_file()
            .is_file(),
        "mission event log must exist synchronously"
    );
}

#[tokio::test]
async fn rest_ticket_mutations_require_a_valid_token() {
    let tmp = TempDir::new().unwrap();
    write_ticket(tmp.path(), "standalone", NO_BLOCKER_BODY);
    write_status(tmp.path(), "standalone", "review");

    let app = hosted_app(tmp.path(), "tok");

    let draft_response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tickets/standalone/draft")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(draft_response.status(), StatusCode::UNAUTHORIZED);

    let approve_response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tickets/standalone/approve")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(approve_response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rest_ticket_mutations_approve_409s_naming_an_unsatisfied_blocker() {
    let tmp = TempDir::new().unwrap();
    write_ticket(tmp.path(), "downstream", DOWNSTREAM_BODY);
    Ticket::record_mission(tmp.path(), "downstream", "m-downstream").unwrap();
    Ticket::write_state(
        tmp.path(),
        "downstream",
        kranz_engine::ticket::TicketState::Review,
        None,
    )
    .unwrap();
    // "upstream" is never recorded as complete (no mission at all), so it is
    // unsatisfied.

    let app = hosted_app(tmp.path(), "tok");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tickets/downstream/approve")
                .header("content-type", "application/json")
                .header("x-kranz-token", "tok")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let json = body_json(response).await;
    let message = json["error"].as_str().unwrap();
    assert!(message.contains("upstream"), "{message}");
}

#[tokio::test]
async fn rest_ticket_mutations_approve_force_overrides_an_unsatisfied_blocker() {
    let tmp = TempDir::new().unwrap();
    write_ticket(tmp.path(), "downstream", DOWNSTREAM_BODY);
    Ticket::record_mission(tmp.path(), "downstream", "m-downstream").unwrap();
    Ticket::write_state(
        tmp.path(),
        "downstream",
        kranz_engine::ticket::TicketState::Review,
        None,
    )
    .unwrap();

    let app = hosted_app(tmp.path(), "tok");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tickets/downstream/approve")
                .header("content-type", "application/json")
                .header("x-kranz-token", "tok")
                .body(Body::from(r#"{"force":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["approved"], true);
    assert_eq!(json["missionId"], "m-downstream");
    assert_eq!(
        Ticket::read_state(tmp.path(), "downstream"),
        kranz_engine::ticket::TicketState::Queued
    );
}

#[tokio::test]
async fn rest_ticket_mutations_approve_409s_on_a_cycle_even_with_force() {
    let tmp = TempDir::new().unwrap();
    write_ticket(tmp.path(), "cycle-a", CYCLE_A_BODY);
    write_ticket(tmp.path(), "cycle-b", CYCLE_B_BODY);
    Ticket::record_mission(tmp.path(), "cycle-a", "m-cycle-a").unwrap();
    Ticket::write_state(
        tmp.path(),
        "cycle-a",
        kranz_engine::ticket::TicketState::Review,
        None,
    )
    .unwrap();

    let app = hosted_app(tmp.path(), "tok");
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tickets/cycle-a/approve")
                .header("content-type", "application/json")
                .header("x-kranz-token", "tok")
                .body(Body::from(r#"{"force":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let json = body_json(response).await;
    let message = json["error"].as_str().unwrap();
    assert!(message.contains("cycle-a"), "{message}");
    assert!(message.contains("cycle-b"), "{message}");
    assert!(message.contains("blocked-by cycle"), "{message}");
}

// ---------------------------------------------------------------------------
// isBlocked projection (f-1-1)
// ---------------------------------------------------------------------------

const CHILD_BODY: &str = "\
---
title: Child
priority: 2
blocked-by: [parent]
---

## Goal
Ship the child feature.
";

const PARENT_BODY: &str = "\
---
title: Parent
priority: 2
---

## Goal
Ship the parent feature.
";

/// Writes a minimal events.jsonl for `mission_id` that folds to
/// `MissionStatus::Complete` (MissionCreated then MissionCompleted), mirroring
/// `write_mission_events` in `crates/engine/tests/ticket_queue_test.rs`.
fn write_complete_mission_events(root: &Path, mission_id: &str) {
    use kranz_engine::events::{Event, EventKind};
    use kranz_engine::types::MissionConfig;

    let paths = kranz_engine::paths::MissionPaths::new(root, mission_id);
    let dir = paths.events_file().parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();

    let ts = chrono::Utc::now();
    let events = [
        Event {
            seq: 1,
            ts,
            mission_id: mission_id.to_string(),
            kind: EventKind::MissionCreated {
                goal: "do the thing".to_string(),
                base_branch: "main".to_string(),
                mission_branch: format!("kranz/mission-{mission_id}"),
                config: MissionConfig::default(),
            },
        },
        Event {
            seq: 2,
            ts,
            mission_id: mission_id.to_string(),
            kind: EventKind::MissionCompleted {},
        },
    ];

    let body: String = events
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(paths.events_file(), body).unwrap();
}

#[tokio::test]
async fn list_tickets_is_blocked_false_when_blocker_mission_complete() {
    let tmp = TempDir::new().unwrap();
    write_ticket(tmp.path(), "child", CHILD_BODY);
    write_status(tmp.path(), "child", "done");
    write_ticket(tmp.path(), "parent", PARENT_BODY);
    write_status(tmp.path(), "parent", "done");
    Ticket::record_mission(tmp.path(), "parent", "m-parent").unwrap();
    write_complete_mission_events(tmp.path(), "m-parent");

    let app = kranz_server::router(tmp.path().to_path_buf(), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/tickets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let json = body_json(response).await;
    let rows = json.as_array().expect("array response");
    let child = rows
        .iter()
        .find(|r| r["slug"] == "child")
        .expect("child row present");
    assert_eq!(child["isBlocked"], false);
    assert_eq!(child["blockedBy"], serde_json::json!(["parent"]));
}

#[tokio::test]
async fn list_tickets_is_blocked_true_when_blocker_mission_absent() {
    let tmp = TempDir::new().unwrap();
    write_ticket(tmp.path(), "child", CHILD_BODY);
    write_status(tmp.path(), "child", "done");
    write_ticket(tmp.path(), "parent", PARENT_BODY);
    // No mission recorded/linked for "parent" — unsatisfied.

    let app = kranz_server::router(tmp.path().to_path_buf(), None);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/tickets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let json = body_json(response).await;
    let rows = json.as_array().expect("array response");
    let child = rows
        .iter()
        .find(|r| r["slug"] == "child")
        .expect("child row present");
    assert_eq!(child["isBlocked"], true);
    assert_eq!(child["blockedBy"], serde_json::json!(["parent"]));
}

#[tokio::test]
async fn get_ticket_400s_for_a_traversal_slug() {
    let tmp = TempDir::new().unwrap();

    let app = kranz_server::router(tmp.path().to_path_buf(), None);
    // `..%2f` percent-decodes to `../`; axum decodes path segments before
    // routing, so this must be rejected by `Ticket::ensure_valid_slug` at
    // the boundary rather than ever touching the filesystem.
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/tickets/..%2fescape")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
