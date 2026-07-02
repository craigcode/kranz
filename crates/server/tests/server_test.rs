//! End-to-end tests for kranz-server against docs/protocol.md: REST via
//! `tower::ServiceExt::oneshot` on the router, WS via a real listener +
//! tokio-tungstenite. Fixtures are real event logs written through
//! `kranz_engine::event_log::EventLog`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use kranz_engine::control;
use kranz_engine::event_log::EventLog;
use kranz_engine::events::EventKind;
use kranz_engine::paths::MissionPaths;
use kranz_engine::types::{
    Assertion, AssertionCheck, ControlCommand, MissionConfig, Plan, PlanFeature, PlanMilestone,
    Role, RunResult, TokenUsage,
};
use serde_json::Value;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tower::ServiceExt;

const MISSION_ID: &str = "m-01";
const WAIT: Duration = Duration::from_secs(10);

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

// ---------------------------------------------------------------------------
// Fixture: a real mission event log written through the engine's EventLog
// ---------------------------------------------------------------------------

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
    }
}

/// Seed a 7-event mission (seq 1..=7) and release the writer lock.
fn seed_mission(repo_root: &Path) -> MissionPaths {
    let paths = MissionPaths::new(repo_root, MISSION_ID);
    let mut log = EventLog::acquire(&paths, MISSION_ID, Duration::ZERO, false).unwrap();
    log.append(EventKind::MissionCreated {
        goal: "Ship the demo".into(),
        base_branch: "main".into(),
        mission_branch: format!("kranz/mission-{MISSION_ID}"),
        config: MissionConfig::default(),
    })
    .unwrap();
    log.append(EventKind::PlanApproved { plan: sample_plan() }).unwrap();
    log.append(EventKind::MilestoneStarted {
        milestone_id: "ms-1".into(),
        start_sha: "abc1234".into(),
    })
    .unwrap();
    log.append(EventKind::FeatureStarted { feature_id: "f-1-1".into() }).unwrap();
    log.append(EventKind::WorkerSpawned {
        run_id: "run-1".into(),
        role: Role::Worker,
        feature_id: Some("f-1-1".into()),
        milestone_id: Some("ms-1".into()),
        sdk_session_id: "00000000-0000-0000-0000-000000000001".into(),
        model: "sonnet".into(),
        prompt_hash: "deadbeef".into(),
        transcript_path: MissionPaths::transcript_rel("run-1"),
    })
    .unwrap();
    log.append(EventKind::WorkerMessage {
        run_id: "run-1".into(),
        tag: "text".into(),
        content: "working on it".into(),
    })
    .unwrap();
    log.append(EventKind::WorkerCompleted {
        run_id: "run-1".into(),
        result: RunResult::Pass,
        tokens: TokenUsage { input: 100, output: 50, cache_read: 0, cache_write: 0 },
        cost_usd: Some(0.5),
        report: None,
    })
    .unwrap();
    drop(log); // flush + release events.jsonl.lock
    paths
}

fn fixture() -> (tempfile::TempDir, PathBuf, MissionPaths, axum::Router) {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().to_path_buf();
    let paths = seed_mission(&repo_root);
    let app = kranz_server::router(repo_root.clone(), None);
    (tmp, repo_root, paths, app)
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

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_and_root_info() {
    let tmp = tempfile::tempdir().unwrap();
    let app = kranz_server::router(tmp.path().to_path_buf(), None);

    let (status, body) = get_json(&app, "/api/health").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));

    // No static dir configured: "/" is an informational text response.
    let response = app
        .clone()
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(std::str::from_utf8(&bytes).unwrap().contains("kranz"));
}

#[tokio::test]
async fn missions_list_folds_and_tolerates_corrupt_logs() {
    let (_tmp, repo_root, _paths, app) = fixture();

    // A second mission with a corrupt log must not fail the whole list.
    let bad_dir = repo_root.join(".kranz").join("missions").join("zz-bad");
    std::fs::create_dir_all(&bad_dir).unwrap();
    std::fs::write(bad_dir.join("events.jsonl"), "not json\nstill not json\n").unwrap();

    let (status, body) = get_json(&app, "/api/missions").await;
    assert_eq!(status, StatusCode::OK);
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 2);

    let good = rows.iter().find(|r| r["id"] == MISSION_ID).unwrap();
    assert_eq!(good["status"], "running");
    assert_eq!(good["goal"], "Ship the demo");
    assert!(good["createdAt"].is_string());

    let bad = rows.iter().find(|r| r["id"] == "zz-bad").unwrap();
    assert_eq!(bad["status"], "failed");
    assert!(bad["error"].is_string());
}

