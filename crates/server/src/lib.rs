//! kranz-server — axum REST + WebSocket layer over a repo's mission data
//! (docs/protocol.md is authoritative for every route and frame shape).
//!
//! The read/steer routes NEVER write `events.jsonl` (single-writer rule
//! §4.3): their only write path is the control inbox
//! (`POST /api/missions/:id/control` → [`kranz_engine::control::enqueue`]).
//! Every read handler re-reads from disk on each request — the engine owns
//! truth and requests are localhost-cheap at human timescales, so there is
//! no in-memory cache to invalidate.
//!
//! Missions created via `POST /api/missions` are HOSTED (M2.5): for those,
//! this process holds the [`MissionEngine`](kranz_engine::orchestrator) —
//! and therefore the single-writer lock — in [`MissionHost`], which is the
//! engine writing `events.jsonl`. See [`host`].

mod error;
mod host;
mod rest;
mod ws;

pub use error::ApiError;
pub use host::MissionHost;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

/// Header carrying the per-serve mutation token (docs/protocol.md
/// "Authority: mutation token").
pub const TOKEN_HEADER: &str = "x-kranz-token";

/// A fresh mutation token: uuid v4 as simple hex. Exposed so embedding
/// shells (the CLI, the Tauri app) mint tokens without their own uuid dep.
pub fn generate_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// A single dashboard file embedded into a caller's binary.
#[derive(Clone, Copy, Debug)]
pub struct EmbeddedFile {
    pub path: &'static str,
    pub bytes: &'static [u8],
    pub content_type: &'static str,
}

/// Static dashboard source for the catch-all frontend routes.
pub enum DashboardStatic {
    Dir(PathBuf),
    Embedded(&'static [EmbeddedFile]),
}

/// Shared handler state: the repo root for the read-only routes (all mission
/// data is re-read from disk per request) plus the hosted-engine registry.
pub struct ServerState {
    pub repo_root: PathBuf,
    /// Shared (`Arc`) so `kranz serve --slack` can hand the SAME registry to
    /// the Slack bridge: web and Slack are two clients of one set of live
    /// engines, never two engines fighting over one mission lock.
    pub host: Arc<MissionHost>,
}

/// Build the full router (public so tests can drive it with
/// `tower::ServiceExt::oneshot` without binding a port).
///
/// Back-compat wrapper: NO mutation token gate (tests only — the real
/// `kranz serve` / Tauri paths always pass a token).
///
/// When `static_dir` is `Some`, non-`/api` paths are served from it with an
/// SPA fallback to its `index.html`; otherwise `/` returns a minimal
/// informational text response.
pub fn router(repo_root: PathBuf, static_dir: Option<PathBuf>) -> Router {
    router_with_static(repo_root, static_dir.map(DashboardStatic::Dir))
}

/// Build the full router with either filesystem or embedded dashboard assets.
/// Back-compat wrapper: NO mutation token gate (tests only).
pub fn router_with_static(repo_root: PathBuf, static_assets: Option<DashboardStatic>) -> Router {
    router_with_token(repo_root, static_assets, None)
}

/// Build the full router with an optional mutation token gating every
/// `POST /api/...` (`None` disables the gate — back-compat test wrappers
/// only; real serving always passes `Some`).
pub fn router_with_token(
    repo_root: PathBuf,
    static_assets: Option<DashboardStatic>,
    token: Option<String>,
) -> Router {
    router_with_host(MissionHost::new(repo_root), static_assets, token)
}

/// The real router constructor: an explicit [`MissionHost`] (tests inject a
/// mock agent backend via [`MissionHost::with_backend`]) plus the optional
/// mutation token.
pub fn router_with_host(
    host: MissionHost,
    static_assets: Option<DashboardStatic>,
    token: Option<String>,
) -> Router {
    router_with_shared_host(Arc::new(host), static_assets, token)
}

/// [`router_with_host`] over an already-shared registry — the `kranz serve
/// --slack` path, where the Slack bridge holds a clone of the same host.
pub fn router_with_shared_host(
    host: Arc<MissionHost>,
    static_assets: Option<DashboardStatic>,
    token: Option<String>,
) -> Router {
    let state = Arc::new(ServerState {
        repo_root: host.repo_root().clone(),
        host,
    });
    let app = Router::new()
        .route("/api/health", get(rest::health))
        .route(
            "/api/missions",
            get(rest::list_missions).post(host::create_mission),
        )
        .route("/api/missions/{id}/state", get(rest::mission_state))
        .route("/api/missions/{id}/events", get(rest::mission_events))
        .route("/api/missions/{id}/plan", get(rest::mission_plan))
        .route(
            "/api/missions/{id}/runs/{run_id}/transcript",
            get(rest::run_transcript),
        )
        .route("/api/missions/{id}/control", post(rest::post_control))
        .route(
            "/api/missions/{id}/planning/turn",
            post(host::planning_turn),
        )
        .route(
            "/api/missions/{id}/planning/request-plan",
            post(host::request_plan),
        )
        .route("/api/missions/{id}/approve", post(host::approve_mission))
        .route("/api/missions/{id}/start", post(host::start_mission))
        .route(
            "/api/missions/{id}/pending-plan",
            get(host::pending_plan_route),
        )
        .route(
            "/api/missions/{id}/approve-pending",
            post(host::approve_pending_route),
        )
        .route(
            "/api/missions/{id}/abandon",
            post(host::abandon_mission_route),
        )
        .route(
            "/api/missions/{id}/release",
            post(host::release_mission_route),
        )
        .route(
            "/api/missions/{id}/delete",
            post(host::delete_mission_route),
        )
        .route("/api/missions/{id}/ws", get(ws::ws_handler))
        .with_state(state);

    let app = match static_assets {
        Some(DashboardStatic::Dir(dir)) => {
            let index = dir.join("index.html");
            app.fallback_service(ServeDir::new(&dir).fallback(ServeFile::new(index)))
        }
        Some(DashboardStatic::Embedded(files)) => {
            app.fallback(move |uri: Uri| async move { embedded_static_response(uri, files) })
        }
        None => app.route("/", get(root_info)),
    };

    // Layer order (outermost last): the CORS layer wraps the JSON gate wraps
    // the token gate, so even rejections carry CORS headers for approved
    // origins and a non-JSON POST is rejected before the token is examined.
    app.layer(middleware::from_fn_with_state(
        token,
        require_mutation_token,
    ))
    .layer(middleware::from_fn(require_json_api_posts))
    .layer(cors_layer())
}

fn embedded_static_response(uri: Uri, files: &'static [EmbeddedFile]) -> Response {
    let requested = uri.path().trim_start_matches('/');
    let requested = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };
    let file = files
        .iter()
        .find(|file| file.path == requested)
        .or_else(|| files.iter().find(|file| file.path == "index.html"));

