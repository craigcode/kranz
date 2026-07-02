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

use axum::routing::{get, post};
use axum::Router;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
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

    // Localhost tooling (dashboard dev server, curl, Tauri webview).
    app.layer(CorsLayer::permissive())
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
