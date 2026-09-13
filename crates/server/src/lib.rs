#![allow(rustdoc::private_intra_doc_links)]

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
mod hooks;
mod host;
mod multi;
mod read_work;
mod rest;
mod tickets;
mod ws;

pub use error::{ApiError, ApiErrorCode};
pub use host::{MissionHost, PendingApproval};
pub use multi::{
    load_host_config, HostConfig, MultiRepoHost, RepoActivity, RepoConfig, RepoContext,
    RepoSlackConfig, RepoSummary, SlackChannelRoute,
};

use axum::body::{Body, HttpBody};
use axum::extract::{Request, State};
use axum::http::{header, HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde_json::json;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

/// Header carrying the per-serve mutation token (docs/protocol.md
/// "Authority: mutation token").
pub const TOKEN_HEADER: &str = "x-kranz-token";

/// Validated mutation authority required by every published router and serve
/// constructor. Keeping the unauthenticated state unrepresentable prevents an
/// embedder from accidentally exposing money-spending `POST /api/...` routes.
#[derive(Clone, PartialEq, Eq)]
pub struct MutationAuthority(String);

impl MutationAuthority {
    /// Validate a token for transport in [`TOKEN_HEADER`]. Tokens are opaque,
    /// but must be non-empty visible ASCII with no whitespace or control
    /// characters so every HTTP client presents the same bytes.
    pub fn new(token: impl Into<String>) -> Result<Self, InvalidMutationAuthority> {
        // Bind as `value` (not the more obvious name): generic-secret-assignment
        // fires on `let <secret-ish> = …` shapes even when the bytes are
        // caller-supplied and never a literal secret.
        let value = token.into();
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(InvalidMutationAuthority);
        }
        Ok(Self(value))
    }

    /// Borrow the token for operator storage or an authenticated client.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MutationAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MutationAuthority([REDACTED])")
    }
}

/// Error returned when a mutation token cannot be represented safely in an
/// HTTP header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidMutationAuthority;

impl fmt::Display for InvalidMutationAuthority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("mutation authority must be non-empty visible ASCII without whitespace")
    }
}

impl std::error::Error for InvalidMutationAuthority {}

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
    /// Serve bind address threaded into CORS / WS origin checks. `None`
    /// keeps the test/back-compat wildcard (any loopback host, any port).
    pub bind_addr: Option<SocketAddr>,
    /// Whether the serve bind is loopback. Loopback keeps the strict browser
    /// origin allowlist on the WS upgrade (reads are tokenless there, so the
    /// Origin check is the guard); off loopback the read token authenticates
    /// and IP-literal / missing origins are accepted.
    pub bind_is_loopback: bool,
}

/// Build a read-only convenience router with a fresh, undisclosed mutation
/// authority. GETs work normally; every mutation is refused because callers
/// cannot present that authority. Embedders that need mutations must call
/// [`router_with_token`] with an explicit [`MutationAuthority`].
///
/// When `static_dir` is `Some`, non-`/api` paths are served from it with an
/// SPA fallback to its `index.html`; otherwise `/` returns a minimal
/// informational text response.
pub fn router(repo_root: PathBuf, static_dir: Option<PathBuf>) -> Router {
    router_with_static(repo_root, static_dir.map(DashboardStatic::Dir))
}

/// Build the read-only convenience router with either filesystem or embedded
/// dashboard assets. Use [`router_with_token`] for authenticated mutations.
pub fn router_with_static(repo_root: PathBuf, static_assets: Option<DashboardStatic>) -> Router {
    let authority = MutationAuthority::new(generate_token())
        .expect("generated UUID mutation authority is valid");
    router_with_token(repo_root, static_assets, authority)
}

/// Build the full router with mutation authority gating every `POST /api/...`.
/// CORS allows any localhost/127.0.0.1 port (and Tauri), and GETs stay
/// tokenless.
pub fn router_with_token(
    repo_root: PathBuf,
    static_assets: Option<DashboardStatic>,
    authority: MutationAuthority,
) -> Router {
    router_with_host(MissionHost::new(repo_root), static_assets, authority)
}

/// The real router constructor: an explicit [`MissionHost`] (tests inject a
/// mock agent backend via [`MissionHost::with_backend`]) plus mandatory
/// mutation authority.
pub fn router_with_host(
    host: MissionHost,
    static_assets: Option<DashboardStatic>,
    authority: MutationAuthority,
) -> Router {
    router_with_shared_host(Arc::new(host), static_assets, authority)
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
    authority: MutationAuthority,
) -> Router {
    router_with_shared_host_and_bind(host, static_assets, authority, None, true, false)
}

/// Router constructor that threads the serve bind port into CORS / WS origin
/// checks and optionally requires the mutation token on GET + WS upgrade
/// (`require_read_token`, independent of `bind_is_loopback` — `--read-auth`
/// can arm it on a loopback bind without relaxing the loopback Host/origin
/// allowlist).
///
/// Port-only back-compat wrapper: assumes the canonical loopback bind IP
/// (127.0.0.1). Real serving goes through [`router_with_shared_host_and_addr`]
/// with the actual bound address so origin approval can pin the exact IP.
pub fn router_with_shared_host_and_bind(
    host: Arc<MissionHost>,
    static_assets: Option<DashboardStatic>,
    authority: MutationAuthority,
    bind_port: Option<u16>,
    bind_is_loopback: bool,
    require_read_token: bool,
) -> Router {
    router_with_shared_host_and_addr(
        host,
        static_assets,
        authority,
        bind_port.map(|port| SocketAddr::from((Ipv4Addr::LOCALHOST, port))),
        bind_is_loopback,
        require_read_token,
    )
}

