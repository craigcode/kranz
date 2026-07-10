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
mod tickets;
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
    /// Serve bind port threaded into CORS / WS origin checks. `None` keeps
    /// the test/back-compat wildcard (any localhost/127.0.0.1 port).
    pub bind_port: Option<u16>,
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
///
/// Test/back-compat path: CORS allows any localhost/127.0.0.1 port (and
/// Tauri), and GETs stay tokenless.
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
///
/// Test/back-compat path: CORS allows any localhost/127.0.0.1 port (and
/// Tauri), and GETs stay tokenless. Prefer
/// [`router_with_shared_host_and_bind`] when the bind address/port are known.
pub fn router_with_shared_host(
    host: Arc<MissionHost>,
    static_assets: Option<DashboardStatic>,
    token: Option<String>,
) -> Router {
    router_with_shared_host_and_bind(host, static_assets, token, None, false)
}

/// Router constructor that threads the serve bind port into CORS / WS origin
/// checks and optionally requires the mutation token on GET + WS upgrade
/// (`require_read_token`, true when the bind address is not loopback).
pub fn router_with_shared_host_and_bind(
    host: Arc<MissionHost>,
    static_assets: Option<DashboardStatic>,
    token: Option<String>,
    bind_port: Option<u16>,
    require_read_token: bool,
) -> Router {
    let bind_is_loopback = !require_read_token;
    let state = Arc::new(ServerState {
        repo_root: host.repo_root().clone(),
        host,
        bind_port,
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
        .route("/api/missions/{id}/plan.md", get(rest::mission_plan_md))
        .route(
            "/api/missions/{id}/revision-diff",
            get(rest::mission_revision_diff),
        )
        .route("/api/missions/{id}/report.md", get(rest::mission_report_md))
        .route("/api/missions/{id}/diff-stat", get(rest::mission_diff_stat))
        .route(
            "/api/missions/{id}/runs/{run_id}/transcript",
            get(rest::run_transcript),
        )
        .route("/api/missions/{id}/control", post(rest::post_control))
        .route("/api/missions/{id}/revise", post(rest::post_revise))
        .route(
            "/api/missions/{id}/revision/approve",
            post(rest::post_revision_approve),
        )
        .route(
            "/api/missions/{id}/revision/reject",
            post(rest::post_revision_reject),
        )
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
        .route("/api/missions/{id}/merge", post(host::merge_mission_route))
        .route("/api/missions/{id}/ws", get(ws::ws_handler))
        .route(
            "/api/tickets",
            get(tickets::list_tickets).post(tickets::create_ticket),
        )
        .route("/api/tickets/{slug}", get(tickets::get_ticket))
        .route("/api/tickets/{slug}/draft", post(tickets::draft_ticket))
        .route("/api/tickets/{slug}/approve", post(tickets::approve_ticket))
        .route("/api/queue", get(host::queue_state_route))
        .route("/api/queue/drain", post(host::drain_queue_route))
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

    // Layer order (outermost last): the CORS layer wraps the Host gate wraps
    // the JSON gate wraps the token gate, so even rejections carry CORS
    // headers for approved origins and a non-JSON POST is rejected before the
    // token is examined.
    app.layer(middleware::from_fn_with_state(
        TokenGate {
            token,
            require_read_token,
        },
        require_mutation_token,
    ))
    .layer(middleware::from_fn(require_json_api_posts))
    .layer(middleware::from_fn_with_state(
        HostGate { bind_is_loopback },
        require_host,
    ))
    .layer(cors_layer(bind_port))
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
fn cors_layer(bind_port: Option<u16>) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(
            move |origin: &HeaderValue, _request_parts| {
                origin.to_str().is_ok_and(|o| origin_allowed(o, bind_port))
            },
        ))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE, HeaderName::from_static(TOKEN_HEADER)])
}

