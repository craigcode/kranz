//! kranz-server — axum REST + WebSocket layer over a repo's mission data
//! (docs/protocol.md is authoritative for every route and frame shape).
//!
//! The server NEVER writes `events.jsonl` (single-writer rule §4.3): the only
//! write path is the control inbox (`POST /api/missions/:id/control` →
//! [`kranz_engine::control::enqueue`]). Every handler re-reads from disk on
//! each request — the engine process owns truth and requests are
//! localhost-cheap at human timescales, so there is no in-memory cache to
//! invalidate.

mod error;
mod rest;
mod ws;

use axum::extract::Request;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

/// Shared handler state. Only the repo root is held; all mission data is
/// re-read from disk per request.
pub struct ServerState {
    pub repo_root: PathBuf,
}

/// Build the full router (public so tests can drive it with
/// `tower::ServiceExt::oneshot` without binding a port).
///
/// When `static_dir` is `Some`, non-`/api` paths are served from it with an
/// SPA fallback to its `index.html`; otherwise `/` returns a minimal
/// informational text response.
pub fn router(repo_root: PathBuf, static_dir: Option<PathBuf>) -> Router {
    let state = Arc::new(ServerState { repo_root });
    let app = Router::new()
        .route("/api/health", get(rest::health))
        .route("/api/missions", get(rest::list_missions))
        .route("/api/missions/{id}/state", get(rest::mission_state))
        .route("/api/missions/{id}/events", get(rest::mission_events))
        .route("/api/missions/{id}/plan", get(rest::mission_plan))
        .route(
            "/api/missions/{id}/runs/{run_id}/transcript",
            get(rest::run_transcript),
        )
        .route("/api/missions/{id}/control", post(rest::post_control))
        .route("/api/missions/{id}/ws", get(ws::ws_handler))
        .with_state(state);

    let app = match static_dir {
        Some(dir) => {
            let index = dir.join("index.html");
            app.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(index)))
        }
        None => app.route("/", get(root_info)),
    };

    // The JSON gate runs on every request; the CORS layer wraps it so even
    // rejections carry CORS headers for approved origins.
    app.layer(middleware::from_fn(require_json_api_posts)).layer(cors_layer())
}

/// CORS for localhost tooling (dashboard dev server, Tauri webview).
///
/// `CorsLayer::permissive()` echoed ANY `Origin` back in
/// `access-control-allow-origin`, so a script on any website could read
/// mission data from this (unauthenticated, fixed-port) server and POST
/// `ControlCommand`s into the orchestrator — a drive-by instruction
/// injection. Only the origins the dashboard can actually run under are
/// approved instead. That closes the drive-by vector because browsers
/// enforce CORS per-origin:
///
/// - the control POST must be `application/json` (see
///   [`require_json_api_posts`]), which is never a CORS-"simple" request, so
///   browsers always send a preflight — an unapproved origin's preflight
///   comes back without allow headers and the POST is never sent;
/// - cross-origin reads require the response to carry an
///   `access-control-allow-origin` approving the reader, which is only
///   emitted for the origins below.
///
/// Non-browser clients (curl, the engine, tests) send no `Origin` header and
/// pass through untouched — CORS is a browser-enforced mechanism.
fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin: &HeaderValue, _request_parts| {
            origin.to_str().is_ok_and(origin_allowed)
        }))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE])
}

/// Trusted origins: `http://localhost:<any port>`, `http://127.0.0.1:<any
/// port>` (dashboard dev servers), `tauri://localhost` (macOS/Linux Tauri
/// webview) and `http://tauri.localhost` (Windows Tauri webview).
///
/// The port wildcard is prefix + `u16` parse — NEVER substring matching,
/// which would also approve e.g. `http://localhost.evil.example`.
fn origin_allowed(origin: &str) -> bool {
    if origin == "tauri://localhost" || origin == "http://tauri.localhost" {
        return true;
    }
    ["http://localhost", "http://127.0.0.1"].iter().any(|base| {
        origin.strip_prefix(base).is_some_and(|rest| {
            rest.is_empty()
                || rest.strip_prefix(':').is_some_and(|port| port.parse::<u16>().is_ok())
        })
    })
}

/// Reject any `POST /api/...` whose content-type is not `application/json`.
///
/// The control handler parses raw bytes, so without this gate a drive-by
/// page could bypass the CORS preflight entirely: `text/plain` (or
/// form-encoded) POSTs are CORS-"simple" and browsers send them cross-origin
/// WITHOUT a preflight — the attacker cannot read the response, but the
/// ControlCommand side effect would already have happened. Requiring JSON
/// forces every browser POST into the preflighted path that
/// [`cors_layer`] guards.
async fn require_json_api_posts(request: Request, next: Next) -> Response {
    if request.method() == Method::POST && request.uri().path().starts_with("/api/") {
        let is_json = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"));
        if !is_json {
            return (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Json(json!({ "error": "POST bodies must be application/json" })),
            )
                .into_response();
        }
    }
    next.run(request).await
}

/// `GET /` when no dashboard bundle is configured.
async fn root_info() -> &'static str {
    "kranz server is running (no dashboard bundle configured).\n\
     REST + WebSocket API under /api — see docs/protocol.md.\n"
}

/// Bind `127.0.0.1:<port>` and serve the router until the process exits.
pub async fn serve(
    repo_root: PathBuf,
    port: u16,
    static_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let app = router(repo_root, static_dir);
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!("kranz server listening on http://{local_addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::origin_allowed;

    #[test]
    fn origin_allowlist_accepts_only_local_dev_and_tauri() {
        for allowed in [
            "http://localhost",
            "http://localhost:80",
            "http://localhost:5173",
            "http://127.0.0.1",
            "http://127.0.0.1:65535",
            "tauri://localhost",
            "http://tauri.localhost",
        ] {
            assert!(origin_allowed(allowed), "should allow {allowed}");
        }
        for denied in [
            "https://evil.example",
            // Prefix tricks a substring check would fall for.
            "http://localhost.evil.example",
            "http://localhost.evil.example:5173",
            "http://127.0.0.1.evil.example",
            "http://localhostx",
            "http://127.0.0.10",
            "http://127.0.0.10:8080",
            // Not a valid u16 port.
            "http://localhost:99999",
            "http://localhost:5173.evil.example",
            // Only the schemes/hosts the dashboard actually runs under.
            "https://localhost:5173",
            "https://tauri.localhost",
            "tauri://evil.example",
            "http://[::1]:5173",
            "null",
            "",
        ] {
            assert!(!origin_allowed(denied), "should deny {denied}");
        }
    }
}