/// The full router constructor: the REAL bound address (when known) scopes
/// CORS / WS origin approval to that exact ip:port plus the dev-server
/// ports, `bind_is_loopback` selects the strict loopback Host/origin
/// allowlist vs the LAN one, and `require_read_token` independently arms the
/// read/WS token gate.
pub fn router_with_shared_host_and_addr(
    host: Arc<MissionHost>,
    static_assets: Option<DashboardStatic>,
    authority: MutationAuthority,
    bind_addr: Option<SocketAddr>,
    bind_is_loopback: bool,
    require_read_token: bool,
) -> Router {
    router_with_multi_repo_host_and_addr(
        Arc::new(MultiRepoHost::with_host(host)),
        static_assets,
        authority,
        bind_addr,
        bind_is_loopback,
        require_read_token,
    )
}

/// Build one process router around a static catalog of per-repository hosts.
/// Each configured id is mounted at `/api/repos/{id}`; the historical
/// unscoped routes are mounted only when the catalog has an explicit default
/// or exactly one healthy repository.
pub fn router_with_multi_repo_host_and_addr(
    multi_host: Arc<MultiRepoHost>,
    static_assets: Option<DashboardStatic>,
    authority: MutationAuthority,
    bind_addr: Option<SocketAddr>,
    bind_is_loopback: bool,
    require_read_token: bool,
) -> Router {
    router_with_read_authority_and_addr(
        multi_host,
        static_assets,
        authority,
        None,
        bind_addr,
        bind_is_loopback,
        require_read_token,
    )
}

/// [`router_with_multi_repo_host_and_addr`] plus a distinct READ-ONLY token
/// (docs/protocol.md "Authority: mutation token"). `read_authority`
/// authenticates GET/HEAD (and the WS upgrade) wherever the read gate is
/// armed, but is never accepted on a mutating route — it is the token safe
/// to hand to dashboards and agents. If absent or equal to mutation authority,
/// a distinct read token is generated. Clients obtain it from `/api/read-token`
/// using either valid token in the header; mutation tokens remain header-only.
pub fn router_with_read_authority_and_addr(
    multi_host: Arc<MultiRepoHost>,
    static_assets: Option<DashboardStatic>,
    authority: MutationAuthority,
    read_authority: Option<String>,
    bind_addr: Option<SocketAddr>,
    bind_is_loopback: bool,
    require_read_token: bool,
) -> Router {
    let gate = TokenGate {
        read_authority: read_authority
            .filter(|read| !read.is_empty() && !token_matches(read, authority.as_str()))
            .unwrap_or_else(generate_token),
        authority,
        require_read_token,
    };
    let exchange_gate = gate.clone();
    let mut repos = Router::new();
    let catalog = Arc::clone(&multi_host);
    let catalog_reads = read_work::ReadWork::default();
    let mut app = Router::new()
        .route("/api/health", get(rest::health))
        .route(
            "/api/repos",
            get(move || {
                let catalog = Arc::clone(&catalog);
                let reads = catalog_reads.clone();
                async move { reads.run(move || Ok(Json(catalog.summaries()))).await }
            }),
        )
        .route("/api/read-token", get(read_token).with_state(exchange_gate));

    // The unscoped compatibility alias shares capacity with its scoped repo.
    let mut repo_reads = std::collections::HashMap::new();
    for context in multi_host.contexts() {
        let prefix = format!("/api/repos/{}", context.id());
        match context.host().cloned() {
            Some(host) => {
                let reads = read_work::ReadWork::default();
                repo_reads.insert(context.id().to_string(), reads.clone());
                repos = repos.nest(
                    &prefix,
                    repo_context_router(
                        context,
                        host,
                        bind_addr,
                        bind_is_loopback,
                        reads,
                        gate.clone(),
                    ),
                );
            }
            None => {
                // A nested router's fallback registers in the outer *fallback*
                // router, which the `/api/{*path}` catch-all below always
                // shadows — an unavailable repository must claim its paths as
                // explicit routes (which beat the catch-all on the static
                // `repos/<id>` segments) for the designed 503 to ever fire.
                let handler = repo_unavailable_handler(&context);
                app = app
                    .route(&prefix, any(handler.clone()))
                    .route(&format!("{prefix}/{{*path}}"), any(handler));
            }
        }
    }

    let mut unavailable_default = None;
    if let Some(context) = multi_host.compatibility_context() {
        match context.host().cloned() {
            Some(host) => {
                let reads = repo_reads
                    .entry(context.id().to_string())
                    .or_default()
                    .clone();
                repos = repos.nest(
                    "/api",
                    repo_context_router(
                        context,
                        host,
                        bind_addr,
                        bind_is_loopback,
                        reads,
                        gate.clone(),
                    ),
                );
            }
            // An explicit `defaultRepo` is not health-filtered; the whole
            // unscoped alias belongs to it, so report its unavailability
            // below instead of mounting anything.
            None => unavailable_default = Some(repo_unavailable_handler(&context)),
        }
    }

    // API misses must never fall through to the SPA fallback. In particular,
    // an ambiguous unscoped mutation in multi-repo mode must fail as JSON,
    // not return `200 index.html` and look successful to an API client. When
    // the unscoped alias targets an unavailable default repository, misses
    // report that unavailability (503 + reason) instead of a generic 404.
    app = match unavailable_default {
        Some(handler) => app
            .route("/api", any(handler.clone()))
            .route("/api/{*path}", any(handler)),
        None => app
            .route("/api", any(api_not_found))
            .route("/api/{*path}", any(api_not_found)),
    };

    // Catalog, unavailable repositories, and API misses are protected too.
    // Healthy repositories apply the same gate before adding their two
    // independently authenticated POST routes.
    let app = app
        .layer(middleware::from_fn_with_state(gate, require_mutation_token))
        .merge(repos);

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
    app.layer(middleware::from_fn(require_json_api_posts))
        .layer(middleware::from_fn_with_state(
            HostGate { bind_is_loopback },
            require_host,
        ))
        .layer(cors_layer(bind_addr))
        // Outermost: every response — including gate rejections — carries the
        // cache policy, so no rejection HTML can poison a browser cache either.
        .layer(middleware::from_fn(cache_response_headers))
}