#[tokio::test]
async fn state_is_folded_from_the_log() {
    let (_tmp, _repo_root, _paths, app) = fixture();

    let (status, body) = get_json(&app, &format!("/api/missions/{MISSION_ID}/state")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mission"]["id"], MISSION_ID);
    assert_eq!(body["mission"]["status"], "running");
    // camelCase serde shapes, straight from the engine types.
    assert_eq!(body["mission"]["baseBranch"], "main");
    assert_eq!(body["mission"]["missionBranch"], format!("kranz/mission-{MISSION_ID}"));
    assert_eq!(body["mission"]["milestones"][0]["id"], "ms-1");
    assert_eq!(body["mission"]["milestones"][0]["features"][0]["status"], "active");
    assert_eq!(body["lastSeq"], 7);
    assert_eq!(body["totalCostUsd"], 0.5);
    assert_eq!(body["totals"]["cacheRead"], 0);
    assert_eq!(body["runs"]["run-1"]["costUsd"], 0.5);
    assert_eq!(body["runs"]["run-1"]["result"], "pass");
}

#[tokio::test]
async fn events_since_filters_by_seq() {
    let (_tmp, _repo_root, _paths, app) = fixture();

    let (status, body) = get_json(&app, &format!("/api/missions/{MISSION_ID}/events")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 7);

    let (status, body) =
        get_json(&app, &format!("/api/missions/{MISSION_ID}/events?since=2")).await;
    assert_eq!(status, StatusCode::OK);
    let events = body.as_array().unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(events[0]["seq"], 3);
    assert!(events.iter().all(|e| e["seq"].as_u64().unwrap() > 2));

    let (status, body) =
        get_json(&app, &format!("/api/missions/{MISSION_ID}/events?since=nope")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn plan_404_until_plan_json_exists() {
    let (_tmp, _repo_root, paths, app) = fixture();

    let (status, body) = get_json(&app, &format!("/api/missions/{MISSION_ID}/plan")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].is_string());

    std::fs::write(paths.plan_file(), serde_json::to_string_pretty(&sample_plan()).unwrap())
        .unwrap();
    let (status, body) = get_json(&app, &format!("/api/missions/{MISSION_ID}/plan")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["goal"], "Ship the demo");
    assert_eq!(body["milestones"][0]["features"][0]["title"], "F1");
}

#[tokio::test]
async fn transcript_404_then_parsed_jsonl_array() {
    let (_tmp, _repo_root, paths, app) = fixture();
    let uri = format!("/api/missions/{MISSION_ID}/runs/run-1/transcript");

    let (status, body) = get_json(&app, &uri).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].is_string());

    std::fs::write(
        paths.transcript_file("run-1"),
        "{\"type\":\"system\",\"subtype\":\"init\"}\n{\"type\":\"result\",\"is_error\":false}\n",
    )
    .unwrap();
    let (status, body) = get_json(&app, &uri).await;
    assert_eq!(status, StatusCode::OK);
    let lines = body.as_array().unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["type"], "system");
    assert_eq!(lines[1]["is_error"], false);
}

