//! `POST /api/hooks/github` end-to-end (design D-F, ticket
//! `trigger-ci-pr-fix-mission`): signature verification, repo matching, the
//! event allowlist, dedup, consent-gated queueing, and the never-land
//! invariant — driven through the real router (mutation-token gate ARMED;
//! hook requests authenticate via HMAC instead) with the engine's mock
//! backend, mirroring the wiring conventions of `tickets_host.rs`. All tests
//! are `ghook_`-prefixed for the contract filter.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use http_body_util::BodyExt;
use kranz_engine::backend::AgentBackend;
use kranz_engine::backend_mock::{mock_init, mock_result_text, mock_text, MockBackend, MockScript};
use kranz_engine::event_log::EventLog;
use kranz_engine::paths::MissionPaths;
use kranz_engine::ticket::{Ticket, TicketState};
use kranz_engine::types::MissionStatus;
use kranz_server::MissionHost;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tower::ServiceExt;

const SECRET: &str = "whsec-test";

// ---------------------------------------------------------------------------
// Git fixtures (same isolation discipline as tickets_host.rs)
// ---------------------------------------------------------------------------

static ENV_ISOLATION: Once = Once::new();

fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing = std::env::temp_dir().join(format!(
            "kranz-hooks-server-test-no-config-{}",
            std::process::id()
        ));
        std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
        std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
        if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
            std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
        }
        // Keep the real ~/.kranz/config.json (and its potential `hooks` key)
        // out of load_hooks.
        let home = std::env::temp_dir().join(format!(
            "kranz-hooks-server-test-home-{}",
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

/// Fresh repo on `main` with one seed commit and an `origin` remote pointing
/// at `octo/hello` on GitHub (the identity the hook route matches against).
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
    raw_git(
        &root,
        &["remote", "add", "origin", "git@github.com:octo/hello.git"],
    );
    (dir, root)
}

/// Repo + `hooks.secret` config; `None` leaves the route unconfigured (the
/// refuse-closed case).
fn init_hook_repo(secret: Option<&str>) -> (TempDir, PathBuf) {
    let (dir, root) = init_repo();
    if let Some(secret) = secret {
        let kranz = root.join(".kranz");
        std::fs::create_dir_all(&kranz).unwrap();
        std::fs::write(
            kranz.join("config.json"),
            json!({ "hooks": { "secret": secret } }).to_string(),
        )
        .unwrap();
    }
    (dir, root)
}

// ---------------------------------------------------------------------------
// Router harness (mutation token ARMED; hook requests carry no token)
// ---------------------------------------------------------------------------

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

/// One scripted draft (planning turn + plan) per hook call the test makes.
fn hook_app(root: &Path, drafts: usize) -> Router {
    let scripts: Vec<MockScript> = (0..drafts)
        .map(|_| {
            orch_script(vec![
                "seeded, thinking...".to_string(),
                plan_json("fix the trigger"),
            ])
        })
        .collect();
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::with_scripts(scripts));
    let host = MissionHost::with_backend(root.to_path_buf(), backend);
    kranz_server::router_with_host(host, None, Some("tok".to_string()))
}

// ---------------------------------------------------------------------------
// Request helpers — note: NO x-kranz-token is ever set on hook requests
// ---------------------------------------------------------------------------

fn sign(secret: &str, body: &str) -> String {
    format!(
        "sha256={}",
        kranz_engine::hooks::hmac_sha256_hex(secret.as_bytes(), body.as_bytes())
    )
}

fn hook_post(event: &str, sign_with: Option<&str>, body: &Value) -> Request<Body> {
    let text = body.to_string();
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/hooks/github")
        .header("content-type", "application/json")
        .header("x-github-event", event);
    if let Some(secret) = sign_with {
        builder = builder.header("x-hub-signature-256", sign(secret, &text));
    }
    builder.body(Body::from(text)).unwrap()
}

