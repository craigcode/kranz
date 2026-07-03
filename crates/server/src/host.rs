//! Hosted-engine registry (docs/protocol.md "Mission lifecycle
//! (server-hosted engine; M2.5)").
//!
//! `kranz serve` can HOST missions: for missions created via
//! `POST /api/missions` this server process IS the single-writer engine — it
//! holds the mission lock, so a concurrent `kranz run` correctly refuses, and
//! either side can resume what the other started (the event log is the source
//! of truth).
//!
//! Concurrency model:
//! - Each planning-phase mission sits behind an `Arc<tokio::sync::Mutex<..>>`
//!   so planning turns serialize per mission; handlers `try_lock` and a
//!   contended lock is a 409 ("a turn is in flight"), never a queue.
//! - `start` consumes the engine out of the registry (`Arc::try_unwrap`
//!   succeeds only when no turn holds a clone) and spawns `engine.run()` as a
//!   background task. When the run ends — Complete, Blocked or Failed — the
//!   task drops the engine (flushing the log and releasing the single-writer
//!   lock) and removes its registry entry, so the mission is observable and
//!   resumable from anywhere.
//! - `start` on a mission NOT in the registry (blocked earlier, or the server
//!   restarted) resumes it from the event log — the re-invocable semantics of
//!   the protocol.
//!
//! The agent backend is constructed lazily on first use, so a read-only
//! `kranz serve` never needs a `claude` binary installed.

use crate::error::ApiError;
use crate::ServerState;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use kranz_engine::backend::AgentBackend;
use kranz_engine::backend_claude::ClaudeBackend;
use kranz_engine::config;
use kranz_engine::cost::{self, CostEstimate};
use kranz_engine::error::EngineError;
use kranz_engine::orchestrator::{MissionEngine, PlanRequest};
use kranz_engine::paths::MissionPaths;
use kranz_engine::types::{MissionStatus, Plan};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The engine cell of a planning-phase mission: turns lock it, `start`
/// consumes it.
type EngineCell = Arc<tokio::sync::Mutex<Box<MissionEngine>>>;

/// One mission hosted by this server process.
enum HostedMission {
    /// In planning (or approved, awaiting start): the live engine, holding
    /// the mission lock and the orchestrator conversation.
    Planning(EngineCell),
    /// `engine.run()` owns the engine inside this background task; the task
    /// removes this entry when the run ends.
    Running(tokio::task::JoinHandle<()>),
}

/// Registry of missions this server process hosts (see module docs).
pub struct MissionHost {
    repo_root: PathBuf,
    /// Lazy real backend — discovered on first mutating use, so read-only
    /// serving works without a `claude` binary. Tests inject a mock via
    /// [`MissionHost::with_backend`].
    backend: tokio::sync::OnceCell<Arc<dyn AgentBackend>>,
    /// Shared with each run task so it can remove its own entry on exit.
    missions: Arc<Mutex<HashMap<String, HostedMission>>>,
}

