//! Integration test for the OTLP HTTP export path (`kranz_cli::otel::emit`).
//!
//! Stands up a minimal local HTTP receiver bound to `127.0.0.1:0` (an
//! ephemeral port) that accepts POSTs to `/v1/traces`, then runs
//! `export_spans` against it and asserts at least one non-empty request
//! arrived. Hermetic: no external services, no fixed ports.

use axum::body::Bytes;
use axum::routing::post;
use axum::Router;
use chrono::{TimeZone, Utc};
use kranz_cli::otel::map::{span_id, trace_id, AttrValue, MissionSpan, SpanStatus};
use kranz_cli::otel::{build_exporter, export_spans};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Clone, Default)]
struct Received {
    count: Arc<AtomicUsize>,
    non_empty: Arc<AtomicUsize>,
}

async fn handle_traces(
    axum::extract::State(state): axum::extract::State<Received>,
    body: Bytes,
) -> &'static str {
    state.count.fetch_add(1, Ordering::SeqCst);
    if !body.is_empty() {
        state.non_empty.fetch_add(1, Ordering::SeqCst);
    }
    "ok"
}

fn sample_spans() -> Vec<MissionSpan> {
    let ts = |secs: i64| Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap();
    vec![MissionSpan {
        trace_id: trace_id("m-otel-export"),
        span_id: span_id("m-otel-export", 1),
        parent_span_id: None,
        name: "mission m-otel-export".to_string(),
        start: ts(0),
        end: ts(10),
        attributes: vec![(
            "kranz.mission.status".to_string(),
            AttrValue::String("complete".to_string()),
        )],
        status: SpanStatus::Ok,
    }]
}

#[tokio::test]
async fn from_start_exports_spans_to_endpoint() {
    let received = Received::default();
    let app = Router::new()
        .route("/v1/traces", post(handle_traces))
        .with_state(received.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let endpoint = format!("http://{addr}/v1/traces");
    let exporter =
        build_exporter(&endpoint).expect("exporter should build against a valid http endpoint");

    export_spans(&exporter, sample_spans()).await;

    // Give the async HTTP client a moment to complete the request.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    assert!(
        received.count.load(Ordering::SeqCst) >= 1,
        "expected at least one export request to reach the mock receiver"
    );
    assert!(
        received.non_empty.load(Ordering::SeqCst) >= 1,
        "expected at least one export request with a non-empty body"
    );
}
