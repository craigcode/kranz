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
use tempfile::TempDir;
use tower::ServiceExt;

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