impl MissionHost {
    /// Host for `repo_root`, discovering the Claude backend on first use.
    pub fn new(repo_root: PathBuf) -> Self {
        MissionHost {
            repo_root,
            backend: tokio::sync::OnceCell::new(),
            missions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Host with an injected backend (tests drive the full lifecycle through
    /// `kranz_engine::backend_mock` without a `claude` binary).
    pub fn with_backend(repo_root: PathBuf, backend: Arc<dyn AgentBackend>) -> Self {
        MissionHost {
            repo_root,
            backend: tokio::sync::OnceCell::new_with(Some(backend)),
            missions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The repository this host creates missions in.
    pub fn repo_root(&self) -> &PathBuf {
        &self.repo_root
    }

    /// The backend, constructing [`ClaudeBackend`] on first use.
    async fn backend(&self, claude_binary: Option<&str>) -> Result<Arc<dyn AgentBackend>, ApiError> {
        let configured = claude_binary.map(str::to_string);
        self.backend
            .get_or_try_init(|| async move {
                let backend = ClaudeBackend::discover(configured.as_deref())?;
                Ok::<Arc<dyn AgentBackend>, EngineError>(Arc::new(backend))
            })
            .await
            .map(Arc::clone)
            .map_err(ApiError::from)
    }

    // -----------------------------------------------------------------------
    // Lifecycle operations (one per endpoint)
    // -----------------------------------------------------------------------

    /// `POST /api/missions`: layered config + optional request patch →
    /// validate → create the mission → hold its engine in the registry.
    pub(crate) async fn create(
        &self,
        goal: &str,
        config_patch: Option<&Value>,
    ) -> Result<String, ApiError> {
        let mut cfg = config::load(&self.repo_root)?;
        if let Some(patch) = config_patch {
            if !patch.is_object() {
                return Err(ApiError::bad_request("'config' must be a JSON object"));
            }
            let mut merged = serde_json::to_value(&cfg)
                .map_err(|e| ApiError::internal(format!("config does not serialize: {e}")))?;
            config::deep_merge(&mut merged, patch);
            cfg = serde_json::from_value(merged).map_err(|e| {
                ApiError::bad_request(format!("'config' patch does not deserialize: {e}"))
            })?;
        }
        config::validate(&cfg)?;

        let backend = self.backend(cfg.claude_binary.as_deref()).await?;
        let engine = MissionEngine::create(backend, self.repo_root.clone(), goal, cfg)?;
        let id = engine.mission_id().to_string();
        self.missions
            .lock()
            .expect("missions registry lock")
            .insert(id.clone(), HostedMission::Planning(new_cell(Box::new(engine))));
        Ok(id)
    }

    /// `POST /api/missions/:id/planning/turn`: one conversational turn. A
    /// captured seed reply (fresh session / re-seed) is prepended — it
    /// happened first in the conversation.
    pub(crate) async fn planning_turn(&self, id: &str, text: &str) -> Result<String, ApiError> {
        let cell = self.planning_cell(id)?;
        let mut engine = try_lock(&cell)?;
        let reply = engine.planning_turn(text).await?;
        Ok(prepend_seed(engine.take_seed_reply(), reply))
    }

    /// `POST /api/missions/:id/planning/request-plan`: demand the plan.
    /// Ready → plan + cost estimate; NotReady → the orchestrator's prose
    /// (back to the conversation).
    pub(crate) async fn request_plan(&self, id: &str) -> Result<Value, ApiError> {
        let cell = self.planning_cell(id)?;
        let mut engine = try_lock(&cell)?;
        let request = engine.request_plan().await?;
        let seed = engine.take_seed_reply();
        match request {
            PlanRequest::Ready(plan) => {
                let estimate =
                    cost::estimate(&plan, &engine.state().config, &cost::EstimateParams::default());
                Ok(json!({ "ready": true, "plan": plan, "estimate": estimate_json(&estimate) }))
            }
            PlanRequest::NotReady(reply) => {
                Ok(json!({ "ready": false, "reply": prepend_seed(seed, reply) }))
            }
        }
    }

    /// `POST /api/missions/:id/approve`: commit plan.json/plan.md/index.md on
    /// the mission branch exactly like the CLI. Returns the mission branch.
    pub(crate) async fn approve(&self, id: &str, plan: Plan) -> Result<String, ApiError> {
        let cell = self.planning_cell(id)?;
        let mut engine = try_lock(&cell)?;
        engine.approve_plan(plan)?;
        Ok(engine.state().mission.mission_branch.clone())
    }

    /// `POST /api/missions/:id/start`: consume the hosted engine into a
    /// background `engine.run()` task — or, for a mission not in the registry
    /// (blocked earlier, server restarted, or CLI-created), resume it from
    /// the event log and run that.
    pub(crate) async fn start(&self, id: &str) -> Result<(), ApiError> {
        // Try to consume a hosted planning-phase engine.
        let taken: Option<Box<MissionEngine>> = {
            let mut map = self.missions.lock().expect("missions registry lock");
            match map.remove(id) {
                None => None,
                Some(HostedMission::Running(handle)) => {
                    if handle.is_finished() {
                        // The task ended but its cleanup lost the race with
                        // this request: treat as not hosted (resume below).
                        None
                    } else {
                        map.insert(id.to_string(), HostedMission::Running(handle));
                        return Err(ApiError::conflict(format!(
                            "mission '{id}' is already running — observe it via GET \
                             /api/missions/{id}/state or steer it via POST \
                             /api/missions/{id}/control"
                        )));
                    }
                }
                Some(HostedMission::Planning(cell)) => match Arc::try_unwrap(cell) {
                    Err(cell) => {
                        // A handler holds a clone: a turn is (or is about to
                        // be) in flight. Put the entry back untouched.
                        map.insert(id.to_string(), HostedMission::Planning(cell));
                        return Err(turn_in_flight());
                    }
                    Ok(mutex) => {
                        let engine = mutex.into_inner();
                        if engine.state().mission.status == MissionStatus::Planning {
                            map.insert(id.to_string(), HostedMission::Planning(new_cell(engine)));
                            return Err(ApiError::conflict(format!(
                                "mission '{id}' has no approved plan yet — approve one via \
                                 POST /api/missions/{id}/approve first"
                            )));
                        }
                        Some(engine)
                    }
                },
            }
        };

        let engine = match taken {
            Some(engine) => engine,
            None => {
                // Re-invocable path: resume from the log. A live engine
                // elsewhere (CLI, or a hosted run racing this request) holds
                // the single-writer lock → EngineError::LockHeld → 409.
                if !MissionPaths::new(&self.repo_root, id).events_file().is_file() {
                    return Err(ApiError::not_found(format!("unknown mission '{id}'")));
                }
                let cfg = config::load(&self.repo_root)?;
                let backend = self.backend(cfg.claude_binary.as_deref()).await?;
                let engine =
                    Box::new(MissionEngine::resume(backend, self.repo_root.clone(), id, false)?);
                match engine.state().mission.status {
                    MissionStatus::Planning => {
                        return Err(ApiError::conflict(format!(
                            "mission '{id}' is still in planning — approve a plan first \
                             (POST /api/missions/{id}/approve, or `kranz plan`)"
                        )))
                    }
                    MissionStatus::Complete => {
                        return Err(ApiError::conflict(format!(
                            "mission '{id}' is already complete — nothing to run"
                        )))
                    }
                    MissionStatus::Failed => {
                        return Err(ApiError::conflict(format!(
                            "mission '{id}' has failed — inspect its log; there is nothing \
                             the engine can resume"
                        )))
                    }
                    _ => {}
                }
                engine
            }
        };

        // Insert the Running entry while holding the map lock across the
        // spawn: if the run ends instantly, its cleanup blocks on this lock
        // until the entry exists, so it can never leave a stale entry behind.
        {
            let mut map = self.missions.lock().expect("missions registry lock");
            let missions = Arc::clone(&self.missions);
            let mission_id = id.to_string();
            let handle = tokio::spawn(run_to_end(engine, mission_id, missions));
            map.insert(id.to_string(), HostedMission::Running(handle));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Registry plumbing
    // -----------------------------------------------------------------------

    /// The engine cell of a planning-phase hosted mission, with helpful 409s
    /// for every other state.
    fn planning_cell(&self, id: &str) -> Result<EngineCell, ApiError> {
        let map = self.missions.lock().expect("missions registry lock");
        match map.get(id) {
            Some(HostedMission::Planning(cell)) => Ok(Arc::clone(cell)),
            Some(HostedMission::Running(_)) => Err(ApiError::conflict(format!(
                "mission '{id}' is running — steer it via POST /api/missions/{id}/control"
            ))),
            None => Err(self.not_hosted(id)),
        }
    }

    /// A mission that exists on disk but has no engine in this registry:
    /// its engine lives elsewhere (CLI) or was released (run ended, server
    /// restarted). Unknown missions are a plain 404.
    fn not_hosted(&self, id: &str) -> ApiError {
        let paths = MissionPaths::new(&self.repo_root, id);
        if paths.events_file().is_file() {
            ApiError::conflict(format!(
                "mission '{id}' is not hosted by this server — resume planning with \
                 `kranz plan --mission {id}`, or start execution via POST \
                 /api/missions/{id}/start"
            ))
        } else {
            ApiError::not_found(format!("unknown mission '{id}'"))
        }
    }
}

/// Drive one hosted mission to a terminal state, then release everything:
/// drop the engine FIRST (flushes the log, releases the single-writer lock),
/// THEN remove the registry entry — from that moment the mission is
/// observable and resumable anywhere (server or CLI).
async fn run_to_end(
    mut engine: Box<MissionEngine>,
    mission_id: String,
    missions: Arc<Mutex<HashMap<String, HostedMission>>>,
) {
    let result = engine.run().await;
    match &result {
        Ok(status) => {
            tracing::info!(mission = %mission_id, status = ?status, "hosted mission run ended")
        }
        Err(e) => {
            tracing::error!(mission = %mission_id, error = %e, "hosted mission run errored")
        }
    }
    drop(engine);
    missions.lock().expect("missions registry lock").remove(&mission_id);
}

fn new_cell(engine: Box<MissionEngine>) -> EngineCell {
    Arc::new(tokio::sync::Mutex::new(engine))
}

/// Planning endpoints never queue behind each other: contended = 409.
fn try_lock(cell: &EngineCell) -> Result<tokio::sync::MutexGuard<'_, Box<MissionEngine>>, ApiError> {
    cell.try_lock().map_err(|_| turn_in_flight())
}

fn turn_in_flight() -> ApiError {
    ApiError::conflict("a turn is in flight for this mission — wait for it to finish")
}

/// Seed replies (fresh session / resume-ack / re-seed) happened first in the
/// conversation, so they go first in the combined reply.
fn prepend_seed(seed: Option<String>, reply: String) -> String {
    match seed {
        Some(seed) => format!("{seed}\n\n{reply}"),
        None => reply,
    }
}

/// [`CostEstimate`] as protocol camelCase JSON (the engine type is a plain
/// contract struct without serde derives).
fn estimate_json(estimate: &CostEstimate) -> Value {
    json!({
        "workerRuns": estimate.worker_runs,
        "validatorRuns": estimate.validator_runs,
        "lowUsd": estimate.low_usd,
        "expectedUsd": estimate.expected_usd,
        "highUsd": estimate.high_usd,
    })
}

// ---------------------------------------------------------------------------
// Axum handlers (docs/protocol.md "Mission lifecycle" table)
// ---------------------------------------------------------------------------

/// `POST /api/missions` — body `{"goal":"...", "config":{...}}` →
/// `201 {"id":"m-…"}`.
pub(crate) async fn create_mission(
    State(server): State<Arc<ServerState>>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let value = parse_body(&body)?;
    let goal = value
        .get("goal")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|goal| !goal.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request(r#"body must be {"goal":"..."} with a non-empty goal"#)
        })?;
    let id = server.host.create(goal, value.get("config")).await?;
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))))
}

/// `POST /api/missions/:id/planning/turn` — body `{"text":"..."}` →
/// `200 {"reply":"..."}`.
pub(crate) async fn planning_turn(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let id = valid_id(&server, &id)?;
    let value = parse_body(&body)?;
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            ApiError::bad_request(r#"body must be {"text":"..."} with non-empty text"#)
        })?;
    let reply = server.host.planning_turn(&id, text).await?;
    Ok(Json(json!({ "reply": reply })))
}