#[tokio::test]
async fn control_post_enqueues_a_drainable_command() {
    let (_tmp, _repo_root, paths, app) = fixture();
    let uri = format!("/api/missions/{MISSION_ID}/control");

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"kind":"msg","text":"focus on tests","interrupt":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["queued"], true);

    let commands = control::drain(&paths).unwrap();
    assert_eq!(commands.len(), 1);
    match &commands[0] {
        ControlCommand::Msg { text, interrupt } => {
            assert_eq!(text, "focus on tests");
            assert!(!interrupt);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // Bad body -> 400 with a JSON error.
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header("content-type", "application/json")
                .body(Body::from(r#"{"kind":"bogus"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn unknown_mission_is_404_with_json_error() {
    let (_tmp, _repo_root, _paths, app) = fixture();
    for uri in
        ["/api/missions/nope/state", "/api/missions/nope/events", "/api/missions/nope/plan"]
    {
        let (status, body) = get_json(&app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert!(body["error"].is_string(), "{uri}");
    }
}

#[tokio::test]
async fn static_dir_serves_files_with_spa_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_root).unwrap();
    let static_dir = tmp.path().join("dist");
    std::fs::create_dir_all(&static_dir).unwrap();
    std::fs::write(static_dir.join("index.html"), "<html>kranz dashboard</html>").unwrap();
    std::fs::write(static_dir.join("app.js"), "console.log('hi')").unwrap();

    let app = kranz_server::router(repo_root, Some(static_dir));

    let response = app
        .clone()
        .oneshot(Request::builder().uri("/app.js").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // A client-side route falls back to index.html (SPA).
    let response = app
        .clone()
        .oneshot(Request::builder().uri("/missions/m-01").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    assert!(std::str::from_utf8(&bytes).unwrap().contains("kranz dashboard"));
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

async fn next_frame(ws: &mut WsStream) -> Value {
    let msg = tokio::time::timeout(WAIT, ws.next())
        .await
        .expect("timed out waiting for a ws frame")
        .expect("ws stream closed unexpectedly")
        .expect("ws protocol error");
    match msg {
        WsMessage::Text(text) => serde_json::from_str(text.as_str()).expect("frame is not JSON"),
        other => panic!("expected a text frame, got {other:?}"),
    }
}

async fn spawn_server(app: axum::Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test]
async fn ws_snapshot_tail_state_ping_and_since_replay() {
    let (_tmp, _repo_root, paths, app) = fixture();
    let addr = spawn_server(app).await;

    // Sanity: the served REST API answers over the network too.
    let health: Value = tokio::time::timeout(
        WAIT,
        async { reqwest::get(format!("http://{addr}/api/health")).await.unwrap().json().await },
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(health["ok"], true);

    let url = format!("ws://{addr}/api/missions/{MISSION_ID}/ws");
    let (mut ws, _) = tokio::time::timeout(WAIT, connect_async(&url)).await.unwrap().unwrap();

    // First frame: a snapshot carrying the fold and its seq.
    let frame = next_frame(&mut ws).await;
    assert_eq!(frame["type"], "snapshot");
    assert_eq!(frame["seq"], 7);
    assert_eq!(frame["state"]["mission"]["id"], MISSION_ID);
    assert_eq!(frame["state"]["lastSeq"], 7);

    // Append two more events from the test (re-acquire the writer lock).
    {
        let mut log = EventLog::acquire(&paths, MISSION_ID, Duration::ZERO, false).unwrap();
        log.append(EventKind::WorkerMessage {
            run_id: "run-1".into(),
            tag: "text".into(),
            content: "one more thing".into(),
        })
        .unwrap();
        log.append(EventKind::FeatureCompleted {
            feature_id: "f-1-1".into(),
            commits: vec!["cafe123".into()],
        })
        .unwrap();
    }

    // Event frames arrive in seq order; the stream delta gets no state frame.
    let frame = next_frame(&mut ws).await;
    assert_eq!(frame["type"], "event");
    assert_eq!(frame["seq"], 8);
    assert_eq!(frame["event"]["type"], "worker.message");

    let frame = next_frame(&mut ws).await;
    assert_eq!(frame["type"], "event");
    assert_eq!(frame["seq"], 9);
    assert_eq!(frame["event"]["type"], "feature.completed");

    // ...and a state frame follows the lifecycle event.
    let frame = next_frame(&mut ws).await;
    assert_eq!(frame["type"], "state");
    assert_eq!(frame["seq"], 9);
    assert_eq!(
        frame["state"]["mission"]["milestones"][0]["features"][0]["status"],
        "complete"
    );

    // JSON ping -> JSON pong.
    tokio::time::timeout(WAIT, ws.send(WsMessage::Text(r#"{"type":"ping"}"#.into())))
        .await
        .unwrap()
        .unwrap();
    let frame = next_frame(&mut ws).await;
    assert_eq!(frame["type"], "pong");

    // Reconnect with ?since=7: replay starts at seq 8, no snapshot.
    let (mut ws2, _) = tokio::time::timeout(WAIT, connect_async(format!("{url}?since=7")))
        .await
        .unwrap()
        .unwrap();
    let frame = next_frame(&mut ws2).await;
    assert_eq!(frame["type"], "event");
    assert_eq!(frame["seq"], 8);
    let frame = next_frame(&mut ws2).await;
    assert_eq!(frame["type"], "event");
    assert_eq!(frame["seq"], 9);
}

#[tokio::test]
async fn ws_since_ahead_of_head_gets_fresh_snapshot() {
    let (_tmp, _repo_root, _paths, app) = fixture();
    let addr = spawn_server(app).await;

    let url = format!("ws://{addr}/api/missions/{MISSION_ID}/ws?since=999");
    let (mut ws, _) = tokio::time::timeout(WAIT, connect_async(url)).await.unwrap().unwrap();
    let frame = next_frame(&mut ws).await;
    assert_eq!(frame["type"], "snapshot");
    assert_eq!(frame["seq"], 7);
}

#[tokio::test]
async fn ws_unknown_mission_is_rejected() {
    let (_tmp, _repo_root, _paths, app) = fixture();
    let addr = spawn_server(app).await;

    let url = format!("ws://{addr}/api/missions/nope/ws");
    let result = tokio::time::timeout(WAIT, connect_async(url)).await.unwrap();
    assert!(result.is_err(), "handshake to an unknown mission must fail");
}