    let Some(file) = file else {
        return StatusCode::NOT_FOUND.into_response();
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, file.content_type)
        .body(Body::from(file.bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
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
        .allow_origin(AllowOrigin::predicate(
            |origin: &HeaderValue, _request_parts| origin.to_str().is_ok_and(origin_allowed),
        ))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE, HeaderName::from_static(TOKEN_HEADER)])
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
                || rest
                    .strip_prefix(':')
                    .is_some_and(|port| port.parse::<u16>().is_ok())
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

/// Require the per-serve mutation token on every `POST /api/...` (protocol
/// "Authority: mutation token"). GETs and the WS upgrade stay tokenless —
/// read-only observation. `None` (back-compat test wrappers only) disables
/// the gate.
///
/// Rationale: the 127.0.0.1 bind + CORS allowlist stop the network and the
/// browser; the token stops other local processes and link-borne CSRF from
/// creating or steering missions that spend money.
async fn require_mutation_token(
    State(token): State<Option<String>>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(expected) = token.as_deref() {
        if request.method() == Method::POST && request.uri().path().starts_with("/api/") {
            let presented = request
                .headers()
                .get(TOKEN_HEADER)
                .and_then(|value| value.to_str().ok());
            if presented != Some(expected) {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(json!({ "error": "missing or invalid token" })),
                )
                    .into_response();
            }
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
/// `token` gates every `POST /api/...` — real callers (the CLI, the Tauri
/// shell) always pass `Some`.
pub async fn serve(
    repo_root: PathBuf,
    port: u16,
    static_dir: Option<PathBuf>,
    token: Option<String>,
) -> anyhow::Result<()> {
    serve_with_static(repo_root, port, static_dir.map(DashboardStatic::Dir), token).await
}

/// Bind `127.0.0.1:<port>` and serve the router until the process exits.
pub async fn serve_with_static(
    repo_root: PathBuf,
    port: u16,
    static_assets: Option<DashboardStatic>,
    token: Option<String>,
) -> anyhow::Result<()> {
    serve_with_shared_host(
        Arc::new(MissionHost::new(repo_root)),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
        static_assets,
        token,
    )
    .await
}

/// [`serve_with_static`] over an already-shared registry (see
/// [`router_with_shared_host`]).
/// `bind` widens reachability beyond loopback (e.g. for the glasses app on
/// the same LAN / tailnet). Every POST stays mutation-token-gated, but GETs
/// (states, transcripts) are tokenless by design — bind beyond loopback only
/// on networks where that is acceptable. The CLI prints a loud warning.
pub async fn serve_with_shared_host(
    host: Arc<MissionHost>,
    bind: IpAddr,
    port: u16,
    static_assets: Option<DashboardStatic>,
    token: Option<String>,
) -> anyhow::Result<()> {
    let app = router_with_shared_host(host, static_assets, token);
    let addr = SocketAddr::from((bind, port));
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
