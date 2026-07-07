//! `GET /api/tickets` and `GET /api/tickets/:slug` — read-only backlog
//! surface over `.kranz/tickets/` (docs/protocol.md). Every ticket is
//! re-parsed from disk per request, matching the rest of this crate's
//! no-cache discipline.

use crate::error::ApiError;
use crate::ServerState;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use kranz_engine::ticket::Ticket;
use serde_json::{json, Value};
use std::sync::Arc;

/// `GET /api/tickets` — a summary row per parseable ticket under
/// `.kranz/tickets/`: slug, priority, pipeline state, title, and blockedBy.
pub(crate) async fn list_tickets(State(server): State<Arc<ServerState>>) -> Json<Value> {
    let rows: Vec<Value> = Ticket::list(server.host.repo_root())
        .iter()
        .map(|ticket| ticket_summary_json(server.host.repo_root(), ticket))
        .collect();
    Json(Value::Array(rows))
}

/// `GET /api/tickets/:slug` — the full parsed ticket plus `needsContext`
/// (the orchestrator's clarifying questions, if any were appended). 400 for
/// an invalid/traversal slug (checked at the route boundary before any
/// filesystem access), 404 for a slug with no ticket file.
pub(crate) async fn get_ticket(
    State(server): State<Arc<ServerState>>,
    UrlPath(slug): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    Ticket::ensure_valid_slug(&slug)
        .map_err(|e| ApiError::bad_request(format!("invalid ticket slug '{slug}': {e}")))?;

    let path = Ticket::tickets_dir(server.host.repo_root()).join(format!("{slug}.md"));
    if !path.is_file() {
        return Err(ApiError::not_found(format!("unknown ticket '{slug}'")));
    }
    let ticket = Ticket::load(&path)
        .map_err(|e| ApiError::internal(format!("failed to parse ticket '{slug}': {e}")))?;

    Ok(Json(ticket_full_json(server.host.repo_root(), &ticket)))
}

/// `POST /api/tickets/:slug/draft` — long-running: mirrors `POST
/// /api/missions/:id/start` by creating the planning mission synchronously
/// (so a real mission id is available for the response) and spawning the
/// draft turns as a background task via [`kranz_server::MissionHost::draft_async`].
/// `202 {"missionId":"m-…"}`; progress is observable over that mission's
/// `GET /api/missions/:id/ws` feed and the terminal outcome (Review vs
/// NeedsContext) via `GET /api/tickets/:slug`. 400 for an invalid slug, 404
/// for a missing ticket — both checked synchronously before anything spawns.
pub(crate) async fn draft_ticket(
    State(server): State<Arc<ServerState>>,
    UrlPath(slug): UrlPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ticket::ensure_valid_slug(&slug)
        .map_err(|e| ApiError::bad_request(format!("invalid ticket slug '{slug}': {e}")))?;
    let mission_id = server.host.draft_async(&slug, false).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "missionId": mission_id })),
    ))
}

/// `POST /api/tickets` — create a ticket from `{"slug", "title", "goal"?,
/// "context"?}`. Scaffolds `.kranz/tickets/<slug>.md` via
/// [`kranz_engine::ticket::Ticket::scaffold`] — the same template the CLI's
/// `kranz ticket new` uses, so goal/context land in the ticket body and a
/// subsequent `GET /api/tickets/:slug` round-trips them. 400 for an invalid
/// slug or a missing/blank `title`, 409 if the slug already has a ticket,
/// `201` with the created ticket's summary JSON (same shape as a list row).
pub(crate) async fn create_ticket(
    State(server): State<Arc<ServerState>>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let value = crate::host::parse_body(&body)?;
    let slug = value
        .get("slug")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request("missing required field 'slug'"))?;
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request("missing required field 'title'"))?;
    let goal = value.get("goal").and_then(Value::as_str);
    let context = value.get("context").and_then(Value::as_str);

    Ticket::ensure_valid_slug(slug)
        .map_err(|e| ApiError::bad_request(format!("invalid ticket slug '{slug}': {e}")))?;

    let path = Ticket::scaffold(server.host.repo_root(), slug, title, goal, context)?;
    let ticket = Ticket::load(&path).map_err(|e| {
        ApiError::internal(format!("failed to parse scaffolded ticket '{slug}': {e}"))
    })?;

    Ok((
        StatusCode::CREATED,
        Json(ticket_summary_json(server.host.repo_root(), &ticket)),
    ))
}

