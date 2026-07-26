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
use kranz_engine::backend::{AgentBackend, AgentEvent, PromptMode, SessionExit, SessionSpec};
use kranz_engine::backend_claude::ClaudeBackend;
use kranz_engine::config;
use kranz_engine::cost::{self, CostEstimate};
use kranz_engine::deps;
use kranz_engine::draft::{drive_draft, DraftOutcome};
use kranz_engine::error::EngineError;
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::git_ops::GitRepo;
use kranz_engine::git_ops::KranzCommitMetadata;
use kranz_engine::merge::{merge_mission, MergeReport};
use kranz_engine::orchestrator::{MissionEngine, PlanRequest};
use kranz_engine::paths::MissionPaths;
use kranz_engine::queue;
use kranz_engine::ticket::Ticket;
use kranz_engine::types::{MissionConfig, MissionStatus, Plan, TokenUsage};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// The engine cell of a planning-phase mission: turns lock it, `start`
/// consumes it.
type EngineCell = Arc<tokio::sync::Mutex<Box<MissionEngine>>>;

/// One mission hosted by this server process.
enum HostedMission {
    /// In planning (or approved, awaiting start): the live engine, holding
    /// the mission lock and the orchestrator conversation, plus when it was
    /// last touched by a planning turn (for the idle sweeper).
    Planning {
        cell: EngineCell,
        last_use: Arc<Mutex<Instant>>,
        /// The last plan `request_plan` returned Ready, awaiting approval —
        /// ONE cache for every surface's approve affordance (Slack buttons,
        /// web, glasses ring). Consumed by [`MissionHost::approve_pending`];
        /// volatile by design (a restart forfeits it — re-request the plan).
        pending_plan: Arc<Mutex<Option<Plan>>>,
    },
    /// `engine.run()` owns the engine inside this background task; the task
    /// removes this entry when the run ends. `_repo_busy` holds the
    /// repo-wide busy lock for the lifetime of the hosted run so a sibling
    /// queue drain / `kranz work` cannot claim the same repo.
    Running {
        handle: tokio::task::JoinHandle<()>,
        _repo_busy: kranz_engine::queue::RepoBusyHold,
    },
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
    /// The lazily-spawned idle-release background task, started at most once
    /// (see [`MissionHost::ensure_sweeper_started`]).
    sweeper: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The single tracked background queue drain slot (see
    /// [`MissionHost::drain`]).
    drain: Mutex<DrainSlot>,
    /// Shared by every repository in a [`crate::MultiRepoHost`]. A permit is
    /// held for the complete background mission/drain lifetime, so the
    /// operator's `host.maxConcurrentRepos` is a real spend/load bound.
    global_run_permits: Option<Arc<Semaphore>>,
    /// The gate-suite executor [`MissionHost::merge`] runs under
    /// `spawn_blocking`; real shell commands by default, a scripted stub in
    /// tests (see [`MissionHost::with_gate_executor`]).
    gate_executor: GateExecutor,
    /// Short-TTL cache for the queue-front readiness probe so a 3s dashboard
    /// poll does not re-shell every backend CLI on every GET /api/queue.
    readiness_front_cache: Mutex<Option<FrontReadinessCache>>,
}

/// Cached `GET /api/queue` readiness for the current queue front only.
struct FrontReadinessCache {
    mission_id: String,
    report: Value,
    at: Instant,
}

const READINESS_FRONT_CACHE_TTL: Duration = Duration::from_secs(5);

/// One background drain task's observable progress — shared between the task
/// (which updates it as it goes) and [`MissionHost::drain`] /
/// [`MissionHost::queue_state`] (which read it back as JSON).
#[derive(Debug, Clone, Default)]
struct DrainState {
    live: bool,
    current_mission_id: Option<String>,
    ran: Vec<String>,
    parked: Vec<String>,
}

/// One gate-suite command execution: `executor(command, cwd)` →
/// `(success, combined_stdout_stderr)`. Boxed so [`MissionHost`] can hold a
/// real shell-backed default and tests can inject a scripted stub — the same
/// seam shape as [`MissionHost::with_backend`] for the agent backend.
type GateExecutor = Arc<dyn Fn(&str, &Path) -> (bool, String) + Send + Sync>;

/// The real gate executor delegates to the engine's 600-second process-tree
/// bounded shell runner with a sanitized environment. It runs only from
/// inside `tokio::task::spawn_blocking` (see [`MissionHost::merge`]).
fn real_gate_executor() -> GateExecutor {
    Arc::new(|command, cwd| kranz_engine::command_exec::run_bounded_gate_command(cwd, command))
}

/// The autoWork watcher's decision function, factored out so it's testable
/// without standing up a full mission: drain only when autoWork is enabled,
/// the queue has something waiting, and no drain is already live.
fn should_auto_drain(auto_work: bool, queue_non_empty: bool, drain_live: bool) -> bool {
    auto_work && queue_non_empty && !drain_live
}

fn drain_state_json(state: &DrainState) -> Value {
    json!({
        "live": state.live,
        "currentMissionId": state.current_mission_id,
        "ran": state.ran,
        "parked": state.parked,
    })
}

/// A tracked background drain: the task handle plus the state it shares with
/// this host. `join.is_finished()` is how [`MissionHost::drain`] decides
/// whether a tracked drain is still live.
struct DrainHandle {
    join: tokio::task::JoinHandle<()>,
    state: Arc<Mutex<DrainState>>,
}

/// The drain tracker's state machine. `Starting` is a reservation held while
/// `config::load` + `self.backend(...)` run with NO lock held (both can
/// `.await`); it closes the race where two concurrent [`MissionHost::drain`]
/// calls both observe "nothing tracked yet" and both spawn a task. A racing
/// caller that sees `Starting` returns its shared [`DrainState`] instead of
/// starting a second drain; the caller that installed the reservation later
/// upgrades it to `Running` (same `Arc<Mutex<DrainState>>`), or clears it back
/// to `Idle` on failure so a later call can retry.
enum DrainSlot {
    Idle,
    Starting(Arc<Mutex<DrainState>>),
    Running(DrainHandle),
}

impl MissionHost {
    /// Host for `repo_root`, discovering the Claude backend on first use.
    pub fn new(repo_root: PathBuf) -> Self {
        MissionHost {
            repo_root,
            backend: tokio::sync::OnceCell::new(),
            missions: Arc::new(Mutex::new(HashMap::new())),
            sweeper: Mutex::new(None),
            drain: Mutex::new(DrainSlot::Idle),
            global_run_permits: None,
            gate_executor: real_gate_executor(),
            readiness_front_cache: Mutex::new(None),
        }
    }

    /// Host with an injected backend (tests drive the full lifecycle through
    /// `kranz_engine::backend_mock` without a `claude` binary).
    pub fn with_backend(repo_root: PathBuf, backend: Arc<dyn AgentBackend>) -> Self {
        MissionHost {
            repo_root,
            backend: tokio::sync::OnceCell::new_with(Some(backend)),
            missions: Arc::new(Mutex::new(HashMap::new())),
            sweeper: Mutex::new(None),
            drain: Mutex::new(DrainSlot::Idle),
            global_run_permits: None,
            gate_executor: real_gate_executor(),
            readiness_front_cache: Mutex::new(None),
        }
    }

    /// Host with an injected gate-suite executor (tests script CI gate
    /// outcomes for [`MissionHost::merge`] hermetically, without ever
    /// shelling out to `cargo`/`npm`). Mirrors [`MissionHost::with_backend`]'s
    /// seam, for the gate suite instead of the agent backend.
    pub fn with_gate_executor<F>(repo_root: PathBuf, gate_executor: F) -> Self
    where
        F: Fn(&str, &Path) -> (bool, String) + Send + Sync + 'static,
    {
        MissionHost {
            repo_root,
            backend: tokio::sync::OnceCell::new(),
            missions: Arc::new(Mutex::new(HashMap::new())),
            sweeper: Mutex::new(None),
            drain: Mutex::new(DrainSlot::Idle),
            global_run_permits: None,
            gate_executor: Arc::new(gate_executor),
            readiness_front_cache: Mutex::new(None),
        }
    }

    /// Host participating in a process-wide multi-repository execution cap.
    pub(crate) fn new_with_global_run_permits(
        repo_root: PathBuf,
        global_run_permits: Arc<Semaphore>,
    ) -> Self {
        MissionHost {
            repo_root,
            backend: tokio::sync::OnceCell::new(),
            missions: Arc::new(Mutex::new(HashMap::new())),
            sweeper: Mutex::new(None),
            drain: Mutex::new(DrainSlot::Idle),
            global_run_permits: Some(global_run_permits),
            gate_executor: real_gate_executor(),
            readiness_front_cache: Mutex::new(None),
        }
    }

    /// Test helper: inject a backend into a host that already shares the
    /// multi-repository run semaphore.
    #[cfg(test)]
    pub(crate) fn with_backend_and_global_run_permits(
        repo_root: PathBuf,
        backend: Arc<dyn AgentBackend>,
        global_run_permits: Arc<Semaphore>,
    ) -> Self {
        MissionHost {
            repo_root,
            backend: tokio::sync::OnceCell::new_with(Some(backend)),
            missions: Arc::new(Mutex::new(HashMap::new())),
            sweeper: Mutex::new(None),
            drain: Mutex::new(DrainSlot::Idle),
            global_run_permits: Some(global_run_permits),
            gate_executor: real_gate_executor(),
            readiness_front_cache: Mutex::new(None),
        }
    }

    /// The repository this host creates missions in.
    pub fn repo_root(&self) -> &PathBuf {
        &self.repo_root
    }

    pub(crate) fn try_global_run_permit(&self) -> Result<Option<OwnedSemaphorePermit>, ApiError> {
        self.global_run_permits
            .as_ref()
            .map(|permits| {
                Arc::clone(permits).try_acquire_owned().map_err(|_| {
                    ApiError::conflict(
                        "host.maxConcurrentRepos is saturated; retry when another repository finishes",
                    )
                })
            })
            .transpose()
    }

