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
use kranz_engine::deps;
use kranz_engine::draft::{drive_draft, DraftOutcome};
use kranz_engine::error::EngineError;
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::orchestrator::{MissionEngine, PlanRequest};
use kranz_engine::paths::MissionPaths;
use kranz_engine::ticket::Ticket;
use kranz_engine::types::{MissionConfig, MissionStatus, Plan};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    /// The lazily-spawned idle-release background task, started at most once
    /// (see [`MissionHost::ensure_sweeper_started`]).
    sweeper: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The single tracked background queue drain, if one has ever been
    /// started (see [`MissionHost::drain`]).
    drain: Mutex<Option<DrainHandle>>,
}

/// One background drain task's observable progress — shared between the task
/// (which updates it as it goes) and [`MissionHost::drain`] /
/// [`MissionHost::queue_state`] (which read it back as JSON).
#[derive(Debug, Clone, Default)]
struct DrainState {
    live: bool,
    current_mission_id: Option<String>,
    ran: Vec<String>,
}

fn drain_state_json(state: &DrainState) -> Value {
    json!({
        "live": state.live,
        "currentMissionId": state.current_mission_id,
        "ran": state.ran,
    })
}

/// A tracked background drain: the task handle plus the state it shares with
/// this host. `join.is_finished()` is how [`MissionHost::drain`] decides
/// whether a tracked drain is still live.
struct DrainHandle {
    join: tokio::task::JoinHandle<()>,
    state: Arc<Mutex<DrainState>>,
}

impl MissionHost {
    /// Host for `repo_root`, discovering the Claude backend on first use.
    pub fn new(repo_root: PathBuf) -> Self {
        MissionHost {
            repo_root,
            backend: tokio::sync::OnceCell::new(),
            missions: Arc::new(Mutex::new(HashMap::new())),
            sweeper: Mutex::new(None),
            drain: Mutex::new(None),
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
            drain: Mutex::new(None),
        }
    }

    /// The repository this host creates missions in.
    pub fn repo_root(&self) -> &PathBuf {
        &self.repo_root
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
            PlanRequest::NotReady(reply) => {
                Ok(json!({ "ready": false, "reply": prepend_seed(seed, reply) }))
            }
        }
    }

    /// `POST /api/missions/:id/approve`: commit plan.json/plan.md/index.md on
    /// the mission branch exactly like the CLI. Returns the mission branch.
    pub async fn approve(&self, id: &str, plan: Plan) -> Result<String, ApiError> {
        let cell = self.planning_cell_or_attach(id).await?;
        let mut engine = try_lock(&cell)?;
        engine.approve_plan(plan)?;
        Ok(engine.state().mission.mission_branch.clone())
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

        let engine = match taken {
            Some(engine) => engine,
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
            Some(HostedMission::Running(handle)) => {
                if !handle.is_finished() {
                    handle.abort();
                }
                // Cancelled or finished either way: await settles the task so
                // the engine is dropped (log flushed, lock freed) before we
                // append the abandon event.
                let _ = handle.await;
            }
        }
        kranz_engine::orchestrator::abandon_mission(
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
        use kranz_engine::orchestrator::{
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
        {
            let guard = self.drain.lock().expect("drain tracker lock");
            if let Some(handle) = guard.as_ref() {
                if !handle.join.is_finished() {
                    return Ok(drain_state_json(
                        &handle.state.lock().expect("drain state lock"),
                    ));
                }
            }
        }

        let cfg = config::load(&self.repo_root)?;
        let backend = self.backend(cfg.claude_binary.as_deref()).await?;
        let repo_root = self.repo_root.clone();

        let state = Arc::new(Mutex::new(DrainState {
            live: true,
            current_mission_id: None,
            ran: Vec::new(),
        }));
        let task_state = Arc::clone(&state);
        let join = tokio::spawn(async move {
            let result = kranz_engine::work::drain_queue(&repo_root, false, |mission_id| {
                let backend = Arc::clone(&backend);
                let repo_root = repo_root.clone();
                let state = Arc::clone(&task_state);
                async move {
                    state.lock().expect("drain state lock").current_mission_id =
                        Some(mission_id.clone());
                    let outcome = run_mission_headless(backend, repo_root, mission_id.clone()).await;
                    let mut guard = state.lock().expect("drain state lock");
                    guard.current_mission_id = None;
                    if outcome.is_ok() {
                        guard.ran.push(mission_id);
                    }
                    outcome
                }
            })
            .await;
            if let Err(e) = result {
                tracing::error!(error = %e, "hosted queue drain errored");
            }
            task_state.lock().expect("drain state lock").live = false;
        });

        let initial = drain_state_json(&state.lock().expect("drain state lock"));
        *self.drain.lock().expect("drain tracker lock") = Some(DrainHandle { join, state });
        Ok(initial)
    }

    /// `GET /api/queue`: the queue front-to-back, who (if anyone) currently
    /// holds the busy lock, and this host's own drain tracker.
    pub fn queue_state(&self) -> Value {
        let entries = kranz_engine::queue::list(&self.repo_root);
        let busy_with = kranz_engine::queue::is_repo_busy(&self.repo_root);
        let drain = match self.drain.lock().expect("drain tracker lock").as_ref() {
            Some(handle) => drain_state_json(&handle.state.lock().expect("drain state lock")),
            None => drain_state_json(&DrainState::default()),
        };
        json!({
            "entries": entries,
            "busyWith": busy_with,
            "drain": drain,
        })
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
            Some(HostedMission::Running(_)) => Err(ApiError::conflict(format!(
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
    missions
        .lock()
        .expect("missions registry lock")
        .remove(&mission_id);
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
        DraftOutcome::NeedsContext {
            mission_id,
            questions,
        } => json!({
            "outcome": "needsContext",
            "missionId": mission_id,
            "questions": questions,
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
        Some(HostedMission::Running(handle)) => {
            let finished = handle.is_finished();
            if !finished {
                map.insert(id.to_string(), HostedMission::Running(handle));
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
                HostedMission::Running(_) => None,
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
        }));
        let never_finishes = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        *host.drain.lock().expect("drain tracker lock") = Some(DrainHandle {
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
            Arc::as_ptr(&guard.as_ref().unwrap().state)
        };
        assert_eq!(before, after, "a second drain must not replace the tracker");
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
}