async fn api_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "API route not found or repository scope required" })),
    )
}

/// API answers and the SPA shell must never be cached. A browser that caches
/// an HTML fallback at an /api URL replays it to `fetch()` long after the
/// server is fixed (2026-07-19: a stale pre-API-404 serve poisoned the
/// dashboard behind a heuristic cache entry; only an incognito window
/// escaped). Hashed /assets/* bundles stay implicitly cacheable; `no-cache`
/// on the shell revalidates per load rather than forbidding storage.
async fn cache_response_headers(request: Request, next: Next) -> Response {
    let is_api = request.uri().path().starts_with("/api");
    let mut response = next.run(request).await;
    let cache_control = if is_api {
        Some("no-store")
    } else if response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.starts_with("text/html"))
    {
        Some("no-cache")
    } else {
        None
    };
    if let Some(value) = cache_control {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    }
    response
}

/// `503 {"error":"repository unavailable", ...}` handler for every path under
/// an unmounted repository. Returned as a `Clone` closure so one context can
/// back both the bare-prefix and `{*path}` routes.
fn repo_unavailable_handler(
    context: &RepoContext,
) -> impl Fn() -> std::future::Ready<(StatusCode, Json<serde_json::Value>)> + Clone {
    let id = context.id().to_string();
    let reason = context
        .unavailable_reason()
        .unwrap_or("repository is unavailable")
        .to_string();
    move || {
        std::future::ready((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "repository unavailable",
                "repoId": id.clone(),
                "detail": reason.clone(),
            })),
        ))
    }
}

fn repo_context_router(
    context: Arc<RepoContext>,
    host: Arc<MissionHost>,
    bind_addr: Option<SocketAddr>,
    bind_is_loopback: bool,
    reads: read_work::ReadWork,
    gate: TokenGate,
) -> Router {
    let state = Arc::new(ServerState {
        repo_root: context.root().to_path_buf(),
        host,
        bind_addr,
        bind_is_loopback,
    });
    repo_api_routes(gate)
        .layer(axum::Extension(reads))
        .with_state(state)
}

fn repo_api_routes(gate: TokenGate) -> Router<Arc<ServerState>> {
    Router::new()
        .route(
            "/missions",
            get(rest::list_missions).post(host::create_mission),
        )
        .route("/missions/outcomes", get(rest::mission_outcomes))
        .route("/escalation-metrics", get(rest::escalation_metrics))
        .route("/standards-metrics", get(rest::standards_metrics))
        .route("/cost-per-merged-change", get(rest::cost_per_merged_change))
        .route("/missions/{id}/state", get(rest::mission_state))
        .route("/missions/{id}/standards", get(rest::mission_standards))
        .route(
            "/missions/{id}/standards/waiver",
            post(rest::post_standards_waiver),
        )
        .route("/missions/{id}/workspace", get(rest::mission_workspace))
        .route("/missions/{id}/events", get(rest::mission_events))
        .route("/missions/{id}/plan", get(rest::mission_plan))
        .route("/missions/{id}/plan.md", get(rest::mission_plan_md))
        .route(
            "/missions/{id}/revision-diff",
            get(rest::mission_revision_diff),
        )
        .route("/missions/{id}/report.md", get(rest::mission_report_md))
        .route("/missions/{id}/diff-stat", get(rest::mission_diff_stat))
        .route("/missions/{id}/pr-handoff", get(rest::mission_pr_handoff))
        .route(
            "/missions/{id}/pr-handoff/create",
            post(rest::mission_pr_create),
        )
        .route("/missions/{id}/readiness", get(rest::mission_readiness))
        .route(
            "/missions/{id}/runs/{run_id}/transcript",
            get(rest::run_transcript),
        )
        .route("/missions/{id}/hook-status", get(rest::mission_hook_status))
        .route("/missions/{id}/control", post(rest::post_control))
        .route("/missions/{id}/revise", post(rest::post_revise))
        .route(
            "/missions/{id}/revision/approve",
            post(rest::post_revision_approve),
        )
        .route(
            "/missions/{id}/revision/reject",
            post(rest::post_revision_reject),
        )
        .route(
            "/missions/{id}/grant/approve",
            post(rest::post_grant_approve),
        )
        .route("/missions/{id}/grant/deny", post(rest::post_grant_deny))
        .route(
            "/missions/{id}/question/answer",
            post(rest::post_question_answer),
        )
        .route("/missions/{id}/planning/turn", post(host::planning_turn))
        .route(
            "/missions/{id}/planning/request-plan",
            post(host::request_plan),
        )
        .route("/missions/{id}/approve", post(host::approve_mission))
        .route("/missions/{id}/start", post(host::start_mission))
        .route("/missions/{id}/pending-plan", get(host::pending_plan_route))
        .route(
            "/missions/{id}/approve-pending",
            post(host::approve_pending_route),
        )
        .route("/missions/{id}/abandon", post(host::abandon_mission_route))
        .route("/missions/{id}/release", post(host::release_mission_route))
        .route("/missions/{id}/delete", post(host::delete_mission_route))
        .route("/missions/{id}/merge", post(host::merge_mission_route))
        .route("/missions/{id}/ws", get(ws::ws_handler))
        .route(
            "/tickets",
            get(tickets::list_tickets).post(tickets::create_ticket),
        )
        .route("/tickets/{slug}", get(tickets::get_ticket))
        .route("/tickets/{slug}/draft", post(tickets::draft_ticket))
        .route("/tickets/{slug}/approve", post(tickets::approve_ticket))
        .route("/queue", get(host::queue_state_route))
        .route("/queue/drain", post(host::drain_queue_route))
        .route_layer(middleware::from_fn_with_state(gate, require_mutation_token))
        // Only these POST routes authenticate independently: GitHub HMAC
        // and per-run hook capability. Keep exceptions local to registration;
        // a future route with a similar suffix must still require authority.
        .route("/hooks/github", post(hooks::github_hook))
        .route(
            "/hook-status",
            post(rest::post_hook_status).route_layer(axum::extract::DefaultBodyLimit::max(
                kranz_engine::hook_status::SIGNAL_BODY_MAX_BYTES,
            )),
        )
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
fn cors_layer(bind_addr: Option<SocketAddr>) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(
            move |origin: &HeaderValue, _request_parts| {
                origin.to_str().is_ok_and(|o| origin_allowed(o, bind_addr))
            },
        ))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::CONTENT_TYPE, HeaderName::from_static(TOKEN_HEADER)])
}