async fn body_json(response: Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

/// Wait for the background draft to reach a terminal ticket state (Review /
/// Queued / NeedsContext …). A draft that errors rolls back to New; the
/// deadline returns that honestly instead of hanging.
async fn wait_ticket_terminal(root: &Path, slug: &str) -> TicketState {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = Ticket::read_state(root, slug);
        if !matches!(state, TicketState::New | TicketState::Drafting) || Instant::now() >= deadline
        {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn trigger_tickets_in(repo: &Path) -> Vec<String> {
    Ticket::list(repo)
        .iter()
        .map(|t| t.slug.clone())
        .filter(|s| s.starts_with("trigger-"))
        .collect()
}

fn mission_status(repo: &Path, mission_id: &str) -> MissionStatus {
    let paths = MissionPaths::new(repo, mission_id);
    let events = EventLog::read_events(&paths.events_file()).unwrap();
    kranz_engine::reducer::fold(&events).unwrap().mission.status
}

// ---------------------------------------------------------------------------
// Payload fixtures
// ---------------------------------------------------------------------------

fn workflow_run_payload(conclusion: &str, head_branch: &str) -> Value {
    json!({
        "action": "completed",
        "workflow_run": {
            "id": 9876543210_u64,
            "name": "gates",
            "head_branch": head_branch,
            "head_sha": "0123456789abcdef0123456789abcdef01234567",
            "conclusion": conclusion,
            "html_url": "https://github.com/octo/hello/actions/runs/9876543210",
            "actor": { "login": "octocat" }
        },
        "repository": { "full_name": "octo/hello", "default_branch": "main" },
        "sender": { "login": "octocat" }
    })
}

fn pr_comment_payload(body: &str) -> Value {
    json!({
        "action": "created",
        "issue": { "number": 42, "pull_request": { "html_url": "https://github.com/octo/hello/pull/42" } },
        "comment": {
            "body": body,
            "html_url": "https://github.com/octo/hello/pull/42#issuecomment-7",
            "user": { "login": "reviewer" }
        },
        "repository": { "full_name": "octo/hello", "default_branch": "main" },
        "sender": { "login": "reviewer" }
    })
}

// ---------------------------------------------------------------------------
// Accepted triggers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ghook_ci_failure_drafts_one_ticket_and_second_event_dedupes() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_hook_repo(Some(SECRET));
    let app = hook_app(&root, 1);
    let payload = workflow_run_payload("failure", "main");

    let response = app
        .clone()
        .oneshot(hook_post("workflow_run", Some(SECRET), &payload))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "drafted");
    assert_eq!(body["ticketSlug"], "trigger-ci-9876543210");
    assert_eq!(body["queued"], false);
    let mission_id = body["missionId"].as_str().unwrap().to_string();

    // The draft runs through the normal pipeline and parks for review.
    let slug = "trigger-ci-9876543210";
    assert_eq!(wait_ticket_terminal(&root, slug).await, TicketState::Review);
    assert_eq!(mission_status(&root, &mission_id), MissionStatus::Approved);

    // Provenance: frontmatter trigger field + body source/actor/consent.
    let ticket = Ticket::load(&Ticket::tickets_dir(&root).join(format!("{slug}.md"))).unwrap();
    assert_eq!(ticket.trigger.as_deref(), Some("ci-failure"));
    assert!(ticket
        .raw_body
        .contains("Source: https://github.com/octo/hello/actions/runs/9876543210"));
    assert!(ticket.raw_body.contains("Actor: octocat"));
    assert!(ticket.raw_body.contains("Consent: draft-only"));

    // A second event for the same run is a no-op: no second ticket, no
    // second mission.
    let response = app
        .oneshot(hook_post("workflow_run", Some(SECRET), &payload))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "duplicate");
    assert_eq!(body["ticketSlug"], slug);
    assert!(body.get("missionId").is_none());

    assert_eq!(trigger_tickets_in(&root), vec![slug.to_string()]);
    assert_eq!(MissionPaths::list_missions(&root), vec![mission_id]);
}