/// Trusted origins for CORS and the WebSocket upgrade Origin check.
///
/// Always: `tauri://localhost` (macOS/Linux Tauri) and
/// `http://tauri.localhost` (Windows Tauri), plus any
/// `http://localhost:<u16>` / `http://127.0.0.1:<u16>` (and empty-port
/// forms). Loopback dashboard workflows — vite on :5173, Tauri, and
/// same-origin on the bind port — must keep working on every bind.
///
/// When `bind_port` is `Some(p)` on a non-loopback concern: the localhost
/// forms above stay allowed (operators often open the LAN URL while the
/// vite proxy still talks same-machine), and port-scoping only applies if
/// callers pass an allowlist that needs it. In practice we always allow
/// every localhost port: a cors-origin DNS-rebinding attack still needs
/// the Host gate + (off-loopback) the mutation token on reads.
///
/// Port matching is prefix + `u16` parse — NEVER substring matching,
/// which would also approve e.g. `http://localhost.evil.example`.
pub(crate) fn origin_allowed(origin: &str, bind_port: Option<u16>) -> bool {
    let _ = bind_port; // reserved: callers pass Some(port) for future LAN scoping
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

/// Host gate: browsers always send `Host`, so DNS rebinding attempts arrive
/// as the attacker-controlled hostname and are rejected before tokenless
/// reads can return mission state or transcripts.
///
/// Path-only in-process requests used by `tower::ServiceExt::oneshot` carry no
/// Host header and are allowed; real network HTTP/1.1 requests present Host.
async fn require_host(State(gate): State<HostGate>, request: Request, next: Next) -> Response {
    if let Some(host) = request.headers().get(header::HOST) {
        if !host
            .to_str()
            .is_ok_and(|h| host_allowed(h, gate.bind_is_loopback))
        {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({ "error": "invalid host" })),
            )
                .into_response();
        }
    }
    next.run(request).await
}

/// Trusted HTTP Host values.
///
/// Always: `localhost` / `127.0.0.1` (optional `:<u16>`), plus IPv6 loopback
/// forms (`::1`, `[::1]`, optional port).
///
/// When `bind_is_loopback` is false (LAN / tailnet serve): any Host whose
/// hostname parses as an IP is accepted — the operator intentionally
/// exposed non-loopback, and the mutation token (including on GET/WS)
/// is what authenticates. Hostname DNS-rebinding still fails the IP
/// parse; browsers sending `evil.example` are rejected.
fn host_allowed(host: &str, bind_is_loopback: bool) -> bool {
    let host = host.trim().to_ascii_lowercase();
    if host_is_loopback(&host) {
        return true;
    }
    if bind_is_loopback {
        return false;
    }
    // Strip optional :port (v4) or ]:port (v6 bracket form).
    let without_port = if let Some(rest) = host.strip_prefix('[') {
        rest.split_once(']').map(|(addr, _)| addr).unwrap_or(rest)
    } else {
        host.rsplit_once(':')
            .and_then(|(addr, port)| {
                if port.parse::<u16>().is_ok() {
                    Some(addr)
                } else {
                    None
                }
            })
            .unwrap_or(host.as_str())
    };
    without_port.parse::<std::net::IpAddr>().is_ok()
}

fn host_is_loopback(host: &str) -> bool {
    for base in ["localhost", "127.0.0.1", "::1"] {
        if host == base {
            return true;
        }
        if let Some(port) = host.strip_prefix(&format!("{base}:")) {
            if port.parse::<u16>().is_ok() {
                return true;
            }
        }
    }
    // Bracketed IPv6 loopback: [::1] or [::1]:port
    if let Some(rest) = host.strip_prefix("[::1]") {
        return rest.is_empty()
            || rest
                .strip_prefix(':')
                .is_some_and(|p| p.parse::<u16>().is_ok());
    }
    false
}