    /// The backend, constructing [`ClaudeBackend`] on first use.
    async fn backend(
        &self,
        claude_binary: Option<&str>,
    ) -> Result<Arc<dyn AgentBackend>, ApiError> {
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
    /// Public: the Slack bridge drives the same lifecycle through this host
    /// (wired by `kranz serve --slack`), so these five operations are the
    /// shared client surface, not axum-private plumbing.
    pub async fn create(
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
            .insert(id.clone(), new_planning(new_cell(Box::new(engine))));
        self.ensure_sweeper_started();
        Ok(id)
    }

    /// Entry point any surface (REST, Slack, CLI-over-HTTP) can call to draft
    /// a backlog ticket non-interactively: validate the slug, load the ticket,
    /// create its planning mission through this host — so the create path
    /// registers it in `missions` and its lifecycle events stream over
    /// `GET /api/missions/:id/ws` exactly like `POST /api/missions` — then run
    /// [`drive_draft`] (roadmap f-1-1) to completion against that hosted
    /// engine. The engine is dropped and the registry entry removed once the
    /// draft turn ends (mirroring [`run_to_end`]'s drop-then-remove ordering)
    /// so the mission stays observable/resumable afterward; no operator
    /// checkout restoration happens here — a headless server has no checkout
    /// to restore.
    pub async fn draft(&self, slug: &str, then_enqueue: bool) -> Result<DraftOutcome, ApiError> {
        Ticket::ensure_valid_slug(slug)?;
        let ticket_path = Ticket::tickets_dir(&self.repo_root).join(format!("{slug}.md"));
        if !ticket_path.is_file() {
            return Err(ApiError::not_found(format!("ticket '{slug}' not found")));
        }
        let ticket = Ticket::load(&ticket_path)?;

        let cfg = config_for_ticket(config::load(&self.repo_root)?, &ticket);
        let backend = self.backend(cfg.claude_binary.as_deref()).await?;
        let engine =
            MissionEngine::create(backend, self.repo_root.clone(), &ticket.mission_goal(), cfg)?;
        let id = engine.mission_id().to_string();
        let cell = new_cell(Box::new(engine));
        self.missions
            .lock()
            .expect("missions registry lock")
            .insert(id.clone(), new_planning(Arc::clone(&cell)));
        self.ensure_sweeper_started();

        let drive_result = {
            let mut engine = cell.lock().await;
            drive_draft(&mut engine, &self.repo_root, &ticket, then_enqueue).await
        };

        // Drop the engine (flushes the log, frees the single-writer lock),
        // then remove the registry entry — from that moment the mission is
        // observable and resumable anywhere, same as `run_to_end`.
        self.missions
            .lock()
            .expect("missions registry lock")
            .remove(&id);
        drop(cell);

        Ok(drive_result?.outcome)
    }

    /// `POST /api/tickets/:slug/draft`: fire-and-observe twin of
    /// [`Self::draft`] for the REST surface — a draft can run for a while (a
    /// planning conversation with the orchestrator), so this creates the
    /// planning mission SYNCHRONOUSLY (registering it exactly like `create`,
    /// so its lifecycle streams over `GET /api/missions/:id/ws` immediately),
    /// then spawns [`drive_draft`] as a background task and returns the
    /// mission id right away. The final outcome (Review vs NeedsContext) is
    /// read back later via `GET /api/tickets/:slug`.
    pub async fn draft_async(&self, slug: &str, then_enqueue: bool) -> Result<String, ApiError> {
        Ticket::ensure_valid_slug(slug)?;
        let ticket_path = Ticket::tickets_dir(&self.repo_root).join(format!("{slug}.md"));
        if !ticket_path.is_file() {
            return Err(ApiError::not_found(format!("ticket '{slug}' not found")));
        }
        let ticket = Ticket::load(&ticket_path)?;

        let cfg = config_for_ticket(config::load(&self.repo_root)?, &ticket);
        let backend = self.backend(cfg.claude_binary.as_deref()).await?;
        let engine =
            MissionEngine::create(backend, self.repo_root.clone(), &ticket.mission_goal(), cfg)?;
        let id = engine.mission_id().to_string();
        let cell = new_cell(Box::new(engine));
        self.missions
            .lock()
            .expect("missions registry lock")
            .insert(id.clone(), new_planning(Arc::clone(&cell)));
        self.ensure_sweeper_started();

        let repo_root = self.repo_root.clone();
        let missions = Arc::clone(&self.missions);
        let mission_id = id.clone();
        tokio::spawn(async move {
            let drive_result = {
                let mut engine = cell.lock().await;
                drive_draft(&mut engine, &repo_root, &ticket, then_enqueue).await
            };
            // Same drop-then-remove ordering as `draft`/`run_to_end`: the
            // engine flushes its log and frees the single-writer lock before
            // the mission stops being "hosted here".
            missions
                .lock()
                .expect("missions registry lock")
                .remove(&mission_id);
            drop(cell);
            if let Err(e) = drive_result {
                tracing::error!(mission = %mission_id, error = %e, "hosted ticket draft errored");
            }
        });

        Ok(id)
    }

    /// `POST /api/tickets/:slug/approve`: the shared `kranz_engine::deps`
    /// gate (cycle detection, unsatisfied-blocker refusal) plus the
    /// enqueue side effects — the exact same core [`kranz_cli`]'s `kranz
    /// ticket approve` calls, so the CLI and REST surfaces can never drift.
    pub fn approve_ticket(
        &self,
        slug: &str,
        force: bool,
    ) -> Result<deps::ApprovedTicket, ApiError> {
        deps::approve_ticket(&self.repo_root, slug, None, force).map_err(ApiError::from)
    }

    /// `POST /api/missions/:id/planning/turn`: one conversational turn. A
    /// captured seed reply (fresh session / re-seed) is prepended — it
    /// happened first in the conversation.
    pub async fn planning_turn(&self, id: &str, text: &str) -> Result<String, ApiError> {
        let cell = self.planning_cell_or_attach(id).await?;
        let mut engine = try_lock(&cell)?;
        let reply = engine.planning_turn(text).await?;
        Ok(prepend_seed(engine.take_seed_reply(), reply))
    }

    /// `POST /api/missions/:id/planning/request-plan`: demand the plan.
    /// Ready → plan + cost estimate; NotReady → the orchestrator's prose
    /// (back to the conversation).
    pub async fn request_plan(&self, id: &str) -> Result<Value, ApiError> {
        let cell = self.planning_cell_or_attach(id).await?;
        let mut engine = try_lock(&cell)?;
        let request = engine.request_plan().await?;
        let seed = engine.take_seed_reply();
        match request {
            PlanRequest::Ready(plan) => {
                // Estimate with params calibrated from this repo's completed
                // missions (built-in defaults when there are none yet).
                let calibration = cost::calibrate(&self.repo_root);
                let estimate = cost::estimate(&plan, &engine.state().config, &calibration.params);
                let estimate = cost::apply_shape(estimate, &plan, &calibration);
                // Park the reviewed plan so ANY surface's approve affordance
                // (Slack buttons, web, glasses ring) can commit it later.
                self.set_pending_plan(id, Some(plan.clone()));
                Ok(json!({
                    "ready": true,
                    "plan": plan,
                    "estimate": estimate_json(&estimate),
                    "calibration": { "missionsUsed": calibration.missions_used },
                }))
            }
            PlanRequest::NotReady(reply) | PlanRequest::WrongPlan { reason: reply } => {
                // A wrong-plan escalation reaches this interactive surface as
                // the planner's reason text, exactly like a not-ready reply —
                // the ticket-parking side effect is the draft flow's job.
                Ok(json!({ "ready": false, "reply": prepend_seed(seed, reply) }))
            }
        }
    }

    /// `POST /api/missions/:id/approve`: commit plan.json/plan.md/index.md on
    /// the mission branch exactly like the CLI. Returns the mission branch.
    pub async fn approve(&self, id: &str, plan: Plan) -> Result<String, ApiError> {
        let cell = self.planning_cell_or_attach(id).await?;
        let branch = {
            let mut engine = try_lock(&cell)?;
            engine.approve_plan(plan)?;
            engine.state().mission.mission_branch.clone()
        };
        self.set_pending_plan(id, None);
        Ok(branch)
    }

    /// `POST /api/missions/:id/start`: consume the hosted engine into a
    /// background `engine.run()` task — or, for a mission not in the registry
    /// (blocked earlier, server restarted, or CLI-created), resume it from
    /// the event log and run that.
    pub async fn start(&self, id: &str) -> Result<(), ApiError> {
        // Try to consume a hosted planning-phase engine.
        let taken: Option<Box<MissionEngine>> = {
            let mut map = self.missions.lock().expect("missions registry lock");
            match map.remove(id) {
                None => None,
                Some(HostedMission::Running { handle, _repo_busy }) => {
                    if handle.is_finished() {
                        // The task ended but its cleanup lost the race with
                        // this request: treat as not hosted (resume below).
                        // Drop the busy hold so a resume can re-acquire.
                        drop(_repo_busy);
                        None
                    } else {
                        map.insert(
                            id.to_string(),
                            HostedMission::Running { handle, _repo_busy },
                        );
                        return Err(ApiError::conflict(format!(
                            "mission '{id}' is already running — observe it via GET \
                             /api/missions/{id}/state or steer it via POST \
                             /api/missions/{id}/control"
                        )));
                    }
                }
                Some(HostedMission::Planning {
                    cell,
                    last_use,
                    pending_plan,
                }) => match Arc::try_unwrap(cell) {
                    Err(cell) => {
                        // A handler holds a clone: a turn is (or is about to
                        // be) in flight. Put the entry back untouched.
                        map.insert(
                            id.to_string(),
                            HostedMission::Planning {
                                cell,
                                last_use,
                                pending_plan,
                            },
                        );
                        return Err(turn_in_flight());
                    }
                    Ok(mutex) => {
                        let engine = mutex.into_inner();
                        if engine.state().mission.status == MissionStatus::Planning {
                            map.insert(id.to_string(), new_planning(new_cell(engine)));
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

        let (engine, from_registry) = match taken {
            Some(engine) => (engine, true),
            None => {
                // Re-invocable path: resume from the log. A live engine
                // elsewhere (CLI, or a hosted run racing this request) holds
                // the single-writer lock → EngineError::LockHeld → 409.
                if !MissionPaths::new(&self.repo_root, id)
                    .events_file()
                    .is_file()
                {
                    return Err(ApiError::not_found(format!("unknown mission '{id}'")));
                }
                let cfg = config::load(&self.repo_root)?;
                let backend = self.backend(cfg.claude_binary.as_deref()).await?;
                let engine = Box::new(MissionEngine::resume(
                    backend,
                    self.repo_root.clone(),
                    id,
                    LockForce::No,
                )?);
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
                (engine, false)
            }
        };

        let global_run_permit = match self.try_global_run_permit() {
            Ok(permit) => permit,
            Err(error) => {
                if from_registry {
                    self.missions
                        .lock()
                        .expect("missions registry lock")
                        .insert(id.to_string(), new_planning(new_cell(engine)));
                }
                return Err(error);
            }
        };

        // Acquire the repo-wide busy lock before spawning: a sibling
        // `kranz work` / hosted drain must not run in parallel. Held for the
        // lifetime of the Running entry (dropped when the run ends).
        let repo_busy = match kranz_engine::queue::acquire_repo_busy(&self.repo_root, id) {
            Ok(hold) => hold,
            Err(e) => {
                if from_registry {
                    // Put the planning/approved engine back so the operator
                    // can retry once the sibling run finishes.
                    self.missions
                        .lock()
                        .expect("missions registry lock")
                        .insert(id.to_string(), new_planning(new_cell(engine)));
                }
                // Resume path: dropping `engine` releases the mission lock.
                return Err(ApiError::from(e));
            }
        };

        // Insert the Running entry while holding the map lock across the
        // spawn: if the run ends instantly, its cleanup blocks on this lock
        // until the entry exists, so it can never leave a stale entry behind.
        {
            let mut map = self.missions.lock().expect("missions registry lock");
            let missions = Arc::clone(&self.missions);
            let mission_id = id.to_string();
            let handle = spawn_with_global_run_permit(
                global_run_permit,
                run_to_end(engine, mission_id, missions),
            );
            map.insert(
                id.to_string(),
                HostedMission::Running {
                    handle,
                    _repo_busy: repo_busy,
                },
            );
        }
        Ok(())
    }

    /// `POST /api/missions/:id/merge`: the human-triggered gated Merge
    /// action (roadmap M6). Loads the mission's `base_branch`/`base_sha`/
    /// `mission_branch` from its event log (no engine needs to be hosted —
    /// merge is independent of the planning/run-loop registry) and runs
    /// [`kranz_engine::merge::merge_mission`] under `spawn_blocking` (git and
    /// the gate suite are both blocking work). Never pushes.
    pub async fn merge(&self, id: &str) -> Result<Value, ApiError> {
        if !MissionPaths::is_safe_id(id) {
            return Err(ApiError::not_found(format!("unknown mission '{id}'")));
        }
        let paths = MissionPaths::new(&self.repo_root, id);
        if !paths.events_file().is_file() {
            return Err(ApiError::not_found(format!("unknown mission '{id}'")));
        }
        // Serialize the complete read/pin/integrate/gate/advance transaction
        // against mission runs and other merges in this repo. The hold moves
        // INTO the blocking task below: if the client disconnects mid-gate-
        // suite this handler future is dropped, but the detached blocking
        // merge keeps mutating the primary tree — a hold living here would
        // be released early, letting a dispatcher claim the busy repo.
        let repo_busy = kranz_engine::queue::acquire_repo_busy(&self.repo_root, id)?;
        let events = EventLog::read_events(&paths.events_file())?;
        let state = kranz_engine::reducer::fold(&events).map_err(ApiError::from)?;
        if state.mission.status != MissionStatus::Complete {
            return Err(ApiError::conflict(format!(
                "mission '{id}' is {:?}; only a complete mission can be merged",
                state.mission.status
            )));
        }
        let base_branch = state.mission.base_branch.clone();
        let base_sha = state.mission.base_sha.clone().ok_or_else(|| {
            ApiError::conflict(format!(
                "mission '{id}' has no pinned base sha — approve a plan first"
            ))
        })?;
        let mission_branch = state.mission.mission_branch.clone();
        let metadata = KranzCommitMetadata {
            mission_id: state.mission.id.clone(),
            cost_usd: state.total_cost_usd,
            tokens: state.totals.clone(),
        };

        let repo_root = self.repo_root.clone();
        let gate_executor = Arc::clone(&self.gate_executor);
        let report = tokio::task::spawn_blocking(move || {
            let repo = GitRepo::open(&repo_root)?;
            let report = merge_mission(
                &repo,
                &base_branch,
                &base_sha,
                &mission_branch,
                Some(metadata),
                |cmd, cwd| gate_executor(cmd, cwd),
            );
            // Explicit: the repo-busy hold is released HERE, once the merge
            // has fully finished — never earlier by a dropped handler future.
            drop(repo_busy);
            report
        })
        .await
        .map_err(|e| ApiError::internal(format!("merge task panicked: {e}")))?
        .map_err(ApiError::from)?;

        match report {
            MergeReport::Merged { commit, stale_base } => Ok(json!({
                "merged": true,
                "commit": commit,
                "staleBase": stale_base.map(|warning| json!({
                    "baseSha": warning.base_sha,
                    "liveBase": warning.live_base,
                    "mergeCommitsSinceBase": warning.merge_commits_since_base,
                    "message": format!(
                        "stale base: {} merge commit(s) landed on {} since the mission base; cross-branch semantic conflicts are more likely, and full gates have run",
                        warning.merge_commits_since_base,
                        warning.live_base,
                    ),
                })),
            })),
            MergeReport::RefusedDirtyTree => Err(ApiError::conflict(
                "refusing to merge: tracked working tree is dirty",
            )),
            MergeReport::GateFailed { gate, output } => Err(ApiError::unprocessable(
                kranz_engine::scrub::scrub(&format!("{gate} failed:\n{output}")),
            )),
            MergeReport::GateConfigInvalid { detail } => Err(ApiError::unprocessable(format!(
                "refusing to merge without a valid repo gate suite: {detail}"
            ))),
            MergeReport::SecretScanFailed { findings } => Err(ApiError::unprocessable(format!(
                "secret scan failed; add a fingerprint to {} only for a reviewed false positive:\n{}",
                kranz_engine::scrub::SECRET_ALLOWLIST_PATH,
                kranz_engine::scrub::format_findings(&findings)
            ))),
            MergeReport::Conflict { files } => Err(ApiError::conflict(format!(
                "merge conflicted in: {}",
                files.join(", ")
            ))),
            MergeReport::RefusedPreMerge { detail } => Err(ApiError::conflict(format!(
                "merge refused before it started: {detail}"
            ))),
        }
    }

    /// Read-only, LLM-backed Q&A for `/kranz ask`: ground the model in current
    /// mission/ticket state and return one answer plus usage. This deliberately
    /// bypasses the hosted mission registry: it must never create a mission,
    /// append mission events, enqueue work, approve, start, or merge.
    pub async fn ask(&self, question: &str) -> Result<Value, ApiError> {
        let question = question.trim();
        if question.is_empty() {
            return Err(ApiError::bad_request("ask requires a question"));
        }
        let cfg = config::load(&self.repo_root)?;
        config::validate(&cfg)?;
        let role = cfg.validator_scrutiny.clone();
        let backend = self.backend(cfg.claude_binary.as_deref()).await?;
        let prompt = ask_prompt(question, &ask_context(&self.repo_root));
        let spec = SessionSpec {
            cwd: self.repo_root.clone(),
            prompt: PromptMode::SingleShot(prompt),
            append_system_prompt: Some(
                "You answer read-only questions about this Kranz repository. \
                 Use only the supplied context; if it is insufficient, say what is missing. \
                 Do not modify files, run commands, create missions, enqueue work, approve, \
                 start, or merge anything."
                    .to_string(),
            ),
            model: role.model,
            effort: role.reasoning_effort,
            session_id: format!("ask-{}", uuid::Uuid::new_v4()),
            resume: None,
            permission_mode: Some("plan".to_string()),
            allowed_tools: vec![],
            disallowed_tools: vec![
                "Bash(*)".to_string(),
                "Edit(*)".to_string(),
                "Write(*)".to_string(),
            ],
            tools: vec![
                "Read".to_string(),
                "Grep".to_string(),
                "Glob".to_string(),
                "LS".to_string(),
            ],
            writable: false,
            settings_json: None,
            json_schema: None,
            max_budget_usd: role.max_budget_usd,
            max_turns: role.max_turns,
            env: HashMap::new(),
            sandbox: None,
        };
        let outcome = run_ask_session(backend, spec).await?;
        Ok(json!({
            "answer": outcome.answer,
            "costUsd": outcome.cost_usd,
            "tokens": outcome.tokens,
        }))
    }

    /// Release a hosted idle engine: drop it from the registry (flushing its
    /// log and freeing the single-writer lock) so an EXTERNAL runner — the
    /// `kranz work` dispatcher, a terminal `kranz plan/run` — can take the
    /// mission over. The approve-and-QUEUE path needs this: without it the
    /// approved engine would sit attached here holding the lock, and the very
    /// dispatcher the queue points at would be refused with `LockHeld`.
    ///
    /// Returns `true` when the mission is now free of THIS host (released, or
    /// was never hosted), `false` when it is actively running here (never
    /// interrupted). A turn in flight is an error, mirroring the other
    /// planning operations.
    pub fn release(&self, id: &str) -> Result<bool, ApiError> {
        release_from(&self.missions, id)
    }

    /// Release every `Planning` entry idle for at least `threshold` (a mission
    /// touched more recently than that is left alone). A mid-turn cell can
    /// never actually be released — [`release`](Self::release) refuses it via
    /// `turn_in_flight`, which this treats as "not idle yet" rather than an
    /// error. Returns the ids this call actually released.
    pub fn sweep_idle(&self, threshold: Duration) -> Vec<String> {
        sweep_idle_from(&self.missions, threshold)
    }

    /// Spawn the idle-release sweeper at most once, the first time a mission
    /// is hosted. It loops for the lifetime of the host: sleep, read the
    /// configured window, sweep. `planningIdleReleaseMinutes == 0` means
    /// "never release" — checked fresh each tick so a live config edit takes
    /// effect without a restart.
    fn ensure_sweeper_started(&self) {
        let mut guard = self.sweeper.lock().expect("sweeper lock");
        if guard.is_some() {
            return;
        }
        let repo_root = self.repo_root.clone();
        let missions = Arc::clone(&self.missions);
        *guard = Some(tokio::spawn(async move {
            const SWEEP_INTERVAL: Duration = Duration::from_secs(60);
            loop {
                tokio::time::sleep(SWEEP_INTERVAL).await;
                let minutes = match config::load(&repo_root) {
                    Ok(cfg) => cfg.planning_idle_release_minutes,
                    Err(_) => continue,
                };
                if minutes == 0 {
                    continue;
                }
                let threshold = Duration::from_secs(minutes * 60);
                let released = sweep_idle_from(&missions, threshold);
                for id in released {
                    tracing::info!(mission = %id, "released idle planning engine");
                }
            }
        }));
    }

    /// Whether a tracked drain is currently live (a `Starting` reservation or
    /// a `Running` handle that hasn't finished). Read-only: never installs a
    /// reservation, so it never races [`Self::drain`]'s own check.
    pub(crate) fn drain_is_live(&self) -> bool {
        match &*self.drain.lock().expect("drain tracker lock") {
            DrainSlot::Idle => false,
            DrainSlot::Starting(_) => true,
            DrainSlot::Running(handle) => !handle.join.is_finished(),
        }
    }

    /// One autoWork check: re-read config fresh (so a live `autoWork` toggle
    /// takes effect without a restart, exactly like the idle sweeper reads
    /// `planningIdleReleaseMinutes`), and kick off a drain when
    /// [`should_auto_drain`] says to. Invoked by the process-wide
    /// [`crate::MultiRepoHost`] watcher (and by tests) — per-host watchers
    /// are not started.
    pub(crate) async fn auto_work_tick(&self) -> bool {
        let cfg = match config::load(&self.repo_root) {
            Ok(cfg) => cfg,
            Err(_) => return false,
        };
        let queue_non_empty = kranz_engine::queue::peek(&self.repo_root).is_some();
        if should_auto_drain(cfg.auto_work, queue_non_empty, self.drain_is_live()) {
            // A sibling dispatcher already owns this repository. Skip it
            // before taking a process-wide permit so the catalog scheduler
            // can try another ready root in this same pass. `drain_once`
            // also stops on busy if ownership races this check.
            if kranz_engine::queue::is_repo_busy(&self.repo_root).is_some() {
                return false;
            }
            match self.drain_once().await {
                Ok(_) => return true,
                Err(e)
                    if e.message
                        .starts_with("host.maxConcurrentRepos is saturated") => {}
                Err(e) => tracing::error!(error = %e.message, "autoWork drain failed"),
            }
        }
        false
    }

    /// `POST /api/missions/:id/abandon`: retire a mission through the
    /// engine's canonical abandon path (terminal-refusing, event-recorded).
    /// A mission hosted HERE is taken out of the registry first — an idle
    /// planning engine is dropped (freeing the lock), a running task is
    /// aborted and awaited (the engine's Drop flushes the log and kills its
    /// agent children) — so the abandon event lands on a quiet log. A lock
    /// held by a FOREIGN process (a terminal `kranz plan/run`) surfaces as
    /// the engine's LockHeld → 409; the web never force-steals.
    pub async fn abandon(&self, id: &str, reason: &str) -> Result<(), ApiError> {
        let taken = self
            .missions
            .lock()
            .expect("missions registry lock")
            .remove(id);
        match taken {
            None => {}
            Some(HostedMission::Planning {
                cell,
                last_use,
                pending_plan,
            }) => match Arc::try_unwrap(cell) {
                Ok(mutex) => drop(mutex.into_inner()),
                Err(cell) => {
                    self.missions
                        .lock()
                        .expect("missions registry lock")
                        .insert(
                            id.to_string(),
                            HostedMission::Planning {
                                cell,
                                last_use,
                                pending_plan,
                            },
                        );
                    return Err(turn_in_flight());
                }
            },
            Some(HostedMission::Running { handle, _repo_busy }) => {
                if !handle.is_finished() {
                    handle.abort();
                }
                // Cancelled or finished either way: await settles the task so
                // the engine is dropped (log flushed, lock freed) before we
                // append the abandon event. Dropping `_repo_busy` releases the
                // repo-wide busy lock.
                let _ = handle.await;
                drop(_repo_busy);
            }
        }
        kranz_engine::mission_catalog::abandon_mission(
            self.repo_root.clone(),
            id,
            reason,
            LockForce::No,
        )
        .map_err(ApiError::from)?;
        Ok(())
    }

    /// `POST /api/missions/:id/delete`: remove a TERMINAL mission's directory,
    /// mirroring `kranz clean` exactly — [`cleanable_class`] decides, `all`
    /// opts in to deleting Complete missions (which otherwise stay: they feed
    /// the cost-calibration corpus), and a live lock is re-checked immediately
    /// before removal so nothing is ever deleted under a running engine.
    /// Only the mission directory and its own `missions/index.md` line go;
    /// branches, tags, and every other mission's index line are left intact
    /// (same contract as the CLI).
    pub fn clean(&self, id: &str, all: bool) -> Result<(), ApiError> {
        use kranz_engine::mission_catalog::{
            cleanable_class, mission_lock_is_live, prune_mission_index_file, CleanClass,
        };
        if self
            .missions
            .lock()
            .expect("missions registry lock")
            .contains_key(id)
        {
            return Err(ApiError::conflict(format!(
                "mission '{id}' is hosted by this server (attached or running) — abandon it \
                 first, or let its run finish"
            )));
        }
        let paths = MissionPaths::new(&self.repo_root, id);
        if !paths.events_file().is_file() {
            return Err(ApiError::not_found(format!("unknown mission '{id}'")));
        }
        let events = EventLog::read_events(&paths.events_file())?;
        let state = kranz_engine::reducer::fold(&events).map_err(ApiError::from)?;
        let has_plan = paths.plan_file().is_file();
        match cleanable_class(state.mission.status, has_plan) {
            CleanClass::Keep => {
                return Err(ApiError::conflict(format!(
                    "mission '{id}' is live ({:?}) — abandon it before deleting",
                    state.mission.status
                )))
            }
            CleanClass::CompleteKeepByDefault if !all => {
                return Err(ApiError::conflict(format!(
                    "mission '{id}' is Complete; completed missions feed the cost-calibration \
                     corpus — pass \"all\": true to delete it anyway"
                )))
            }
            CleanClass::Stale | CleanClass::CompleteKeepByDefault => {}
        }
        // Same last-instant liveness re-check as the CLI's remove_missions: a
        // husk can go live between the fold and the removal.
        if mission_lock_is_live(&paths) {
            return Err(ApiError::conflict(format!(
                "mission '{id}' became live — nothing was deleted"
            )));
        }
        kranz_engine::queue::remove(&self.repo_root, id);
        std::fs::remove_dir_all(paths.mission_dir())
            .map_err(|e| ApiError::internal(format!("removing mission '{id}': {e}")))?;
        prune_mission_index_file(&self.repo_root, id);
        Ok(())
    }

    /// The reviewed plan awaiting approval, if any (clone). `GET
    /// /api/missions/:id/pending-plan` and the glasses PLAN page read this.
    pub fn pending_plan(&self, id: &str) -> Option<Plan> {
        let map = self.missions.lock().expect("missions registry lock");
        match map.get(id) {
            Some(HostedMission::Planning { pending_plan, .. }) => {
                pending_plan.lock().expect("pending plan lock").clone()
            }
            _ => None,
        }
    }

    fn set_pending_plan(&self, id: &str, plan: Option<Plan>) {
        let map = self.missions.lock().expect("missions registry lock");
        if let Some(HostedMission::Planning { pending_plan, .. }) = map.get(id) {
            *pending_plan.lock().expect("pending plan lock") = plan;
        }
    }

    /// Approve the PARKED plan if one exists: `Ok(Some(branch))` committed,
    /// `Ok(None)` nothing pending — for callers with their own no-plan
    /// fallback (the Slack bridge's state-aware routing). Consumes the
    /// pending plan on success; a failed approve puts it back so a retry can
    /// fire.
    pub async fn try_approve_pending(&self, id: &str) -> Result<Option<String>, ApiError> {
        let Some(plan) = ({
            let map = self.missions.lock().expect("missions registry lock");
            match map.get(id) {
                Some(HostedMission::Planning { pending_plan, .. }) => {
                    pending_plan.lock().expect("pending plan lock").take()
                }
                _ => None,
            }
        }) else {
            return Ok(None);
        };
        match self.approve(id, plan.clone()).await {
            Ok(branch) => Ok(Some(branch)),
            Err(e) => {
                self.set_pending_plan(id, Some(plan));
                Err(e)
            }
        }
    }

    /// [`Self::try_approve_pending`] with nothing-pending as a 409 — the
    /// REST shape (`POST /api/missions/:id/approve-pending`).
    pub async fn approve_pending(&self, id: &str) -> Result<String, ApiError> {
        self.try_approve_pending(id).await?.ok_or_else(|| {
            ApiError::conflict(format!(
                "mission '{id}' has no reviewed plan pending — request the plan first \
                 (POST /api/missions/{id}/planning/request-plan, /kranz plan, or the UI)"
            ))
        })
    }

    /// `POST /api/queue/drain`: run the queue drain/claim/skip loop
    /// ([`kranz_engine::work::drain_queue`]) as a background task on this
    /// serve process. This is just ANOTHER dispatcher: it does not register
    /// missions in the `missions` planning registry, and arbitrates against
    /// an external `kranz work` process exactly as today — through the queue
    /// claim files and the events.jsonl single-writer lock, no new locking.
    ///
    /// IDEMPOTENT while a drain is live: a second call while the tracked
    /// drain task has not finished returns THAT drain's current state
    /// instead of spawning a second one.
    pub async fn drain(&self) -> Result<Value, ApiError> {
        self.drain_with_mode(false).await
    }

    /// Auto-work drains at most one queue front so the process-wide scheduler
    /// can rotate fairly to another ready repository after this mission.
    async fn drain_once(&self) -> Result<Value, ApiError> {
        self.drain_with_mode(true).await
    }

    async fn drain_with_mode(&self, once: bool) -> Result<Value, ApiError> {
        // Fast path: a live drain already owns the slot.
        {
            let guard = self.drain.lock().expect("drain tracker lock");
            match &*guard {
                DrainSlot::Starting(state) => {
                    return Ok(drain_state_json(&state.lock().expect("drain state lock")));
                }
                DrainSlot::Running(handle) if !handle.join.is_finished() => {
                    return Ok(drain_state_json(
                        &handle.state.lock().expect("drain state lock"),
                    ));
                }
                DrainSlot::Idle | DrainSlot::Running(_) => {}
            }
        }

        // Discover config/backend before reserving the drain slot or taking a
        // process-wide run permit. Holding either across `.await` would either
        // publish a false-live Starting reservation (on later saturation) or
        // starve sibling repositories during Claude discovery.
        let cfg = config::load(&self.repo_root)?;
        let backend = self.backend(cfg.claude_binary.as_deref()).await?;
        let repo_root = self.repo_root.clone();

        // Re-check under the drain lock, then acquire the global permit and
        // install Starting in one critical section so saturation never leaves
        // a rolled-back live reservation for concurrent callers to observe.
        let (state, global_run_permit) = {
            let mut guard = self.drain.lock().expect("drain tracker lock");
            match &*guard {
                DrainSlot::Starting(state) => {
                    return Ok(drain_state_json(&state.lock().expect("drain state lock")));
                }
                DrainSlot::Running(handle) if !handle.join.is_finished() => {
                    return Ok(drain_state_json(
                        &handle.state.lock().expect("drain state lock"),
                    ));
                }
                DrainSlot::Idle | DrainSlot::Running(_) => {}
            }
            let global_run_permit = self.try_global_run_permit()?;
            let state = Arc::new(Mutex::new(DrainState {
                live: true,
                current_mission_id: None,
                ran: Vec::new(),
                parked: Vec::new(),
            }));
            *guard = DrainSlot::Starting(Arc::clone(&state));
            (state, global_run_permit)
        };

        // Cold spawn path only (never the early-return branches above): the
        // dispatch branch is still whatever the operator's checkout was, so
        // capture it now, before the spawned task (or any concurrent racer)
        // can ever land on a mission branch. See `drain_task` for the
        // restore-on-exit half of this contract.
        let task_state = Arc::clone(&state);
        let join = tokio::spawn(async move {
            let _global_run_permit = global_run_permit;
            drain_task(repo_root.clone(), task_state, once, move |mission_id| {
                let backend = Arc::clone(&backend);
                let repo_root = repo_root.clone();
                async move { run_mission_headless(backend, repo_root, mission_id).await }
            })
            .await;
        });

        let initial = drain_state_json(&state.lock().expect("drain state lock"));
        *self.drain.lock().expect("drain tracker lock") =
            DrainSlot::Running(DrainHandle { join, state });
        Ok(initial)
    }

    /// `GET /api/queue`: the queue front-to-back, who (if anyone) currently
    /// holds the busy lock, and this host's own drain tracker.
    ///
    /// Readiness is probed for the **front entry only** (with a short TTL
    /// cache). Deeper entries omit `readiness` so a long queue cannot turn
    /// every dashboard poll into N CLI shells.
    pub fn queue_state(&self) -> Value {
        let entries = kranz_engine::queue::list(&self.repo_root);
        let busy_with = kranz_engine::queue::is_repo_busy(&self.repo_root);
        let drain = match &*self.drain.lock().expect("drain tracker lock") {
            DrainSlot::Running(handle) => {
                drain_state_json(&handle.state.lock().expect("drain state lock"))
            }
            DrainSlot::Starting(state) => {
                drain_state_json(&state.lock().expect("drain state lock"))
            }
            DrainSlot::Idle => drain_state_json(&DrainState::default()),
        };

        let front_readiness = entries.first().map(|e| {
            let mid = e.mission_id.as_str();
            {
                let cache = self
                    .readiness_front_cache
                    .lock()
                    .expect("readiness front cache lock");
                if let Some(cached) = cache.as_ref() {
                    if cached.mission_id == mid && cached.at.elapsed() < READINESS_FRONT_CACHE_TTL {
                        return (mid.to_string(), cached.report.clone());
                    }
                }
            }
            let report = kranz_engine::backend_readiness::probe_mission(&self.repo_root, mid)
                .ok()
                .and_then(|r| serde_json::to_value(r).ok())
                .unwrap_or(Value::Null);
            *self
                .readiness_front_cache
                .lock()
                .expect("readiness front cache lock") = Some(FrontReadinessCache {
                mission_id: mid.to_string(),
                report: report.clone(),
                at: Instant::now(),
            });
            (mid.to_string(), report)
        });

        let entries_json: Vec<Value> = entries
            .into_iter()
            .map(|e| {
                let readiness = front_readiness.as_ref().and_then(|(id, report)| {
                    if id == &e.mission_id && !report.is_null() {
                        Some(report.clone())
                    } else {
                        None
                    }
                });
                json!({
                    "missionId": e.mission_id,
                    "ticketSlug": e.ticket_slug,
                    "priority": e.priority,
                    "seq": e.seq,
                    "readiness": readiness,
                })
            })
            .collect();
        let mut state = json!({
            "entries": entries_json,
            "busyWith": busy_with,
            "drain": drain,
        });
        // Additive observation for automation: when this host participates in
        // host.maxConcurrentRepos, surface whether the process-wide budget is
        // currently exhausted so agents can distinguish "no work" from "capped".
        if let Some(permits) = &self.global_run_permits {
            let available = permits.available_permits();
            state["maxConcurrentReposAvailable"] = json!(available);
            state["maxConcurrentReposSaturated"] = json!(available == 0);
        }
        state
    }

    // -----------------------------------------------------------------------
    // Registry plumbing
    // -----------------------------------------------------------------------

    /// The engine cell of a planning-phase hosted mission, with helpful 409s
    /// for every other state.
    fn planning_cell(&self, id: &str) -> Result<EngineCell, ApiError> {
        let map = self.missions.lock().expect("missions registry lock");
        match map.get(id) {
            Some(HostedMission::Planning { cell, last_use, .. }) => {
                *last_use.lock().expect("last-use lock") = Instant::now();
                Ok(Arc::clone(cell))
            }
            Some(HostedMission::Running { .. }) => Err(ApiError::conflict(format!(
                "mission '{id}' is running — steer it via POST /api/missions/{id}/control"
            ))),
            None => Err(self.not_hosted(id)),
        }
    }

    /// [`planning_cell`], attaching an un-hosted in-planning mission from disk
    /// first when needed. This is what lets a mission whose engine was released
    /// (CLI-created, bridge seed turn, server restart) continue planning through
    /// this host: resume it under the single-writer lock, adopt it into the
    /// registry, and hand back its cell. A mission held live elsewhere surfaces
    /// as the engine's `LockHeld` (409) — unless the holder is this registry
    /// itself racing us, in which case the second lookup finds the winner.
    async fn planning_cell_or_attach(&self, id: &str) -> Result<EngineCell, ApiError> {
        let miss = match self.planning_cell(id) {
            Ok(cell) => return Ok(cell),
            Err(miss) => miss,
        };
        // Only "exists on disk but not hosted" is attachable; Running entries
        // and unknown missions keep their original error.
        if !MissionPaths::new(&self.repo_root, id)
            .events_file()
            .is_file()
            || self
                .missions
                .lock()
                .expect("missions registry lock")
                .contains_key(id)
        {
            return Err(miss);
        }
        let cfg = config::load(&self.repo_root)?;
        let backend = self.backend(cfg.claude_binary.as_deref()).await?;
        let engine = match MissionEngine::resume(backend, self.repo_root.clone(), id, LockForce::No)
        {
            Ok(engine) => Box::new(engine),
            // LockHeld can mean a concurrent request won the attach race and
            // the winner's engine now sits in the registry: prefer that cell.
            Err(EngineError::LockHeld(holder)) => {
                return self
                    .planning_cell(id)
                    .map_err(|_| ApiError::from(EngineError::LockHeld(holder)))
            }
            Err(e) => return Err(e.into()),
        };
        if engine.state().mission.status != MissionStatus::Planning {
            // Dropping the engine releases the just-taken lock.
            return Err(ApiError::conflict(format!(
                "mission '{id}' is not in planning (status {:?}) — planning turns only \
                 apply before a plan is approved",
                engine.state().mission.status
            )));
        }
        let cell = new_cell(engine);
        let mut map = self.missions.lock().expect("missions registry lock");
        // We hold the mission's file lock, so nobody else can have inserted a
        // LIVE engine meanwhile; insert unconditionally.
        map.insert(id.to_string(), new_planning(Arc::clone(&cell)));
        drop(map);
        self.ensure_sweeper_started();
        Ok(cell)
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
fn spawn_with_global_run_permit<F>(
    global_run_permit: Option<OwnedSemaphorePermit>,
    task: F,
) -> tokio::task::JoinHandle<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        // Task ownership is deliberate: abort and panic both drop the permit
        // even when registry cleanup inside the future never runs.
        let _global_run_permit = global_run_permit;
        task.await;
    })
}

async fn run_to_end(
    mut engine: Box<MissionEngine>,
    mission_id: String,
    missions: Arc<Mutex<HashMap<String, HostedMission>>>,
) {
    let repo_root = engine.paths().repo_root.clone();
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
    // Reconcile the linked ticket's .status sidecar to match the mission's
    // terminal/blocked status. Non-fatal: a reconcile failure must never
    // affect the registry cleanup below.
    if let Err(e) = kranz_engine::work::reconcile_ticket_for_mission(&repo_root, &mission_id) {
        tracing::warn!(mission = %mission_id, error = %e, "failed to reconcile linked ticket");
    }
    missions
        .lock()
        .expect("missions registry lock")
        .remove(&mission_id);
}

struct AskRunOutcome {
    answer: String,
    cost_usd: f64,
    tokens: TokenUsage,
}

async fn run_ask_session(
    backend: Arc<dyn AgentBackend>,
    spec: SessionSpec,
) -> Result<AskRunOutcome, ApiError> {
    let mut session = backend.start(spec).await.map_err(ApiError::from)?;
    let mut streamed_text = String::new();
    let mut result_text = None;
    let mut tokens = TokenUsage::default();
    let mut cost_usd = 0.0;
    let mut result_error = false;
    while let Some(event) = session.next_event().await.map_err(ApiError::from)? {
        match event {
            AgentEvent::Text { text, .. } => streamed_text.push_str(&text),
            AgentEvent::Result {
                text,
                is_error,
                usage,
                cost_usd: cost,
                ..
            } => {
                result_error |= is_error;
                tokens.add(&usage);
                cost_usd += cost.unwrap_or(0.0);
                if !text.trim().is_empty() {
                    result_text = Some(text);
                }
            }
            _ => {}
        }
    }
    match session.exit_status() {
        Some(SessionExit::Completed) if !result_error => {
            let answer = result_text.unwrap_or(streamed_text).trim().to_string();
            if answer.is_empty() {
                return Err(ApiError::internal("ask turn produced an empty answer"));
            }
            Ok(AskRunOutcome {
                answer,
                cost_usd,
                tokens,
            })
        }
        Some(SessionExit::Completed) => Err(ApiError::internal("ask turn failed")),
        Some(SessionExit::Failed(reason)) => {
            Err(ApiError::internal(format!("ask turn failed: {reason}")))
        }
        Some(SessionExit::Aborted) => Err(ApiError::internal("ask turn aborted")),
        None => Err(ApiError::internal("ask turn ended without an exit status")),
    }
}

fn ask_prompt(question: &str, context: &str) -> String {
    format!(
        "Answer this operator question about the Kranz repository.\n\n\
         Rules:\n\
         - Ground the answer only in the context below.\n\
         - If the context is insufficient, say what is missing.\n\
         - Keep the answer concise but specific, citing mission ids or ticket slugs when relevant.\n\
         - This is read-only: do not propose that you have changed state.\n\n\
         Question:\n{question}\n\nContext:\n{context}"
    )
}

fn ask_context(repo_root: &Path) -> String {
    let mut out = String::new();
    out.push_str("## Missions\n");
    let mut ids = MissionPaths::list_missions(repo_root);
    ids.sort();
    ids.reverse();
    if ids.is_empty() {
        out.push_str("(none)\n");
    }
    for id in ids.into_iter().take(20) {
        let paths = MissionPaths::new(repo_root, &id);
        let Ok(events) = EventLog::read_events(&paths.events_file()) else {
            continue;
        };
        let Ok(state) = kranz_engine::reducer::fold(&events) else {
            continue;
        };
        out.push_str(&format!(
            "- {}: {:?}; goal: {}; branch: {}; cost: ${:.4}; tokens in/out/cacheRead/cacheWrite: {}/{}/{}/{}\n",
            state.mission.id,
            state.mission.status,
            one_line(&state.mission.goal),
            state.mission.mission_branch,
            state.total_cost_usd,
            state.totals.input,
            state.totals.output,
            state.totals.cache_read,
            state.totals.cache_write,
        ));
        for decision in state.recent_decisions.iter().rev().take(3) {
            out.push_str(&format!("  decision: {}\n", one_line(decision)));
        }
        let report = paths.report_file();
        if let Ok(text) = std::fs::read_to_string(report) {
            out.push_str(&format!(
                "  report excerpt: {}\n",
                truncate(&one_line(&text), 500)
            ));
        }
    }

    out.push_str("\n## Tickets\n");
    let tickets = Ticket::list(repo_root);
    if tickets.is_empty() {
        out.push_str("(none)\n");
    }
    for ticket in tickets.iter().take(40) {
        let state = Ticket::read_state(repo_root, &ticket.slug);
        out.push_str(&format!(
            "- {} [{:?}, p{}]: {}; blocked-by: {}\n",
            ticket.slug,
            state,
            ticket.priority,
            one_line(&ticket.title),
            if ticket.blocked_by.is_empty() {
                "none".to_string()
            } else {
                ticket.blocked_by.join(", ")
            }
        ));
    }

    out.push_str("\n## Queue\n");
    let entries = queue::list(repo_root);
    if entries.is_empty() {
        out.push_str("(empty)\n");
    }
    for entry in entries.iter().take(20) {
        out.push_str(&format!(
            "- {} priority={} ticket={}\n",
            entry.mission_id,
            entry.priority,
            entry.ticket_slug.as_deref().unwrap_or("-")
        ));
    }
    out
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The spawned-task body behind [`MissionHost::drain`], factored out so a
/// host-level test can drive it directly (no `tokio::spawn`, so it stays
/// deterministic) with a fake `run_mission`. Captures the operator's dispatch
/// checkout, runs [`kranz_engine::work::drain_queue`] to completion, then
/// restores that checkout — honoring the SAME contract as the CLI
/// dispatcher's `restore_work_checkout` (`crates/cli/src/backlog.rs`), so a
/// hosted drain can never leave the repo stranded on a
/// `kranz/mission-*` branch.
async fn drain_task<R, Fut>(
    repo_root: PathBuf,
    state: Arc<Mutex<DrainState>>,
    once: bool,
    run_mission: R,
) where
    R: Fn(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<i32>>,
{
    drain_task_with_probe(
        repo_root,
        state,
        once,
        run_mission,
        kranz_engine::backend_readiness::probe_mission,
    )
    .await;
}

/// [`drain_task`] with an injectable readiness probe so checkout-restoration
/// tests remain hermetic on clean CI runners that intentionally have no agent
/// CLI installed.
async fn drain_task_with_probe<R, Fut, P>(
    repo_root: PathBuf,
    state: Arc<Mutex<DrainState>>,
    once: bool,
    run_mission: R,
    readiness_probe: P,
) where
    R: Fn(String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<i32>>,
    P: Fn(
        &Path,
        &str,
    ) -> kranz_engine::error::Result<kranz_engine::backend_readiness::ReadinessReport>,
{
    // Capture BEFORE `drain_queue` runs anything — nothing has touched the
    // checkout yet, so this is genuinely the operator's dispatch-time branch.
    let dispatch_branch = GitRepo::open(&repo_root)
        .ok()
        .and_then(|g| g.current_branch().ok());

    let result = kranz_engine::work::drain_queue_with_probe(
        &repo_root,
        once,
        |mission_id| {
            let state = Arc::clone(&state);
            let fut = run_mission(mission_id.clone());
            async move {
                state.lock().expect("drain state lock").current_mission_id =
                    Some(mission_id.clone());
                let outcome = fut.await;
                let mut guard = state.lock().expect("drain state lock");
                guard.current_mission_id = None;
                if outcome.is_ok() {
                    guard.ran.push(mission_id);
                }
                outcome
            }
        },
        readiness_probe,
    )
    .await;

    match &result {
        Ok(report) if !report.stopped_busy => {
            {
                let mut guard = state.lock().expect("drain state lock");
                for id in &report.parked {
                    if !guard.parked.contains(id) {
                        guard.parked.push(id.clone());
                    }
                }
            }
            restore_drain_checkout(&repo_root, dispatch_branch.as_deref());
        }
        Ok(report) => {
            let mut guard = state.lock().expect("drain state lock");
            for id in &report.parked {
                if !guard.parked.contains(id) {
                    guard.parked.push(id.clone());
                }
            }
            // `stopped_busy` (only possible with `once`, which the hosted
            // drain never sets — wired for parity with `cmd_work` anyway):
            // a sibling dispatcher may still be mid-mission, so leave the
            // checkout exactly where it is.
        }
        Err(e) => {
            tracing::error!(error = %e, "hosted queue drain errored");
            // Same restore as the success path: an errored drain must not
            // leave the operator stranded on a mission branch.
            restore_drain_checkout(&repo_root, dispatch_branch.as_deref());
        }
    }
    state.lock().expect("drain state lock").live = false;
}

/// Dispatcher-exit checkout restore for the hosted queue drain: mirrors
/// `restore_work_checkout` in `crates/cli/src/backlog.rs` verbatim. `None`
/// (capture failed, or nothing to restore) is a no-op; restoring TO a
/// `kranz/mission-*` branch is refused (that would recreate the very
/// stranding this exists to end); already back on the captured branch is a
/// no-op; a dirty TRACKED working tree aborts the restore (never carry
/// uncommitted operator edits across a branch switch) and leaves the
/// checkout on the mission branch with a warning logged.
fn restore_drain_checkout(repo_root: &Path, original: Option<&str>) {
    let Some(original) = original else { return };
    if original.starts_with("kranz/mission-") {
        return;
    }
    let Ok(git) = GitRepo::open(repo_root) else {
        return;
    };
    if git.current_branch().ok().as_deref() == Some(original) {
        return;
    }
    match git.is_clean_tracked() {
        Ok(true) => match git.checkout(original) {
            Ok(()) => tracing::info!(branch = %original, "hosted drain restored operator checkout"),
            Err(e) => {
                tracing::warn!(branch = %original, error = %e, "hosted drain could not restore checkout")
            }
        },
        Ok(false) => tracing::warn!(
            "hosted drain leaving checkout in place: tracked files have uncommitted changes"
        ),
        Err(e) => {
            tracing::warn!(error = %e, "hosted drain could not probe the working tree; checkout left in place")
        }
    }
}

/// Headless `run_mission` injected into [`kranz_engine::work::drain_queue`]
/// by [`MissionHost::drain`]: resume the mission under the single-writer
/// lock and run it to a terminal state, with no live tail/printer attached
/// (unlike the CLI's `kranz work`) since no terminal is attached to a serve
/// process.
async fn run_mission_headless(
    backend: Arc<dyn AgentBackend>,
    repo_root: PathBuf,
    mission_id: String,
) -> anyhow::Result<i32> {
    let mut engine = MissionEngine::resume(backend, repo_root, &mission_id, LockForce::No)?;
    let status = engine.run().await?;
    Ok(exit_code_for(status))
}

/// Map a terminal [`MissionStatus`] to the exit code the CLI's
/// `kranz work`/`kranz exec` report, matching `kranz_cli::exec::exit_code_for`.
fn exit_code_for(status: MissionStatus) -> i32 {
    match status {
        MissionStatus::Complete => 0,
        MissionStatus::Blocked => 2,
        _ => 1,
    }
}

/// Apply a ticket's per-ticket budget override to the orchestrator role
/// (mirrors `kranz_cli::backlog::config_for_ticket`), so draft spend is
/// bounded by the ticket's `maxBudgetUsd` when it sets one.
fn config_for_ticket(mut cfg: MissionConfig, ticket: &Ticket) -> MissionConfig {
    if let Some(budget) = ticket.max_budget_usd {
        cfg.orchestrator.max_budget_usd = Some(budget);
    }
    cfg
}

/// [`DraftOutcome`] as protocol camelCase JSON (the engine type is a plain
/// contract enum without serde derives) — the shape a REST `draft` handler
/// hands back. Not yet wired to a route (that's a later feature); kept here
/// so [`MissionHost::draft`]'s result has a ready serialization.
#[allow(dead_code)]
fn draft_outcome_json(outcome: &DraftOutcome) -> Value {
    match outcome {
        DraftOutcome::ParkedForReview {
            mission_id,
            mission_branch,
        } => json!({
            "outcome": "parkedForReview",
            "missionId": mission_id,
            "missionBranch": mission_branch,
        }),
        DraftOutcome::Enqueued { mission_id } => json!({
            "outcome": "enqueued",
            "missionId": mission_id,
        }),
        DraftOutcome::PlanAsProse { mission_id } => json!({
            "outcome": "planAsProse",
            "missionId": mission_id,
            "message": "the orchestrator produced a plan but emitted it as prose instead of \
                        through the plan channel, so nothing was queued; re-run draft for \
                        this ticket",
        }),
        DraftOutcome::NeedsContext {
            mission_id,
            questions,
        } => json!({
            "outcome": "needsContext",
            "missionId": mission_id,
            "questions": questions,
        }),
        DraftOutcome::WrongPlan { mission_id, reason } => json!({
            "outcome": "wrongPlan",
            "missionId": mission_id,
            "reason": reason,
        }),
    }
}

fn new_cell(engine: Box<MissionEngine>) -> EngineCell {
    Arc::new(tokio::sync::Mutex::new(engine))
}

/// A fresh `Planning` entry, last-used now.
fn new_planning(cell: EngineCell) -> HostedMission {
    HostedMission::Planning {
        cell,
        last_use: Arc::new(Mutex::new(Instant::now())),
        pending_plan: Arc::new(Mutex::new(None)),
    }
}

/// Shared by [`MissionHost::release`] and the sweeper: drop an idle planning
/// engine from the registry (flushing its log and freeing the single-writer
/// lock), refuse a mid-turn one, and leave running/absent entries be.
fn release_from(
    missions: &Mutex<HashMap<String, HostedMission>>,
    id: &str,
) -> Result<bool, ApiError> {
    let mut map = missions.lock().expect("missions registry lock");
    match map.remove(id) {
        None => Ok(true),
        Some(HostedMission::Running { handle, _repo_busy }) => {
            let finished = handle.is_finished();
            if !finished {
                map.insert(
                    id.to_string(),
                    HostedMission::Running { handle, _repo_busy },
                );
            }
            Ok(finished)
        }
        Some(HostedMission::Planning {
            cell,
            last_use,
            pending_plan,
        }) => match Arc::try_unwrap(cell) {
            Ok(mutex) => {
                drop(mutex.into_inner()); // flushes the log, frees the lock
                Ok(true)
            }
            Err(cell) => {
                map.insert(
                    id.to_string(),
                    HostedMission::Planning {
                        cell,
                        last_use,
                        pending_plan,
                    },
                );
                Err(turn_in_flight())
            }
        },
    }
}

/// Collect ids of `Planning` entries idle for at least `threshold`, release
/// each via [`release_from`], and return the ids actually freed. A mid-turn
/// cell (its `try_unwrap` fails inside `release_from`) is skipped, not an
/// error — it simply isn't idle yet from the sweeper's point of view.
/// `Running` entries are never candidates.
fn sweep_idle_from(
    missions: &Mutex<HashMap<String, HostedMission>>,
    threshold: Duration,
) -> Vec<String> {
    let idle_ids: Vec<String> = {
        let map = missions.lock().expect("missions registry lock");
        map.iter()
            .filter_map(|(id, mission)| match mission {
                HostedMission::Planning { last_use, .. } => {
                    let elapsed = last_use.lock().expect("last-use lock").elapsed();
                    (elapsed >= threshold).then(|| id.clone())
                }
                HostedMission::Running { .. } => None,
            })
            .collect()
    };
    idle_ids
        .into_iter()
        .filter(|id| matches!(release_from(missions, id), Ok(true)))
        .collect()
}

/// Planning endpoints never queue behind each other: contended = 409.
fn try_lock(
    cell: &EngineCell,
) -> Result<tokio::sync::MutexGuard<'_, Box<MissionEngine>>, ApiError> {
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
    let confidence = match estimate.confidence {
        kranz_engine::cost::Confidence::High => "high",
        kranz_engine::cost::Confidence::Low => "low",
    };
    json!({
        "workerRuns": estimate.worker_runs,
        "validatorRuns": estimate.validator_runs,
        "lowUsd": estimate.low_usd,
        "expectedUsd": estimate.expected_usd,
        "highUsd": estimate.high_usd,
        "confidence": confidence,
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

/// `GET /api/missions/:id/pending-plan` → `200 {"pending":true,"plan":{…}}`
/// or `200 {"pending":false}`. The parked plan from the last Ready
/// request-plan — what the approve affordances (buttons, ring) will commit.
pub(crate) async fn pending_plan_route(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let id = valid_id(&server, &id)?;
    Ok(Json(match server.host.pending_plan(&id) {
        Some(plan) => json!({ "pending": true, "plan": plan }),
        None => json!({ "pending": false }),
    }))
}

/// `POST /api/missions/:id/approve-pending` — optional body
/// `{"start": true}` → approve the parked plan (409 when none), then
/// optionally start. `200 {"branch": …, "started": bool}`.
pub(crate) async fn approve_pending_route(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let id = valid_id(&server, &id)?;
    let value = parse_body(&body)?;
    let start = value.get("start").and_then(Value::as_bool).unwrap_or(false);
    let branch = server.host.approve_pending(&id).await?;
    if start {
        server.host.start(&id).await?;
    }
    Ok(Json(json!({ "branch": branch, "started": start })))
}

/// `POST /api/missions/:id/abandon` — optional body `{"reason":"..."}` →
/// `200 {"abandoned": true}`. See [`MissionHost::abandon`].
pub(crate) async fn abandon_mission_route(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let id = valid_id(&server, &id)?;
    let value = parse_body(&body)?;
    let reason = value
        .get("reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("abandoned by operator");
    server.host.abandon(&id, reason).await?;
    Ok(Json(json!({ "abandoned": true })))
}

/// `POST /api/missions/:id/release` — no body → `200 {"released": bool}`. See
/// [`MissionHost::release`]. A mission absent from disk is 404; a not-hosted
/// but on-disk mission is an idempotent 200 (already free). POST (not a
/// dedicated verb) so the mutation-token gate applies by construction.
pub(crate) async fn release_mission_route(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let id = valid_id(&server, &id)?;
    let _ = parse_body(&body)?;
    if !MissionPaths::new(server.host.repo_root(), &id)
        .events_file()
        .is_file()
    {
        return Err(ApiError::not_found(format!("mission '{id}' not found")));
    }
    let released = server.host.release(&id)?;
    Ok(Json(json!({ "released": released })))
}

/// `POST /api/missions/:id/delete` — optional body `{"all": true}` (opt in to
/// deleting a Complete mission) → `200 {"deleted": true}`. See
/// [`MissionHost::clean`]. POST (not the DELETE verb) so the mutation-token
/// gate — which covers `POST /api/...` — applies by construction.
pub(crate) async fn delete_mission_route(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let id = valid_id(&server, &id)?;
    let value = parse_body(&body)?;
    let all = value.get("all").and_then(Value::as_bool).unwrap_or(false);
    server.host.clean(&id, all)?;
    Ok(Json(json!({ "deleted": true })))
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

/// `POST /api/missions/:id/merge` → `200 {"merged":true,"commit":"..."}` on
/// success. See [`MissionHost::merge`] for the non-2xx shapes (dirty tree /
/// gate failure / conflict).
pub(crate) async fn merge_mission_route(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let id = valid_id(&server, &id)?;
    Ok(Json(server.host.merge(&id).await?))
}

/// `POST /api/queue/drain` — no required body → `200 <drain-state JSON>`.
/// See [`MissionHost::drain`]; idempotent while a drain is already live.
pub(crate) async fn drain_queue_route(
    State(server): State<Arc<ServerState>>,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    let _ = parse_body(&body)?;
    Ok(Json(server.host.drain().await?))
}

/// `GET /api/queue` → `200 {"entries":[...], "busyWith": <id|null>, "drain": {...}}`.
/// See [`MissionHost::queue_state`]. Tokenless: read-only.
pub(crate) async fn queue_state_route(
    State(server): State<Arc<ServerState>>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(server.host.queue_state()))
}

/// Validate the URL id with the same traversal rules as the read endpoints.
fn valid_id(server: &ServerState, id: &str) -> Result<String, ApiError> {
    crate::rest::mission_paths(server, id)?;
    Ok(id.to_string())
}

pub(crate) fn parse_body(body: &Bytes) -> Result<Value, ApiError> {
    if body.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_slice(body)
        .map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))
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
    use kranz_engine::backend_mock::{mock_init, mock_result_text, MockBackend, MockScript};
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
            let home =
                std::env::temp_dir().join(format!("kranz-host-test-home-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&home);
            std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, &home);
        });
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
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
    async fn ask_runs_read_only_one_shot_without_creating_mission_state() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend = Arc::new(MockBackend::with_scripts(vec![MockScript::single_shot(
            "Nothing is currently blocked.",
        )]));
        let host = MissionHost::with_backend(root.clone(), backend.clone());

        let before = MissionPaths::list_missions(&root);
        let value = host.ask("what is blocked?").await.unwrap();

        assert_eq!(value["answer"], "Nothing is currently blocked.");
        assert_eq!(
            MissionPaths::list_missions(&root),
            before,
            "ask must not create or mutate mission directories"
        );
        let specs = backend.started_specs();
        assert_eq!(specs.len(), 1);
        assert!(!specs[0].writable, "ask session is read-only");
        assert_eq!(specs[0].permission_mode.as_deref(), Some("plan"));
        let prompt = match &specs[0].prompt {
            PromptMode::SingleShot(prompt) => prompt,
            other => panic!("ask must be one-shot, got {other:?}"),
        };
        assert!(prompt.contains("what is blocked?"));
        assert!(prompt.contains("## Missions"));
    }

    #[tokio::test]
    async fn contended_planning_mutex_is_409_for_turns_and_start() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);
        let id = host.create("ship it", None).await.expect("create mission");

        // Hold the per-mission engine mutex exactly like an in-flight turn.
        let cell = host.planning_cell(&id).expect("hosted planning cell");
        let _guard = cell.try_lock().expect("uncontended lock");

        let err = host
            .planning_turn(&id, "hello")
            .await
            .expect_err("turn must 409");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert!(err.message.contains("turn is in flight"), "{}", err.message);

        let err = host
            .request_plan(&id)
            .await
            .expect_err("request-plan must 409");
        assert_eq!(err.status, StatusCode::CONFLICT);

        // `start` also refuses while a turn holds the engine (the Arc clone
        // keeps try_unwrap failing) — and the entry survives the attempt.
        let err = host.start(&id).await.expect_err("start must 409");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert!(
            host.planning_cell(&id).is_ok(),
            "registry entry must survive"
        );
    }

    #[tokio::test]
    async fn start_without_an_approved_plan_is_409() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);
        let id = host.create("ship it", None).await.expect("create mission");

        let err = host
            .start(&id)
            .await
            .expect_err("start must 409 in planning");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert!(err.message.contains("no approved plan"), "{}", err.message);
        // The engine went back into the registry: planning can continue.
        assert!(host.planning_cell(&id).is_ok());
    }

    #[tokio::test]
    async fn start_is_409_when_repo_busy() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        // Hold the repo busy lock BEFORE hosting a mission — a live
        // events.jsonl.lock for the mission under test would block a
        // sibling acquire (legacy probe), so take the hold first.
        let _hold =
            kranz_engine::queue::acquire_repo_busy(&root, "m-sibling").expect("sibling busy hold");
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root.clone(), backend);
        let id = host.create("ship it", None).await.expect("create mission");
        let plan: Plan = serde_json::from_value(plan_json()).expect("plan");
        host.approve(&id, plan).await.expect("approve");

        let err = host.start(&id).await.expect_err("start must 409 when busy");
        assert_eq!(err.status, StatusCode::CONFLICT);
        assert!(
            err.message.contains("busy"),
            "expected busy conflict, got: {}",
            err.message
        );
        // Engine restored to the registry so the operator can retry.
        assert!(host.planning_cell(&id).is_ok());
    }

    #[tokio::test]
    async fn start_is_409_when_global_repository_limit_is_saturated() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let permits = Arc::new(Semaphore::new(1));
        let _other_repo = Arc::clone(&permits).try_acquire_owned().unwrap();
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let mut host = MissionHost::with_backend(root, backend);
        host.global_run_permits = Some(permits);
        let id = host.create("ship it", None).await.expect("create mission");
        let plan: Plan = serde_json::from_value(plan_json()).expect("plan");
        host.approve(&id, plan).await.expect("approve");

        let error = host.start(&id).await.expect_err("global cap must refuse");

        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(error.message.contains("maxConcurrentRepos"));
        assert!(
            host.planning_cell(&id).is_ok(),
            "refused start must restore the hosted engine"
        );
    }

