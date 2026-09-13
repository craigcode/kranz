//! Regressions for the public v0.2.0 server review.

use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use kranz_server::{MissionHost, MultiRepoHost, MutationAuthority};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

const MUTATION: &str = "patch-mutation";
const READ: &str = "patch-read";

fn app(root: &std::path::Path, read_auth: bool, read: Option<&str>) -> axum::Router {
    kranz_server::router_with_read_authority_and_addr(
        Arc::new(MultiRepoHost::with_host(Arc::new(MissionHost::new(
            root.into(),
        )))),
        None,
        MutationAuthority::new(MUTATION).unwrap(),
        read.map(str::to_owned),
        None,
        true,
        read_auth,
    )
}

#[tokio::test]
async fn security_patch_unknown_length_body_requires_json() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), false, Some(READ));
    for content_type in ["text/plain", "application/json"] {
        let body = Body::from_stream(futures::stream::iter([Ok::<_, std::io::Error>(
            Bytes::from_static(b"{}"),
        )]));
        let response = app
            .clone()
            .oneshot(
                Request::post("/api/missions/missing/control")
                    .header("x-kranz-token", MUTATION)
                    .header("content-type", content_type)
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();
        if content_type == "text/plain" {
            assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        } else {
            assert_ne!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }
    }
    let response = app
        .oneshot(
            Request::post("/api/queue/drain")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "empty posts still reach auth"
    );
}

#[tokio::test]
async fn security_patch_chunked_http_post_requires_json() {
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = app(root.path(), false, Some(READ));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(format!(
            "POST /api/missions/missing/control HTTP/1.1\r\nHost: {addr}\r\nContent-Type: text/plain\r\nx-kranz-token: {MUTATION}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\n{{}}\r\n0\r\n\r\n"
        ).as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    }).await;
    server.abort();
    assert!(response.unwrap().starts_with("HTTP/1.1 415"));
}

#[tokio::test]
async fn security_patch_hook_suffix_does_not_exempt_other_routes() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path(), true, Some(READ));
    for path in [
        "/api/future/hook-status",
        "/api/future/hooks/github",
        "/api/repos/future/hooks/github",
        "/api/missions/missing/hook-status",
    ] {
        let response = app
            .clone()
            .oneshot(Request::post(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
    }
    for path in ["/api/hook-status", "/api/hooks/github"] {
        let response = app
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "POST-only route: {path}"
        );
    }
}

#[tokio::test]
async fn security_patch_header_exchange_never_grants_mutation_authority() {
    let root = tempfile::tempdir().unwrap();
    for read_auth in [false, true] {
        for configured in [None, Some(READ), Some(MUTATION)] {
            let app = app(root.path(), read_auth, configured);
            for path in [
                "/api/read-token",
                "/api/read-token?token=patch-read",
                "/api/read-token?token=patch-mutation",
            ] {
                let response = app
                    .clone()
                    .oneshot(Request::get(path).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "exchange needs a header even on loopback: {path}"
                );
            }
            let response = app
                .clone()
                .oneshot(
                    Request::get("/api/read-token")
                        .header("x-kranz-token", MUTATION)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["cache-control"], "no-store");
            let body: Value =
                serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            let read = body["token"].as_str().unwrap();
            assert_ne!(read, MUTATION);
            if configured == Some(READ) {
                assert_eq!(read, READ);
            }
            let response = app
                .clone()
                .oneshot(
                    Request::get("/api/read-token")
                        .header("x-kranz-token", read)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "read-only dashboards can exchange too"
            );
            let response = app
                .clone()
                .oneshot(
                    Request::get(format!("/api/missions?token={read}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let response = app
                .clone()
                .oneshot(
                    Request::post("/api/queue/drain")
                        .header("x-kranz-token", read)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            if read_auth {
                let response = app
                    .clone()
                    .oneshot(
                        Request::get(format!("/api/missions?token={MUTATION}"))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            }
        }
    }
}