/// Reject any `POST /api/...` with a non-empty body whose content-type is
/// not `application/json`.
///
/// The control handler parses raw bytes, so without this gate a drive-by
/// page could bypass the CORS preflight entirely: `text/plain` (or
/// form-encoded) POSTs are CORS-"simple" and browsers send them cross-origin
/// WITHOUT a preflight — the attacker cannot read the response, but the
/// ControlCommand side effect would already have happened. Requiring JSON
/// forces every browser POST into the preflighted path that
/// [`cors_layer`] guards.
///
/// An EMPTY body (no `Content-Length`, or `Content-Length: 0`) is exempt
/// from the content-type check: it carries no payload for a drive-by page
/// to control, so relaxing the mime requirement here doesn't reopen the
/// CSRF vector above — bodyless POSTs still have to clear
/// [`require_mutation_token`] downstream, which is the gate actually
/// closing it. (A chunked/streaming request that omits `Content-Length`
/// but streams a non-empty body is not treated as empty here — that's an
/// accepted non-goal, not a bypass this middleware promises to catch.)
async fn require_json_api_posts(request: Request, next: Next) -> Response {
    if request.method() == Method::POST && request.uri().path().starts_with("/api/") {
        let is_empty_body = request
            .headers()
            .get(header::CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| value == "0");
        let is_json = request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"));
        if !is_empty_body && !is_json {
            return (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Json(json!({ "error": "POST bodies must be application/json" })),
            )
                .into_response();
        }
    }
    next.run(request).await
}

/// Token gate state: the optional mutation token plus whether non-loopback
/// binds also require it on GET / WS upgrade.
#[derive(Clone)]
struct TokenGate {
    token: Option<String>,
    require_read_token: bool,
}

/// Host gate state: whether the serve bind is loopback (strict Host) or
/// LAN/tailnet (accept any Host that parses as an IP).
#[derive(Clone)]
struct HostGate {
    bind_is_loopback: bool,
}