    #[tokio::test]
    async fn global_run_permit_is_released_when_hosted_task_panics() {
        let permits = Arc::new(Semaphore::new(1));
        let permit = Arc::clone(&permits).try_acquire_owned().unwrap();
        assert_eq!(permits.available_permits(), 0);

        let handle = spawn_with_global_run_permit(Some(permit), async {
            panic!("simulated hosted-run panic");
        });
        assert!(handle.await.unwrap_err().is_panic());

        assert_eq!(permits.available_permits(), 1);
    }

    #[tokio::test]
    async fn sweep_idle_leaves_a_mid_turn_mission_hosted() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);
        let id = host.create("ship it", None).await.expect("create mission");

        // Hold the per-mission engine mutex exactly like an in-flight turn.
        let cell = host.planning_cell(&id).expect("hosted planning cell");
        let _guard = cell.try_lock().expect("uncontended lock");

        let released = host.sweep_idle(std::time::Duration::ZERO);
        assert!(!released.contains(&id), "{released:?}");
        assert!(
            host.planning_cell(&id).is_ok(),
            "mission must remain hosted"
        );
    }

    #[tokio::test]
    async fn release_route_is_409_mid_turn() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);
        let id = host.create("ship it", None).await.expect("create mission");

        // Hold the per-mission engine mutex exactly like an in-flight turn.
        let cell = host.planning_cell(&id).expect("hosted planning cell");
        let _guard = cell.try_lock().expect("uncontended lock");

        let app = crate::router_with_host(host, None, Some("tok".to_string()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/missions/{id}/release"))
                    .header("content-type", "application/json")
                    .header("x-kranz-token", "tok")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn bodyless_post_with_valid_token_is_not_rejected_as_unsupported_media_type() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);
        let id = host.create("ship it", None).await.expect("create mission");

        let app = crate::router_with_host(host, None, Some("tok".to_string()));
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/missions/{id}/start"))
                    // No content-type and no content-length: an empty
                    // bodyless POST, the case `curl -X POST .../start`
                    // (no `-d`) sends.
                    .header("x-kranz-token", "tok")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // A freshly created mission has no approved plan, so `start` 409s —
        // the point of this test is that it is NOT 415, i.e. the missing
        // content-type on an empty body no longer trips the media-type gate.
        assert_ne!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn bodyless_post_gate_still_rejects_non_empty_non_json_bodies() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);
        let id = host.create("ship it", None).await.expect("create mission");

        let app = crate::router_with_host(host, None, Some("tok".to_string()));
        let payload = "not json";
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/missions/{id}/release"))
                    .header("content-type", "text/plain")
                    .header("content-length", payload.len().to_string())
                    .header("x-kranz-token", "tok")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn create_rejects_an_invalid_config_patch() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);

        // 9 is out of the 1..=8 range config::validate allows (M3), so the
        // create must be rejected as a bad request. (2..=8 is now valid — it
        // opts into parallel workers — so an out-of-range value is used here.)
        let patch = json!({ "maxParallelWorkers": 9 });
        let err = host
            .create("ship it", Some(&patch))
            .await
            .expect_err("must reject");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }

    // -----------------------------------------------------------------------
    // Queue drain (roadmap f-1-2)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn empty_queue_drain_returns_ok_and_settles_idle() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);

        let body = host
            .drain()
            .await
            .expect("drain must not error on an empty queue");
        assert!(body.get("live").is_some(), "{body}");

        // The background task finds nothing queued and settles quickly.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let state = host.queue_state();
            if state["drain"]["live"] == false {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "drain never settled idle: {state}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn drain_is_409_when_global_repository_limit_is_saturated() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let permits = Arc::new(Semaphore::new(1));
        let _other_repo = Arc::clone(&permits).try_acquire_owned().unwrap();
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let mut host = MissionHost::with_backend(root, backend);
        host.global_run_permits = Some(permits);

        let error = host.drain().await.expect_err("global cap must refuse");

        assert_eq!(error.status, StatusCode::CONFLICT);
        assert!(error.message.contains("maxConcurrentRepos"));
        assert!(matches!(
            &*host.drain.lock().expect("drain tracker lock"),
            DrainSlot::Idle
        ));
    }

    #[tokio::test]
    async fn queue_state_reports_global_concurrency_saturation() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let permits = Arc::new(Semaphore::new(1));
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let mut host = MissionHost::with_backend(root, backend);
        host.global_run_permits = Some(Arc::clone(&permits));

        let open = host.queue_state();
        assert_eq!(open["maxConcurrentReposAvailable"], 1);
        assert_eq!(open["maxConcurrentReposSaturated"], false);

        let _hold = permits.try_acquire_owned().unwrap();
        let saturated = host.queue_state();
        assert_eq!(saturated["maxConcurrentReposAvailable"], 0);
        assert_eq!(saturated["maxConcurrentReposSaturated"], true);
    }

    #[tokio::test]
    async fn second_drain_while_live_returns_tracked_state_without_spawning_second() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);

        // Fabricate a live drain tracker directly — deterministic, instead
        // of racing a real queue against a fast mock backend.
        let state = Arc::new(Mutex::new(DrainState {
            live: true,
            current_mission_id: Some("m-fake".to_string()),
            ran: vec!["m-earlier".to_string()],
            parked: Vec::new(),
        }));
        let never_finishes = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        *host.drain.lock().expect("drain tracker lock") = DrainSlot::Running(DrainHandle {
            join: never_finishes,
            state: Arc::clone(&state),
        });
        let before = Arc::as_ptr(&state);

        let first = host.drain().await.expect("drain must not error");
        let second = host.drain().await.expect("drain must not error");
        assert_eq!(first, second);
        assert_eq!(first["live"], true);
        assert_eq!(first["currentMissionId"], "m-fake");
        assert_eq!(first["ran"], json!(["m-earlier"]));

        // The tracker still points at the SAME state Arc: no second task
        // was spawned to replace it.
        let after = {
            let guard = host.drain.lock().expect("drain tracker lock");
            match &*guard {
                DrainSlot::Running(handle) => Arc::as_ptr(&handle.state),
                _ => panic!("expected the tracker to still be Running"),
            }
        };
        assert_eq!(before, after, "a second drain must not replace the tracker");
    }

    #[tokio::test]
    async fn two_concurrent_cold_drains_spawn_exactly_one() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);

        // Fire two drains concurrently from a cold (Idle) tracker. Neither
        // `config::load` nor `self.backend(...)` yields here (the backend
        // is pre-populated via `with_backend`, and this test runs on the
        // default current-thread flavor), so the first call's poll runs
        // synchronously all the way through installing the `Starting`
        // reservation, spawning the task, and upgrading to `Running` before
        // the second call is ever polled. The second call therefore always
        // observes an in-progress drain (`Starting` or `Running`, task not
        // yet scheduled) and returns its tracked state instead of spawning
        // a second drain task.
        //
        // NOTE: because nothing yields here, this test alone cannot catch a
        // regression that deletes the `DrainSlot::Starting` deflection arm —
        // see `starting_reservation_is_not_overwritten_or_double_spawned`
        // below for the deterministic test that actually guards that arm.
        let (first, second) = tokio::join!(host.drain(), host.drain());
        let first = first.expect("first drain must not error");
        let second = second.expect("second drain must not error");
        assert_eq!(first["live"], true, "{first}");
        assert_eq!(second["live"], true, "{second}");

        // Exactly one drain is tracked: a single Starting-or-Running slot,
        // never two independently spawned tasks.
        match &*host.drain.lock().expect("drain tracker lock") {
            DrainSlot::Running(_) | DrainSlot::Starting(_) => {}
            DrainSlot::Idle => {
                panic!("expected a live drain to be tracked after two concurrent calls")
            }
        }

        // The single tracked drain settles idle on its own — nothing is
        // left running forever, which would indicate a leaked second task.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let state = host.queue_state();
            if state["drain"]["live"] == false {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "drain never settled idle: {state}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // -----------------------------------------------------------------------
    // Hosted-drain checkout capture/restore (mirrors `restore_work_checkout`
    // in `crates/cli/src/backlog.rs` — see `drain_task`/`restore_drain_checkout`)
    // -----------------------------------------------------------------------

    /// A fresh `DrainState` and one queued entry for `mission_id`, ready to
    /// feed [`drain_task`] directly (bypassing `tokio::spawn` for a
    /// deterministic test).
    fn seed_one_queued(root: &Path, mission_id: &str) -> Arc<Mutex<DrainState>> {
        kranz_engine::queue::enqueue(
            root,
            kranz_engine::queue::QueueEntry {
                mission_id: mission_id.to_string(),
                ticket_slug: None,
                priority: 5,
                seq: 0,
            },
        )
        .expect("enqueue");
        Arc::new(Mutex::new(DrainState::default()))
    }

    fn proceed_readiness(
        _repo_root: &Path,
        mission_id: &str,
    ) -> kranz_engine::error::Result<kranz_engine::backend_readiness::ReadinessReport> {
        Ok(kranz_engine::backend_readiness::ReadinessReport {
            mission_id: mission_id.to_string(),
            roles: Vec::new(),
            overall: kranz_engine::backend_readiness::ReadinessStatus::Ok,
            warnings: Vec::new(),
        })
    }

    #[tokio::test]
    async fn auto_work_drain_mode_processes_only_one_queue_front() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let state = seed_one_queued(&root, "m-first");
        kranz_engine::queue::enqueue(
            &root,
            kranz_engine::queue::QueueEntry {
                mission_id: "m-second".to_string(),
                ticket_slug: None,
                priority: 5,
                seq: 0,
            },
        )
        .expect("enqueue second mission");

        drain_task_with_probe(
            root.clone(),
            Arc::clone(&state),
            true,
            |_mission_id| async { Ok(0) },
            proceed_readiness,
        )
        .await;

        assert_eq!(state.lock().expect("drain state lock").ran, ["m-first"]);
        let remaining = kranz_engine::queue::list(&root);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].mission_id, "m-second");
    }

    #[tokio::test]
    async fn hosted_drain_restores_dispatch_checkout() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let state = seed_one_queued(&root, "m-restore");

        let run_root = root.clone();
        drain_task_with_probe(
            root.clone(),
            Arc::clone(&state),
            false,
            move |mission_id| {
                let root = run_root.clone();
                async move {
                    let git = GitRepo::open(&root)?;
                    let branch = format!("kranz/mission-{mission_id}");
                    git.create_branch(&branch, None)?;
                    git.checkout(&branch)?;
                    Ok(0)
                }
            },
            proceed_readiness,
        )
        .await;

        assert_eq!(
            state.lock().expect("drain state lock").ran,
            ["m-restore"],
            "the injected mission runner must execute"
        );

        let git = GitRepo::open(&root).expect("open repo");
        assert_eq!(
            git.current_branch().expect("current branch"),
            "main",
            "the operator's dispatch-time checkout must be restored on drain exit"
        );
    }

    #[tokio::test]
    async fn hosted_drain_restores_dispatch_checkout_on_err() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let state = seed_one_queued(&root, "m-err-restore");

        let run_root = root.clone();
        let runner_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let called = Arc::clone(&runner_called);
        drain_task_with_probe(
            root.clone(),
            state,
            false,
            move |mission_id| {
                let root = run_root.clone();
                let called = Arc::clone(&called);
                async move {
                    called.store(true, std::sync::atomic::Ordering::SeqCst);
                    let git = GitRepo::open(&root)?;
                    let branch = format!("kranz/mission-{mission_id}");
                    git.create_branch(&branch, None)?;
                    git.checkout(&branch)?;
                    Err(anyhow::anyhow!("simulated drain runner failure"))
                }
            },
            proceed_readiness,
        )
        .await;

        assert!(
            runner_called.load(std::sync::atomic::Ordering::SeqCst),
            "the injected mission runner must execute"
        );

        let git = GitRepo::open(&root).expect("open repo");
        assert_eq!(
            git.current_branch().expect("current branch"),
            "main",
            "an errored drain must still restore the operator's dispatch-time checkout"
        );
    }

    #[tokio::test]
    async fn hosted_drain_skips_restore_when_started_on_mission_branch() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        {
            let git = GitRepo::open(&root).expect("open repo");
            git.create_branch("kranz/mission-existing", None)
                .expect("create existing mission branch");
            git.checkout("kranz/mission-existing")
                .expect("checkout existing mission branch");
        }
        let state = seed_one_queued(&root, "m-skip");

        drain_task_with_probe(
            root.clone(),
            Arc::clone(&state),
            false,
            |_mission_id| async { Ok(0) },
            proceed_readiness,
        )
        .await;

        assert_eq!(
            state.lock().expect("drain state lock").ran,
            ["m-skip"],
            "the injected mission runner must execute"
        );

        let git = GitRepo::open(&root).expect("open repo");
        assert_eq!(
            git.current_branch().expect("current branch"),
            "kranz/mission-existing",
            "started on a mission branch: no restore must be attempted"
        );
    }

    #[tokio::test]
    async fn hosted_drain_leaves_checkout_when_tracked_tree_dirty() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let state = seed_one_queued(&root, "m-dirty");

        let run_root = root.clone();
        drain_task_with_probe(
            root.clone(),
            Arc::clone(&state),
            false,
            move |mission_id| {
                let root = run_root.clone();
                async move {
                    let git = GitRepo::open(&root)?;
                    let branch = format!("kranz/mission-{mission_id}");
                    git.create_branch(&branch, None)?;
                    git.checkout(&branch)?;
                    std::fs::write(root.join("README.md"), "dirty tracked edit\n")?;
                    Ok(0)
                }
            },
            proceed_readiness,
        )
        .await;

        assert_eq!(
            state.lock().expect("drain state lock").ran,
            ["m-dirty"],
            "the injected mission runner must execute"
        );

        let git = GitRepo::open(&root).expect("open repo");
        assert_eq!(
            git.current_branch().expect("current branch"),
            "kranz/mission-m-dirty",
            "a dirty tracked tree must abort the restore, leaving the checkout on the mission \
             branch"
        );
    }

    #[tokio::test]
    async fn hosted_drain_second_call_does_not_capture_or_restore() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        {
            let git = GitRepo::open(&root).expect("open repo");
            git.create_branch("feature-branch", None)
                .expect("create feature branch");
            git.checkout("feature-branch")
                .expect("checkout feature branch");
        }
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root.clone(), backend);

        // Fabricate a live drain tracker (same pattern as
        // `second_drain_while_live_returns_tracked_state_without_spawning_second`)
        // so the idempotent early-return path is exercised without racing a
        // real spawn.
        let tracked_state = Arc::new(Mutex::new(DrainState {
            live: true,
            current_mission_id: Some("m-inflight".to_string()),
            ran: Vec::new(),
            parked: Vec::new(),
        }));
        let never_finishes = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        *host.drain.lock().expect("drain tracker lock") = DrainSlot::Running(DrainHandle {
            join: never_finishes,
            state: Arc::clone(&tracked_state),
        });

        let result = host
            .drain()
            .await
            .expect("second drain call must not error");
        assert_eq!(result["live"], true, "{result}");

        // No capture/restore happened: the checkout this test set up before
        // the second call is untouched.
        let git = GitRepo::open(&root).expect("open repo");
        assert_eq!(
            git.current_branch().expect("current branch"),
            "feature-branch",
            "the idempotent second drain() must not mutate the checkout"
        );

        // No second task was spawned: the tracker still points at the same
        // state Arc installed above.
        match &*host.drain.lock().expect("drain tracker lock") {
            DrainSlot::Running(handle) => {
                assert_eq!(
                    Arc::as_ptr(&handle.state),
                    Arc::as_ptr(&tracked_state),
                    "a second drain must not replace the tracker or spawn a second task"
                );
            }
            _ => panic!("expected the tracker to still be Running"),
        };
    }

    /// Deterministically guards the `DrainSlot::Starting(state) => return
    /// ...` deflection arm in [`MissionHost::drain`]: manually install a
    /// `Starting` reservation, call `drain()`, and assert it returns the
    /// tracked live state WITHOUT overwriting the slot or spawning a task.
    /// If that match arm is deleted (falling through to the Idle/Running
    /// catch-all), this test fails because the slot gets overwritten with a
    /// fresh `Starting`/`Running` reservation (different `Arc::as_ptr`) and a
    /// real drain task gets spawned against this test's (git-less) repo.
    #[tokio::test]
    async fn starting_reservation_is_not_overwritten_or_double_spawned() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);

        let state = Arc::new(Mutex::new(DrainState {
            live: true,
            current_mission_id: Some("m-reserved".to_string()),
            ran: Vec::new(),
            parked: Vec::new(),
        }));
        *host.drain.lock().expect("drain tracker lock") = DrainSlot::Starting(Arc::clone(&state));
        let before = Arc::as_ptr(&state);

        let result = host.drain().await.expect("drain must not error");
        assert_eq!(result["live"], true, "{result}");
        assert_eq!(result["currentMissionId"], "m-reserved");

        // The slot must STILL be the same Starting reservation: not
        // overwritten to a new Starting/Running, and no task spawned.
        let after = match &*host.drain.lock().expect("drain tracker lock") {
            DrainSlot::Starting(tracked) => Arc::as_ptr(tracked),
            DrainSlot::Running(_) => panic!(
                "the Starting reservation was upgraded/replaced by this call — the deflection \
                 arm was bypassed and a second drain was spawned"
            ),
            DrainSlot::Idle => panic!("the Starting reservation was cleared by this call"),
        };
        assert_eq!(
            before, after,
            "drain() must return the SAME tracked reservation, not install a new one"
        );
    }

    /// A failed drain construction (here: an unparseable `.kranz/config.json`)
    /// must clear the reservation back to `Idle` so a later call can retry —
    /// otherwise every future drain would deflect forever onto a dead
    /// reservation that no task will ever settle.
    #[tokio::test]
    async fn failed_drain_construction_clears_the_reservation_to_idle() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        std::fs::create_dir_all(root.join(".kranz")).expect("mkdir .kranz");
        std::fs::write(root.join(".kranz").join("config.json"), "not json")
            .expect("write malformed config");

        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);

        host.drain()
            .await
            .expect_err("malformed config must fail drain construction");

        let is_idle = matches!(
            &*host.drain.lock().expect("drain tracker lock"),
            DrainSlot::Idle
        );
        assert!(
            is_idle,
            "a failed drain construction must reset the tracker to Idle"
        );
    }

    /// `queue_state()` must report the transient `Starting` reservation
    /// window as a live drain — a caller polling `GET /api/queue` right after
    /// `POST /api/queue/drain` must not observe a false "not live" gap.
    #[tokio::test]
    async fn queue_state_reports_a_starting_reservation_as_live() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);

        let state = Arc::new(Mutex::new(DrainState {
            live: true,
            current_mission_id: Some("m-starting".to_string()),
            ran: Vec::new(),
            parked: Vec::new(),
        }));
        *host.drain.lock().expect("drain tracker lock") = DrainSlot::Starting(state);

        let queue_state = host.queue_state();
        assert_eq!(queue_state["drain"]["live"], true, "{queue_state}");
        assert_eq!(queue_state["drain"]["currentMissionId"], "m-starting");
    }

    #[tokio::test]
    async fn queue_drain_route_requires_token_but_queue_route_does_not() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let Some((_dir, root)) = init_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root, backend);
        let app = crate::router_with_host(host, None, Some("tok".to_string()));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/queue/drain")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/queue/drain")
                    .header("x-kranz-token", "tok")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/queue")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    // -----------------------------------------------------------------------
    // autoWork watcher (roadmap f-2-3)
    // -----------------------------------------------------------------------

    #[test]
    fn should_auto_drain_truth_table() {
        // Only true when all three conditions line up.
        assert!(should_auto_drain(true, true, false));
        // autoWork off: never drain, regardless of the queue or live state.
        assert!(!should_auto_drain(false, true, false));
        assert!(!should_auto_drain(false, false, false));
        // Queue empty: nothing to drain even with autoWork on.
        assert!(!should_auto_drain(true, false, false));
        // A drain is already live: never start a second one.
        assert!(!should_auto_drain(true, true, true));
        assert!(!should_auto_drain(false, false, true));
    }

    /// Writes `{"autoWork": enabled}` to the repo's `.kranz/config.json`
    /// (the project config layer `config::load` reads on every call,
    /// including the watcher's per-tick reload).
    fn write_auto_work_config(root: &std::path::Path, enabled: bool) {
        let dir = root.join(".kranz");
        std::fs::create_dir_all(&dir).expect("create .kranz dir");
        std::fs::write(
            dir.join("config.json"),
            json!({ "autoWork": enabled }).to_string(),
        )
        .expect("write config.json");
    }

    /// One orchestrator turn batch: text + matching Result (mirrors the
    /// identical helper in `tests/host_test.rs`).
    fn turn(reply: &str) -> Vec<kranz_engine::backend::AgentEvent> {
        vec![
            kranz_engine::backend_mock::mock_text(reply),
            mock_result_text(reply),
        ]
    }

    /// A completed single-shot preflight probe session whose reply
    /// authenticates, consumed once by `MissionEngine::worker_auth_verdict`
    /// before the first worker/validator session of the mission spawns.
    fn preflight_authenticated_script() -> MockScript {
        MockScript::single_shot("ack")
    }

    /// Worker script: completed single-shot run with a passing WorkerReport.
    fn worker_pass() -> MockScript {
        MockScript::single_shot_json(&json!({
            "result": "pass",
            "summary": "implemented and tested",
            "filesTouched": [],
            "testsAdded": [],
            "testEvidence": "all green",
            "commits": []
        }))
    }

    /// A minimal one-milestone/one-feature plan in wire (camelCase) shape.
    fn plan_json() -> Value {
        json!({
            "goal": "ship the demo",
            "validationContract": [],
            "milestones": [{
                "title": "M1",
                "features": [{
                    "title": "F1",
                    "spec": "build the thing",
                    "validationCriteria": ["it works"]
                }]
            }]
        })
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auto_work_tick_drains_a_queued_mission_when_enabled() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        write_auto_work_config(&root, true);

        let judgement =
            json!({ "decision": "complete", "guidance": "", "summary": "worker did the job" });
        let orch = MockScript::streaming(vec![mock_init("orch-auto"), mock_result_text("seed-hi")])
            .responding(vec![
                turn("scoping the demo"),
                turn(&plan_json().to_string()),
            ]);
        let orch_run = MockScript::streaming(vec![
            mock_init("orch-auto-run"),
            mock_result_text("resumed"),
        ])
        .responding(vec![turn(&judgement.to_string()), turn("NONE")]);
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::with_scripts(vec![
            orch,
            preflight_authenticated_script(),
            worker_pass(),
            orch_run,
        ]));
        let host = MissionHost::with_backend(root.clone(), backend);

        let id = host
            .create(
                "drain me via autoWork",
                Some(&json!({ "skipScrutiny": true, "skipFunctional": true })),
            )
            .await
            .expect("create mission");
        host.planning_turn(&id, "go").await.expect("planning turn");
        let plan_body = host.request_plan(&id).await.expect("request plan");
        assert_eq!(plan_body["ready"], true, "{plan_body}");
        let plan: Plan =
            serde_json::from_value(plan_body["plan"].clone()).expect("plan deserializes");
        host.approve(&id, plan).await.expect("approve");
        host.release(&id).expect("release");

        kranz_engine::queue::enqueue(
            &root,
            kranz_engine::queue::QueueEntry {
                mission_id: id.clone(),
                ticket_slug: None,
                priority: 2,
                seq: 0,
            },
        )
        .expect("enqueue");

        // No explicit drain()/POST call — the watcher's tick alone must
        // notice the queued entry and kick a drain off.
        host.auto_work_tick().await;
        assert!(
            host.drain_is_live(),
            "autoWork tick with autoWork=true and a non-empty queue must start a drain"
        );

        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let state = host.queue_state();
            if state["entries"]
                .as_array()
                .map(|a| a.is_empty())
                .unwrap_or(false)
                && state["drain"]["live"] == false
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "autoWork drain never completed: {state}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn auto_work_tick_leaves_the_queue_untouched_when_disabled() {
        let Some((_dir, root)) = init_repo() else {
            return;
        };
        // Absent key: default is false, exercised the same as an explicit
        // `{"autoWork": false}` layer. No mission needs to actually be
        // runnable here — the watcher must never even attempt a drain, so a
        // bare queue entry is enough to prove it's left alone.
        let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
        let host = MissionHost::with_backend(root.clone(), backend);

        kranz_engine::queue::enqueue(
            &root,
            kranz_engine::queue::QueueEntry {
                mission_id: "m-untouched".to_string(),
                ticket_slug: None,
                priority: 2,
                seq: 0,
            },
        )
        .expect("enqueue");

        host.auto_work_tick().await;

        assert!(
            !host.drain_is_live(),
            "autoWork=false must never start a drain"
        );
        let entries = kranz_engine::queue::list(&root);
        assert_eq!(
            entries.len(),
            1,
            "queue entry must be left untouched when autoWork is disabled: {entries:?}"
        );
        assert_eq!(entries[0].mission_id, "m-untouched");
    }
}