/// `POST /api/tickets/:slug/approve` — optional body `{"force": bool}`
/// (default `false`) → `200 {"approved":true,"missionId":"m-…"}`. Runs the
/// same gate the CLI's `kranz ticket approve` runs
/// ([`kranz_engine::deps::approve_ticket`]): 409 naming the unsatisfied
/// blocker when blocked and `force` is false; 409 with the cycle path on a
/// `blocked-by` cycle even when `force` is true; 409 when the ticket is not
/// in Review.
pub(crate) async fn approve_ticket(
    State(server): State<Arc<ServerState>>,
    UrlPath(slug): UrlPath<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    Ticket::ensure_valid_slug(&slug)
        .map_err(|e| ApiError::bad_request(format!("invalid ticket slug '{slug}': {e}")))?;
    let value = crate::host::parse_body(&body)?;
    let force = value.get("force").and_then(Value::as_bool).unwrap_or(false);
    let approved = server.host.approve_ticket(&slug, force)?;
    Ok(Json(
        json!({ "approved": true, "missionId": approved.mission_id }),
    ))
}

/// Summary projection for the list route: no goal/context/body text, just
/// enough to render a backlog table.
fn ticket_summary_json(repo_root: &std::path::Path, ticket: &Ticket) -> Value {
    let state = Ticket::read_state(repo_root, &ticket.slug);
    json!({
        "slug": ticket.slug,
        "priority": ticket.priority,
        "state": state,
        "title": ticket.title,
        "blockedBy": ticket.blocked_by,
        "isBlocked": kranz_engine::deps::is_blocked(repo_root, &ticket.slug).unwrap_or(false),
        "missionId": Ticket::mission_for(repo_root, &ticket.slug),
        "merged": kranz_engine::merged::ticket_merged(repo_root, &ticket.slug),
    })
}

/// Full projection for the show route: every parsed field plus the derived
/// `needsContext` question list.
fn ticket_full_json(repo_root: &std::path::Path, ticket: &Ticket) -> Value {
    let state = Ticket::read_state(repo_root, &ticket.slug);
    json!({
        "slug": ticket.slug,
        "title": ticket.title,
        "priority": ticket.priority,
        "schedule": ticket.schedule,
        "blockedBy": ticket.blocked_by,
        "goal": ticket.goal,
        "context": ticket.context,
        "scopingAnswers": ticket.scoping_answers,
        "acceptanceHints": ticket.acceptance_hints,
        "state": state,
        "needsContext": needs_context_questions(&ticket.raw_body),
        "isBlocked": kranz_engine::deps::is_blocked(repo_root, &ticket.slug).unwrap_or(false),
        "missionId": Ticket::mission_for(repo_root, &ticket.slug),
        "merged": kranz_engine::merged::ticket_merged(repo_root, &ticket.slug),
    })
}

/// Port of `kranz_cli::backlog::needs_context_block`'s heading scan, but
/// returning the bare question strings (bullet items, marker stripped)
/// instead of the raw markdown block — a REST consumer wants data, not
/// text to re-render.
fn needs_context_questions(raw_body: &str) -> Vec<String> {
    let mut collecting = false;
    let mut out = Vec::new();
    for line in raw_body.lines() {
        let is_section = line.trim_start().starts_with("##");
        if collecting && is_section {
            break; // next section ends the block
        }
        if is_section
            && line
                .trim_start_matches('#')
                .trim()
                .to_ascii_lowercase()
                .starts_with("needs context")
        {
            collecting = true;
            continue;
        }
        if collecting {
            if let Some(item) = bullet_item(line) {
                out.push(item);
            }
        }
    }
    out
}