/// Require the per-serve mutation token on every `POST /api/...` (protocol
/// "Authority: mutation token"). When [`TokenGate::require_read_token`] is
/// set (non-loopback bind), GETs / HEADs under `/api/` (except `/api/health`)
/// require the token too — via the `x-kranz-token` header or a `?token=`
/// query (browsers cannot set WS headers). `token: None` (back-compat test
/// wrappers only) disables the gate entirely.
///
/// Rationale: the 127.0.0.1 bind + CORS allowlist stop the network and the
/// browser; the token stops other local processes and link-borne CSRF from
/// creating or steering missions that spend money. Off-loopback, tokenless
/// reads would expose mission state to the LAN, so reads are gated too.
async fn require_mutation_token(
    State(gate): State<TokenGate>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(expected) = gate.token.as_deref() {
        let path = request.uri().path();
        let is_health = path == "/api/health";
        let needs_token = path.starts_with("/api/")
            && !is_health
            && (request.method() == Method::POST
                || (gate.require_read_token
                    && (request.method() == Method::GET || request.method() == Method::HEAD)));
        if needs_token {
            let header_ok = request
                .headers()
                .get(TOKEN_HEADER)
                .and_then(|value| value.to_str().ok())
                == Some(expected);
            let query_ok = request
                .uri()
                .query()
                .map(|q| {
                    q.split('&').any(|pair| {
                        let mut parts = pair.splitn(2, '=');
                        matches!(parts.next(), Some("token"))
                            && parts
                                .next()
                                .is_some_and(|v| percent_decode_token(v) == expected)
                    })
                })
                .unwrap_or(false);
            if !header_ok && !query_ok {
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

/// Minimal percent-decode for `?token=` values (`%XX` only — tokens are
/// uuid hex so this only needs to round-trip `encodeURIComponent`).
fn percent_decode_token(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
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
/// the same LAN / tailnet). Every POST stays mutation-token-gated; when
/// `bind` is not loopback, GETs and WS upgrades require the token too. The
/// CLI prints a loud warning for non-loopback binds.
pub async fn serve_with_shared_host(
    host: Arc<MissionHost>,
    bind: IpAddr,
    port: u16,
    static_assets: Option<DashboardStatic>,
    token: Option<String>,
) -> anyhow::Result<()> {
    let shutdown = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install ctrl-c handler");
        }
    };
    serve_with_shutdown(host, bind, port, static_assets, token, shutdown).await
}

/// Same as [`serve_with_shared_host`], but takes an explicit shutdown
/// signal instead of always waiting on Ctrl-C — the testable seam that lets
/// callers (and tests) make the serve future return deterministically.
pub async fn serve_with_shutdown(
    host: Arc<MissionHost>,
    bind: IpAddr,
    port: u16,
    static_assets: Option<DashboardStatic>,
    token: Option<String>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let require_read_token = !bind.is_loopback();
    let app = router_with_shared_host_and_bind(
        host,
        static_assets,
        token,
        Some(port),
        require_read_token,
    );
    let addr = SocketAddr::from((bind, port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;
    tracing::info!("kranz server listening on http://{local_addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{host_allowed, origin_allowed};

    #[test]
    fn origin_allowlist_accepts_only_local_dev_and_tauri() {
        // Back-compat / test path: any localhost port.
        for allowed in [
            "http://localhost",
            "http://localhost:80",
            "http://localhost:5173",
            "http://127.0.0.1",
            "http://127.0.0.1:65535",
            "tauri://localhost",
            "http://tauri.localhost",
        ] {
            assert!(origin_allowed(allowed, None), "should allow {allowed}");
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
            assert!(!origin_allowed(denied, None), "should deny {denied}");
        }
    }

    #[test]
    fn origin_allowlist_keeps_vite_proxy_on_any_bind_port() {
        // Bind-port scoping must NOT reject the vite proxy (:5173) — operators
        // use it against both loopback and LAN serves.
        let port = Some(4560u16);
        for allowed in [
            "http://localhost:4560",
            "http://127.0.0.1:4560",
            "http://localhost:5173",
            "http://127.0.0.1:8080",
            "http://localhost",
            "http://127.0.0.1",
            "tauri://localhost",
            "http://tauri.localhost",
        ] {
            assert!(
                origin_allowed(allowed, port),
                "should allow {allowed} for bind 4560"
            );
        }
        for denied in [
            "http://localhost.evil.example:4560",
            "https://localhost:4560",
            "https://evil.example",
        ] {
            assert!(
                !origin_allowed(denied, port),
                "should deny {denied} for bind 4560"
            );
        }
    }

    #[test]
    fn host_allowlist_loopback_rejects_lan_and_dns() {
        for allowed in [
            "localhost",
            "localhost:4560",
            "LOCALHOST:5173",
            "127.0.0.1",
            "127.0.0.1:65535",
            "::1",
            "[::1]",
            "[::1]:4560",
        ] {
            assert!(
                host_allowed(allowed, true),
                "loopback bind should allow {allowed}"
            );
        }
        for denied in [
            "evil.example",
            "evil.example:4560",
            "localhost.evil.example",
            "192.168.1.10",
            "192.168.1.10:4560",
            "10.0.0.1:8080",
            "",
        ] {
            assert!(
                !host_allowed(denied, true),
                "loopback bind should deny {denied}"
            );
        }
    }

    #[test]
    fn host_allowlist_lan_accepts_ip_hosts() {
        for allowed in [
            "192.168.1.10",
            "192.168.1.10:4560",
            "10.0.0.1:8080",
            "localhost",
            "127.0.0.1:4560",
            "[::1]:4560",
        ] {
            assert!(
                host_allowed(allowed, false),
                "LAN bind should allow {allowed}"
            );
        }
        for denied in [
            "evil.example",
            "evil.example:4560",
            "localhost.evil.example",
            "",
        ] {
            assert!(
                !host_allowed(denied, false),
                "LAN bind should still deny DNS Host {denied}"
            );
        }
    }
}
