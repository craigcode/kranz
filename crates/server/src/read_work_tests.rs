use super::*;
use axum::body::Body;
use axum::http::Request;
use axum::{Extension, Router};
use futures::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use kranz_engine::backend_mock::MockBackend;
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::events::EventKind;
use kranz_engine::paths::MissionPaths;
use kranz_engine::types::MissionConfig;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use tower::ServiceExt;

const WAIT: Duration = Duration::from_secs(10);

fn app(root: &Path, reads: ReadWork) -> Router {
    // This literal also verifies that the public ServerState API retains its
    // original constructible shape; the limiter belongs to the router.
    let state = Arc::new(crate::ServerState {
        repo_root: root.into(),
        host: Arc::new(crate::MissionHost::with_backend(
            root.into(),
            Arc::new(MockBackend::new()),
        )),
        bind_addr: None,
        bind_is_loopback: true,
    });
    Router::new()
        .route("/api/health", axum::routing::get(crate::rest::health))
        .nest("/api", crate::repo_api_routes().with_state(state))
        .layer(Extension(reads))
}

fn request(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn seed(root: &Path, messages: usize) -> MissionPaths {
    // Only a fresh fixture repository's authority is created; no host secret
    // is read into test output. This exercises MAC verification as well as seq.
    kranz_engine::paths::load_or_create_authority_key(root).unwrap();
    let paths = MissionPaths::new(root, "m-read-work");
    let mut log = EventLog::acquire(&paths, "m-read-work", Duration::ZERO, LockForce::No).unwrap();
    log.append(EventKind::MissionCreated {
        goal: "original-goal".into(),
        base_branch: "main".into(),
        mission_branch: "kranz/read-work".into(),
        config: MissionConfig::default(),
    })
    .unwrap();
    for _ in 0..messages {
        log.append(EventKind::UserMessage {
            text: "x".repeat(2048),
            interrupt: false,
        })
        .unwrap();
    }
    drop(log);
    paths
}

async fn hold(
    reads: ReadWork,
) -> (
    std::sync::mpsc::Sender<()>,
    tokio::task::JoinHandle<Result<(), ApiError>>,
) {
    // Wait for any already-running WS poll to finish before occupying the
    // actual pool. This makes saturation deterministic without timer races.
    let permit = reads.permits.clone().acquire_owned().await.unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let task = tokio::spawn(async move {
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _ = started_tx.send(());
            release_rx
                .recv_timeout(WAIT)
                .map_err(|error| ApiError::internal(error.to_string()))?;
            Ok(())
        })
        .await
        .unwrap()
    });
    tokio::time::timeout(WAIT, started_rx)
        .await
        .unwrap()
        .unwrap();
    (release_tx, task)
}