/// Trusted origins for CORS and the browser WebSocket upgrade Origin check.
///
/// Always: `tauri://localhost` (macOS/Linux Tauri) and
/// `http://tauri.localhost` (Windows Tauri). Beyond those, an origin is
/// approved only when its host is LOCAL (the `localhost` name or a loopback
/// IP literal — DNS names like `localhost.evil.example` fail the IP parse)
/// AND its port fits the bind scoping below. On loopback binds GETs and the
/// WS upgrade are tokenless, so this allowlist is what stands between an
/// unrelated local page and mission state.
///
/// Scoping against the bound address (`Some(bind)`):
/// - the dev-server ports (vite :5173, Tauri devUrl :1420) are approved for
///   canonical localhost only (`localhost`, `127.0.0.1`, or `::1`), never
///   another address in 127/8 that a co-resident process can claim;
/// - the bind port is approved only for the SAME-ORIGIN page: an IP host
///   must equal the bound IP (any loopback IP when the bind is
///   unspecified/0.0.0.0, which listens on them all), and the `localhost`
///   name only when the bind IP is one localhost resolves to (127.0.0.1,
///   ::1, or unspecified). Pinning the IP — not just the port — matters: on
///   Linux an unprivileged co-resident process can bind ANOTHER loopback
///   address (127.0.0.2) on kranz's own port and serve a hostile page; a
///   port-only rule would hand that page tokenless cross-origin reads.
///
/// `bind_addr: None` (back-compat test wrappers only) keeps the old
/// any-loopback-host, any-port wildcard.
pub(crate) fn origin_allowed(origin: &str, bind_addr: Option<SocketAddr>) -> bool {
    if origin == "tauri://localhost" || origin == "http://tauri.localhost" {
        return true;
    }
    // Dev-server origins that must keep working on every bind: the vite
    // proxy (5173) and Tauri's devUrl (1420). Restrict these privileged ports
    // to canonical localhost; every other 127/8 address is independently
    // bindable by an unprivileged co-resident process.
    const DEV_PORTS: [u16; 2] = [5173, 1420];
    let Some(authority) = origin.strip_prefix("http://") else {
        return false;
    };
    let Some((host, port)) = split_host_port(authority) else {
        return false;
    };
    let host_ip = host.parse::<std::net::IpAddr>().ok();
    let host_local = host == "localhost" || host_ip.is_some_and(|ip| ip.is_loopback());
    if !host_local {
        return false;
    }
    let Some(bind) = bind_addr else {
        return true; // back-compat wildcard
    };
    if DEV_PORTS.contains(&port) {
        return host == "localhost"
            || host_ip == Some(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST))
            || host_ip == Some(std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST));
    }
    if port != bind.port() {
        return false;
    }
    match host_ip {
        Some(ip) => ip == bind.ip() || (bind.ip().is_unspecified() && ip.is_loopback()),
        // The `localhost` NAME resolves to 127.0.0.1 / ::1 — approve it only
        // when the server actually answers there.
        None => {
            bind.ip().is_unspecified()
                || bind.ip() == std::net::IpAddr::V4(Ipv4Addr::LOCALHOST)
                || bind.ip() == std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
        }
    }
}

/// Split an origin authority — `host[:port]` or `[v6][:port]` — into
/// hostname and port (default 80). `None` on malformed brackets or ports.
/// An unbracketed hostname containing `:` is a bare IPv6 authority, which
/// cannot carry a port.
fn split_host_port(authority: &str) -> Option<(&str, u16)> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (addr, tail) = rest.split_once(']')?;
        let port = if tail.is_empty() {
            80
        } else {
            tail.strip_prefix(':')?.parse().ok()?
        };
        return Some((addr, port));
    }
    match authority.rsplit_once(':') {
        Some((host, _)) if host.contains(':') => Some((authority, 80)),
        Some((host, port)) => Some((host, port.parse().ok()?)),
        None => Some((authority, 80)),
    }
}

