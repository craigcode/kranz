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
use kranz_engine::types::MissionConfig;
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
        let missing = std::env::temp_dir()
            .join(format!("kranz-server-host-test-no-config-{}", std::process::id()));
        std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
        std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
        if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
            std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
        }
        let home = std::env::temp_dir()
            .join(format!("kranz-server-host-test-home-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&home);
        std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, &home);
    });
}

fn git_available() -> bool {
    Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
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
    let out = Command::new("git").args(args).current_dir(dir).output().expect("spawn git");
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

/// Worker script: completed single-shot run with a passing WorkerReport.
fn worker_pass() -> MockScript {
    MockScript::single_shot_json(&json!({
        "result": "pass",
        "summary": "implemented and tested",
        "filesTouched": [],
        "testsAdded": [],
        "testEvidence": "all green",
        "commits": []
    }))
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
    let value =
        if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
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
    let value =
        if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
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
    host.planning_turn("m-rel", "hi").await.expect("planning turn attaches");
    let paths = MissionPaths::new(&root, "m-rel");
    assert!(
        EventLog::acquire(&paths, "m-rel", Duration::ZERO, LockForce::No).is_err(),
        "while attached, an external acquire must be LockHeld-refused"
    );

    // …and release frees it for an external runner.
    assert!(host.release("m-rel").expect("release"), "idle engine releases cleanly");
    let log = EventLog::acquire(&paths, "m-rel", Duration::ZERO, LockForce::No);
    assert!(log.is_ok(), "after release, an external acquire succeeds");
    drop(log);

    // Unknown / never-hosted missions are trivially free.
    assert!(host.release("m-unknown").expect("release unknown"));
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
    ] {
        let (status, _) = post_json(&app, uri, None, json!({})).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} must require the token");
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
            turn(&judgement.to_string()),
        ]);
    // Session-start order: the orchestrator starts during planning, the
    // worker at run time.
    let backend = Arc::new(MockBackend::with_scripts(vec![orch, worker_pass()]));
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
    assert_eq!(state["config"]["skipScrutiny"], true, "config patch applied");

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
    let (status, body) =
        post_json(&app, &format!("/api/missions/{id}/start"), Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("approve"), "{body}");

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
    assert!(paths.mission_dir().join("plan.md").is_file(), "plan.md committed");
    let index = std::fs::read_to_string(paths.missions_dir().join("index.md")).unwrap();
    assert!(index.contains(&id), "missions catalog lists the mission: {index}");

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
    let (status, body) =
        post_json(&app, &format!("/api/missions/{id}/start"), Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["running"], true);

    wait_for_status(&app, &id, "complete").await;

    // The run task released the registry entry AND the mission lock:
    // starting a complete mission resumes from the log and reports the
    // terminal state as a conflict (nothing left to run).
    let deadline = tokio::time::Instant::now() + RUN_TIMEOUT;
    loop {
        let (status, body) =
            post_json(&app, &format!("/api/missions/{id}/start"), Some(TOKEN), json!({})).await;
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

    let (status, body) =
        post_json(&app, "/api/missions", Some(TOKEN), json!({ "goal": "ship the demo" })).await;
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

    let (status, _) =
        post_json(&app, &format!("/api/missions/{id}/start"), Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // Second start while the run task is live → 409 "already running".
    let (status, body) =
        post_json(&app, &format!("/api/missions/{id}/start"), Some(TOKEN), json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("already running"), "{body}");

    // Planning endpoints on a running mission point at the control inbox.
    let (status, body) = post_json(
        &app,
        &format!("/api/missions/{id}/planning/turn"),
        Some(TOKEN),
        json!({ "text": "change of plans" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("control"), "{body}");

    // Steering stays available (and tokenless GETs still observe).
    let (status, _) = post_json(
        &app,
        &format!("/api/missions/{id}/control"),
        Some(TOKEN),
        json!({ "kind": "msg", "text": "focus", "interrupt": false }),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
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
    let orch =
        MockScript::streaming(vec![mock_init("orch-attach"), mock_result_text("attach-hi")])
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
        let mut log =
            EventLog::acquire(&paths, "m-done", Duration::ZERO, LockForce::No).unwrap();
        let plan: kranz_engine::types::Plan = serde_json::from_value(plan_json()).unwrap();
        log.append(EventKind::PlanApproved { plan }).unwrap();
    }
    let (status, body) = post_json(
        &app,
        "/api/missions/m-done/planning/turn",
        Some(TOKEN),
        json!({ "text": "hi" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["error"].as_str().unwrap().contains("not in planning"), "{body}");

    // Unknown mission → plain 404.
    let (status, body) = post_json(
        &app,
        "/api/missions/m-nope/planning/turn",
        Some(TOKEN),
        json!({ "text": "hi" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let (status, _) =
        post_json(&app, "/api/missions/m-nope/start", Some(TOKEN), json!({})).await;
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