/// `POST /api/missions/:id/planning/request-plan` →
/// `200 {"ready":true,"plan":{...},"estimate":{...}}` or
/// `200 {"ready":false,"reply":"..."}`.
pub(crate) async fn request_plan(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let id = valid_id(&server, &id)?;
    Ok(Json(server.host.request_plan(&id).await?))
}

/// `POST /api/missions/:id/approve` — body `{"plan":{...}}` →
/// `200 {"branch":"kranz/mission-…"}`.
pub(crate) async fn approve_mission(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let id = valid_id(&server, &id)?;
    let value = parse_body(&body)?;
    let plan = value
        .get("plan")
        .cloned()
        .ok_or_else(|| ApiError::bad_request(r#"body must be {"plan":{...}}"#))?;
    let plan: Plan = serde_json::from_value(plan)
        .map_err(|e| ApiError::bad_request(format!("'plan' is not a valid Plan: {e}")))?;
    let branch = server.host.approve(&id, plan).await?;
    Ok(Json(json!({ "branch": branch })))
}

/// `POST /api/missions/:id/start` → `202 {"running":true}`.
pub(crate) async fn start_mission(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id = valid_id(&server, &id)?;
    server.host.start(&id).await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "running": true }))))
}

/// Validate the URL id with the same traversal rules as the read endpoints.
fn valid_id(server: &ServerState, id: &str) -> Result<String, ApiError> {
    crate::rest::mission_paths(server, id)?;
    Ok(id.to_string())
}