fn corrupt_prefix(paths: &MissionPaths) {
    let bytes = std::fs::read_to_string(paths.events_file()).unwrap();
    let (first, rest) = bytes.split_once('\n').unwrap();
    let mut first: Value = serde_json::from_str(first).unwrap();
    let mac = first["m"].as_str().expect("fixture must be authenticated");
    let corrupt = format!(
        "{}{}",
        if mac.starts_with('0') { '1' } else { '0' },
        &mac[1..]
    );
    first["m"] = corrupt.into();
    // Preserve the event and chain hash: only full MAC validation detects
    // this corrupted earlier line, even when the caller requests no suffix.
    std::fs::write(
        paths.events_file(),
        format!("{}\n{rest}", serde_json::to_string(&first).unwrap()),
    )
    .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn read_work_http_overload_rejects_expensive_routes_while_health_stays_live() {
    let tmp = tempfile::tempdir().unwrap();
    seed(tmp.path(), 1);
    let reads = ReadWork::new(1);
    let router = app(tmp.path(), reads.clone());
    let (release, task) = hold(reads).await;
    for route in [
        "/missions",
        "/missions/outcomes",
        "/escalation-metrics",
        "/standards-metrics",
        "/cost-per-merged-change",
        "/missions/m-read-work/state",
        "/missions/m-read-work/events?since=1",
        "/missions/m-read-work/workspace",
        "/missions/m-read-work/standards",
        "/missions/m-read-work/diff-stat",
        "/missions/m-read-work/revision-diff",
        "/missions/m-read-work/readiness",
        "/missions/m-read-work/hook-status",
        "/missions/m-read-work/pending-plan",
        "/tickets",
        "/tickets/example",
        "/queue",
    ] {
        let response = tokio::time::timeout(
            WAIT,
            router.clone().oneshot(request(&format!("/api{route}"))),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{route}"
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value["error"]
            .as_str()
            .unwrap()
            .contains("readers are busy"));
    }
    let response = tokio::time::timeout(WAIT, router.clone().oneshot(request("/api/health")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    release.send(()).unwrap();
    task.await.unwrap().unwrap();
    let response = router
        .oneshot(request("/api/missions/m-read-work/state"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test(flavor = "current_thread")]
async fn read_work_large_concurrent_rest_reads_keep_full_authenticated_prefix_validation() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = seed(tmp.path(), 512);
    assert!(std::fs::metadata(paths.events_file()).unwrap().len() > 1024 * 1024);
    let router = app(tmp.path(), ReadWork::new(8));
    let mut tasks = Vec::new();
    for i in 0..8 {
        let router = router.clone();
        tasks.push(tokio::spawn(async move {
            let uri = if i % 2 == 0 {
                "/api/missions/m-read-work/state"
            } else {
                "/api/missions/m-read-work/events?since=512"
            };
            let response = router.oneshot(request(uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            if i % 2 == 0 {
                assert_eq!(value["lastSeq"], 513);
            } else {
                assert_eq!(value[0]["seq"], 513);
            }
        }));
    }
    for task in tasks {
        tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
    }
    corrupt_prefix(&paths);
    // A cursor beyond the corrupt line (even beyond head) never skips MAC
    // verification of the prefix. No snapshot/state cache masks the rewrite.
    for route in ["state", "events?since=512", "events?since=99999"] {
        let response = router
            .clone()
            .oneshot(request(&format!("/api/missions/m-read-work/{route}")))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "{route}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn read_work_ws_busy_poll_preserves_cursor_and_revalidates_corrupt_prefix() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;
    let tmp = tempfile::tempdir().unwrap();
    let paths = seed(tmp.path(), 0);
    let reads = ReadWork::new(1);
    let router = app(tmp.path(), reads.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut req = format!("ws://{addr}/api/missions/m-read-work/ws")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", "http://localhost:5173".parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let snapshot = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let snapshot: Value = serde_json::from_str(snapshot.to_text().unwrap()).unwrap();
    assert_eq!(snapshot["seq"], 1);
    let (release, task) = hold(reads).await;
    let mut log = EventLog::acquire(&paths, "m-read-work", Duration::ZERO, LockForce::No).unwrap();
    log.append(EventKind::UserMessage {
        text: "new event during overload".into(),
        interrupt: false,
    })
    .unwrap();
    drop(log);
    // Give the busy poll a tick, then prove the same socket remains live.
    tokio::time::sleep(Duration::from_millis(300)).await;
    ws.send(Message::Text(json!({"type":"ping"}).to_string().into()))
        .await
        .unwrap();
    let pong = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(pong.to_text().unwrap()).unwrap()["type"],
        "pong"
    );
    release.send(()).unwrap();
    task.await.unwrap().unwrap();
    let event = tokio::time::timeout(WAIT, ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let event: Value = serde_json::from_str(event.to_text().unwrap()).unwrap();
    assert_eq!(event["type"], "event");
    assert_eq!(event["seq"], 2);
    corrupt_prefix(&paths);
    tokio::time::timeout(WAIT, async {
        while let Some(message) = ws.next().await {
            match message {
                Ok(Message::Close(_)) | Err(_) => return,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    server.abort();
}