/// Origin policy for the WebSocket upgrade.
///
/// Browsers send the page origin on SAME-origin WS handshakes too, so a
/// dashboard served off a LAN/tailnet bind presents `http://<ip>:<port>` —
/// which [`origin_allowed`] rejects. Off loopback the read token already
/// gates the upgrade, so the Origin check only needs to keep DNS-named
/// (rebinding) pages out: accept any origin whose host parses as an IP
/// literal, and accept a MISSING Origin (native, non-browser clients). On
/// loopback binds reads are tokenless and the strict browser allowlist
/// stays the guard — missing Origin remains rejected there.
pub(crate) fn ws_origin_allowed(
    origin: Option<&str>,
    bind_addr: Option<SocketAddr>,
    bind_is_loopback: bool,
) -> bool {
    match origin {
        None => !bind_is_loopback,
        Some(origin) => {
            origin_allowed(origin, bind_addr) || (!bind_is_loopback && origin_host_is_ip(origin))
        }
    }
}

/// `http://<ip>[:port]` (v4 or bracketed v6) — the origin shape a browser
/// sends for a page loaded straight off a LAN/tailnet serve. DNS-named
/// origins fail the IP parse, keeping rebinding pages out.
fn origin_host_is_ip(origin: &str) -> bool {
    origin
        .strip_prefix("http://")
        .and_then(split_host_port)
        .is_some_and(|(host, _)| host.parse::<std::net::IpAddr>().is_ok())
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
/// Always: `localhost` (optional `:<u16>`) and any LOOPBACK IP literal —
/// `127.0.0.1`, other 127/8 addresses, `::1` (bare or bracketed), each with
/// an optional port. Loopback literals cannot be planted by DNS rebinding
/// (browsers send the attacker's hostname, not the IP it resolves to), and
/// operators legitimately bind e.g. `--host 127.0.0.2`.
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
    host_ip(&host).is_some()
}

fn host_is_loopback(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    if let Some(port) = host.strip_prefix("localhost:") {
        return port.parse::<u16>().is_ok();
    }
    host_ip(host).is_some_and(|ip| ip.is_loopback())
}