fn parse_body(body: &Bytes) -> Result<Value, ApiError> {
    if body.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))
}

// ---------------------------------------------------------------------------
// Unit tests: try_lock contention (deterministic — the HTTP-level race of
// two concurrent in-flight turns is covered here instead, by holding the
// per-mission mutex exactly like an in-flight turn does)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use kranz_engine::backend_mock::MockBackend;
    use std::process::Command;
    use std::sync::Once;

    static ENV_ISOLATION: Once = Once::new();

    /// Mask the host's global/system git config (same discipline as the
    /// engine's mission tests) AND the home directory: `MissionHost::create`
    /// goes through `config::load`, which would otherwise read the
    /// developer's real ~/.kranz/config.json.
    fn isolate_git_env() {
        ENV_ISOLATION.call_once(|| {
            let missing = std::env::temp_dir()
                .join(format!("kranz-host-test-no-config-{}", std::process::id()));
            std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
            std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
            if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
                std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
            }
            let home = std::env::temp_dir()
                .join(format!("kranz-host-test-home-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&home);
            std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, &home);
        });
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = Command::new("git").args(args).current_dir(dir).output().expect("spawn git");
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Throwaway repo with one commit; `None` (skip) when git is missing.
    fn init_repo() -> Option<(tempfile::TempDir, PathBuf)> {
        isolate_git_env();
        let git_works = Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !git_works {
            eprintln!("skipping test: git is not on PATH");
            return None;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let init = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git init");
        if !init.status.success() {
            git(dir.path(), &["init"]);
            git(dir.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);
        }
        git(dir.path(), &["config", "user.name", "test"]);
        git(dir.path(), &["config", "user.email", "test@example.com"]);
        std::fs::write(dir.path().join("README.md"), "seed\n").unwrap();
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-m", "seed"]);
        let root = std::fs::canonicalize(dir.path()).expect("canonicalize");
        Some((dir, root))
    }

    #[tokio::test]
    async fn contended_planning_mutex_is_409_for_turns_and_start() {
        let Some((_dir, root)) = init_repo() else { return };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);
        let id = host.create("ship it", None).await.expect("create mission");

        // Hold the per-mission engine mutex exactly like an in-flight turn.
        let cell = host.planning_cell(&id).expect("hosted planning cell");
        let _guard = cell.try_lock().expect("uncontended lock");

        let err = host.planning_turn(&id, "hello").await.expect_err("turn must 409");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert!(err.message.contains("turn is in flight"), "{}", err.message);

        let err = host.request_plan(&id).await.expect_err("request-plan must 409");
        assert_eq!(err.status, StatusCode::CONFLICT);

        // `start` also refuses while a turn holds the engine (the Arc clone
        // keeps try_unwrap failing) — and the entry survives the attempt.
        let err = host.start(&id).await.expect_err("start must 409");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert!(host.planning_cell(&id).is_ok(), "registry entry must survive");
    }

    #[tokio::test]
    async fn start_without_an_approved_plan_is_409() {
        let Some((_dir, root)) = init_repo() else { return };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);
        let id = host.create("ship it", None).await.expect("create mission");

        let err = host.start(&id).await.expect_err("start must 409 in planning");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert!(err.message.contains("no approved plan"), "{}", err.message);
        // The engine went back into the registry: planning can continue.
        assert!(host.planning_cell(&id).is_ok());
    }

    #[tokio::test]
    async fn create_rejects_an_invalid_config_patch() {
        let Some((_dir, root)) = init_repo() else { return };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);

        let patch = json!({ "maxParallelWorkers": 4 });
        let err = host.create("ship it", Some(&patch)).await.expect_err("must reject");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }
}
