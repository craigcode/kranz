//! Signed-body-only replay key and 24-hour server-clock replay memory for
//! the Sentry signal source (`docs/scoping/signal-intake.md`, 2026-11-13
//! operator decision). Drives `SentrySignalSource::verify` directly against a
//! `ReplayCache` with an explicit server-clock `now`.

use chrono::{TimeZone, Utc};
use kranz_engine::signals::sentry::{SentryConfig, SentrySignalSource};
use kranz_engine::signals::source::{DropReason, ReplayCache, SignalSource};
use std::collections::BTreeMap;

const SECRET: &str = "dummy-replay-test-secret";

fn sentry_source() -> SentrySignalSource {
    SentrySignalSource::new(SentryConfig {
        secret: Some(SECRET.to_string()),
        allowed_hosts: vec!["sentry.io".to_string()],
        api_base_url: None,
        token_env: None,
    })
}

fn body_for(marker: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "action": "created",
        "data": {
            "issue": {
                "id": "1",
                "shortId": "KRANZ-1",
                "marker": marker
            }
        }
    }))
    .unwrap()
}

fn headers_for(
    body: &[u8],
    timestamp: chrono::DateTime<Utc>,
    request_id: Option<&str>,
    sig_transform: impl FnOnce(String) -> String,
) -> BTreeMap<String, String> {
    let sig = SentrySignalSource::sign_body(SECRET, body);
    let mut headers = BTreeMap::new();
    headers.insert("sentry-hook-resource".to_string(), "issue".to_string());
    headers.insert(
        "sentry-hook-timestamp".to_string(),
        timestamp.timestamp().to_string(),
    );
    headers.insert("sentry-hook-signature".to_string(), sig_transform(sig));
    if let Some(rid) = request_id {
        headers.insert("request-id".to_string(), rid.to_string());
    }
    headers
}

#[test]
fn signed_replay_fresh_headers_do_not_bypass() {
    let source = sentry_source();
    let mut cache = ReplayCache::default();
    let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let body = body_for("same-body");

    let h0 = headers_for(&body, t0, Some("r1"), |s| s);
    source.verify(&h0, &body, t0, &mut cache).unwrap();

    // Fresh timestamp header and a new request-id: still a replay.
    let t1 = t0 + chrono::Duration::seconds(10);
    let h1 = headers_for(&body, t1, Some("r2"), |s| s);
    assert_eq!(
        source.verify(&h1, &body, t1, &mut cache),
        Err(DropReason::ReplayedDelivery)
    );

    // X-Sentry-Delivery instead of Request-ID: still a replay.
    let mut h2 = headers_for(&body, t1, None, |s| s);
    h2.insert("x-sentry-delivery".to_string(), "r3".to_string());
    assert_eq!(
        source.verify(&h2, &body, t1, &mut cache),
        Err(DropReason::ReplayedDelivery)
    );

    // Uppercased signature: still a replay.
    let h3 = headers_for(&body, t1, Some("r4"), |s| s.to_ascii_uppercase());
    assert_eq!(
        source.verify(&h3, &body, t1, &mut cache),
        Err(DropReason::ReplayedDelivery)
    );

    // sha256=-prefixed signature: still a replay.
    let h4 = headers_for(&body, t1, Some("r5"), |s| format!("sha256={s}"));
    assert_eq!(
        source.verify(&h4, &body, t1, &mut cache),
        Err(DropReason::ReplayedDelivery)
    );
}

#[test]
fn signed_replay_remembered_for_24h_on_server_clock() {
    let source = sentry_source();
    let mut cache = ReplayCache::default();
    let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let body = body_for("remembered");

    let h0 = headers_for(&body, t0, Some("r1"), |s| s);
    source.verify(&h0, &body, t0, &mut cache).unwrap();

    for (idx, offset) in [301i64, 3600, 86_399].into_iter().enumerate() {
        let now = t0 + chrono::Duration::seconds(offset);
        let headers = headers_for(&body, now, Some(&format!("r-{idx}")), |s| s);
        assert_eq!(
            source.verify(&headers, &body, now, &mut cache),
            Err(DropReason::ReplayedDelivery),
            "expected replay at offset {offset}s"
        );
    }
}

#[test]
fn signed_replay_distinct_bodies_same_request_id_both_accepted() {
    let source = sentry_source();
    let mut cache = ReplayCache::default();
    let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

    let body_a = body_for("a");
    let body_b = body_for("b");

    let ha = headers_for(&body_a, t0, Some("shared-request-id"), |s| s);
    assert!(source.verify(&ha, &body_a, t0, &mut cache).is_ok());

    let hb = headers_for(&body_b, t0, Some("shared-request-id"), |s| s);
    assert!(source.verify(&hb, &body_b, t0, &mut cache).is_ok());
}

#[test]
fn signed_replay_skew_still_refused() {
    let source = sentry_source();
    let mut cache = ReplayCache::default();
    let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let body = body_for("skewed");

    let skewed_ts = t0 + chrono::Duration::seconds(301);
    let headers = headers_for(&body, skewed_ts, Some("r1"), |s| s);
    assert_eq!(
        source.verify(&headers, &body, t0, &mut cache),
        Err(DropReason::TimestampSkew)
    );
}

#[test]
fn signed_replay_full_cache_shortens_window() {
    let source = sentry_source();
    let mut cache = ReplayCache::new(16, 300);
    let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

    let bodies: Vec<Vec<u8>> = (0..17).map(|i| body_for(&format!("body-{i}"))).collect();

    for (idx, body) in bodies.iter().enumerate() {
        let headers = headers_for(body, t0, Some(&format!("r-{idx}")), |s| s);
        assert!(
            source.verify(&headers, body, t0, &mut cache).is_ok(),
            "insert {idx} should succeed"
        );
    }

    // The 17th insert evicted the oldest (body 0); it is accepted again.
    let first_body = &bodies[0];
    let headers = headers_for(first_body, t0, Some("r-again"), |s| s);
    assert!(
        source.verify(&headers, first_body, t0, &mut cache).is_ok(),
        "evicted first body should be accepted again"
    );
}
