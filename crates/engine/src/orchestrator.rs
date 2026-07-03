//! Mission engine — the orchestrator loop (plan §4.5).
//!
//! [`MissionEngine`] owns the single-writer event log, the reduced state, the
//! git repo, and one long-lived streaming orchestrator session. Every event
//! goes through [`MissionEngine::emit`] (append → reduce → snapshot) so
//! log/state/snapshot never drift; events appended directly by
//! [`runner::run_worker`]/[`runner::run_validator`] are folded back in through
//! [`MissionEngine::catch_up`] immediately after each run.
//!
//! ## Orchestrator session protocol
//!
//! The real backend ([`crate::backend_claude`]) writes the
//! [`PromptMode::Streaming`] initial prompt as the first stdin user message,
//! and *every* user message — the initial one included — runs one turn ending
//! in its own `Result` event. The engine therefore keeps a strict 1:1 send/
//! pump discipline:
//!
//! 1. `ensure_orchestrator` starts the session with the *seed* as the
//!    streaming initial prompt (planning intro during Planning, a resume nudge
//!    when resuming a previous sdk session, [`digest::render_reseed`]
//!    otherwise) and pumps that seed turn to its `Result`.
//! 2. Every subsequent turn is `send_user_message(digest + message)` followed
//!    by a pump to the next `Result` (plan §4.8: the engine owns state, the
//!    digest re-grounds every turn).
//!
//! If the stream closes or stalls mid-turn (see `orch_stall_timeout`), the
//! session is dropped and the turn retried once against a fresh re-seeded
//! session; a second consecutive failure is [`EngineError::Backend`].
//!
//! ## Git hygiene
//!
//! The engine's own bookkeeping (`events.jsonl`, `state.json`, `control/`,
//! `runs/`) lives inside the repo under `.kranz/` and churns constantly, so
//! the engine writes a `.kranz/.gitignore` covering exactly those files.
//! `plan.json` is deliberately *not* ignored — it is committed to the mission
//! branch at approval (plan §4.4). This keeps the §4.4 dirty-tree discipline
//! meaningful: a dirty tree after a worker run is *worker* dirt.

use crate::backend::{AgentBackend, AgentEvent, AgentSession, PromptMode, SessionSpec};
use crate::config;
use crate::control;
use crate::digest;
use crate::error::{EngineError, Result};
use crate::event_log::EventLog;
use crate::events::{Event, EventKind};
use crate::git_ops::GitRepo;
use crate::paths::MissionPaths;
use crate::permissions;
use crate::prompts;
use crate::reducer;
use crate::runner;
use crate::scrub;
use crate::types::*;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// Max chars of an `orchestrator.decision` summary (matches digest cap).
const DECISION_SUMMARY_MAX: usize = 200;

/// Max chars of `worker.message` content (mirrors the runner's cap).
const MESSAGE_CONTENT_MAX: usize = 2000;

/// Sleep between loop iterations while paused (§4.5 step b).
const PAUSE_POLL: Duration = Duration::from_millis(300);

/// Poll interval of the interrupt watcher during worker runs.
const INTERRUPT_POLL: Duration = Duration::from_millis(150);

/// Hard cap on one contract `command` assertion at the final gate.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

/// Default cap on the silence between two orchestrator stream events before
/// the session is declared dead (long thinking pauses are expected; ten
/// minutes of *nothing* on a stream-json pipe is not).
const DEFAULT_ORCH_STALL_TIMEOUT: Duration = Duration::from_secs(600);

/// Retry nudge sent when a JSON decision turn fails to parse.
const JSON_RETRY_MSG: &str =
    "Your previous reply was not parseable. Output ONLY the requested JSON object — \
     no prose, no code fences, nothing else.";

/// Tail kept from a failed contract command's output.
const COMMAND_OUTPUT_TAIL: usize = 1500;

