//! M2.5 end-to-end tests: the mutation token gate and the full hosted
//! mission lifecycle (create → planning turns → request-plan → approve →
//! start → COMPLETE) driven over the REST surface with the engine's mock
//! backend — no `claude` binary, no model calls.
//!
//! Script discipline mirrors crates/engine/tests/mission_test.rs: the
//! orchestrator is one streaming session whose seed turn is
//! `[mock_init, mock_result_text(..)]`, and every engine turn consumes
//! exactly one `on_message` batch ending in a `Result`. Workers/validators
//! are single-shot scripts, popped FIFO in session-start order.
//!
//! Turn-in-flight try_lock contention is covered deterministically at the
//! unit level in crates/server/src/host.rs (two racing HTTP turns would be
//! timing-dependent here); this file covers every other 409.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kranz_engine::backend::AgentBackend;
use kranz_engine::backend_mock::{mock_init, mock_result_text, mock_text, MockBackend, MockScript};
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::events::EventKind;
use kranz_engine::paths::MissionPaths;
use kranz_engine::types::{MissionConfig, Plan};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Once};
use std::time::Duration;
use tempfile::TempDir;
use tower::ServiceExt;

const TOKEN: &str = "sesame-1234";

/// Generous bound proving the hosted run cannot hang the test.
const RUN_TIMEOUT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Git fixtures (same isolation discipline as the engine's mission tests)
// ---------------------------------------------------------------------------

static ENV_ISOLATION: Once = Once::new();

/// Mask the host's global/system git config AND the home directory:
/// `POST /api/missions` goes through `config::load`, which would otherwise
/// read the developer's real ~/.kranz/config.json into the test missions.
fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing = std::env::temp_dir().join(format!(
            "kranz-server-host-test-no-config-{}",
            std::process::id()
        ));
        std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
        std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
        if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
            std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
        }
        let home = std::env::temp_dir().join(format!(
            "kranz-server-host-test-home-{}",
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

/// Returns false (after a skip note) when git is missing.
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

/// Fresh repo on branch `main` with one seed commit, canonicalized root.
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
// Script / request helpers
// ---------------------------------------------------------------------------

/// One orchestrator turn batch: text + matching Result.
fn turn(reply: &str) -> Vec<kranz_engine::backend::AgentEvent> {
    vec![mock_text(reply), mock_result_text(reply)]
}

/// Dirty-tree-turn reply (§4.4): the worker left uncommitted changes; commit
/// them as-is so they land on the mission branch.
fn dirty_tree_commit_as_is() -> String {
    json!({ "action": "commit-as-is", "note": "worker delivered files" }).to_string()
}

/// Worker script: completed single-shot run with a passing WorkerReport.
fn worker_pass() -> MockScript {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = format!("delivered-{n}.txt");
    MockScript::single_shot_json(&json!({
        "result": "pass",
        "summary": "implemented and tested",
        "filesTouched": [path],
        "testsAdded": [],
        "testEvidence": "all green",
        "commits": []
    }))
    .writes_file(&path, "delivered by the mock worker\n")
}

/// A minimal one-milestone/one-feature plan in wire (camelCase) shape.
fn plan_json() -> Value {
    json!({
        "goal": "ship the demo",
        "validationContract": [],
        "milestones": [{
            "title": "M1",
            "features": [{
                "title": "F1",
                "spec": "build the thing",
                "validationCriteria": ["it works"]
            }]
        }]
    })
}

/// Router hosting missions from `root` through the given mock backend, with
/// the mutation token gate armed.
fn hosted_app(root: &Path, backend: Arc<MockBackend>) -> axum::Router {
    let backend: Arc<dyn AgentBackend> = backend;
    let host = kranz_server::MissionHost::with_backend(root.to_path_buf(), backend);
    kranz_server::router_with_host(host, None, Some(TOKEN.to_string()))
}

async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

async fn post_json(
    app: &axum::Router,
    uri: &str,
    token: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("x-kranz-token", token);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

/// Poll `GET state` until the mission reaches `status` (or panic on timeout).
/// Non-200 reads (e.g. a fold racing a write) are retried, not failed.
async fn wait_for_status(app: &axum::Router, id: &str, status: &str) {
    let uri = format!("/api/missions/{id}/state");
    let deadline = tokio::time::Instant::now() + RUN_TIMEOUT;
    loop {
        let (code, state) = get_json(app, &uri).await;
        if code == StatusCode::OK && state["mission"]["status"] == status {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "mission never reached '{status}'; last: {code} {state}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---------------------------------------------------------------------------
// Pending plan: one parked cache for every approve surface
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pending_plan_parks_on_ready_and_approve_pending_commits() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let orch = MockScript::streaming(vec![mock_init("orch-pp"), mock_result_text("seed")])
        .responding(vec![turn("scoping"), turn(&plan_json().to_string())]);
    let backend = Arc::new(MockBackend::with_scripts(vec![orch]));
    let app = hosted_app(&root, backend);

    let (status, body) = post_json(
        &app,
        "/api/missions",
        Some(TOKEN),
        json!({ "goal": "park a plan" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap().to_string();

    // Nothing pending before a Ready request-plan.
    let (status, body) = get_json(&app, &format!("/api/missions/{id}/pending-plan")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pending"], false);
    // Approving with nothing parked is an honest 409.
    let (status, _) = post_json(
        &app,
        &format!("/api/missions/{id}/approve-pending"),
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // One conversational turn, then a Ready request-plan parks the plan.
    let (status, _) = post_json(
        &app,
        &format!("/api/missions/{id}/planning/turn"),
        Some(TOKEN),
        json!({ "text": "go" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/planning/request-plan"),
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ready"], true);

    let (status, body) = get_json(&app, &format!("/api/missions/{id}/pending-plan")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pending"], true);
    assert_eq!(body["plan"]["milestones"][0]["features"][0]["title"], "F1");

    // approve-pending commits the SAME files the body-approve path does…
    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/approve-pending"),
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["branch"], format!("kranz/mission-{id}"));
    assert_eq!(body["started"], false);
    assert!(
        MissionPaths::new(&root, &id).plan_file().is_file(),
        "plan.json committed"
    );

    // …consumes the parked plan, and a second approve is a clean 409.
    let (status, body) = get_json(&app, &format!("/api/missions/{id}/pending-plan")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pending"], false, "{body}");
    let (status, _) = post_json(
        &app,
        &format!("/api/missions/{id}/approve-pending"),
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn approve_clears_parked_plan_so_no_stale_slack_approve() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let orch = MockScript::streaming(vec![mock_init("orch-pp"), mock_result_text("seed")])
        .responding(vec![turn("scoping"), turn(&plan_json().to_string())]);
    let backend = Arc::new(MockBackend::with_scripts(vec![orch]));
    let app = hosted_app(&root, backend);

    let (status, body) = post_json(
        &app,
        "/api/missions",
        Some(TOKEN),
        json!({ "goal": "park a plan" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap().to_string();

    let (status, _) = post_json(
        &app,
        &format!("/api/missions/{id}/planning/turn"),
        Some(TOKEN),
        json!({ "text": "go" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/planning/request-plan"),
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ready"], true);
    let plan = body["plan"].clone();

    let (status, body) = get_json(&app, &format!("/api/missions/{id}/pending-plan")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pending"], true);

    // Reproduce the OLD dashboard path: direct POST /approve with a
    // client-held copy of the plan, bypassing the pending-plan cache.
    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/approve"),
        Some(TOKEN),
        json!({ "plan": plan }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["branch"], format!("kranz/mission-{id}"));

    // The parked plan must be cleared by the direct approve too.
    let (status, body) = get_json(&app, &format!("/api/missions/{id}/pending-plan")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pending"], false, "{body}");

    // A later Slack /kranz approve must not re-run against the
    // already-approved mission — it should be an honest 409, not a
    // stale re-approve or a 500.
    let (status, _) = post_json(
        &app,
        &format!("/api/missions/{id}/approve-pending"),
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

// ---------------------------------------------------------------------------
// Abandon / delete (web twins of `kranz abandon` / `kranz clean`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn abandon_then_delete_lifecycle_over_rest() {
    isolate_git_env();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    // A planning husk (no plan.json): abandonable, then Stale → deletable.
    seed_mission_log(&root, "m-husk");
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
    let host = kranz_server::MissionHost::with_backend(root.clone(), backend);
    let app = kranz_server::router_with_host(host, None, Some(TOKEN.to_string()));

    // Abandon: 200, reason recorded, state folds terminal.
    let (status, body) = post_json(
        &app,
        "/api/missions/m-husk/abandon",
        Some(TOKEN),
        json!({ "reason": "duplicate from live test" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["abandoned"], true);
    let (_, state) = get_json(&app, "/api/missions/m-husk/state").await;
    assert_eq!(state["mission"]["status"], "abandoned");

    // Abandoning again: terminal → 409, not a server error.
    let (status, body) =
        post_json(&app, "/api/missions/m-husk/abandon", Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Delete: Abandoned is Stale → removed without any opt-in.
    let (status, body) =
        post_json(&app, "/api/missions/m-husk/delete", Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["deleted"], true);
    assert!(
        !MissionPaths::new(&root, "m-husk").mission_dir().exists(),
        "mission directory removed"
    );

    // Gone means gone: both endpoints 404 now.
    let (status, _) = post_json(&app, "/api/missions/m-husk/abandon", Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = post_json(&app, "/api/missions/m-husk/delete", Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_guards_live_and_complete_missions() {
    isolate_git_env();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
    let host = kranz_server::MissionHost::with_backend(root.clone(), backend);
    let app = kranz_server::router_with_host(host, None, Some(TOKEN.to_string()));

    // Approved (Running-status, plan committed) = live work → never deleted.
    seed_mission_log(&root, "m-live");
    {
        let paths = MissionPaths::new(&root, "m-live");
        let mut log = EventLog::acquire(&paths, "m-live", Duration::ZERO, LockForce::No).unwrap();
        let plan: kranz_engine::types::Plan = serde_json::from_value(plan_json()).unwrap();
        log.append(EventKind::PlanApproved {
            plan,
            base_sha: None,
        })
        .unwrap();
    }
    let (status, body) =
        post_json(&app, "/api/missions/m-live/delete", Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Complete: kept by default (calibration corpus), deleted only with all.
    seed_mission_log(&root, "m-done");
    {
        let paths = MissionPaths::new(&root, "m-done");
        let mut log = EventLog::acquire(&paths, "m-done", Duration::ZERO, LockForce::No).unwrap();
        let plan: kranz_engine::types::Plan = serde_json::from_value(plan_json()).unwrap();
        log.append(EventKind::PlanApproved {
            plan,
            base_sha: None,
        })
        .unwrap();
        log.append(EventKind::MissionCompleted {}).unwrap();
    }
    let (status, body) =
        post_json(&app, "/api/missions/m-done/delete", Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("calibration"),
        "{body}"
    );
    let (status, body) = post_json(
        &app,
        "/api/missions/m-done/delete",
        Some(TOKEN),
        json!({ "all": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!MissionPaths::new(&root, "m-done").mission_dir().exists());
}

#[tokio::test]
async fn delete_prunes_missions_index() {
    isolate_git_env();
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
    let host = kranz_server::MissionHost::with_backend(root.clone(), backend);
    let app = kranz_server::router_with_host(host, None, Some(TOKEN.to_string()));

    seed_mission_log(&root, "m-husk");
    seed_mission_log(&root, "m-other");

    let paths = MissionPaths::new(&root, "m-husk");
    std::fs::write(
        paths.missions_dir().join("index.md"),
        "# Kranz missions\n\
         - 2026-01-01 · [m-husk](m-husk/plan.md) — goal one\n\
         - 2026-01-02 · [m-other](m-other/plan.md) — goal two\n",
    )
    .unwrap();

    let (status, body) = post_json(
        &app,
        "/api/missions/m-husk/abandon",
        Some(TOKEN),
        json!({ "reason": "prune test" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, body) =
        post_json(&app, "/api/missions/m-husk/delete", Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!MissionPaths::new(&root, "m-husk").mission_dir().exists());

    let index = std::fs::read_to_string(paths.missions_dir().join("index.md")).unwrap();
    assert!(
        !index.contains("m-husk"),
        "deleted mission's line pruned: {index}"
    );
    assert!(
        index.contains("[m-other](m-other/plan.md) — goal two"),
        "unrelated mission's line kept: {index}"
    );
}

// ---------------------------------------------------------------------------
// Release: an attached engine frees the single-writer lock
// ---------------------------------------------------------------------------

/// The approve-and-queue path depends on `release`: an attached engine holds
/// the mission's single-writer lock, and without releasing it the external
/// `kranz work` dispatcher the queue points at is refused with `LockHeld`.
#[tokio::test]
async fn release_frees_the_mission_lock_for_external_runners() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    seed_mission_log(&root, "m-rel");
    let orch = MockScript::streaming(vec![mock_init("orch-rel"), mock_result_text("hello")])
        .responding(vec![turn("still planning")]);
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::with_scripts(vec![orch]));
    let host = kranz_server::MissionHost::with_backend(root.clone(), backend);

    // Attach via a planning turn: the host now holds the lock…
    host.planning_turn("m-rel", "hi")
        .await
        .expect("planning turn attaches");
    let paths = MissionPaths::new(&root, "m-rel");
    assert!(
        EventLog::acquire(&paths, "m-rel", Duration::ZERO, LockForce::No).is_err(),
        "while attached, an external acquire must be LockHeld-refused"
    );

    // …and release frees it for an external runner.
    assert!(
        host.release("m-rel").expect("release"),
        "idle engine releases cleanly"
    );
    let log = EventLog::acquire(&paths, "m-rel", Duration::ZERO, LockForce::No);
    assert!(log.is_ok(), "after release, an external acquire succeeds");
    drop(log);

    // Unknown / never-hosted missions are trivially free.
    assert!(host.release("m-unknown").expect("release unknown"));
}

/// `sweep_idle(Duration::ZERO)` treats every attached planning engine as idle
/// and releases it — the same lock-freeing effect as `release`, but driven by
/// the idle sweeper instead of an explicit call.
#[tokio::test]
async fn sweep_idle_releases_an_attached_mission_and_frees_its_lock() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    seed_mission_log(&root, "m-sweep");
    let orch = MockScript::streaming(vec![mock_init("orch-sweep"), mock_result_text("hello")])
        .responding(vec![turn("still planning")]);
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::with_scripts(vec![orch]));
    let host = kranz_server::MissionHost::with_backend(root.clone(), backend);

    host.planning_turn("m-sweep", "hi")
        .await
        .expect("planning turn attaches");
    let paths = MissionPaths::new(&root, "m-sweep");
    assert!(
        EventLog::acquire(&paths, "m-sweep", Duration::ZERO, LockForce::No).is_err(),
        "while attached, an external acquire must be LockHeld-refused"
    );

    let released = host.sweep_idle(Duration::ZERO);
    assert!(released.contains(&"m-sweep".to_string()), "{released:?}");

    let log = EventLog::acquire(&paths, "m-sweep", Duration::ZERO, LockForce::No);
    assert!(log.is_ok(), "after the sweep, an external acquire succeeds");
}

// The mid-turn-survives-sweep_idle case needs the private `planning_cell`
// accessor (to hold the cell mutex exactly like an in-flight turn) — it lives
// alongside `contended_planning_mutex_is_409_for_turns_and_start` in
// crates/server/src/host.rs's own `#[cfg(test)]` module instead.

// ---------------------------------------------------------------------------
// Release: web twin of `MissionHost::release`, over REST
// ---------------------------------------------------------------------------

#[tokio::test]
async fn release_route_frees_the_lock_over_rest() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    seed_mission_log(&root, "m-rel-rest");
    let orch = MockScript::streaming(vec![mock_init("orch-rel-rest"), mock_result_text("hello")])
        .responding(vec![turn("still planning")]);
    let backend = Arc::new(MockBackend::with_scripts(vec![orch]));
    let app = hosted_app(&root, backend);

    // Attach via a planning turn over REST: the host now holds the lock…
    let (status, body) = post_json(
        &app,
        "/api/missions/m-rel-rest/planning/turn",
        Some(TOKEN),
        json!({ "text": "hi" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let paths = MissionPaths::new(&root, "m-rel-rest");
    assert!(
        EventLog::acquire(&paths, "m-rel-rest", Duration::ZERO, LockForce::No).is_err(),
        "while attached, an external acquire must be LockHeld-refused"
    );

    let (status, body) = post_json(
        &app,
        "/api/missions/m-rel-rest/release",
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["released"], true);

    let log = EventLog::acquire(&paths, "m-rel-rest", Duration::ZERO, LockForce::No);
    assert!(log.is_ok(), "after release, an external acquire succeeds");
}

#[tokio::test]
async fn release_route_is_404_for_an_unknown_mission() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let backend = Arc::new(MockBackend::new());
    let app = hosted_app(&root, backend);

    let (status, _) = post_json(
        &app,
        "/api/missions/m-does-not-exist/release",
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Mutation token (protocol "Authority: mutation token")
// ---------------------------------------------------------------------------

/// Seed a minimal readable mission log (no git needed).
fn seed_mission_log(repo_root: &Path, id: &str) {
    let paths = MissionPaths::new(repo_root, id);
    let mut log = EventLog::acquire(&paths, id, Duration::ZERO, LockForce::No).unwrap();
    log.append(EventKind::MissionCreated {
        goal: "observe".into(),
        base_branch: "main".into(),
        mission_branch: format!("kranz/mission-{id}"),
        config: MissionConfig::default(),
    })
    .unwrap();
}

fn seed_pending_revision_log(repo_root: &Path, id: &str) {
    let paths = MissionPaths::new(repo_root, id);
    let plan: Plan = serde_json::from_value(plan_json()).unwrap();
    let mut revised = plan.clone();
    revised.goal = "ship the revised demo".into();
    revised.milestones[0].features[0].spec = "build the safer thing".into();

    let mut log = EventLog::acquire(&paths, id, Duration::ZERO, LockForce::No).unwrap();
    log.append(EventKind::MissionCreated {
        goal: "observe".into(),
        base_branch: "main".into(),
        mission_branch: format!("kranz/mission-{id}"),
        config: MissionConfig::default(),
    })
    .unwrap();
    log.append(EventKind::PlanApproved {
        plan,
        base_sha: Some("base-1".into()),
    })
    .unwrap();
    log.append(EventKind::PlanRevisionProposed {
        revision: 1,
        plan: revised,
        instructions: "make it safer".into(),
    })
    .unwrap();
    std::fs::write(paths.plan_md_file(), "# Mission plan\n\nold plan\n").unwrap();
}

#[tokio::test]
async fn mutation_token_gates_every_post_and_no_get() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    seed_mission_log(&root, "m-01");
    let app = kranz_server::router_with_token(root, None, Some(TOKEN.to_string()));

    let control = "/api/missions/m-01/control";
    let pause = json!({ "kind": "pause" });

    // Missing token → 401 with the documented error body.
    let (status, body) = post_json(&app, control, None, pause.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "missing or invalid token");

    // Wrong token → 401.
    let (status, body) = post_json(&app, control, Some("wrong"), pause.clone()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "missing or invalid token");

    // The hosted-lifecycle POSTs are gated too (401 before any other check).
    for uri in [
        "/api/missions",
        "/api/missions/m-01/planning/turn",
        "/api/missions/m-01/planning/request-plan",
        "/api/missions/m-01/approve",
        "/api/missions/m-01/start",
        "/api/missions/m-01/revise",
        "/api/missions/m-01/revision/approve",
        "/api/missions/m-01/revision/reject",
        "/api/missions/m-01/release",
        "/api/queue/drain",
    ] {
        let (status, _) = post_json(&app, uri, None, json!({})).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{uri} must require the token"
        );
    }

    // Correct token → the control command is accepted.
    let (status, body) = post_json(&app, control, Some(TOKEN), pause).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["queued"], true);

    // GETs stay tokenless (read-only observation).
    let (status, _) = get_json(&app, "/api/missions").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get_json(&app, "/api/missions/m-01/state").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get_json(&app, "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = get_json(&app, "/api/queue").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn revision_routes_expose_diff_and_enqueue_decisions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    seed_pending_revision_log(&root, "m-rev");
    let app = kranz_server::router_with_token(root.clone(), None, Some(TOKEN.to_string()));

    let (status, body) = get_json(&app, "/api/missions/m-rev/revision-diff").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["revision"], 1);
    assert_eq!(body["instructions"], "make it safer");
    assert!(body["diff"].as_str().unwrap().contains("revised-plan.md"));

    let (status, body) = post_json(
        &app,
        "/api/missions/m-rev/revision/approve",
        Some(TOKEN),
        json!({ "revision": 2 }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("awaiting revision 1"));

    let (status, body) = post_json(
        &app,
        "/api/missions/m-rev/revision/approve",
        Some(TOKEN),
        json!({ "revision": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["queued"], true);
    let drained = kranz_engine::control::drain(&MissionPaths::new(&root, "m-rev")).unwrap();
    assert_eq!(drained.len(), 1);
    assert!(matches!(
        drained[0].1,
        kranz_engine::types::ControlCommand::ApproveRevision { revision: 1 }
    ));

    // `drain` is non-destructive, so the approve command above is still queued;
    // clear the inbox so the reject assertion sees only its own command. The
    // pending revision lives in the event log, so the mission is still
    // revisable and the reject route mirrors approve but selects RejectRevision.
    for (path, _) in kranz_engine::control::drain(&MissionPaths::new(&root, "m-rev")).unwrap() {
        std::fs::remove_file(path).unwrap();
    }
    let (status, body) = post_json(
        &app,
        "/api/missions/m-rev/revision/reject",
        Some(TOKEN),
        json!({ "revision": 1 }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["queued"], true);
    let drained = kranz_engine::control::drain(&MissionPaths::new(&root, "m-rev")).unwrap();
    assert_eq!(drained.len(), 1);
    assert!(matches!(
        drained[0].1,
        kranz_engine::types::ControlCommand::RejectRevision { revision: 1 }
    ));

    let (status, body) = post_json(
        &app,
        "/api/missions/m-rev/revise",
        Some(TOKEN),
        json!({ "instructions": "  " }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("must not be empty"));
}

// ---------------------------------------------------------------------------
// Hosted lifecycle: goal → conversation → plan → approve → start → COMPLETE
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn hosted_lifecycle_reaches_complete_without_a_terminal() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // One streaming orchestrator session spans planning AND the run.
    // Turns, in injected-message order:
    //   1. planning turn                     → conversational reply
    //   2. request-plan #1                   → prose (not a plan)
    //   3. request-plan #1 JSON retry        → prose again ⇒ NotReady
    //   4. request-plan #2                   → the plan JSON ⇒ Ready
    //   5. judgement for f-1-1 (run phase)   → complete
    let judgement =
        json!({ "decision": "complete", "guidance": "", "summary": "worker did the job" });
    let orch = MockScript::streaming(vec![mock_init("orch-session"), mock_result_text("seed-hi")])
        .responding(vec![
            turn("let's scope the demo"),
            turn("what platforms must this support?"),
            turn("really, tell me about platforms first"),
            turn(&plan_json().to_string()),
            turn(&dirty_tree_commit_as_is()),
            turn(&judgement.to_string()),
            // Mission-5 era: completion runs a lesson-capture turn; NONE
            // records nothing and lets the mission close.
            turn("NONE"),
        ]);
    // Session-start order: the orchestrator starts during planning, then the
    // one-per-mission auth preflight probe (mission m-165b6f, f-2-1/f-2-2)
    // right before the first worker spawn, then the worker at run time.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        orch,
        MockScript::single_shot("ack"),
        worker_pass(),
    ]));
    let app = hosted_app(&root, backend);

    // Create — with a config patch (both validators off keeps the run to a
    // single worker + judgement).
    let (status, body) = post_json(
        &app,
        "/api/missions",
        Some(TOKEN),
        json!({
            "goal": "ship the demo",
            "config": { "skipScrutiny": true, "skipFunctional": true }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().expect("created mission id").to_string();

    // Observable immediately via the read API, in planning.
    let (status, state) = get_json(&app, &format!("/api/missions/{id}/state")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["mission"]["status"], "planning");
    assert_eq!(
        state["config"]["skipScrutiny"], true,
        "config patch applied"
    );

    // Planning turn: the seed turn's reply is prepended, blank-line separated.
    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/planning/turn"),
        Some(TOKEN),
        json!({ "text": "hello there" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["reply"], "seed-hi\n\nlet's scope the demo");

    // request-plan while the orchestrator wants to keep talking: NotReady
    // returns its (retry) prose to the conversation.
    let request_plan_uri = format!("/api/missions/{id}/planning/request-plan");
    let (status, body) = post_json(&app, &request_plan_uri, Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ready"], false);
    assert_eq!(body["reply"], "really, tell me about platforms first");

    // request-plan once the orchestrator emits the JSON: plan + estimate.
    let (status, body) = post_json(&app, &request_plan_uri, Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ready"], true);
    assert_eq!(body["plan"]["milestones"][0]["features"][0]["title"], "F1");
    assert!(body["estimate"]["expectedUsd"].as_f64().unwrap() > 0.0);
    assert!(
        body["estimate"]["lowUsd"].as_f64().unwrap()
            < body["estimate"]["highUsd"].as_f64().unwrap()
    );
    // Estimate provenance: a fresh repo has no completed missions, so the
    // params are the built-in defaults.
    assert_eq!(body["calibration"]["missionsUsed"], 0, "{body}");
    let plan = body["plan"].clone();

    // Start before approval is refused with advice.
    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/start"),
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body["error"].as_str().unwrap().contains("approve"),
        "{body}"
    );

    // Approve: branch + the same plan.json/plan.md/index.md commit as the CLI.
    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/approve"),
        Some(TOKEN),
        json!({ "plan": plan }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["branch"], format!("kranz/mission-{id}"));
    let paths = MissionPaths::new(&root, &id);
    assert!(paths.plan_file().is_file(), "plan.json committed");
    assert!(
        paths.mission_dir().join("plan.md").is_file(),
        "plan.md committed"
    );
    let index = std::fs::read_to_string(paths.missions_dir().join("index.md")).unwrap();
    assert!(
        index.contains(&id),
        "missions catalog lists the mission: {index}"
    );

    // Approving again is a lifecycle conflict, not a server error.
    let (status, _) = post_json(
        &app,
        &format!("/api/missions/{id}/approve"),
        Some(TOKEN),
        json!({ "plan": plan_json() }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Start: 202, then the background engine.run() drives the mission to
    // COMPLETE (worker → judgement → milestone tag → empty final gate).
    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/start"),
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["running"], true);

    wait_for_status(&app, &id, "complete").await;

    // The run task released the registry entry AND the mission lock:
    // starting a complete mission resumes from the log and reports the
    // terminal state as a conflict (nothing left to run).
    let deadline = tokio::time::Instant::now() + RUN_TIMEOUT;
    loop {
        let (status, body) = post_json(
            &app,
            &format!("/api/missions/{id}/start"),
            Some(TOKEN),
            json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        let error = body["error"].as_str().unwrap_or_default().to_string();
        if error.contains("complete") {
            break; // resumed from the log: entry + lock were released
        }
        // Tiny window: the run task may still be dropping the engine.
        assert!(
            tokio::time::Instant::now() < deadline,
            "registry entry/lock never released; last error: {error}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The approved plan is served by the read API.
    let (status, body) = get_json(&app, &format!("/api/missions/{id}/plan")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["goal"], "ship the demo");
}

// ---------------------------------------------------------------------------
// start/steer conflicts while a hosted run is live
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn second_start_and_planning_turns_conflict_while_running() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // The judgement turn's batch is deliberately missing: the run parks in
    // the orchestrator turn, keeping the mission running for the duration of
    // the test. Session-start order here: worker first (run phase), then the
    // orchestrator (first needed at the judgement).
    let orch = MockScript::streaming(vec![mock_init("orch-session"), mock_result_text("ready")]);
    let backend = Arc::new(MockBackend::with_scripts(vec![worker_pass(), orch]));
    let app = hosted_app(&root, backend);

    let (status, body) = post_json(
        &app,
        "/api/missions",
        Some(TOKEN),
        json!({ "goal": "ship the demo" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap().to_string();

    // Approve straight away (no conversation needed — the plan is supplied).
    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/approve"),
        Some(TOKEN),
        json!({ "plan": plan_json() }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, _) = post_json(
        &app,
        &format!("/api/missions/{id}/start"),
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // Second start while the run task is live → 409 "already running".
    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/start"),
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body["error"].as_str().unwrap().contains("already running"),
        "{body}"
    );

    // Planning endpoints on a running mission point at the control inbox.
    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/planning/turn"),
        Some(TOKEN),
        json!({ "text": "change of plans" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body["error"].as_str().unwrap().contains("control"),
        "{body}"
    );

    // Steering stays available (and tokenless GETs still observe).
    let (status, _) = post_json(
        &app,
        &format!("/api/missions/{id}/control"),
        Some(TOKEN),
        json!({ "kind": "msg", "text": "focus", "interrupt": false }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    wait_for_status(&app, &id, "running").await;
    let (status, state) = get_json(&app, &format!("/api/missions/{id}/state")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state["mission"]["status"], "running");
}

// ---------------------------------------------------------------------------
// Non-hosted / unknown missions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn planning_endpoints_attach_non_hosted_missions_and_404_unknown() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    // A mission that exists on disk but whose engine was released (a CLI
    // planning session that exited, a bridge seed turn, a server restart).
    // The host ADOPTS it on demand — the Slack planning-conversation path —
    // instead of refusing with "not hosted".
    seed_mission_log(&root, "m-cli");
    let orch = MockScript::streaming(vec![
        mock_init("orch-attach"),
        mock_result_text("attach-hi"),
    ])
    .responding(vec![turn("resumed and listening")]);
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::with_scripts(vec![orch]));
    let host = kranz_server::MissionHost::with_backend(root.clone(), backend);
    let app = kranz_server::router_with_host(host, None, Some(TOKEN.to_string()));

    let (status, body) = post_json(
        &app,
        "/api/missions/m-cli/planning/turn",
        Some(TOKEN),
        json!({ "text": "hi" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["reply"], "attach-hi\n\nresumed and listening");

    // An on-disk mission PAST planning is refused with its status named
    // (attach must never hand a planning cell to an approved/running mission).
    seed_mission_log(&root, "m-done");
    {
        let paths = MissionPaths::new(&root, "m-done");
        let mut log = EventLog::acquire(&paths, "m-done", Duration::ZERO, LockForce::No).unwrap();
        let plan: kranz_engine::types::Plan = serde_json::from_value(plan_json()).unwrap();
        log.append(EventKind::PlanApproved {
            plan,
            base_sha: None,
        })
        .unwrap();
    }
    let (status, body) = post_json(
        &app,
        "/api/missions/m-done/planning/turn",
        Some(TOKEN),
        json!({ "text": "hi" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("not in planning"),
        "{body}"
    );

    // Unknown mission → plain 404.
    let (status, body) = post_json(
        &app,
        "/api/missions/m-nope/planning/turn",
        Some(TOKEN),
        json!({ "text": "hi" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, _) = post_json(&app, "/api/missions/m-nope/start", Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Bad request bodies are 400s, not 500s.
    let (status, _) = post_json(&app, "/api/missions", Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = post_json(
        &app,
        "/api/missions/m-cli/approve",
        Some(TOKEN),
        json!({ "plan": { "bogus": true } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// Queue drain (roadmap f-1-2): POST /api/queue/drain over the full router
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn queue_drain_route_runs_a_queued_mission_to_complete() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let judgement =
        json!({ "decision": "complete", "guidance": "", "summary": "worker did the job" });
    // Planning happens in one orchestrator session; `/release` (below) drops
    // that engine — and its live backend session — so the drain's headless
    // resume starts a BRAND NEW orchestrator session for the run-phase
    // judgement, which needs its own init/result pair and turns.
    let orch = MockScript::streaming(vec![mock_init("orch-drain"), mock_result_text("seed-hi")])
        .responding(vec![
            turn("scoping the demo"),
            turn(&plan_json().to_string()),
        ]);
    let orch_run = MockScript::streaming(vec![
        mock_init("orch-drain-run"),
        mock_result_text("resumed"),
    ])
    .responding(vec![
        turn(&dirty_tree_commit_as_is()),
        turn(&judgement.to_string()),
        turn("NONE"),
    ]);
    let backend = Arc::new(MockBackend::with_scripts(vec![
        orch,
        // `/release` drops the planning-phase engine; the drain's headless
        // resume builds a brand new engine whose cached auth verdict starts
        // unset, so it drives its own one-per-mission preflight probe right
        // before the resumed run's first worker spawn.
        MockScript::single_shot("ack"),
        worker_pass(),
        orch_run,
    ]));
    let app = hosted_app(&root, backend);

    let (status, body) = post_json(
        &app,
        "/api/missions",
        Some(TOKEN),
        json!({
            "goal": "drain me",
            "config": { "skipScrutiny": true, "skipFunctional": true }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().expect("created mission id").to_string();

    let (status, _) = post_json(
        &app,
        &format!("/api/missions/{id}/planning/turn"),
        Some(TOKEN),
        json!({ "text": "go" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/planning/request-plan"),
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ready"], true);
    let plan = body["plan"].clone();

    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/approve"),
        Some(TOKEN),
        json!({ "plan": plan }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Free the mission from this host's planning registry (and its lock) so
    // the drain's OWN headless resume can claim it — exactly the same
    // release an external `kranz work` dispatcher would need.
    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/release"),
        Some(TOKEN),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["released"], true);

    // Enqueue it directly (bypassing the ticket-approve path — a bare entry
    // is exactly what the queue-runnable dashboard affordance would enqueue).
    kranz_engine::queue::enqueue(
        &root,
        kranz_engine::queue::QueueEntry {
            mission_id: id.clone(),
            ticket_slug: None,
            priority: 2,
            seq: 0,
        },
    )
    .expect("enqueue");

    let (status, body) = post_json(&app, "/api/queue/drain", Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["live"], true, "{body}");

    // The queue entry's claim was retired by the drain.
    let deadline = tokio::time::Instant::now() + RUN_TIMEOUT;
    loop {
        let (status, body) = get_json(&app, "/api/queue").await;
        assert_eq!(status, StatusCode::OK);
        if body["entries"].as_array().unwrap().is_empty() && body["drain"]["live"] == false {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "queue never drained: {body}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