/// The content of a dash bullet (`- item`), trimmed, else None. Mirrors
/// `kranz_engine::ticket`'s private helper of the same name.
fn bullet_item(line: &str) -> Option<String> {
    let t = line.trim_start();
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(marker) {
            let item = rest.trim().to_string();
            if !item.is_empty() {
                return Some(item);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use kranz_engine::event_log::{EventLog, LockForce};
    use kranz_engine::events::EventKind;
    use kranz_engine::paths::MissionPaths;
    use kranz_engine::ticket::TicketState;
    use kranz_engine::types::{
        Assertion, AssertionCheck, MissionConfig, Plan, PlanFeature, PlanMilestone,
    };
    use std::path::{Path, PathBuf};
    use std::time::Duration;
    use tempfile::TempDir;
    use tower::ServiceExt;

    const TICKET_BODY: &str = "\
---
title: Sample
priority: 2
---

## Goal
Ship the thing.
";

    fn write_ticket(repo: &std::path::Path, slug: &str) {
        let dir = Ticket::tickets_dir(repo);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{slug}.md")), TICKET_BODY).unwrap();
    }

    fn load(repo: &std::path::Path, slug: &str) -> Ticket {
        Ticket::load(&Ticket::tickets_dir(repo).join(format!("{slug}.md"))).unwrap()
    }

    #[test]
    fn pipeline_summary_json_surfaces_recorded_mission_id() {
        let tmp = TempDir::new().unwrap();
        write_ticket(tmp.path(), "linked");
        Ticket::record_mission(tmp.path(), "linked", "m-abc123").unwrap();

        let ticket = load(tmp.path(), "linked");
        let json = ticket_summary_json(tmp.path(), &ticket);
        assert_eq!(json["missionId"], "m-abc123");
    }

    #[test]
    fn pipeline_full_json_surfaces_recorded_mission_id() {
        let tmp = TempDir::new().unwrap();
        write_ticket(tmp.path(), "linked");
        Ticket::record_mission(tmp.path(), "linked", "m-abc123").unwrap();

        let ticket = load(tmp.path(), "linked");
        let json = ticket_full_json(tmp.path(), &ticket);
        assert_eq!(json["missionId"], "m-abc123");
    }

    #[test]
    fn pipeline_missing_mission_id_is_null_in_both_projections() {
        let tmp = TempDir::new().unwrap();
        write_ticket(tmp.path(), "unlinked");

        let ticket = load(tmp.path(), "unlinked");
        assert_eq!(
            ticket_summary_json(tmp.path(), &ticket)["missionId"],
            Value::Null
        );
        assert_eq!(
            ticket_full_json(tmp.path(), &ticket)["missionId"],
            Value::Null
        );
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn post(uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header(crate::TOKEN_HEADER, "tok")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn pipeline_create_ticket_succeeds_and_appears_in_list_as_new() {
        let tmp = TempDir::new().unwrap();
        let app = crate::router_with_token(tmp.path().to_path_buf(), None, Some("tok".into()));

        let response = app
            .clone()
            .oneshot(post(
                "/api/tickets",
                json!({"slug": "new-thing", "title": "New Thing"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let created = body_json(response).await;
        assert_eq!(created["slug"], "new-thing");
        assert_eq!(created["title"], "New Thing");
        assert_eq!(created["state"], "new");
        assert_eq!(created["missionId"], Value::Null);

        let list_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/tickets")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let rows = body_json(list_response).await;
        let rows = rows.as_array().unwrap();
        let row = rows
            .iter()
            .find(|r| r["slug"] == "new-thing")
            .expect("created ticket present in list");
        assert_eq!(row["state"], "new");
    }

    #[tokio::test]
    async fn pipeline_create_ticket_invalid_slug_returns_400() {
        let tmp = TempDir::new().unwrap();
        let app = crate::router_with_token(tmp.path().to_path_buf(), None, Some("tok".into()));

        let response = app
            .oneshot(post(
                "/api/tickets",
                json!({"slug": "../escape", "title": "Bad"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn pipeline_create_ticket_duplicate_slug_returns_409() {
        let tmp = TempDir::new().unwrap();
        write_ticket(tmp.path(), "dup");
        let app = crate::router_with_token(tmp.path().to_path_buf(), None, Some("tok".into()));

        let response = app
            .oneshot(post(
                "/api/tickets",
                json!({"slug": "dup", "title": "Duplicate"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn pipeline_create_ticket_goal_and_context_round_trip_through_get() {
        let tmp = TempDir::new().unwrap();
        let app = crate::router_with_token(tmp.path().to_path_buf(), None, Some("tok".into()));

        let response = app
            .clone()
            .oneshot(post(
                "/api/tickets",
                json!({
                    "slug": "round-trip",
                    "title": "Round Trip",
                    "goal": "ship the thing",
                    "context": "some background",
                }),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let get_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/tickets/round-trip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get_response.status(), StatusCode::OK);
        let json = body_json(get_response).await;
        assert_eq!(json["goal"], "ship the thing");
        assert_eq!(json["context"], "some background");
    }

    // -----------------------------------------------------------------
    // `merged` bit on the ticket projection (F1.2)
    // -----------------------------------------------------------------

    static ENV_ISOLATION: std::sync::Once = std::sync::Once::new();

    /// Mask the host's global/system git config so identity, signing and
    /// hooks never leak into the throwaway repos (mirrors
    /// crates/server/tests/server_test.rs's isolate_git_env).
    fn isolate_git_env() {
        ENV_ISOLATION.call_once(|| {
            let missing = std::env::temp_dir().join(format!(
                "kranz-tickets-test-no-config-{}",
                std::process::id()
            ));
            std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
            std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
            if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
                std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
            }
        });
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
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

    fn raw_git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Fresh repo on branch `main` with one seed commit; returns
    /// (tempdir, canonicalized root, seed commit sha).
    fn init_repo() -> (TempDir, PathBuf, String) {
        let dir = TempDir::new().unwrap();
        let init = std::process::Command::new("git")
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
        let base_sha = raw_git(&root, &["rev-parse", "HEAD"]).trim().to_string();
        (dir, root, base_sha)
    }

    fn sample_plan() -> Plan {
        Plan {
            goal: "Ship the demo".into(),
            validation_contract: vec![Assertion {
                id: "a-1".into(),
                statement: "cargo test passes".into(),
                check: AssertionCheck::Command,
                command: Some("cargo test".into()),
            }],
            milestones: vec![PlanMilestone {
                title: "M1".into(),
                features: vec![PlanFeature {
                    title: "F1".into(),
                    spec: "build the thing".into(),
                    validation_criteria: vec!["it works".into()],
                }],
            }],
            command_grants: vec![],
            touch_set: vec![],
        }
    }

    /// Seed a mission whose plan is approved (base_sha pinned to the repo's
    /// seed commit) and whose event log then folds to `MissionStatus::Complete`.
    /// When `create_branch` and `merge_into_base` are both true, the mission
    /// branch is merged into `main` before the events are appended.
    fn seed_complete_mission(
        repo_root: &Path,
        id: &str,
        base_sha: &str,
        create_branch: bool,
        merge_into_base: bool,
    ) {
        let paths = MissionPaths::new(repo_root, id);
        let branch = format!("kranz/mission-{id}");
        let mut log = EventLog::acquire(&paths, id, Duration::ZERO, LockForce::No).unwrap();
        log.append(EventKind::MissionCreated {
            goal: "Ship the demo".into(),
            base_branch: "main".into(),
            mission_branch: branch.clone(),
            config: MissionConfig::default(),
        })
        .unwrap();
        log.append(EventKind::PlanApproved {
            plan: sample_plan(),
            base_sha: Some(base_sha.to_string()),
        })
        .unwrap();
        log.append(EventKind::MissionCompleted {}).unwrap();
        drop(log);

        if create_branch {
            raw_git(repo_root, &["checkout", "-b", &branch, base_sha]);
            std::fs::write(repo_root.join("feature.txt"), "new feature\n").unwrap();
            raw_git(repo_root, &["add", "--", "feature.txt"]);
            raw_git(repo_root, &["commit", "-m", "add feature"]);
            raw_git(repo_root, &["checkout", "main"]);
            if merge_into_base {
                raw_git(repo_root, &["merge", "--no-ff", "--no-edit", &branch]);
            }
        }
    }

    #[tokio::test]
    async fn ticket_projection_merged_false_for_done_ticket_on_unmerged_mission() {
        if !setup() {
            return;
        }
        let (_dir, repo_root, base_sha) = init_repo();
        write_ticket(&repo_root, "unmerged-work");
        Ticket::record_mission(&repo_root, "unmerged-work", "m-unmerged").unwrap();
        Ticket::write_state(&repo_root, "unmerged-work", TicketState::Done, None).unwrap();
        seed_complete_mission(&repo_root, "m-unmerged", &base_sha, true, false);

        let app = crate::router(repo_root.clone(), None);

        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/tickets")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let rows = body_json(list_response).await;
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["slug"] == "unmerged-work")
            .expect("ticket present in list");
        assert_eq!(row["merged"], false);

        let show_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/tickets/unmerged-work")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(show_response.status(), StatusCode::OK);
        let json = body_json(show_response).await;
        assert_eq!(json["merged"], false);
    }

    #[tokio::test]
    async fn ticket_projection_merged_true_for_done_ticket_on_merged_mission() {
        if !setup() {
            return;
        }
        let (_dir, repo_root, base_sha) = init_repo();
        write_ticket(&repo_root, "merged-work");
        Ticket::record_mission(&repo_root, "merged-work", "m-merged").unwrap();
        Ticket::write_state(&repo_root, "merged-work", TicketState::Done, None).unwrap();
        seed_complete_mission(&repo_root, "m-merged", &base_sha, true, true);

        let app = crate::router(repo_root.clone(), None);

        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/tickets")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let rows = body_json(list_response).await;
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["slug"] == "merged-work")
            .expect("ticket present in list");
        assert_eq!(row["merged"], true);

        let show_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/tickets/merged-work")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(show_response.status(), StatusCode::OK);
        let json = body_json(show_response).await;
        assert_eq!(json["merged"], true);
    }

    #[tokio::test]
    async fn ticket_projection_merged_null_for_new_unlinked_ticket() {
        let tmp = TempDir::new().unwrap();
        write_ticket(tmp.path(), "fresh");
        let app = crate::router(tmp.path().to_path_buf(), None);

        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/tickets")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_response.status(), StatusCode::OK);
        let rows = body_json(list_response).await;
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["slug"] == "fresh")
            .expect("ticket present in list");
        assert!(row["merged"].is_null());

        let show_response = app
            .oneshot(
                Request::builder()
                    .uri("/api/tickets/fresh")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(show_response.status(), StatusCode::OK);
        let json = body_json(show_response).await;
        assert!(json["merged"].is_null());
    }
}