// ---------------------------------------------------------------------------
// JSON decision shapes (parsed leniently via runner::parse_report)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JudgementDecision {
    decision: String,
    #[serde(default)]
    guidance: String,
    #[serde(default)]
    summary: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DirtyTreeDecision {
    action: String,
    #[serde(default)]
    note: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnblockDecision {
    action: String,
    #[serde(default)]
    note: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixFeatureSpec {
    title: String,
    spec: String,
    #[serde(default)]
    validation_criteria: Vec<String>,
}

/// One finding the orchestrator waived instead of converting (conversion
/// turn, §4.5 g).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WaivedFinding {
    subject: String,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixFeaturesDecision {
    #[serde(default)]
    fix_features: Vec<FixFeatureSpec>,
    /// Findings waived with a justification instead of fixed. Defaulted so
    /// old-shape answers (fixFeatures + summary only) still parse.
    #[serde(default)]
    waived: Vec<WaivedFinding>,
    #[serde(default)]
    summary: String,
}

/// What the findings-conversion turn decided (see [`MissionEngine::convert_findings`]).
enum FindingsConversion {
    /// Convert into fix features. Unparseable answers and answers that
    /// neither fix nor waive land here with specs synthesized 1:1 from the
    /// findings — the conservative default.
    Fix { specs: Vec<FixFeatureSpec>, summary: String, text: String },
    /// Every finding waived, each with a one-line justification.
    Waive { waived: Vec<WaivedFinding> },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Verdict {
    id: String,
    pass: bool,
    #[serde(default)]
    evidence: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VerdictsDecision {
    #[serde(default)]
    verdicts: Vec<Verdict>,
    #[serde(default)]
    summary: String,
}

// ---------------------------------------------------------------------------
// MissionEngine
// ---------------------------------------------------------------------------

/// The mission engine: composes the event log, reducer state, git repo,
/// runner, control inbox, and the long-lived orchestrator session into the
/// §4.5 loop.
pub struct MissionEngine {
    backend: Arc<dyn AgentBackend>,
    paths: MissionPaths,
    log: EventLog,
    state: MissionState,
    repo: GitRepo,
    /// Long-lived streaming orchestrator session (lazy; None until needed).
    orch: Option<Box<dyn AgentSession>>,
    /// Sdk session id of the current/most recent orchestrator session, used
    /// for `--resume` across engine restarts.
    orch_session_id: Option<String>,
    /// Run id (`orch-<n>`) of the live orchestrator session.
    orch_run_id: Option<String>,
    /// Open transcript file of the live orchestrator session.
    orch_transcript: Option<std::fs::File>,
    /// See [`DEFAULT_ORCH_STALL_TIMEOUT`]; shrunk by tests.
    orch_stall_timeout: Duration,
}

impl MissionEngine {
    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

    /// Create a brand-new mission: validate config, open the repo, pick a
    /// mission id, acquire the event log, and emit `mission.created`.
    pub fn create(
        backend: Arc<dyn AgentBackend>,
        repo_root: impl Into<PathBuf>,
        goal: &str,
        cfg: MissionConfig,
    ) -> Result<Self> {
        config::validate(&cfg)?;
        let repo_root = canonical_root(repo_root.into());
        let repo = GitRepo::open(&repo_root)?;
        repo.ensure_identity()?;

        let mission_id = format!("m-{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
        let paths = MissionPaths::new(&repo_root, &mission_id);
        write_kranz_gitignore(&paths)?;

        let mut log = EventLog::acquire(
            &paths,
            &mission_id,
            Duration::from_millis(cfg.event_stream_throttle_ms),
            false,
        )?;

        let base_branch = repo.current_branch()?;
        let mission_branch = format!("kranz/mission-{mission_id}");
        let created = log.append(EventKind::MissionCreated {
            goal: goal.to_string(),
            base_branch,
            mission_branch,
            config: cfg,
        })?;
        let state = reducer::fold(std::slice::from_ref(&created))?;
        reducer::write_snapshot(&state, &paths.state_file())?;

        Ok(MissionEngine {
            backend,
            paths,
            log,
            state,
            repo,
            orch: None,
            orch_session_id: None,
            orch_run_id: None,
            orch_transcript: None,
            orch_stall_timeout: DEFAULT_ORCH_STALL_TIMEOUT,
        })
    }

    /// Resume an existing mission from its event log (§4.3 kill-safety).
    ///
    /// Rebuilds state by folding the log, re-acquires the single-writer lock
    /// (`force_lock` steals a stale one), and remembers the sdk session id of
    /// the most recent orchestrator session for `--resume`. No agent session
    /// is started here — sessions are lazy.
    pub fn resume(
        backend: Arc<dyn AgentBackend>,
        repo_root: impl Into<PathBuf>,
        mission_id: &str,
        force_lock: bool,
    ) -> Result<Self> {
        let repo_root = canonical_root(repo_root.into());
        let repo = GitRepo::open(&repo_root)?;
        repo.ensure_identity()?;

        let paths = MissionPaths::new(&repo_root, mission_id);
        write_kranz_gitignore(&paths)?;

        let events = EventLog::read_events(&paths.events_file())?;
        let state = reducer::fold(&events)?;

        // The most recent orchestrator session's sdk id (events are in seq
        // order, so the last matching worker.spawned wins).
        let orch_session_id = events.iter().rev().find_map(|e| match &e.kind {
            EventKind::WorkerSpawned { role: Role::Orchestrator, sdk_session_id, .. } => {
                Some(sdk_session_id.clone())
            }
            _ => None,
        });

        let log = EventLog::acquire(
            &paths,
            mission_id,
            Duration::from_millis(state.config.event_stream_throttle_ms),
            force_lock,
        )?;
        reducer::write_snapshot(&state, &paths.state_file())?;

        Ok(MissionEngine {
            backend,
            paths,
            log,
            state,
            repo,
            orch: None,
            orch_session_id,
            orch_run_id: None,
            orch_transcript: None,
            orch_stall_timeout: DEFAULT_ORCH_STALL_TIMEOUT,
        })
    }

    // -----------------------------------------------------------------------
    // Accessors / test hooks
    // -----------------------------------------------------------------------

    /// Current reduced state (read-only).
    pub fn state(&self) -> &MissionState {
        &self.state
    }

    /// Mission id.
    pub fn mission_id(&self) -> &str {
        &self.state.mission.id
    }

    /// Mission data paths.
    pub fn paths(&self) -> &MissionPaths {
        &self.paths
    }

    /// Shrink the orchestrator stall timeout (tests exercise the death/reseed
    /// path without waiting ten minutes).
    pub fn set_orch_stall_timeout(&mut self, timeout: Duration) {
        self.orch_stall_timeout = timeout;
    }

    /// Test hook (plan §4.8 acceptance): drop the live orchestrator session
    /// and forget its sdk id, so the next turn takes the fresh re-seed path
    /// (digest + plan.json). Behaviour must not visibly change.
    ///
    /// Dropping the boxed session kills the real CLI child via
    /// `kill_on_drop`; the mock simply drops.
    pub fn force_reseed(&mut self) {
        self.orch = None;
        self.orch_run_id = None;
        self.orch_transcript = None;
        self.orch_session_id = None;
    }

    // -----------------------------------------------------------------------
    // emit / catch_up — the log/state/snapshot lockstep
    // -----------------------------------------------------------------------

    /// Append one event, fold it into state, and refresh the snapshot.
    ///
    /// The snapshot write is mandatory for lifecycle events and best-effort
    /// for `worker.message` stream deltas (recoverable by refolding the log).
    fn emit(&mut self, kind: EventKind) -> Result<Event> {
        let event = self.log.append(kind)?;
        reducer::apply(&mut self.state, &event)?;
        let snapshot = reducer::write_snapshot(&self.state, &self.paths.state_file());
        if event.kind.is_stream_delta() {
            if let Err(e) = snapshot {
                tracing::debug!(error = %e, "best-effort snapshot write failed on stream delta");
            }
        } else {
            snapshot?;
        }
        Ok(event)
    }

    /// Append one `orchestrator.decision`, credential-scrubbing both fields:
    /// summary and detail carry (snippets of) model-authored turn text, which
    /// must never reach events.jsonl unredacted. The summary is additionally
    /// truncated to [`DECISION_SUMMARY_MAX`] (scrub first, so truncation can
    /// never split a secret into an unrecognized prefix).
    fn emit_decision(&mut self, summary: &str, detail: Option<String>) -> Result<()> {
        self.emit(EventKind::OrchestratorDecision {
            summary: scrub::scrub_and_truncate(summary, DECISION_SUMMARY_MAX),
            detail: detail.map(|d| scrub::scrub(&d)),
        })?;
        Ok(())
    }

    /// Fold events appended by `runner::run_*` (which writes to the log
    /// directly) into engine state. Must be called immediately after every
    /// runner invocation, before any further `emit`.
    fn catch_up(&mut self) -> Result<()> {
        self.log.flush()?;
        let events = EventLog::read_events_after(self.log.events_path(), self.state.last_seq)?;
        for event in &events {
            reducer::apply(&mut self.state, event)?;
        }
        reducer::write_snapshot(&self.state, &self.paths.state_file())?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Planning API (Phase D CLI)
    // -----------------------------------------------------------------------

    /// One conversational planning turn: ensure the orchestrator session
    /// exists (seeded for planning), send the user's text, and return the
    /// assistant's full response text.
    pub async fn planning_turn(&mut self, user_text: &str) -> Result<String> {
        self.orch_turn(user_text).await
    }

    /// Demand the plan JSON (types::Plan, camelCase). Lenient parse with one
    /// retry; the plan is returned unapproved.
    pub async fn request_plan(&mut self) -> Result<Plan> {
        let message = format!(
            "Emit the plan now. Output ONLY a JSON object conforming exactly to this JSON \
             Schema — no prose before or after:\n{}\n",
            plan_schema()
        );
        let (plan, _text) = self.json_decision::<Plan>(&message).await?;
        plan.ok_or_else(|| {
            EngineError::Backend("orchestrator did not produce a parseable plan JSON".to_string())
        })
    }

    /// Approve a plan: normalize it, create + check out the mission branch,
    /// write and commit `plan.json` (the engine writes and commits — the
    /// orchestrator never touches files, plan §4.4), and emit `plan.approved`.
    pub fn approve_plan(&mut self, mut plan: Plan) -> Result<()> {
        if self.state.mission.status != MissionStatus::Planning {
            return Err(EngineError::InvalidState(format!(
                "approve_plan requires Planning status, mission is {:?}",
                self.state.mission.status
            )));
        }
        if plan.milestones.is_empty() {
            return Err(EngineError::InvalidState("plan has no milestones".to_string()));
        }
        if let Some(empty) = plan.milestones.iter().find(|m| m.features.is_empty()) {
            return Err(EngineError::InvalidState(format!(
                "plan milestone '{}' has no features",
                empty.title
            )));
        }
        assign_assertion_ids(&mut plan.validation_contract);

        // Git first: if anything fails here, no event was emitted and
        // approve_plan can simply be retried.
        let base = self.state.mission.base_branch.clone();
        let branch = self.state.mission.mission_branch.clone();
        if !self.repo.branch_exists(&branch)? {
            self.repo.create_branch(&branch, Some(&base))?;
        }
        self.repo.checkout(&branch)?;

        let plan_file = self.paths.plan_file();
        if let Some(parent) = plan_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&plan_file, serde_json::to_string_pretty(&plan)?)?;
        self.repo.commit_paths(
            &[plan_file.as_path()],
            &format!("[kranz] approved plan for {}", self.state.mission.id),
        )?;

        self.emit(EventKind::PlanApproved { plan })?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // run() — THE LOOP (plan §4.5)
    // -----------------------------------------------------------------------

    /// Drive the mission until it is Complete or Failed (returned), Blocked
    /// (returned so the user can intervene), or the process is killed (safe:
    /// the log is the source of truth). Paused missions loop in place,
    /// draining the control inbox, until a Resume arrives.
    pub async fn run(&mut self) -> Result<MissionStatus> {
        if self.state.mission.status == MissionStatus::Planning {
            return Err(EngineError::InvalidState(
                "cannot run a mission whose plan is not approved".to_string(),
            ));
        }
        loop {
            // (a) drain the control inbox.
            self.drain_control()?;

            match self.state.mission.status {
                MissionStatus::Complete => return Ok(MissionStatus::Complete),
                MissionStatus::Failed => return Ok(MissionStatus::Failed),
                // (b) paused: idle-drain until resumed.
                MissionStatus::Paused => {
                    tokio::time::sleep(PAUSE_POLL).await;
                    continue;
                }
                _ => {}
            }

            // (d) first incomplete milestone; none → final gate (h).
            let Some(mi) = first_incomplete(&self.state) else {
                match self.final_gate().await? {
                    Some(status) => return Ok(status),
                    None => continue,
                }
            };

            // (e) blocked milestone: only a queued user message can move it.
            if self.state.mission.milestones[mi].status == MilestoneStatus::Blocked {
                match self.handle_blocked(mi).await? {
                    Some(status) => return Ok(status),
                    None => continue,
                }
            }

            // (c) queued user messages → consult the orchestrator.
            if !self.state.pending_user_messages.is_empty() {
                self.consult_user_messages().await?;
                continue; // re-evaluate: the decision may precede config changes etc.
            }

            // (f) milestone start + next feature, else (g) validation round.
            if self.state.mission.milestones[mi].status == MilestoneStatus::Pending {
                let start_sha = self.repo.head_sha()?;
                let milestone_id = self.state.mission.milestones[mi].id.clone();
                self.emit(EventKind::MilestoneStarted { milestone_id, start_sha })?;
            }
            match next_feature(&self.state.mission.milestones[mi]) {
                Some(fi) => self.run_feature(mi, fi).await?,
                None => self.validation_round(mi).await?,
            }
        }
    }

    // -----------------------------------------------------------------------
    // Control inbox
    // -----------------------------------------------------------------------

    /// Drain queued control commands into events. Pause/Resume are guarded so
    /// duplicates don't spam the log; a config patch that would not
    /// deserialize/validate is skipped with a warning (appending it would
    /// poison the reducer for every future reader).
    ///
    /// Each inbox file is deleted only AFTER its command was durably applied
    /// (the `emit` appended the event). A crash between apply and delete
    /// re-processes the file on the next drain — a tolerated duplicate:
    /// Pause/Resume are idempotence-guarded above, and a repeated user
    /// message/config patch is benign, whereas deleting first would lose the
    /// command outright.
    fn drain_control(&mut self) -> Result<()> {
        for (path, cmd) in control::drain(&self.paths)? {
            match cmd {
                ControlCommand::Pause => {
                    if self.state.mission.status != MissionStatus::Paused {
                        self.emit(EventKind::MissionPaused {})?;
                    }
                }
                ControlCommand::Resume => {
                    if self.state.mission.status == MissionStatus::Paused {
                        self.emit(EventKind::MissionResumed {})?;
                    }
                }
                ControlCommand::ConfigChange { patch } => {
                    if let Err(e) = preview_config_patch(&self.state.config, &patch) {
                        // Invalid patch: warn and fall through to the delete —
                        // re-processing it forever would only spam the log.
                        tracing::warn!(error = %e, "skipping invalid config patch");
                    } else {
                        self.emit(EventKind::ConfigChanged { patch })?;
                    }
                }
                ControlCommand::Msg { text, interrupt } => {
                    self.emit(EventKind::UserMessage { text, interrupt })?;
                }
            }
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    /// §4.5 step (c): forward queued user messages to the orchestrator as a
    /// free-text consultation; the resulting `orchestrator.decision` clears
    /// the pending queue (reducer).
    async fn consult_user_messages(&mut self) -> Result<()> {
        let messages = self.state.pending_user_messages.clone();
        let rendered = messages
            .iter()
            .map(|m| format!("- {m}"))
            .collect::<Vec<_>>()
            .join("\n");
        let text = self
            .orch_turn(&format!(
                "The user sent the following message(s) while the mission was running:\n\
                 {rendered}\n\n\
                 Decide how to proceed; you may adjust remaining work. Reply in plain text."
            ))
            .await?;
        let summary = first_nonempty_line(&text).to_string();
        self.emit_decision(&summary, Some(text))?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Blocked milestone (e)
    // -----------------------------------------------------------------------

    /// A blocked milestone returns `Blocked` unless the user queued a message,
    /// in which case the orchestrator decides via a JSON turn how to proceed.
    /// Returns `Some(status)` to make `run()` return, `None` to continue.
    async fn handle_blocked(&mut self, mi: usize) -> Result<Option<MissionStatus>> {
        if self.state.pending_user_messages.is_empty() {
            return Ok(Some(MissionStatus::Blocked));
        }
        let milestone_id = self.state.mission.milestones[mi].id.clone();
        let messages = self.state.pending_user_messages.join("\n- ");
        let message = format!(
            "Milestone {milestone_id} is BLOCKED. The user sent:\n- {messages}\n\n\
             Decide how to proceed. Respond with ONLY this JSON:\n\
             {{\"action\":\"unblock-raise-cap\"|\"unblock-skip-findings\"|\"skip-milestone\"|\"stay-blocked\",\"note\":\"string\"}}"
        );
        let (decision, text) = self.json_decision::<UnblockDecision>(&message).await?;
        // Conservative default (documented): stay blocked.
        let (action, note) = match decision {
            Some(d) => (d.action.trim().to_ascii_lowercase(), d.note),
            None => ("stay-blocked".to_string(), "unparseable unblock decision".to_string()),
        };
        self.emit_decision(&format!("unblock decision for {milestone_id}: {action}"), Some(text))?;

        match action.as_str() {
            "unblock-raise-cap" | "unblock-skip-findings" => {
                self.emit(EventKind::MilestoneUnblocked {
                    milestone_id,
                    reason: if note.is_empty() { action } else { note },
                })?;
                Ok(None)
            }
            "skip-milestone" => {
                // Unblock first so the mission status leaves Blocked, then
                // skip the remaining (pending/active) features and close the
                // milestone untagged. Failed/skipped features keep their
                // status — rewriting them as skipped would falsify history.
                self.emit(EventKind::MilestoneUnblocked {
                    milestone_id: milestone_id.clone(),
                    reason: "milestone skipped by orchestrator decision".to_string(),
                })?;
                let to_skip: Vec<String> = self.state.mission.milestones[mi]
                    .features
                    .iter()
                    .filter(|f| {
                        matches!(f.status, FeatureStatus::Pending | FeatureStatus::Active)
                    })
                    .map(|f| f.id.clone())
                    .collect();
                for feature_id in to_skip {
                    self.emit(EventKind::FeatureSkipped {
                        feature_id,
                        reason: "milestone skipped".to_string(),
                    })?;
                }
                self.emit(EventKind::MilestoneCompleted { milestone_id, tag: None })?;
                Ok(None)
            }
            _ => Ok(Some(MissionStatus::Blocked)),
        }
    }

    // -----------------------------------------------------------------------
    // Feature execution (f)
    // -----------------------------------------------------------------------

    /// Run one feature to a terminal state: worker run(s) with interrupt
    /// wiring, the §4.4 dirty-tree discipline, an orchestrator judgement turn,
    /// and the bounded respawn loop.
    async fn run_feature(&mut self, mi: usize, fi: usize) -> Result<()> {
        if self.state.mission.milestones[mi].features[fi].status == FeatureStatus::Pending {
            let feature_id = self.state.mission.milestones[mi].features[fi].id.clone();
            self.emit(EventKind::FeatureStarted { feature_id })?;
        }

        let mut guidance: Option<String> = None;
        loop {
            // Snapshot everything the runner needs (avoids borrowing state
            // across the run).
            let feature = self.state.mission.milestones[mi].features[fi].clone();
            let goal = self.state.mission.goal.clone();
            let milestone_title = self.state.mission.milestones[mi].title.clone();
            let cfg = self.state.config.clone();
            let pre_run_sha = self.repo.head_sha()?;

            // Interrupt wiring: a control watcher polls the inbox and fires
            // the notify on `Msg { interrupt: true }`; run_session aborts the
            // worker and the outcome comes back Partial.
            let cancel = Arc::new(Notify::new());
            let watcher = tokio::spawn(control::ControlWatcher::wait_for_interrupt(
                self.paths.clone(),
                INTERRUPT_POLL,
                Arc::clone(&cancel),
            ));
            let backend = Arc::clone(&self.backend);
            let outcome = runner::run_worker(
                backend.as_ref(),
                &mut self.log,
                &self.paths,
                &cfg,
                &feature,
                &goal,
                &milestone_title,
                guidance.as_deref(),
                Some(cancel),
            )
            .await;
            watcher.abort();
            // Fold the runner's events into state even when the run errored
            // (worker.spawned may already be on disk).
            let caught = self.catch_up();
            let outcome = outcome?;
            caught?;

            // Interrupt (or any queued command) → events now, so the
            // judgement digest reflects them.
            self.drain_control()?;

            // §4.4 dirty-tree discipline (applies to interrupted runs too).
            if !self.repo.is_clean()? && !self.resolve_dirty_tree(&feature.id).await? {
                return Ok(()); // orchestrator chose fail-feature
            }
            let commits: Vec<String> = self
                .repo
                .commits_between(&pre_run_sha, "HEAD")?
                .iter()
                .map(|c| format!("{} {}", c.sha, c.subject))
                .collect();
            let diff_stat = self.repo.diff_stat(&pre_run_sha, "HEAD").unwrap_or_default();

            match self
                .judge_worker_run(&feature.id, outcome.report.as_ref(), &commits, &diff_stat)
                .await?
            {
                JudgementOutcome::Complete => {
                    self.emit(EventKind::FeatureCompleted { feature_id: feature.id, commits })?;
                    return Ok(());
                }
                JudgementOutcome::Failed(reason) => {
                    self.emit(EventKind::FeatureFailed { feature_id: feature.id, reason })?;
                    return Ok(());
                }
                JudgementOutcome::Respawn(new_guidance) => {
                    let respawns = self.state.mission.milestones[mi].features[fi].respawns;
                    if respawns < self.state.config.max_respawns {
                        guidance = Some(new_guidance);
                        continue;
                    }
                    self.emit(EventKind::FeatureFailed {
                        feature_id: feature.id,
                        reason: "respawn budget exhausted".to_string(),
                    })?;
                    return Ok(());
                }
            }
        }
    }

    /// Dirty tree after a worker run: ask the orchestrator (JSON), defaulting
    /// to commit-as-is (deterministic, documented). Returns `false` when the
    /// feature was failed instead.
    async fn resolve_dirty_tree(&mut self, feature_id: &str) -> Result<bool> {
        let message = format!(
            "The worker for feature {feature_id} left uncommitted changes in the working \
             tree. Decide what to do. Respond with ONLY this JSON:\n\
             {{\"action\":\"commit-as-is\"|\"fail-feature\",\"note\":\"string\"}}"
        );
        let (decision, text) = self.json_decision::<DirtyTreeDecision>(&message).await?;
        // Conservative default (documented): commit-as-is — worker output is
        // preserved on the mission branch for inspection either way.
        let (action, note) = match decision {
            Some(d) => (d.action.trim().to_ascii_lowercase(), d.note),
            None => ("commit-as-is".to_string(), "unparseable dirty-tree decision".to_string()),
        };
        self.emit_decision(&format!("dirty tree after {feature_id}: {action}"), Some(text))?;
        if action == "fail-feature" {
            self.emit(EventKind::FeatureFailed {
                feature_id: feature_id.to_string(),
                reason: if note.is_empty() { "dirty tree; orchestrator failed the feature".into() } else { note },
            })?;
            return Ok(false);
        }
        self.repo.add_all_and_commit(&format!("[{feature_id}] checkpoint (engine commit)"))?;
        Ok(true)
    }

    /// Post-run judgement turn (§4.5 f): report + commits + diff stat →
    /// JSON `{decision, guidance, summary}`. Unparseable after retry →
    /// conservative default: respawn-if-budget-else-fail (mapped to Respawn
    /// here; the caller enforces the budget).
    async fn judge_worker_run(
        &mut self,
        feature_id: &str,
        report: Option<&WorkerReport>,
        commits: &[String],
        diff_stat: &str,
    ) -> Result<JudgementOutcome> {
        let report_text = match report {
            Some(r) => serde_json::to_string_pretty(r)?,
            None => "NO REPORT — treat sceptically".to_string(),
        };
        let commits_text = if commits.is_empty() {
            "(none)".to_string()
        } else {
            commits.iter().map(|c| format!("- {c}")).collect::<Vec<_>>().join("\n")
        };
        let message = format!(
            "A worker run for feature {feature_id} just finished. Judge it.\n\n\
             WORKER REPORT:\n{report_text}\n\n\
             COMMITS THIS RUN:\n{commits_text}\n\n\
             DIFF STAT:\n{diff_stat}\n\n\
             Respond with ONLY this JSON:\n\
             {{\"decision\":\"complete\"|\"failed\"|\"respawn\",\"guidance\":\"string\",\"summary\":\"string\"}}"
        );
        let (decision, text) = self.json_decision::<JudgementDecision>(&message).await?;

        let (verdict, guidance, summary) = match decision {
            Some(d) => {
                let verdict = d.decision.trim().to_ascii_lowercase();
                let summary = if d.summary.is_empty() { verdict.clone() } else { d.summary };
                (verdict, d.guidance, summary)
            }
            None => (
                // Conservative default (documented): respawn while budget
                // remains, else fail — never silently complete.
                "respawn".to_string(),
                "previous judgement was unparseable; re-attempt the feature and produce \
                 a clear worker report"
                    .to_string(),
                "judgement unparseable; conservative default (respawn/fail)".to_string(),
            ),
        };
        self.emit_decision(&format!("judgement for {feature_id}: {summary}"), Some(text))?;

        Ok(match verdict.as_str() {
            "complete" => JudgementOutcome::Complete,
            "failed" | "fail" => JudgementOutcome::Failed(summary),
            // "respawn" and anything unrecognized take the conservative path.
            _ => JudgementOutcome::Respawn(if guidance.is_empty() { summary } else { guidance }),
        })
    }

    // -----------------------------------------------------------------------
    // Validation round (g)
    // -----------------------------------------------------------------------

    /// Milestone validation: scrutiny then functional validators (v1:
    /// sequential; each skippable by config). Findings go to the conversion
    /// turn, where the orchestrator turns each into a fix feature or waives
    /// it; no findings — or all findings waived — means a tag + completion.
    async fn validation_round(&mut self, mi: usize) -> Result<()> {
        let milestone_id = self.state.mission.milestones[mi].id.clone();
        self.emit(EventKind::MilestoneValidating { milestone_id: milestone_id.clone() })?;

        let start_sha = self.state.mission.milestones[mi]
            .start_sha
            .clone()
            .ok_or_else(|| {
                EngineError::InvalidState(format!(
                    "milestone {milestone_id} reached validation without a start sha"
                ))
            })?;

        let mut roles = Vec::new();
        if !self.state.config.skip_scrutiny {
            roles.push(Role::ValidatorScrutiny);
        }
        if !self.state.config.skip_functional {
            roles.push(Role::ValidatorFunctional);
        }

        let mut findings: Vec<(String, Finding)> = Vec::new();
        for role in roles {
            let milestone = self.state.mission.milestones[mi].clone();
            let contract = self.state.mission.validation_contract.clone();
            let cfg = self.state.config.clone();
            let backend = Arc::clone(&self.backend);
            let outcome = runner::run_validator(
                backend.as_ref(),
                &mut self.log,
                &self.paths,
                &cfg,
                role,
                &milestone,
                &contract,
                &start_sha,
                None,
            )
            .await;
            let caught = self.catch_up();
            let outcome = outcome?;
            caught?;
            if let Some(report) = outcome.validator_report {
                for finding in report.findings {
                    findings.push((outcome.run_id.clone(), finding));
                }
            }
        }

        for (run_id, finding) in &findings {
            self.emit(EventKind::ValidationFinding {
                milestone_id: milestone_id.clone(),
                run_id: run_id.clone(),
                finding: finding.clone(),
            })?;
        }

        if findings.is_empty() {
            let tag = self.tag_milestone(&milestone_id);
            self.emit(EventKind::MilestoneCompleted { milestone_id, tag })?;
            return Ok(());
        }

        // The conversion turn runs even with the fix-cycle cap exhausted:
        // the cap bounds fix ROUNDS, not the orchestrator's right to judge
        // findings — an all-waived answer completes the milestone where the
        // old flow would have blocked on trivia.
        let findings: Vec<Finding> = findings.into_iter().map(|(_, f)| f).collect();
        match self.convert_findings(&milestone_id, &findings).await? {
            FindingsConversion::Waive { waived } => {
                self.emit_waive_decision(&waived)?;
                let tag = self.tag_milestone(&milestone_id);
                self.emit(EventKind::MilestoneCompleted { milestone_id, tag })?;
            }
            FindingsConversion::Fix { specs, summary, text } => {
                if self.fix_cycle_exhausted(mi) {
                    self.emit_decision(
                        &format!(
                            "fix-cycle cap reached; {} fix feature(s) wanted for {milestone_id}: {summary}",
                            specs.len()
                        ),
                        Some(text),
                    )?;
                    self.emit(EventKind::MilestoneBlocked {
                        milestone_id,
                        reason: format!(
                            "{} validation finding(s) but the fix-cycle cap ({}) is reached",
                            findings.len(),
                            self.state.config.max_fix_cycles_per_milestone
                        ),
                    })?;
                    return Ok(());
                }
                self.emit_fix_features(mi, specs, &summary, text)?;
            }
        }
        Ok(())
    }

    /// Would one more fix round exceed `max_fix_cycles_per_milestone`?
    /// Checked after the conversion turn (waivable findings must reach the
    /// orchestrator even at the cap) but before any `fixfeature.created` is
    /// emitted (§4.5 g: never create the features first).
    fn fix_cycle_exhausted(&self, mi: usize) -> bool {
        self.state.mission.milestones[mi].fix_cycles + 1
            > self.state.config.max_fix_cycles_per_milestone
    }

    /// Annotated milestone tag; a pre-existing tag (milestone re-completed
    /// after final-gate fixes) downgrades to `None` rather than failing the
    /// mission.
    fn tag_milestone(&self, milestone_id: &str) -> Option<String> {
        let name = format!("kranz/{}/{}", self.state.mission.id, milestone_id);
        match self.repo.tag(&name, "kranz milestone complete") {
            Ok(()) => Some(name),
            Err(e) => {
                tracing::warn!(tag = %name, error = %e, "milestone tag failed; completing untagged");
                None
            }
        }
    }

    /// The findings-conversion turn (§4.5 g): every finding is put to the
    /// orchestrator, which converts each into a fix feature or waives it
    /// with a one-line justification. The contract is the bar — severity
    /// alone decides nothing.
    ///
    /// Conservative fallbacks: an unparseable answer (after retry) and an
    /// answer that neither fixes nor waives are both treated as
    /// convert-everything, with specs synthesized 1:1 from the findings — a
    /// parse failure must never silently waive, and an empty round would
    /// re-validate immediately and spin without ever bumping `fix_cycles`.
    async fn convert_findings(
        &mut self,
        milestone_id: &str,
        findings: &[Finding],
    ) -> Result<FindingsConversion> {
        let findings_json = serde_json::to_string_pretty(findings)?;
        let message = format!(
            "Validation of milestone {milestone_id} produced these findings:\n{findings_json}\n\n\
             For each finding decide: convert it to a fix-feature (it violates or endangers \
             the validation contract / feature criteria; fresh worker sessions will implement \
             fix features) or WAIVE it with a one-line justification (cosmetic, \
             out-of-contract, or not worth a fresh worker session). The contract is the bar; \
             minor severity is not automatically waivable and major severity is not \
             automatically fixable — judge. Respond with ONLY this JSON:\n\
             {{\"fixFeatures\":[{{\"title\":\"string\",\"spec\":\"string\",\"validationCriteria\":[\"string\"]}}],\"waived\":[{{\"subject\":\"string\",\"reason\":\"string\"}}],\"summary\":\"string\"}}"
        );
        let (decision, text) = self.json_decision::<FixFeaturesDecision>(&message).await?;
        let (specs, waived, summary) = match decision {
            Some(d) => (d.fix_features, d.waived, d.summary),
            None => (
                Vec::new(),
                Vec::new(),
                "unparseable fix-features decision; synthesized from findings".to_string(),
            ),
        };
        if specs.is_empty() && !waived.is_empty() {
            return Ok(FindingsConversion::Waive { waived });
        }
        let specs = if specs.is_empty() {
            findings
                .iter()
                .map(|f| FixFeatureSpec {
                    title: format!("Fix finding: {}", f.subject),
                    spec: format!(
                        "Address this validation finding.\nEvidence: {}\nSuggested fix: {}",
                        f.evidence, f.suggested_fix
                    ),
                    validation_criteria: vec![format!("finding '{}' no longer reproduces", f.subject)],
                })
                .collect()
        } else {
            specs
        };
        Ok(FindingsConversion::Fix { specs, summary, text })
    }

    /// Emit the all-waived `orchestrator.decision`: summary names the waived
    /// subjects, detail carries the justifications. Both fields are
    /// credential-scrubbed by [`Self::emit_decision`] — waiver reasons are
    /// model-authored text.
    fn emit_waive_decision(&mut self, waived: &[WaivedFinding]) -> Result<()> {
        let subjects =
            waived.iter().map(|w| w.subject.as_str()).collect::<Vec<_>>().join(", ");
        let reasons = waived
            .iter()
            .map(|w| format!("- {}: {}", w.subject, w.reason))
            .collect::<Vec<_>>()
            .join("\n");
        self.emit_decision(
            &format!("waived {} finding(s): {subjects}", waived.len()),
            Some(reasons),
        )
    }

    /// Emit the fix-features decision plus one `fixfeature.created` per spec
    /// (the fix path of a conversion turn; the caller has already checked
    /// the fix-cycle cap).
    fn emit_fix_features(
        &mut self,
        mi: usize,
        specs: Vec<FixFeatureSpec>,
        summary: &str,
        text: String,
    ) -> Result<()> {
        let milestone_id = self.state.mission.milestones[mi].id.clone();
        self.emit_decision(
            &format!("{} fix feature(s) for {milestone_id}: {summary}", specs.len()),
            Some(text),
        )?;

        let cycle = self.state.mission.milestones[mi].fix_cycles + 1;
        for (i, spec) in specs.into_iter().enumerate() {
            // Belt and braces: both sources (the orchestrator turn text and
            // validator findings) are already scrubbed, but these strings are
            // model-authored and land verbatim in `fixfeature.created` events,
            // so scrub them once more at the emit boundary.
            let feature = Feature {
                id: format!("{milestone_id}-fix-{cycle}-{}", i + 1),
                title: scrub::scrub(&spec.title),
                spec: scrub::scrub(&spec.spec),
                validation_criteria: spec
                    .validation_criteria
                    .iter()
                    .map(|c| scrub::scrub(c))
                    .collect(),
                origin: FeatureOrigin::Fix,
                status: FeatureStatus::Pending,
                worker_runs: Vec::new(),
                commits: Vec::new(),
                respawns: 0,
            };
            self.emit(EventKind::FixFeatureCreated {
                milestone_id: milestone_id.clone(),
                feature,
            })?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Final contract gate (h)
    // -----------------------------------------------------------------------

    /// All milestones complete: run every `command` assertion ourselves and
    /// put `agent-judgement` assertions to the orchestrator. Failures become
    /// findings routed through the same conversion turn as a validation
    /// round, on the LAST milestone: fix features reopen it, an all-waived
    /// answer completes the mission.
    /// Returns `Some(status)` to end `run()`, `None` to continue the loop.
    async fn final_gate(&mut self) -> Result<Option<MissionStatus>> {
        if self.state.mission.status != MissionStatus::Validating {
            self.emit(EventKind::MissionValidating {})?;
        }

        let contract = self.state.mission.validation_contract.clone();
        let mut findings: Vec<Finding> = Vec::new();

        // command assertions — engine-run (design.md: the hard gate).
        for assertion in contract.iter().filter(|a| a.check == AssertionCheck::Command) {
            let Some(command) = assertion.command.as_deref() else {
                findings.push(Finding {
                    subject: assertion.id.clone(),
                    severity: "critical".to_string(),
                    evidence: "assertion has check=command but no command".to_string(),
                    suggested_fix: String::new(),
                });
                continue;
            };
            let (ok, output) = run_shell_command(self.paths.repo_root.as_path(), command).await;
            if !ok {
                findings.push(Finding {
                    subject: assertion.id.clone(),
                    severity: "critical".to_string(),
                    evidence: format!("command failed: {command}\n{output}"),
                    suggested_fix: String::new(),
                });
            }
        }

        // agent-judgement assertions — one orchestrator verdicts turn.
        let judgement: Vec<&Assertion> = contract
            .iter()
            .filter(|a| a.check == AssertionCheck::AgentJudgement)
            .collect();
        if !judgement.is_empty() {
            findings.extend(self.judge_contract_assertions(&judgement).await?);
        }

        if findings.is_empty() {
            self.emit(EventKind::MissionCompleted {})?;
            return Ok(Some(MissionStatus::Complete));
        }

        // Surface gate findings on the event feed (dashboard visibility),
        // attributed to the reserved engine run id since no validator session
        // exists behind them.
        let li = self.state.mission.milestones.len() - 1;
        let last_milestone_id = self.state.mission.milestones[li].id.clone();
        for finding in &findings {
            self.emit(EventKind::ValidationFinding {
                milestone_id: last_milestone_id.clone(),
                run_id: crate::reducer::ENGINE_RUN_ID.to_string(),
                finding: finding.clone(),
            })?;
        }
        // Gate findings go through the same conversion turn as a milestone
        // validation round: the orchestrator may waive them all, in which
        // case the mission proceeds to completion.
        match self.convert_findings(&last_milestone_id, &findings).await? {
            FindingsConversion::Waive { waived } => {
                self.emit_waive_decision(&waived)?;
                self.emit(EventKind::MissionCompleted {})?;
                Ok(Some(MissionStatus::Complete))
            }
            FindingsConversion::Fix { specs, summary, text } => {
                if self.fix_cycle_exhausted(li) {
                    self.emit_decision(
                        &format!(
                            "fix-cycle cap reached; {} fix feature(s) wanted for {last_milestone_id}: {summary}",
                            specs.len()
                        ),
                        Some(text),
                    )?;
                    self.emit(EventKind::MilestoneBlocked {
                        milestone_id: last_milestone_id,
                        reason: format!(
                            "{} final-gate finding(s) but the fix-cycle cap ({}) is reached",
                            findings.len(),
                            self.state.config.max_fix_cycles_per_milestone
                        ),
                    })?;
                    return Ok(None); // loop → blocked branch → Blocked
                }
                // Reopen the last milestone: milestone.validating makes the
                // following fixfeature.created bump fix_cycles and flip it
                // back to Active (reducer semantics) so the main loop picks
                // the fix features up.
                self.emit(EventKind::MilestoneValidating {
                    milestone_id: last_milestone_id,
                })?;
                self.emit_fix_features(li, specs, &summary, text)?;
                Ok(None)
            }
        }
    }

    /// One verdicts turn for all agent-judgement assertions. Unparseable
    /// (after retry) or missing verdicts fail conservatively — a gate that
    /// cannot be verified must not pass.
    async fn judge_contract_assertions(
        &mut self,
        assertions: &[&Assertion],
    ) -> Result<Vec<Finding>> {
        let listed = assertions
            .iter()
            .map(|a| format!("- [{}] {}", a.id, a.statement))
            .collect::<Vec<_>>()
            .join("\n");
        let base = self.state.mission.base_branch.clone();
        let diff_stat = self.repo.diff_stat(&base, "HEAD").unwrap_or_default();
        let message = format!(
            "Final contract gate. Verify each of these agent-judgement assertions against \
             the mission's work (diff stat of {base}..HEAD below). Inspect the repository \
             read-only as needed.\n\nASSERTIONS:\n{listed}\n\nDIFF STAT:\n{diff_stat}\n\n\
             Respond with ONLY this JSON:\n\
             {{\"verdicts\":[{{\"id\":\"string\",\"pass\":true,\"evidence\":\"string\"}}],\"summary\":\"string\"}}"
        );
        let (decision, text) = self.json_decision::<VerdictsDecision>(&message).await?;

        let mut findings = Vec::new();
        let summary = match decision {
            Some(d) => {
                for assertion in assertions {
                    match d.verdicts.iter().find(|v| v.id == assertion.id) {
                        Some(v) if v.pass => {}
                        Some(v) => findings.push(Finding {
                            subject: assertion.id.clone(),
                            severity: "critical".to_string(),
                            evidence: if v.evidence.is_empty() {
                                "orchestrator judged the assertion failed".to_string()
                            } else {
                                v.evidence.clone()
                            },
                            suggested_fix: String::new(),
                        }),
                        None => findings.push(Finding {
                            subject: assertion.id.clone(),
                            severity: "critical".to_string(),
                            evidence: "no verdict returned for this assertion".to_string(),
                            suggested_fix: String::new(),
                        }),
                    }
                }
                d.summary
            }
            None => {
                for assertion in assertions {
                    findings.push(Finding {
                        subject: assertion.id.clone(),
                        severity: "critical".to_string(),
                        evidence: "verdict turn unparseable; assertion could not be verified"
                            .to_string(),
                        suggested_fix: String::new(),
                    });
                }
                "unparseable verdicts; all judgement assertions failed conservatively".to_string()
            }
        };
        self.emit_decision(&format!("final gate verdicts: {summary}"), Some(text))?;
        Ok(findings)
    }

    // -----------------------------------------------------------------------
    // Orchestrator session management (i)
    // -----------------------------------------------------------------------

    /// One orchestrator turn with re-seed resilience: ensure the session,
    /// send the digest-prefixed message, pump to the turn's `Result`. If the
    /// session dies mid-turn, re-seed once and retry; two consecutive
    /// failures → [`EngineError::Backend`].
    async fn orch_turn(&mut self, message: &str) -> Result<String> {
        let mut last_err: Option<EngineError> = None;
        for attempt in 0..2u8 {
            self.ensure_orchestrator().await?;
            // Digest rendered fresh per attempt — state may have moved.
            let full = format!("{}\n\n{}", digest::render(&self.state), message);
            let turn = async {
                let session = self.orch.as_mut().expect("ensured above");
                session.send_user_message(&full).await?;
                Ok::<(), EngineError>(())
            }
            .await;
            let result = match turn {
                Ok(()) => {
                    self.transcribe_injected(&full)?;
                    self.pump_turn().await
                }
                Err(e) => Err(e),
            };
            match result {
                Ok(text) => return Ok(text),
                Err(e) => {
                    tracing::warn!(attempt, error = %e, "orchestrator turn failed");
                    // Session is unusable: drop it AND forget the sdk id so
                    // the retry takes the fresh re-seed path (§4.8), not
                    // another resume of a dead session.
                    self.force_reseed();
                    last_err = Some(e);
                }
            }
        }
        Err(EngineError::Backend(format!(
            "orchestrator turn failed twice (re-seed did not recover): {}",
            last_err.expect("two failures recorded")
        )))
    }

    /// Ensure the long-lived streaming orchestrator session exists.
    ///
    /// Seeding (see module docs): Planning → goal + interrogate-then-propose
    /// instructions; known previous sdk session → `--resume` with a nudge;
    /// otherwise a fresh session seeded with digest + plan.json
    /// ([`digest::render_reseed`]), announced with an `orchestrator.decision`.
    /// The seed turn is pumped to its `Result` so later turns stay 1:1.
    /// A failed resume falls back to the fresh re-seed path once.
    async fn ensure_orchestrator(&mut self) -> Result<()> {
        if self.orch.is_some() {
            return Ok(());
        }
        let planning = self.state.mission.status == MissionStatus::Planning;
        let resume_id = self.orch_session_id.clone();

        let (seed, resume) = if let Some(prev) = resume_id {
            (
                "The engine resumed this orchestrator session after a restart. \
                 Acknowledge briefly and await instructions."
                    .to_string(),
                Some(prev),
            )
        } else if planning {
            (
                format!(
                    "MISSION GOAL:\n{}\n\nYou are in the planning phase. Interrogate the \
                     goal and the repository (read-only), ask the user sharp questions if \
                     anything material is ambiguous, then propose the validation contract, \
                     milestones and features. Do not emit the plan JSON until asked.",
                    self.state.mission.goal
                ),
                None,
            )
        } else {
            (digest::render_reseed(&self.state, &self.plan_json()?), None)
        };
        let reseeded = resume.is_none() && !planning;

        match self.start_orchestrator(seed, resume.clone()).await {
            Ok(()) => {}
            Err(e) if resume.is_some() => {
                // Resume failed (spawn error or dead seed turn): fresh
                // re-seed — a tested property, not an emergency (§4.8).
                tracing::warn!(error = %e, "orchestrator resume failed; re-seeding fresh");
                self.force_reseed();
                // During planning there is no plan to re-seed from: restart
                // the planning conversation from the goal instead.
                let seed = if planning {
                    format!(
                        "MISSION GOAL:\n{}\n\nYou are in the planning phase; a previous \
                         planning conversation was lost. Re-establish context from the \
                         repository (read-only), then continue shaping the validation \
                         contract, milestones and features with the user. Do not emit \
                         the plan JSON until asked.",
                        self.state.mission.goal
                    )
                } else {
                    digest::render_reseed(&self.state, &self.plan_json()?)
                };
                self.start_orchestrator(seed, None).await?;
                self.emit(EventKind::OrchestratorDecision {
                    summary: "orchestrator session re-seeded".to_string(),
                    detail: None,
                })?;
                return Ok(());
            }
            Err(e) => return Err(e),
        }
        if reseeded {
            self.emit(EventKind::OrchestratorDecision {
                summary: "orchestrator session re-seeded".to_string(),
                detail: None,
            })?;
        }
        Ok(())
    }

    /// Start one streaming orchestrator session, emit its `worker.spawned`,
    /// open its transcript, and pump the seed turn to its `Result`.
    async fn start_orchestrator(&mut self, seed: String, resume: Option<String>) -> Result<()> {
        let cfg = self.state.config.clone();
        let role_cfg = cfg.role(Role::Orchestrator).clone();

        // The orchestrator prompt sizes features by the WORKER turn budget.
        let mut vars: HashMap<&str, String> = HashMap::new();
        vars.insert(
            "turnBudget",
            cfg.worker
                .max_turns
                .map(|n| n.to_string())
                .unwrap_or_else(|| "a reasonable number of".to_string()),
        );
        let system_prompt = prompts::render(prompts::text(Role::Orchestrator), &vars);

        let session_id = uuid::Uuid::new_v4().to_string();
        let mut spec = SessionSpec {
            cwd: self.paths.repo_root.clone(),
            prompt: PromptMode::Streaming(seed),
            append_system_prompt: Some(system_prompt),
            model: role_cfg.model.clone(),
            effort: role_cfg.reasoning_effort.clone(),
            session_id: session_id.clone(),
            resume: resume.clone(),
            permission_mode: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            settings_json: None,
            json_schema: None,
            max_budget_usd: role_cfg.max_budget_usd,
            max_turns: role_cfg.max_turns,
            env: HashMap::new(),
        };
        permissions::apply(permissions::for_role(Role::Orchestrator, &cfg, &[]), &mut spec);

        let session = self.backend.start(spec).await?;

        // Bookkeeping mirrors runner::run_session: the recorded sdk id is the
        // resumed id when resuming, else the fresh engine-chosen id.
        let sdk_session_id = resume.unwrap_or(session_id);
        let orch_count = self
            .state
            .runs
            .values()
            .filter(|r| r.role == Role::Orchestrator)
            .count();
        let run_id = format!("orch-{}", orch_count + 1);

        std::fs::create_dir_all(self.paths.runs_dir())?;
        let transcript = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.paths.transcript_file(&run_id))?;

        self.emit(EventKind::WorkerSpawned {
            run_id: run_id.clone(),
            role: Role::Orchestrator,
            feature_id: None,
            milestone_id: None,
            sdk_session_id: sdk_session_id.clone(),
            model: role_cfg.model,
            prompt_hash: prompts::hash(Role::Orchestrator),
            transcript_path: MissionPaths::transcript_rel(&run_id),
        })?;

        self.orch = Some(session);
        self.orch_session_id = Some(sdk_session_id);
        self.orch_run_id = Some(run_id);
        self.orch_transcript = Some(transcript);

        // The seed is a full turn (the backend sends the streaming initial
        // prompt as the first user message); consume its Result so every
        // later send/pump pair stays aligned.
        match self.pump_turn().await {
            Ok(_ack) => Ok(()),
            Err(e) => {
                self.orch = None;
                self.orch_run_id = None;
                self.orch_transcript = None;
                Err(e)
            }
        }
    }

    /// Pump the live orchestrator session until the current turn's `Result`,
    /// mirroring every event to the transcript and `worker.message` deltas,
    /// and folding the turn's usage into totals via `worker.completed`.
    ///
    /// Returns the turn's text: the `Result` text when non-empty, else the
    /// concatenated assistant `Text` blocks — credential-scrubbed at this
    /// single choke point, so everything derived from a turn (decision
    /// details, parsed JSON decisions, fix-feature specs, verdict evidence)
    /// is redacted before it can reach events.jsonl.
    async fn pump_turn(&mut self) -> Result<String> {
        let run_id = self.orch_run_id.clone().ok_or_else(|| {
            EngineError::InvalidState("pump_turn without a live orchestrator run".to_string())
        })?;
        let mut texts: Vec<String> = Vec::new();
        loop {
            let stall = self.orch_stall_timeout;
            let next = {
                let session = self.orch.as_mut().ok_or_else(|| {
                    EngineError::InvalidState("pump_turn without a session".to_string())
                })?;
                tokio::time::timeout(stall, session.next_event()).await
            };
            let event = match next {
                Err(_elapsed) => {
                    return Err(EngineError::Backend(format!(
                        "orchestrator stream stalled (> {:?} without an event)",
                        stall
                    )))
                }
                Ok(result) => result?,
            };
            let Some(event) = event else {
                // Surface WHY the process died (exit code + stderr tail) —
                // without this the failure is undiagnosable from the outside.
                let detail = self
                    .orch
                    .as_ref()
                    .and_then(|s| s.exit_status())
                    .map(|e| format!("{e:?}"))
                    .unwrap_or_else(|| "no exit status".to_string());
                let msg = format!("orchestrator stream closed mid-turn ({detail})");
                let _ = self.emit(EventKind::WorkerMessage {
                    run_id: run_id.clone(),
                    tag: "system".to_string(),
                    content: scrub::scrub(&msg),
                });
                return Err(EngineError::Backend(msg));
            };
            self.mirror_orch_event(&run_id, &event)?;
            match event {
                AgentEvent::Text { text, .. } => texts.push(text),
                AgentEvent::Result { text, is_error, usage, cost_usd, .. } => {
                    // Per-turn accounting: streaming sessions emit one Result
                    // per injected turn (design.md), so each becomes one
                    // worker.completed carrying that turn's usage — totals
                    // accumulate in the reducer.
                    self.emit(EventKind::WorkerCompleted {
                        run_id: run_id.clone(),
                        result: if is_error { RunResult::Fail } else { RunResult::Pass },
                        tokens: usage,
                        cost_usd,
                        report: None,
                    })?;
                    if is_error {
                        return Err(EngineError::Backend(format!(
                            "orchestrator turn returned an error result: {}",
                            scrub::scrub(&text)
                        )));
                    }
                    let turn_text = if text.trim().is_empty() { texts.join("\n") } else { text };
                    return Ok(scrub::scrub(&turn_text));
                }
                _ => {}
            }
        }
    }

    /// Mirror one orchestrator stream event: raw (scrubbed) line to the
    /// transcript; Text/ToolUse/ToolResult to `worker.message` deltas (same
    /// mapping as [`runner::RunSink`]).
    fn mirror_orch_event(&mut self, run_id: &str, event: &AgentEvent) -> Result<()> {
        let raw = match event {
            AgentEvent::Init { raw, .. }
            | AgentEvent::Text { raw, .. }
            | AgentEvent::ToolUse { raw, .. }
            | AgentEvent::ToolResult { raw, .. }
            | AgentEvent::Result { raw, .. }
            | AgentEvent::Other { raw } => raw,
        };
        if let Some(transcript) = self.orch_transcript.as_mut() {
            writeln!(transcript, "{}", scrub::scrub(&serde_json::to_string(raw)?))?;
        }
        let (tag, content) = match event {
            AgentEvent::Text { text, .. } => ("text", text.clone()),
            AgentEvent::ToolUse { tool, summary, .. } => {
                ("tool-use", format!("{tool}: {summary}"))
            }
            AgentEvent::ToolResult { tool, denied, summary, .. } => {
                let content = match tool {
                    Some(tool) => format!("{tool}: {summary}"),
                    None => summary.clone(),
                };
                (if *denied { "denied" } else { "tool-result" }, content)
            }
            _ => return Ok(()),
        };
        self.emit(EventKind::WorkerMessage {
            run_id: run_id.to_string(),
            tag: tag.to_string(),
            content: scrub::scrub_and_truncate(&content, MESSAGE_CONTENT_MAX),
        })?;
        Ok(())
    }

    /// Record an injected user message in the orchestrator transcript (the
    /// stream only carries the model's side).
    fn transcribe_injected(&mut self, text: &str) -> Result<()> {
        if let Some(transcript) = self.orch_transcript.as_mut() {
            let line = serde_json::json!({
                "type": "user",
                "subtype": "kranz-injected",
                "message": { "content": [{ "type": "text", "text": scrub::scrub(text) }] },
            });
            writeln!(transcript, "{line}")?;
        }
        Ok(())
    }

    /// One JSON decision turn: send, parse leniently, retry once demanding
    /// bare JSON. Returns the parsed value (None = caller applies its
    /// conservative default) plus the raw text of the last reply.
    async fn json_decision<T: DeserializeOwned>(
        &mut self,
        message: &str,
    ) -> Result<(Option<T>, String)> {
        let text = self.orch_turn(message).await?;
        if let Some(parsed) = runner::parse_report::<T>(&text) {
            return Ok((Some(parsed), text));
        }
        let retry = self.orch_turn(JSON_RETRY_MSG).await?;
        match runner::parse_report::<T>(&retry) {
            Some(parsed) => Ok((Some(parsed), retry)),
            None => Ok((None, retry)),
        }
    }

    /// The approved plan JSON: `plan.json` from disk, else re-serialized from
    /// state (the log always has plan.approved when milestones exist).
    fn plan_json(&self) -> Result<String> {
        match std::fs::read_to_string(self.paths.plan_file()) {
            Ok(text) => Ok(text),
            Err(_) => {
                let mission = &self.state.mission;
                let plan = Plan {
                    goal: mission.goal.clone(),
                    validation_contract: mission.validation_contract.clone(),
                    milestones: mission
                        .milestones
                        .iter()
                        .map(|m| PlanMilestone {
                            title: m.title.clone(),
                            features: m
                                .features
                                .iter()
                                .map(|f| PlanFeature {
                                    title: f.title.clone(),
                                    spec: f.spec.clone(),
                                    validation_criteria: f.validation_criteria.clone(),
                                })
                                .collect(),
                        })
                        .collect(),
                };
                Ok(serde_json::to_string_pretty(&plan)?)
            }
        }
    }
}

/// What the judgement turn decided for a worker run.
enum JudgementOutcome {
    Complete,
    Failed(String),
    /// Respawn with this guidance (budget enforced by the caller).
    Respawn(String),
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Index of the first milestone (in plan order) that is not Complete.
fn first_incomplete(state: &MissionState) -> Option<usize> {
    state
        .mission
        .milestones
        .iter()
        .position(|m| m.status != MilestoneStatus::Complete)
}

/// Index of the next feature to work: Pending, or Active (a crashed run —
/// respawn candidate). Skipped/Failed/Complete features are left alone.
fn next_feature(milestone: &Milestone) -> Option<usize> {
    milestone
        .features
        .iter()
        .position(|f| matches!(f.status, FeatureStatus::Pending | FeatureStatus::Active))
}

/// Assign `a-1..` ids to contract assertions with missing ids and
/// de-duplicate colliding ones (unique-ish, plan §4.5).
fn assign_assertion_ids(contract: &mut [Assertion]) {
    let mut seen = std::collections::HashSet::new();
    let mut counter = 0usize;
    for assertion in contract.iter_mut() {
        let id = assertion.id.trim().to_string();
        let id = if id.is_empty() || seen.contains(&id) {
            loop {
                counter += 1;
                let candidate = format!("a-{counter}");
                if !seen.contains(&candidate) {
                    break candidate;
                }
            }
        } else {
            id
        };
        seen.insert(id.clone());
        assertion.id = id;
    }
}

/// First non-empty line of a text (decision summaries).
fn first_nonempty_line(text: &str) -> &str {
    text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// Canonicalize the repo root when possible (macOS tempdirs are symlinks
/// under /var → /private/var; git pathspec matching needs the real path).
fn canonical_root(root: PathBuf) -> PathBuf {
    std::fs::canonicalize(&root).unwrap_or(root)
}

/// Write `.kranz/.gitignore` (module docs: keep engine churn out of the §4.4
/// dirty-tree discipline; plan.json stays committable). Never overwrites a
/// user-edited file.
fn write_kranz_gitignore(paths: &MissionPaths) -> Result<()> {
    let dir = paths.kranz_dir();
    std::fs::create_dir_all(&dir)?;
    let file = dir.join(".gitignore");
    if !file.exists() {
        std::fs::write(
            &file,
            "# kranz engine bookkeeping — never part of mission commits\n\
             .gitignore\n\
             config.json\n\
             missions/*/events.jsonl\n\
             missions/*/events.jsonl.lock\n\
             missions/*/state.json\n\
             missions/*/state.json.tmp\n\
             missions/*/control/\n\
             missions/*/runs/\n",
        )?;
    }
    Ok(())
}

/// Pre-flight a `config.changed` patch: the merged result must deserialize
/// and validate, or the event must not be appended (the reducer would poison
/// every future fold of the log).
fn preview_config_patch(current: &MissionConfig, patch: &serde_json::Value) -> Result<()> {
    let mut value = serde_json::to_value(current)?;
    config::deep_merge(&mut value, patch);
    let merged: MissionConfig = serde_json::from_value(value)
        .map_err(|e| EngineError::Config(format!("patch produces invalid config: {e}")))?;
    config::validate(&merged)
}

/// Run one user-authored contract command line at the final gate.
///
/// DELIBERATE shell usage (the one place in the engine): contract commands
/// are user-authored shell lines ("npm test -- --grep auth") that need real
/// shell semantics — argument splitting here would corrupt them. `cmd /C` on
/// Windows, `sh -c` elsewhere; cwd = repo root; 10-minute cap.
async fn run_shell_command(cwd: &std::path::Path, command: &str) -> (bool, String) {
    run_shell_command_with_timeout(cwd, command, COMMAND_TIMEOUT).await
}

/// [`run_shell_command`] with an explicit timeout (separated so tests can
/// exercise the timeout path without waiting ten minutes).
///
/// Timeout kill semantics: on unix the shell is started as the leader of a
/// new process group and the WHOLE group gets SIGKILL — killing only the
/// wrapper (kill_on_drop) would leave `sleep 300 &`-style descendants running
/// (and holding the output pipes) long after the gate gave up. The killed
/// shell itself is reaped by tokio's background orphan reaper (kill_on_drop);
/// group members are re-parented to init and reaped there.
async fn run_shell_command_with_timeout(
    cwd: &std::path::Path,
    command: &str,
    timeout: Duration,
) -> (bool, String) {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    cmd.current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Unix: new process group with the shell as leader, so the timeout path
    // can kill the entire command tree, not just the `sh -c` wrapper.
    #[cfg(unix)]
    cmd.process_group(0);

    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return (false, format!("failed to spawn shell: {e}")),
    };
    #[cfg(unix)]
    let group_pid = child.id();

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Err(_elapsed) => {
            // The dropped wait future already killed the shell wrapper via
            // kill_on_drop; SIGKILL the whole group so its descendants die
            // too (a still-live member keeps the pgid valid, and the leader
            // zombie pins it until reaped).
            #[cfg(unix)]
            if let Some(pid) = group_pid {
                // Negative pid targets every process in the group.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            // TODO(windows): kill_on_drop terminates only the `cmd /C`
            // wrapper; killing the whole tree needs Job Objects.
            (false, format!("timed out after {}s", timeout.as_secs()))
        }
        Ok(Err(e)) => (false, format!("failed waiting for shell: {e}")),
        Ok(Ok(output)) => {
            let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.trim().is_empty() {
                combined.push_str("\n--- stderr ---\n");
                combined.push_str(stderr.trim_end());
            }
            (output.status.success(), tail_chars(combined.trim_end(), COMMAND_OUTPUT_TAIL))
        }
    }
}

/// Last `max` characters of `text` (char-safe).
fn tail_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    text.chars().skip(count - max).collect()
}

/// JSON Schema for [`Plan`] (camelCase), embedded in the request_plan turn.
fn plan_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["goal", "validationContract", "milestones"],
        "properties": {
            "goal": { "type": "string" },
            "validationContract": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "statement", "check"],
                    "properties": {
                        "id": { "type": "string" },
                        "statement": { "type": "string" },
                        "check": { "type": "string", "enum": ["command", "agent-judgement"] },
                        "command": { "type": "string" }
                    }
                }
            },
            "milestones": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["title", "features"],
                    "properties": {
                        "title": { "type": "string" },
                        "features": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["title", "spec", "validationCriteria"],
                                "properties": {
                                    "title": { "type": "string" },
                                    "spec": { "type": "string" },
                                    "validationCriteria": {
                                        "type": "array",
                                        "items": { "type": "string" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Unit tests for the tricky pure helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn assertion(id: &str) -> Assertion {
        Assertion {
            id: id.to_string(),
            statement: "s".to_string(),
            check: AssertionCheck::AgentJudgement,
            command: None,
        }
    }

    #[test]
    fn assign_assertion_ids_fills_missing_and_dedupes() {
        let mut contract = vec![assertion(""), assertion("x"), assertion("x"), assertion("a-2")];
        assign_assertion_ids(&mut contract);
        let ids: Vec<&str> = contract.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids[0], "a-1", "missing id gets a-1");
        assert_eq!(ids[1], "x", "explicit unique id is kept");
        assert_ne!(ids[2], "x", "duplicate must be renamed");
        let unique: std::collections::HashSet<&&str> = ids.iter().collect();
        assert_eq!(unique.len(), 4, "all ids unique: {ids:?}");
    }

    #[test]
    fn next_feature_picks_pending_and_active_only() {
        let feature = |id: &str, status| Feature {
            id: id.to_string(),
            title: String::new(),
            spec: String::new(),
            validation_criteria: vec![],
            origin: FeatureOrigin::Plan,
            status,
            worker_runs: vec![],
            commits: vec![],
            respawns: 0,
        };
        let ms = Milestone {
            id: "ms-1".to_string(),
            title: String::new(),
            features: vec![
                feature("f1", FeatureStatus::Complete),
                feature("f2", FeatureStatus::Failed),
                feature("f3", FeatureStatus::Skipped),
                feature("f4", FeatureStatus::Active),
                feature("f5", FeatureStatus::Pending),
            ],
            status: MilestoneStatus::Active,
            fix_cycles: 0,
            start_sha: None,
        };
        assert_eq!(next_feature(&ms), Some(3), "Active (crashed) before Pending");
        let mut done = ms.clone();
        done.features[3].status = FeatureStatus::Complete;
        done.features[4].status = FeatureStatus::Complete;
        assert_eq!(next_feature(&done), None);
    }

    #[test]
    fn tail_chars_keeps_the_end() {
        assert_eq!(tail_chars("abcdef", 3), "def");
        assert_eq!(tail_chars("ab", 3), "ab");
        assert_eq!(tail_chars("héllo", 2), "lo");
    }

    #[test]
    fn first_nonempty_line_skips_blanks() {
        assert_eq!(first_nonempty_line("\n\n  hello\nworld"), "hello");
        assert_eq!(first_nonempty_line(""), "");
    }

    #[test]
    fn preview_config_patch_rejects_invalid() {
        let cfg = MissionConfig::default();
        let bad = serde_json::json!({ "maxParallelWorkers": 4 });
        assert!(preview_config_patch(&cfg, &bad).is_err());
        let good = serde_json::json!({ "worker": { "model": "haiku" } });
        assert!(preview_config_patch(&cfg, &good).is_ok());
    }

    /// Timeout kill discipline: the whole process GROUP dies, not just the
    /// `sh -c` wrapper — a backgrounded child must not survive the gate
    /// giving up (unix only; Windows still kills only the wrapper, see the
    /// TODO in `run_shell_command_with_timeout`).
    #[cfg(unix)]
    #[tokio::test]
    async fn shell_command_timeout_kills_the_whole_process_tree() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("child.pid");
        // A background child that would outlive the wrapper by minutes; its
        // pid is written out before the shell parks in `wait`.
        let command = format!("sleep 300 & echo $! > '{}'; wait", pidfile.display());

        let (ok, output) = tokio::time::timeout(
            Duration::from_secs(10),
            run_shell_command_with_timeout(dir.path(), &command, Duration::from_millis(500)),
        )
        .await
        .expect("timed-out command must return promptly");
        assert!(!ok, "command must be reported failed: {output}");
        assert!(output.contains("timed out"), "got: {output}");

        let pid: i32 = std::fs::read_to_string(&pidfile)
            .expect("shell wrote the background pid before the timeout")
            .trim()
            .parse()
            .expect("pidfile contains a pid");

        // The group SIGKILL must take the background child down: poll until
        // kill(pid, 0) no longer reports it (dead + reaped by init), bounded.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "background child {pid} survived the group kill"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[test]
    fn plan_schema_matches_plan_shape() {
        // A Plan serialized to JSON uses exactly the keys the schema names.
        let plan = Plan {
            goal: "g".into(),
            validation_contract: vec![Assertion {
                id: "a-1".into(),
                statement: "s".into(),
                check: AssertionCheck::Command,
                command: Some("true".into()),
            }],
            milestones: vec![PlanMilestone {
                title: "m".into(),
                features: vec![PlanFeature {
                    title: "f".into(),
                    spec: "s".into(),
                    validation_criteria: vec!["c".into()],
                }],
            }],
        };
        let value = serde_json::to_value(&plan).unwrap();
        let schema = plan_schema();
        let props = schema["properties"].as_object().unwrap();
        for key in value.as_object().unwrap().keys() {
            assert!(props.contains_key(key), "schema missing top-level key {key}");
        }
    }
}