/// Parse the hostname of a `Host` header value — bare IP (v4, or unbracketed
/// v6, which may itself contain `:` and carries no port), `v4:port`, or
/// `[v6]` with optional `:port` — as an IP address. `None` for DNS names,
/// malformed brackets, and invalid ports.
fn host_ip(host: &str) -> Option<std::net::IpAddr> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Some(ip);
    }
    if let Some(rest) = host.strip_prefix('[') {
        let (addr, tail) = rest.split_once(']')?;
        if !(tail.is_empty()
            || tail
                .strip_prefix(':')
                .is_some_and(|p| p.parse::<u16>().is_ok()))
        {
            return None;
        }
        return addr.parse().ok();
    }
    let (addr, port) = host.rsplit_once(':')?;
    if port.parse::<u16>().is_err() {
        return None;
    }
    addr.parse().ok()
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
/// A body already known to be empty is exempt. Unknown-length streams must
/// declare JSON even if they later end without data. This uses the body's
/// end-of-stream signal without buffering or consuming a request; missing
/// Content-Length (including chunked requests) is not proof of emptiness.
async fn require_json_api_posts(request: Request, next: Next) -> Response {
    if request.method() == Method::POST && request.uri().path().starts_with("/api/") {
        let is_empty_body = request.body().is_end_stream();
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

/// Token gate state: mandatory mutation authority plus whether non-loopback
/// binds also require it on GET / WS upgrade, and the distinct read-only token
/// accepted on gated reads only.
#[derive(Clone)]
struct TokenGate {
    authority: MutationAuthority,
    read_authority: String,
    require_read_token: bool,
}

/// Host gate state: whether the serve bind is loopback (strict Host) or
/// LAN/tailnet (accept any Host that parses as an IP).
#[derive(Clone)]
struct HostGate {
    bind_is_loopback: bool,
}

/// Require mutation authority on protected POST routes. Gated GET/HEAD reads
/// accept either token in the header, but only read authority in a query.
/// Browser WebSockets obtain that read authority through [`read_token`];
/// rejecting mutation tokens in URLs also protects non-WebSocket reads.
/// Hook routes apply their own authentication at registration instead.
async fn require_mutation_token(
    State(gate): State<TokenGate>,
    request: Request,
    next: Next,
) -> Response {
    let expected = gate.authority.as_str();
    let path = request.uri().path();
    let is_health = path == "/api/health";
    let is_read = request.method() == Method::GET || request.method() == Method::HEAD;
    let needs_auth =
        !is_health && (request.method() == Method::POST || (gate.require_read_token && is_read));
    if needs_auth {
        let read_ok = |presented: &str| is_read && token_matches(presented, &gate.read_authority);
        let header_ok = request
            .headers()
            .get(TOKEN_HEADER)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|presented| token_matches(presented, expected) || read_ok(presented));
        let query_ok = gate.require_read_token
            && is_read
            && request
                .uri()
                .query()
                .map(|q| {
                    q.split('&').any(|pair| {
                        let mut parts = pair.splitn(2, '=');
                        matches!(parts.next(), Some("token"))
                            && parts.next().is_some_and(|v| {
                                let decoded = percent_decode_token(v);
                                read_ok(&decoded)
                            })
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
    next.run(request).await
}

/// Exchange either header credential for read-only authority. This handler
/// always checks its own header, including on otherwise tokenless loopback
/// reads. Query credentials never grant access to the exchange response.
async fn read_token(State(gate): State<TokenGate>, request: Request) -> Response {
    let valid = request
        .headers()
        .get(TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|presented| {
            token_matches(presented, gate.authority.as_str())
                || token_matches(presented, &gate.read_authority)
        });
    if !valid {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "missing or invalid token" })),
        )
            .into_response();
    }
    Json(json!({ "token": gate.read_authority })).into_response()
}

/// Constant-time token equality: off-loopback binds expose the token gate
/// to remote timing probes, and a short-circuiting `==` leaks how many
/// leading bytes matched. Only the length is observable (standard for
/// `ct_eq`, and unavoidable), which reveals nothing useful about a
/// fixed-length uuid-hex token.
fn token_matches(presented: &str, expected: &str) -> bool {
    use subtle::ConstantTimeEq;
    presented.as_bytes().ct_eq(expected.as_bytes()).into()
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
/// `authority` gates every `POST /api/...`.
pub async fn serve(
    repo_root: PathBuf,
    port: u16,
    static_dir: Option<PathBuf>,
    authority: MutationAuthority,
) -> anyhow::Result<()> {
    serve_with_static(
        repo_root,
        port,
        static_dir.map(DashboardStatic::Dir),
        authority,
    )
    .await
}

/// Bind `127.0.0.1:<port>` and serve the router until the process exits.
pub async fn serve_with_static(
    repo_root: PathBuf,
    port: u16,
    static_assets: Option<DashboardStatic>,
    authority: MutationAuthority,
) -> anyhow::Result<()> {
    serve_with_shared_host(
        Arc::new(MissionHost::new(repo_root)),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
        static_assets,
        authority,
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
    authority: MutationAuthority,
) -> anyhow::Result<()> {
    let shutdown = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install ctrl-c handler");
        }
    };
    serve_with_shutdown(host, bind, port, static_assets, authority, shutdown).await
}

/// Same as [`serve_with_shared_host`], but takes an explicit shutdown
/// signal instead of always waiting on Ctrl-C — the testable seam that lets
/// callers (and tests) make the serve future return deterministically.
pub async fn serve_with_shutdown(
    host: Arc<MissionHost>,
    bind: IpAddr,
    port: u16,
    static_assets: Option<DashboardStatic>,
    authority: MutationAuthority,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let listener = bind_listener(bind, port).await?;
    serve_on_listener(host, listener, static_assets, authority, shutdown).await
}

/// Bind `bind:port` and return the listener. Callers that need the REAL
/// bound address before serving — `--port 0` picks an ephemeral port, and
/// the CLI prints/opens the URL — bind first and hand the listener to
/// [`serve_on_listener`].
pub async fn bind_listener(bind: IpAddr, port: u16) -> anyhow::Result<tokio::net::TcpListener> {
    Ok(tokio::net::TcpListener::bind(SocketAddr::from((bind, port))).await?)
}

/// Serve the router on an already-bound listener. The router is built from
/// the listener's REAL local address, so `--port 0` scopes the origin
/// allowlist to the actual ephemeral port and a non-loopback bind gets the
/// read-token gate.
pub async fn serve_on_listener(
    host: Arc<MissionHost>,
    listener: tokio::net::TcpListener,
    static_assets: Option<DashboardStatic>,
    authority: MutationAuthority,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    serve_multi_on_listener(
        Arc::new(MultiRepoHost::with_host(host)),
        listener,
        static_assets,
        authority,
        None,
        false,
        shutdown,
    )
    .await
}

/// Serve a static multi-repository catalog on an already-bound listener.
/// `read_auth` forces the read-token gate (GETs and the WS upgrade) even on
/// a loopback bind — off-loopback binds always require it regardless.
/// `read_authority`, when set, is the read-only token accepted on those
/// gated reads (never on mutations); the mutation `authority` keeps working
/// for reads too.
pub async fn serve_multi_on_listener(
    multi_host: Arc<MultiRepoHost>,
    listener: tokio::net::TcpListener,
    static_assets: Option<DashboardStatic>,
    authority: MutationAuthority,
    read_authority: Option<String>,
    read_auth: bool,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let local_addr = listener.local_addr()?;
    let bind_is_loopback = local_addr.ip().is_loopback();
    let require_read_token = !bind_is_loopback || read_auth;
    let app = router_with_read_authority_and_addr(
        multi_host,
        static_assets,
        authority,
        read_authority,
        Some(local_addr),
        bind_is_loopback,
        require_read_token,
    );
    tracing::info!("kranz server listening on http://{local_addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        host_allowed, origin_allowed, router_with_multi_repo_host_and_addr, EmbeddedFile,
        HostConfig, MultiRepoHost, RepoConfig, RepoSlackConfig,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use kranz_engine::event_log::{EventLog, LockForce};
    use kranz_engine::events::EventKind;
    use kranz_engine::paths::MissionPaths;
    use kranz_engine::types::MissionConfig;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    fn authority() -> super::MutationAuthority {
        super::MutationAuthority::new("tok").unwrap()
    }

    fn seed_planning_mission(root: &Path, goal: &str) {
        std::fs::create_dir_all(root).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(root)
            .status()
            .unwrap();
        assert!(status.success());
        let paths = MissionPaths::new(root, "same-id");
        let mut log = EventLog::acquire(&paths, "same-id", Duration::ZERO, LockForce::No).unwrap();
        log.append(EventKind::MissionCreated {
            goal: goal.to_string(),
            base_branch: "main".to_string(),
            mission_branch: "kranz/mission-same-id".to_string(),
            config: MissionConfig::default(),
        })
        .unwrap();
    }

    fn repo_config(id: &str, root: PathBuf) -> RepoConfig {
        RepoConfig {
            id: id.to_string(),
            root,
            display_name: None,
            group: None,
            pinned: false,
            slack: RepoSlackConfig::default(),
        }
    }

    #[tokio::test]
    async fn unavailable_repo_routes_return_503_with_reason() {
        let temp = tempfile::tempdir().unwrap();
        let good = temp.path().join("good");
        seed_planning_mission(&good, "goal");
        let missing = temp.path().join("missing");

        let multi = Arc::new(
            MultiRepoHost::from_config(HostConfig {
                default_repo: None,
                max_concurrent_repos: 1,
                repos: vec![repo_config("good", good), repo_config("gone", missing)],
            })
            .unwrap(),
        );
        let app = router_with_multi_repo_host_and_addr(multi, None, authority(), None, true, false);

        // Bare prefix and deep path both 503 with the reason (explicit routes
        // beat the `/api/{*path}` catch-all; a nested fallback would not).
        for uri in ["/api/repos/gone", "/api/repos/gone/queue"] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE, "{uri}");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["error"], "repository unavailable", "{uri}");
            assert_eq!(json["repoId"], "gone", "{uri}");
            assert!(json["detail"].as_str().unwrap().contains("does not exist"));
        }

        // An unknown repo id still misses as 404 — unavailable stays
        // distinguishable from a typo.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/repos/nope/queue")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // The healthy sibling is unaffected.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/repos/good/missions/same-id/state")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unavailable_default_repo_reports_503_on_the_unscoped_alias() {
        let temp = tempfile::tempdir().unwrap();
        let good = temp.path().join("good");
        seed_planning_mission(&good, "goal");
        let missing = temp.path().join("missing");

        let multi = Arc::new(
            MultiRepoHost::from_config(HostConfig {
                default_repo: Some("gone".to_string()),
                max_concurrent_repos: 1,
                repos: vec![repo_config("good", good), repo_config("gone", missing)],
            })
            .unwrap(),
        );
        let app = router_with_multi_repo_host_and_addr(multi, None, authority(), None, true, false);

        // Every unscoped route addresses the default repo; its unavailability
        // is reported instead of a generic "scope required" 404.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/queue")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["repoId"], "gone");

        // The health probe and healthy scoped routes stay live.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/repos/good/missions/same-id/state")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn repo_scoped_routes_isolate_duplicate_mission_ids_and_mutations() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        seed_planning_mission(&a, "goal-a");
        seed_planning_mission(&b, "goal-b");

        let multi = Arc::new(
            MultiRepoHost::from_config(HostConfig {
                default_repo: None,
                max_concurrent_repos: 1,
                repos: vec![repo_config("a", a.clone()), repo_config("b", b.clone())],
            })
            .unwrap(),
        );
        static EMBEDDED: &[EmbeddedFile] = &[EmbeddedFile {
            path: "index.html",
            bytes: b"dashboard",
            content_type: "text/html",
        }];
        let app = router_with_multi_repo_host_and_addr(
            multi,
            Some(super::DashboardStatic::Embedded(EMBEDDED)),
            authority(),
            None,
            true,
            false,
        );

        for (repo_id, expected_goal) in [("a", "goal-a"), ("b", "goal-b")] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/repos/{repo_id}/missions/same-id/state"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let state: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(state["mission"]["goal"], expected_goal);
        }

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/repos/a/missions/same-id/control")
                    .header("content-type", "application/json")
                    .header(super::TOKEN_HEADER, "tok")
                    .body(Body::from(r#"{"kind":"pause"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(
            std::fs::read_dir(MissionPaths::new(&a, "same-id").control_dir())
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            std::fs::read_dir(MissionPaths::new(&b, "same-id").control_dir())
                .unwrap()
                .count(),
            0
        );

        // No explicit default and two healthy roots: the legacy mutation path
        // is not mounted and therefore cannot guess a target.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/missions/same-id/control")
                    .header("content-type", "application/json")
                    .header(super::TOKEN_HEADER, "tok")
                    .body(Body::from(r#"{"kind":"pause"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()["content-type"], "application/json");

        let response = app
            .oneshot(Request::builder().uri("/api").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()["content-type"], "application/json");
    }

    #[test]
    fn origin_allowlist_accepts_only_local_dev_and_tauri() {
        // Back-compat / test path (bind None): any port, any loopback host —
        // localhost by name or a loopback IP literal (127/8, [::1]); a page
        // on 127.0.0.10 is the same trust class as one on 127.0.0.1.
        for allowed in [
            "http://localhost",
            "http://localhost:80",
            "http://localhost:5173",
            "http://127.0.0.1",
            "http://127.0.0.1:65535",
            "http://127.0.0.10:8080",
            "http://[::1]:5173",
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
            // Non-loopback IP origins are the WS LAN path's business
            // (ws_origin_allowed), never CORS-approved here.
            "http://192.168.1.5:4560",
            // Not a valid u16 port.
            "http://localhost:99999",
            "http://localhost:5173.evil.example",
            // Only the schemes/hosts the dashboard actually runs under.
            "https://localhost:5173",
            "https://tauri.localhost",
            "tauri://evil.example",
            "null",
            "",
        ] {
            assert!(!origin_allowed(denied, None), "should deny {denied}");
        }
    }

    #[test]
    fn origin_allowlist_scopes_localhost_to_bind_and_dev_ports() {
        // Same-origin (bound ip:port), the vite proxy (:5173), and Tauri dev
        // (:1420) must work; any OTHER localhost port is an unrelated local
        // app whose page must not get cross-origin read approval.
        let bind = Some(std::net::SocketAddr::from(([127, 0, 0, 1], 4560)));
        for allowed in [
            "http://localhost:4560",
            "http://127.0.0.1:4560",
            "http://localhost:5173",
            "http://127.0.0.1:5173",
            "http://localhost:1420",
            "tauri://localhost",
            "http://tauri.localhost",
        ] {
            assert!(
                origin_allowed(allowed, bind),
                "should allow {allowed} for bind 127.0.0.1:4560"
            );
        }
        for denied in [
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://localhost", // implied :80 != 4560
            "http://127.0.0.1",
            // Co-resident loopback listener on kranz's OWN port: a different
            // loopback IP is a different process (unprivileged bind on
            // Linux); its page must not get tokenless cross-origin reads.
            "http://127.0.0.2:4560",
            "http://127.0.0.10:4560",
            "http://127.0.0.2:5173",
            "http://127.0.0.10:1420",
            "http://[::1]:4560",
            "http://localhost.evil.example:4560",
            "https://localhost:4560",
            "https://evil.example",
        ] {
            assert!(
                !origin_allowed(denied, bind),
                "should deny {denied} for bind 127.0.0.1:4560"
            );
        }
        // A serve actually bound on :80 keeps its own portless same-origin.
        assert!(origin_allowed(
            "http://localhost",
            Some(std::net::SocketAddr::from(([127, 0, 0, 1], 80)))
        ));
    }

    #[test]
    fn origin_allowlist_follows_the_actual_bound_ip() {
        // `--host 127.0.0.2`: its own page works, the canonical-localhost
        // forms (which that serve does NOT answer on) do not.
        let bind = Some(std::net::SocketAddr::from(([127, 0, 0, 2], 4560)));
        assert!(origin_allowed("http://127.0.0.2:4560", bind));
        assert!(!origin_allowed("http://127.0.0.1:4560", bind));
        assert!(!origin_allowed("http://localhost:4560", bind));
        // Dev-server pages stay approved regardless of bind IP.
        assert!(origin_allowed("http://localhost:5173", bind));

        // `--host ::1`: bracketed v6 same-origin plus the localhost name.
        let bind_v6 = Some(std::net::SocketAddr::from((
            std::net::Ipv6Addr::LOCALHOST,
            4560,
        )));
        assert!(origin_allowed("http://[::1]:4560", bind_v6));
        assert!(origin_allowed("http://localhost:4560", bind_v6));
        assert!(!origin_allowed("http://127.0.0.2:4560", bind_v6));

        // `--host 0.0.0.0` listens on every interface: any loopback page on
        // the bind port is genuinely this server.
        let bind_any = Some(std::net::SocketAddr::from(([0, 0, 0, 0], 4560)));
        assert!(origin_allowed("http://127.0.0.1:4560", bind_any));
        assert!(origin_allowed("http://127.0.0.5:4560", bind_any));
        assert!(origin_allowed("http://localhost:4560", bind_any));
        assert!(!origin_allowed("http://localhost:8080", bind_any));
    }

    #[test]
    fn ws_origin_loopback_keeps_strict_browser_allowlist() {
        use super::ws_origin_allowed;
        let bind = Some(std::net::SocketAddr::from(([127, 0, 0, 1], 4560)));
        assert!(ws_origin_allowed(Some("http://localhost:4560"), bind, true));
        assert!(ws_origin_allowed(Some("http://localhost:5173"), bind, true));
        assert!(
            !ws_origin_allowed(None, bind, true),
            "missing Origin stays rejected on loopback (reads are tokenless)"
        );
        assert!(!ws_origin_allowed(
            Some("http://192.168.1.5:4560"),
            bind,
            true
        ));
        assert!(
            !ws_origin_allowed(Some("http://127.0.0.2:4560"), bind, true),
            "co-resident loopback listener page must not open the tokenless WS"
        );
        assert!(
            !ws_origin_allowed(Some("http://127.0.0.2:5173"), bind, true),
            "a dev port must not privilege another independently bindable loopback IP"
        );
        assert!(!ws_origin_allowed(Some("http://evil.example"), bind, true));
    }

    #[test]
    fn ws_origin_lan_accepts_ip_literals_and_native_clients() {
        use super::ws_origin_allowed;
        let bind = Some(std::net::SocketAddr::from(([0, 0, 0, 0], 4560)));
        // Same-origin LAN dashboard, bracketed v6, dev proxy, and header-less
        // native clients all pass — the read token authenticates the upgrade.
        assert!(ws_origin_allowed(
            Some("http://192.168.1.5:4560"),
            bind,
            false
        ));
        assert!(ws_origin_allowed(
            Some("http://[fd00::5]:4560"),
            bind,
            false
        ));
        assert!(ws_origin_allowed(
            Some("http://localhost:5173"),
            bind,
            false
        ));
        assert!(ws_origin_allowed(None, bind, false));
        // DNS-named (rebinding) pages and non-http schemes stay out.
        for denied in [
            "http://evil.example:4560",
            "http://192.168.1.5.evil.example:4560",
            "https://192.168.1.5:4560",
            "http://[::1:4560",
            "null",
            "",
        ] {
            assert!(
                !ws_origin_allowed(Some(denied), bind, false),
                "should deny {denied} off loopback"
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
            // Any loopback literal serves: `--host 127.0.0.2` must answer.
            "127.0.0.2:4560",
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
            // A full (non-loopback) IPv6 address, NOT ::1 with a port.
            "::1:4560",
            // Unclosed bracket.
            "[::1",
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