#[tokio::test]
async fn ghook_fix_label_comment_drafts_ticket_for_review() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_hook_repo(Some(SECRET));
    let app = hook_app(&root, 1);

    let response = app
        .oneshot(hook_post(
            "issue_comment",
            Some(SECRET),
            &pr_comment_payload("kranz:fix the flaky test"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "drafted");
    assert_eq!(body["ticketSlug"], "trigger-pr-42");
    assert_eq!(body["queued"], false);

    let slug = "trigger-pr-42";
    assert_eq!(wait_ticket_terminal(&root, slug).await, TicketState::Review);
    // Draft-only consent: nothing is queued.
    assert!(kranz_engine::queue::list(&root).is_empty());

    let ticket = Ticket::load(&Ticket::tickets_dir(&root).join(format!("{slug}.md"))).unwrap();
    assert_eq!(ticket.trigger.as_deref(), Some("pr-comment"));
    assert!(ticket
        .raw_body
        .contains("Source: https://github.com/octo/hello/pull/42#issuecomment-7"));
    assert!(ticket.raw_body.contains("Actor: reviewer"));
    assert!(ticket.raw_body.contains("Consent: draft-only"));
}

#[tokio::test]
async fn ghook_fix_and_queue_label_drafts_and_queues_without_running() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_hook_repo(Some(SECRET));
    let app = hook_app(&root, 1);

    let response = app
        .oneshot(hook_post(
            "issue_comment",
            Some(SECRET),
            &pr_comment_payload("kranz:fix-and-queue the flaky test"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = body_json(response).await;
    assert_eq!(body["outcome"], "drafted");
    assert_eq!(body["queued"], true);
    let mission_id = body["missionId"].as_str().unwrap().to_string();

    let slug = "trigger-pr-42";
    assert_eq!(wait_ticket_terminal(&root, slug).await, TicketState::Queued);

    // The queue holds exactly the drafted mission…
    let entries = kranz_engine::queue::list(&root);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].mission_id, mission_id);
    assert_eq!(entries[0].ticket_slug.as_deref(), Some(slug));

    // …and the mission is NOT running: plan approved (through the normal
    // draft-time approval), but no run activity — only `kranz work` starts
    // it.
    assert_eq!(mission_status(&root, &mission_id), MissionStatus::Approved);

    // Pre-consent is on the record.
    let ticket = Ticket::load(&Ticket::tickets_dir(&root).join(format!("{slug}.md"))).unwrap();
    assert!(ticket.raw_body.contains("Consent: fix-and-queue"));
}

// ---------------------------------------------------------------------------
// Refusals (closed) and ignores
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ghook_bad_signature_is_401_and_drafts_nothing() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_hook_repo(Some(SECRET));
    let app = hook_app(&root, 0);

    let response = app
        .oneshot(hook_post(
            "workflow_run",
            Some("whsec-WRONG"),
            &workflow_run_payload("failure", "main"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(trigger_tickets_in(&root).is_empty());
    assert!(MissionPaths::list_missions(&root).is_empty());
}

#[tokio::test]
async fn ghook_absent_secret_refuses_closed_403() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_hook_repo(None);
    let app = hook_app(&root, 0);

    // Even a well-formed signature header cannot open an unconfigured route.
    let response = app
        .clone()
        .oneshot(hook_post(
            "workflow_run",
            Some(SECRET),
            &workflow_run_payload("failure", "main"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // …and an unsigned request gets the same closed refusal.
    let response = app
        .oneshot(hook_post(
            "workflow_run",
            None,
            &workflow_run_payload("failure", "main"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    assert!(trigger_tickets_in(&root).is_empty());
    assert!(MissionPaths::list_missions(&root).is_empty());
}

#[tokio::test]
async fn ghook_wrong_repo_is_403_and_drafts_nothing() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_hook_repo(Some(SECRET));
    let app = hook_app(&root, 0);

    let mut payload = workflow_run_payload("failure", "main");
    payload["repository"]["full_name"] = json!("octo/other");
    let response = app
        .oneshot(hook_post("workflow_run", Some(SECRET), &payload))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(trigger_tickets_in(&root).is_empty());
    assert!(MissionPaths::list_missions(&root).is_empty());
}

#[tokio::test]
async fn ghook_unallowlisted_event_and_non_failure_run_are_202_ignored() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_hook_repo(Some(SECRET));
    let app = hook_app(&root, 0);

    // A `push` event is not in the allowlist.
    let response = app
        .clone()
        .oneshot(hook_post(
            "push",
            Some(SECRET),
            &json!({ "ref": "refs/heads/main", "repository": { "full_name": "octo/hello" } }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(body_json(response).await["outcome"], "ignored");

    // A successful workflow run matches no trigger rule.
    let response = app
        .oneshot(hook_post(
            "workflow_run",
            Some(SECRET),
            &workflow_run_payload("success", "main"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(body_json(response).await["outcome"], "ignored");

    assert!(trigger_tickets_in(&root).is_empty());
    assert!(MissionPaths::list_missions(&root).is_empty());
}

// ---------------------------------------------------------------------------
// Authority and the never-land invariant
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ghook_token_gate_still_arms_every_other_post_route() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_hook_repo(Some(SECRET));
    let app = hook_app(&root, 1);

    // The hook route passes WITHOUT the mutation token (HMAC is its
    // authority)…
    let response = app
        .clone()
        .oneshot(hook_post(
            "workflow_run",
            Some(SECRET),
            &workflow_run_payload("failure", "main"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    // …while a tokenless POST to any other route is still refused.
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tickets")
                .header("content-type", "application/json")
                .body(Body::from(json!({"slug":"x","title":"y"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[test]
fn ghook_handler_source_has_no_git_mutation_paths() {
    // The hook handler drafts/queues tickets and nothing more: it must never
    // reference a git-mutation entry point or raw mutation command (the
    // repo's never-land-from-automation invariant, asserted at source level).
    const NEEDLES: [&str; 8] = [
        "git push",
        "git merge",
        "git publish",
        "push_mission",
        "merge_mission",
        "merge_no_ff",
        "fast_forward",
        "publish",
    ];
    let source = include_str!("../src/hooks.rs");
    for needle in NEEDLES {
        assert!(
            !source.contains(needle),
            "crates/server/src/hooks.rs must not reference '{needle}'"
        );
    }
}
