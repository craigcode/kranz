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

use crate::auth_verify::AuthVerdict;
use crate::backend::{
    AgentBackend, AgentEvent, AgentSession, PromptMode, SessionExit, SessionSpec,
};
use crate::command_exec::{run_shell_command_sandboxed, tail_chars};
use crate::config;
use crate::contract_gates;
use crate::contract_lint;
use crate::contract_sweep;
use crate::control;
use crate::cost;
use crate::digest;
use crate::error::{EngineError, Result};
use crate::event_log::{EventLog, LockForce};
use crate::events::{Event, EventKind};
use crate::findings::{synthesize_fix_specs, FindingsConversion, FixFeatureSpec};
use crate::gate_results;
use crate::git_ops::{with_kranz_trailers, CommitInfo, GitRepo, KranzCommitMetadata};
use crate::judgement::{lesson_provenance_clean, JudgementOutcome};
use crate::knowledge::{self, KnowledgeQuery};
use crate::mission_catalog::{is_terminal_status, mark_mission_index_report};
use crate::paths::MissionPaths;
use crate::permissions;
use crate::planning::{
    assign_assertion_ids, completed_features_unchanged, considered_alternatives_requirement,
    norm_title, upsert_mission_index, validate_considered_alternatives,
    validate_revised_plan_for_gate,
};
use crate::preflight::PREFLIGHT_CLEAR_SUMMARY;
use crate::prompts;
use crate::reducer;
use crate::report_render::{
    render_mission_report, render_plan_markdown, render_research_markdown,
    render_revised_plan_markdown, Research,
};
use crate::runner;
use crate::scrub;
use crate::ticket::Ticket;
use crate::types::*;
use crate::validator_integrity;
use crate::validator_snapshot;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// Max chars of an `orchestrator.decision` summary (matches digest cap).
const DECISION_SUMMARY_MAX: usize = 200;

/// Max chars of `worker.message` content (mirrors the runner's cap).
const MESSAGE_CONTENT_MAX: usize = 2000;

/// Caps for the structured human-question payloads (ticket
/// `structured-human-question-events`) — the Mission Control AskUserQuestion
/// UX contract reference (caps, options, free text) made engine-side:
/// bounded so a model-authored ask can never bloat the append-only log, and
/// scrubbed at write like every other model-authored string the engine
/// persists.
/// Max questions taken from one worker report; the rest is dropped with an
/// operator-visible decision note (never silently).
const QUESTIONS_PER_REPORT_CAP: usize = 4;
/// Max chars of one question's text on `question.opened`.
const QUESTION_TEXT_MAX: usize = 500;
/// Max structured choices kept per question (the Slack card renders one
/// button per option, so this also bounds the chrome).
const QUESTION_OPTIONS_CAP: usize = 4;
/// Max chars of one option's label.
const QUESTION_OPTION_MAX: usize = 100;
/// Max chars of an operator's answer on `question.answered` (operator-typed
/// text still gets scrubbed — a pasted token must never reach the
/// corpus-exported log).
const ANSWER_TEXT_MAX: usize = 500;

/// Sleep between loop iterations while paused (§4.5 step b).
const PAUSE_POLL: Duration = Duration::from_millis(300);

/// Poll interval of the interrupt watcher during worker runs.
const INTERRUPT_POLL: Duration = Duration::from_millis(150);

/// Default cap on the silence between two orchestrator stream events before
/// the session is declared dead (long thinking pauses are expected; ten
/// minutes of *nothing* on a stream-json pipe is not).
const DEFAULT_ORCH_STALL_TIMEOUT: Duration = Duration::from_secs(600);

/// Default deadline for an unanswered grant request before it fails closed
/// (deny-default safety valve). Shrunk by tests via
/// [`MissionEngine::set_grant_request_timeout`].
const DEFAULT_GRANT_REQUEST_TIMEOUT: Duration = Duration::from_secs(3600);

/// Max grant requests one milestone's validation may raise per process run.
/// Each approval extends `command_grants` and re-runs the validator, which can
/// hit a *fresh* command and request again; without a ceiling an auto-approver
/// would spin that park→approve→re-validate loop unbounded. Over the cap, the
/// milestone blocks with the existing refusal semantics instead.
const GRANT_REQUEST_CAP: u32 = 3;

/// Retry nudge sent when a JSON decision turn fails to parse.
pub(crate) const JSON_RETRY_MSG: &str =
    "Your previous reply was not parseable. Output ONLY the requested JSON object — \
     no prose, no code fences, nothing else.";

// ---------------------------------------------------------------------------
// JSON decision shapes (parsed leniently via runner::parse_report)
// ---------------------------------------------------------------------------

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
    /// For a milestone parked on a dispatch-pool judgement (KRZ-303/304):
    /// the zero-based index of the candidate the operator chose (the
    /// `-c<i>` suffix of the `kranz/pool/*` branches), or null when no
    /// candidate was selected. Purely the resolution RECORD's payload —
    /// the engine never merges a candidate.
    #[serde(default)]
    candidate: Option<u32>,
    /// Optional operator guidance injected verbatim into the next validator
    /// task (and its retry) — the only channel by which unblock text can
    /// reach a fresh validator session.
    #[serde(default)]
    validator_guidance: Option<String>,
    /// For action "unblock-add-fix": the repair feature to schedule before
    /// re-validation (a fmt pass, a doc fix, …). Missing fields are
    /// synthesized from the note.
    #[serde(default)]
    fix: Option<FixFeatureSpec>,
}

/// Outcome of a [`MissionEngine::request_plan`] turn.
///
/// "Not ready to emit, wants to keep talking" is a normal conversational
/// state during planning — the orchestrator may still have open questions —
/// so it is a variant here, not an [`EngineError`]. Only genuine transport/
/// session failures surface as `Err`.
#[derive(Debug)]
pub enum PlanRequest {
    /// The plan parsed; returned unapproved.
    Ready(Plan),
    /// Neither the plan turn nor the JSON-only retry produced parseable plan
    /// JSON. Carries the orchestrator's reply text (scrubbed): the retry
    /// turn's text, or the first turn's when the retry's is empty — so the
    /// caller can show the user what the model actually said.
    NotReady(String),
    /// The orchestrator CAN plan but believes the plan is likely WRONG — the
    /// goal is misframed, the premise is broken, the spec is confidently
    /// off. A planner-initiated escalation only: it arrives exclusively as an
    /// explicit `{"wrongPlan": "…"}` JSON reply and is never inferred from
    /// prose. Carries the one-paragraph reason.
    WrongPlan { reason: String },
}

struct SelectedBackend {
    backend: Arc<dyn AgentBackend>,
    kind: BackendKind,
    cfg: MissionConfig,
    fallback_reason: Option<String>,
}

/// The parallelization decision for one milestone (roadmap M3): which of the
/// pending features are INDEPENDENT enough to run concurrently, and the order
/// their branches must merge back in. Parsed leniently; a missing/empty answer
/// takes the conservative all-sequential default (see [`MissionEngine::plan_parallel_batch`]).
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ParallelDecision {
    /// Feature ids the orchestrator judged independent (safe to run in
    /// separate worktrees concurrently). Unknown ids are ignored by the caller.
    #[serde(default)]
    independent: Vec<String>,
    /// Declared merge order for the independent features (feature ids). The
    /// caller merges in this order, falling back to plan order for any
    /// independent id the orchestrator omitted here.
    #[serde(default)]
    merge_order: Vec<String>,
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
    pub(crate) paths: MissionPaths,
    pub(crate) log: EventLog,
    pub(crate) state: MissionState,
    pub(crate) repo: GitRepo,
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
    /// Reply text of the most recent seed turn (fresh session, resume-ack, or
    /// re-seed), captured instead of discarded so the UI can surface it — the
    /// planning seed's reply routinely ends with scoping questions the user
    /// must see. Drained by [`MissionEngine::take_seed_reply`].
    pending_seed_reply: Option<String>,
    /// Research evidence extracted from the most recent Ready plan JSON, held
    /// in memory until approval renders and commits `research.md` beside
    /// `plan.md` (repo-knowledge-store slice 1). Not runtime-durable: it spans
    /// the draft→approve window within one engine instance, which both the CLI
    /// draft flow and the registry-held hosted flow keep alive.
    pub(crate) pending_research: Option<Research>,
    /// Lazily-built [`crate::backend_codex::CodexBackend`] cache for roles
    /// whose `backend = "codex"`. `None` until the first successful probe; a
    /// failed probe is never cached (so a codex install that appears
    /// mid-mission is picked up on the next role spawn).
    codex_backend: Option<Arc<dyn AgentBackend>>,
    /// Lazily-built [`crate::backend_droid::DroidBackend`] cache for roles
    /// whose `backend = "droid"`. Mirrors `codex_backend`: `None` until the
    /// first successful probe; a failed probe is never cached.
    droid_backend: Option<Arc<dyn AgentBackend>>,
    /// Lazily-built [`crate::backend_kimi::KimiBackend`] cache for roles
    /// whose `backend = "kimi"`. Mirrors `codex_backend`: `None` until the
    /// first successful probe; a failed probe is never cached.
    kimi_backend: Option<Arc<dyn AgentBackend>>,
    /// Lazily-built [`crate::backend_cursor::CursorBackend`] cache for roles
    /// whose `backend = "cursor"`. Mirrors `codex_backend`: `None` until the
    /// first successful probe; a failed probe is never cached.
    cursor_backend: Option<Arc<dyn AgentBackend>>,
    /// The tree mission-branch work runs in for the current `run()` call
    /// (M7 tier 1). `None` in checkout mode (and before the first `run()`),
    /// where [`Self::active_root`]/[`Self::active_repo`] fall back to
    /// `self.paths.repo_root`/`self.repo`. In worktree mode, `run()` sets
    /// this to the mission integration worktree from `setup_mission_worktree`
    /// for the duration of the run.
    active_tree: Option<(PathBuf, GitRepo)>,
    /// Primary checkout's branch as of the start of this `run()` call, in
    /// worktree mode only (M7 tier 1, feature f-1-2). Compared against the
    /// primary's current branch by the out-of-contract sweep's
    /// primary-checkout cleanliness check: the primary must never move once
    /// mission-branch work is routed to the integration worktree.
    primary_branch_at_start: Option<String>,
    /// Once-per-mission cache of the worker HOME relocate-vs-inherit decision
    /// (mission m-165b6f, f-2-1): computed on the first worker spawn by
    /// driving [`crate::auth_verify::verify_worker_auth`] against `self.backend`,
    /// then reused for every subsequent worker in this mission so the trivial
    /// preflight session is spawned exactly once, not once per worker. `None`
    /// until the first call to [`Self::worker_auth_verdict`].
    worker_auth_verdict: Option<AuthVerdict>,
    /// See [`DEFAULT_GRANT_REQUEST_TIMEOUT`]; shrunk by tests.
    grant_request_timeout: Duration,
    /// When the currently-parked grant request was raised, for the timeout →
    /// deny-default valve. Set alongside `pending_grant_request`, cleared when
    /// it resolves. Ephemeral: a restart re-arms the clock, but the parked
    /// request itself is durable in `pending_grant_request`.
    grant_requested_at: Option<std::time::Instant>,
    /// Per-milestone count of grant requests raised this process run, capped by
    /// `grant_request_cap`. Ephemeral: a restart re-arms the budget.
    grant_requests: HashMap<String, u32>,
    /// Per-feature count of worker respawns caused by a `WorkerDeny` grant park
    /// (each park re-runs the worker on re-entry). Subtracted from
    /// `feature.respawns` in the judgement `max_respawns` check so an operator
    /// approving deny-lifts doesn't consume the failure-retry budget. Ephemeral:
    /// a restart drops the credit and re-couples the counters, so pre-restart
    /// grant re-runs count against `max_respawns` again and can exhaust the
    /// budget earlier than intended — fail-safe (fails closed, never loops),
    /// the same trade-off as the cap counter.
    grant_respawns: HashMap<String, u32>,
    /// Ceiling on grant requests per milestone per run (default
    /// [`GRANT_REQUEST_CAP`]; shrunk by tests to exercise the cap boundary).
    grant_request_cap: u32,
    /// The workspace provisioned by the WorkspaceProvider seam for the
    /// current `run()` call (design D-B). Set by
    /// [`Self::provision_workspace`], consumed by
    /// [`Self::teardown_workspace`] at the end of the run. Ephemeral: a new
    /// `run()` (e.g. after resume) re-provisions.
    pub(crate) workspace_handle: Option<crate::workspace_provider::WorkspaceHandle>,
    /// The provider `run()` resolved for this run (design D-B), Arc-shared
    /// so `validation_round` can drive the golden-data reset-between-rounds
    /// hook (design D-D) through the same seam without borrowing `self`.
    /// `None` outside `run()` (unit tests calling `validation_round`
    /// directly skip the reset).
    pub(crate) workspace_provider: Option<Arc<dyn crate::workspace_provider::WorkspaceProvider>>,
}

impl MissionEngine {
    // -----------------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------------

    /// Create a brand-new mission: validate config, open the repo, pick a
    /// mission id, acquire the event log, and emit `mission.created`.
    ///
    /// When `goal` carries a task class folded in by [`crate::ticket::Ticket::mission_goal`]
    /// (execution-class backlog tickets), routes the executor to the local
    /// tier before the config is stored on `mission.created` and records the
    /// routing decision — every seed path (`kranz draft`/`exec`, REST, Slack)
    /// creates missions from that folded goal string, so this is the single
    /// place ticket→routing wiring needs to live.
    pub fn create(
        backend: Arc<dyn AgentBackend>,
        repo_root: impl Into<PathBuf>,
        goal: &str,
        mut cfg: MissionConfig,
    ) -> Result<Self> {
        config::validate(&cfg)?;
        let task_class = crate::ticket::parse_task_class_from_goal(goal);
        let routing_summary = task_class
            .as_deref()
            .map(|task_class| config::route_task_class_executor(&mut cfg, Some(task_class)).1);
        let repo_root = canonical_root(repo_root.into());
        let repo = GitRepo::open(&repo_root)?;
        repo.ensure_identity()?;
        // The current branch becomes this mission's base. Basing one mission
        // on another's branch inherits unmerged work and records a poisoned
        // base (observed live: sequential drafts stacked three mission
        // branches on each other) — loud refusal beats silent stacking.
        let base_branch = repo.current_branch()?;
        if base_branch.starts_with("kranz/mission-") {
            return Err(EngineError::InvalidState(format!(
                "refusing to create a mission while '{base_branch}' is checked out — \
                 another mission's branch would become this mission's base; \
                 check out the intended base (e.g. main) first"
            )));
        }

        let mission_id = format!("m-{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
        let paths = MissionPaths::new(&repo_root, &mission_id);
        write_kranz_gitignore(&paths)?;

        // A brand-new mission id can never have a legitimate lock holder, so
        // never force: a collision here is a bug worth surfacing, not one to
        // steal through.
        let mut log = EventLog::acquire(
            &paths,
            &mission_id,
            Duration::from_millis(cfg.event_stream_throttle_ms),
            LockForce::No,
        )?;

        let mission_branch = format!("kranz/mission-{mission_id}");
        let (created, audits) = log.append_with_redaction_audits(EventKind::MissionCreated {
            goal: goal.to_string(),
            base_branch,
            mission_branch,
            config: cfg,
        })?;
        let mut events = vec![created];
        events.extend(audits);
        let state = reducer::fold(&events)?;
        reducer::write_snapshot(&state, &paths.state_file())?;

        let mut engine = MissionEngine {
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
            pending_seed_reply: None,
            pending_research: None,
            codex_backend: None,
            droid_backend: None,
            kimi_backend: None,
            cursor_backend: None,
            active_tree: None,
            primary_branch_at_start: None,
            worker_auth_verdict: None,
            grant_request_timeout: DEFAULT_GRANT_REQUEST_TIMEOUT,
            grant_requested_at: None,
            grant_requests: HashMap::new(),
            grant_respawns: HashMap::new(),
            grant_request_cap: GRANT_REQUEST_CAP,
            workspace_handle: None,
            workspace_provider: None,
        };
        if let Some(summary) = routing_summary {
            engine.emit_decision(summary, None)?;
        }
        Ok(engine)
    }

    /// Resume an existing mission from its event log (§4.3 kill-safety).
    ///
    /// Rebuilds state by folding the log, re-acquires the single-writer lock
    /// (`force` selects the [`LockForce`] steal tier; a provably dead holder
    /// is always stolen), and remembers the sdk session id of the most recent
    /// orchestrator session for `--resume`. No agent session is started here
    /// — sessions are lazy.
    pub fn resume(
        backend: Arc<dyn AgentBackend>,
        repo_root: impl Into<PathBuf>,
        mission_id: &str,
        force: LockForce,
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
            EventKind::WorkerSpawned {
                role: Role::Orchestrator,
                sdk_session_id,
                ..
            } => Some(sdk_session_id.clone()),
            _ => None,
        });

        let log = EventLog::acquire(
            &paths,
            mission_id,
            Duration::from_millis(state.config.event_stream_throttle_ms),
            force,
        )?;

        // Reap per-feature worktrees/branches orphaned by a crash mid parallel
        // batch (M3). Any `kranz/wt/<mission>/*` worktree or branch exists only
        // while a lock-holding engine is mid-batch, so with the lock now held
        // these are leaks from a dead engine. Removing them stops accumulation
        // AND lets a re-forked Pending feature run cleanly (the branch no
        // longer "already exists"). MUST run after EventLog::acquire: the
        // sweep is destructive (`worktree remove --force`, `branch -D`), and
        // running it lock-free would let a second `kranz run` rip live
        // worktrees out from under a running engine before failing LockHeld.
        // Best-effort and idempotent: remove_worktree/delete_branch_force
        // tolerate absence; branch deletion runs after prune (git refuses to
        // -D a branch checked out in a still-registered worktree).
        for milestone in &state.mission.milestones {
            for feature in &milestone.features {
                for path in [
                    parallel_worktree_path(&repo_root, mission_id, &feature.id),
                    legacy_parallel_worktree_path(mission_id, &feature.id),
                ] {
                    if path.exists() {
                        let _ = repo.remove_worktree(&path);
                    }
                }
                // Dispatch-pool candidate worktree DIRS (KRZ-303) are crash
                // leaks under the same lifetime rule (they exist only while a
                // lock-holding engine is mid-dispatch). Pool BRANCHES are
                // deliberately NOT deleted here: they are the recorded
                // candidate deliverables — deleting them would destroy the
                // evidence the mission parked to preserve.
                for index in 0..crate::config::MAX_WORKER_CANDIDATES {
                    let path = pool_worktree_path(&repo_root, mission_id, &feature.id, index);
                    if path.exists() {
                        let _ = repo.remove_worktree(&path);
                    }
                }
            }
        }
        // A leaked mission integration worktree (M7 tier 1) is the same story:
        // it exists only while a lock-holding engine has one set up, so with
        // the lock now held it is a crash leak. Reap it the same way.
        for integration_path in [
            mission_worktree_path(&repo_root, mission_id),
            legacy_mission_worktree_path(mission_id),
        ] {
            if integration_path.exists() {
                let _ = repo.remove_worktree(&integration_path);
            }
        }
        let _ = repo.prune_worktrees();
        for milestone in &state.mission.milestones {
            for feature in &milestone.features {
                let branch = format!("kranz/wt/{mission_id}/{}", feature.id);
                if repo.branch_exists(&branch).unwrap_or(false) {
                    let _ = repo.delete_branch_force(&branch);
                }
            }
        }
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
            pending_seed_reply: None,
            pending_research: None,
            codex_backend: None,
            droid_backend: None,
            kimi_backend: None,
            cursor_backend: None,
            active_tree: None,
            primary_branch_at_start: None,
            worker_auth_verdict: None,
            grant_request_timeout: DEFAULT_GRANT_REQUEST_TIMEOUT,
            grant_requested_at: None,
            grant_requests: HashMap::new(),
            grant_respawns: HashMap::new(),
            grant_request_cap: GRANT_REQUEST_CAP,
            workspace_handle: None,
            workspace_provider: None,
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

    /// The tree mission-branch git operations run in for the current run
    /// (M7 tier 1). Checkout mode (or before the first `run()` in worktree
    /// mode): the primary repo root. Worktree mode mid-run: the mission
    /// integration worktree set up by `run()`.
    pub(crate) fn active_root(&self) -> &Path {
        match &self.active_tree {
            Some((root, _)) => root.as_path(),
            None => self.paths.repo_root.as_path(),
        }
    }

    /// The [`GitRepo`] paired with [`Self::active_root`].
    pub(crate) fn active_repo(&self) -> &GitRepo {
        match &self.active_tree {
            Some((_, repo)) => repo,
            None => &self.repo,
        }
    }

    /// [`MissionPaths`] rooted at [`Self::active_root`] (mirrors `self.paths`'
    /// join logic, just against whichever tree mission-branch git ops run in
    /// right now). Use this instead of `self.paths` for any file that gets
    /// committed onto the mission branch, so worktree mode writes land in the
    /// integration worktree rather than the primary checkout.
    pub(crate) fn active_paths(&self) -> MissionPaths {
        MissionPaths::new(self.active_root(), self.state.mission.id.clone())
    }

    /// Shrink the orchestrator stall timeout (tests exercise the death/reseed
    /// path without waiting ten minutes).
    pub fn set_orch_stall_timeout(&mut self, timeout: Duration) {
        self.orch_stall_timeout = timeout;
    }

    /// Shrink the grant-request timeout (tests exercise the timeout →
    /// deny-default path without waiting an hour).
    pub fn set_grant_request_timeout(&mut self, timeout: Duration) {
        self.grant_request_timeout = timeout;
    }

    /// Shrink the per-milestone grant-request cap (tests exercise the
    /// cap-boundary → block path without scripting three approvals).
    pub fn set_grant_request_cap(&mut self, cap: u32) {
        self.grant_request_cap = cap;
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
    ///
    /// Fold-validate BEFORE append (ticket `emit-never-poisons-log`; source
    /// m-83d1ed, where a re-proposed fixfeature payload appended fine and then
    /// failed the fold — the append-only log was left holding an event no
    /// replay can ever fold, and recovery meant surgery on the audit log).
    /// The fold is computed against a CLONE of the current state; on failure
    /// nothing is appended, the error surfaces to the caller, and log and
    /// state stay exactly as they were. On success the real path below folds
    /// the same event a second time — the honest price of the invariant,
    /// trivial next to an agent turn. Stream deltas are exempt: their apply
    /// arm is infallible by construction and they are the hot path, so
    /// cloning state per delta would tax the one caller that emits thousands.
    ///
    /// One named gap: the probe validates the UNscrubbed kind while the real
    /// fold applies the redaction-scrubbed event. Scrubbing only rewrites
    /// secret-shaped substrings inside string payloads, which no
    /// fold-validity rule keys on — a payload id literally shaped like an API
    /// key is the pathological exception, accepted and documented.
    pub(crate) fn emit(&mut self, kind: EventKind) -> Result<Event> {
        if !kind.is_stream_delta() {
            let mut probe = self.state.clone();
            let probe_event = Event {
                seq: self.state.last_seq + 1,
                ts: chrono::Utc::now(),
                mission_id: self.paths.mission_id.clone(),
                kind: kind.clone(),
            };
            reducer::apply(&mut probe, &probe_event)?;
        }
        let (event, audits) = self.log.append_with_redaction_audits(kind)?;
        let stream_delta = event.kind.is_stream_delta();
        reducer::apply(&mut self.state, &event)?;
        for audit in &audits {
            reducer::apply(&mut self.state, audit)?;
        }
        let snapshot = reducer::write_snapshot(&self.state, &self.paths.state_file());
        if stream_delta && audits.is_empty() {
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
    pub(crate) fn emit_decision(&mut self, summary: &str, detail: Option<String>) -> Result<()> {
        self.emit(EventKind::OrchestratorDecision {
            summary: scrub::scrub_and_truncate(summary, DECISION_SUMMARY_MAX),
            detail: detail.map(|d| scrub::scrub(&d)),
        })?;
        Ok(())
    }

    /// Public entry point for callers outside this module (e.g. the ticket
    /// draft seeding path) to record an `orchestrator.decision`, such as the
    /// executor-tier routing choice made when a mission is created from a
    /// ticket.
    pub fn record_decision(&mut self, summary: &str, detail: Option<String>) -> Result<()> {
        self.emit_decision(summary, detail)
    }

    /// Choose the backend for a role and return a config clone whose role
    /// model has been normalized for the backend actually used.
    ///
    /// `*.backend == "codex"` / `"droid"` probes the corresponding CLI and
    /// lazily caches the constructed backend on success. Probe failure falls
    /// back to the injected Claude backend and returns a loud
    /// `fallback_reason`; callers MUST record it before spawning.
    fn select_backend(&mut self, role: Role) -> SelectedBackend {
        let requested = self.state.config.backend_kind(role);
        let role_name = role_label(role);
        let mut cfg = self.state.config.clone();
        let set_effective_model = |cfg: &mut MissionConfig, kind: BackendKind| {
            let role_cfg = match role {
                Role::Orchestrator => &mut cfg.orchestrator,
                Role::Worker => &mut cfg.worker,
                Role::ValidatorScrutiny => &mut cfg.validator_scrutiny,
                Role::ValidatorFunctional => &mut cfg.validator_functional,
            };
            role_cfg.model = config::effective_model(role, kind, &role_cfg.model);
        };

        match requested {
            BackendKind::Claude => {
                set_effective_model(&mut cfg, BackendKind::Claude);
                SelectedBackend {
                    backend: Arc::clone(&self.backend),
                    kind: BackendKind::Claude,
                    cfg,
                    fallback_reason: None,
                }
            }
            BackendKind::Local | BackendKind::Acp => {
                // No-fallback kinds: `config::validate` has already guaranteed
                // the role's endpoint/command config, and there is no binary
                // to probe — construction cannot fail.
                set_effective_model(&mut cfg, requested);
                let backend = self
                    .resolve_kind_backend(requested, role)
                    .expect("validate guarantees local/acp role config");
                SelectedBackend {
                    backend,
                    kind: requested,
                    cfg,
                    fallback_reason: None,
                }
            }
            BackendKind::Codex | BackendKind::Droid | BackendKind::Kimi | BackendKind::Cursor => {
                match self.resolve_kind_backend(requested, role) {
                    Ok(backend) => {
                        set_effective_model(&mut cfg, requested);
                        SelectedBackend {
                            backend,
                            kind: requested,
                            cfg,
                            fallback_reason: None,
                        }
                    }
                    Err(err) => {
                        set_effective_model(&mut cfg, BackendKind::Claude);
                        SelectedBackend {
                            backend: Arc::clone(&self.backend),
                            kind: BackendKind::Claude,
                            cfg,
                            fallback_reason: Some(format!(
                                "{} backend requested for the {role_name} but not available \
                                 ({err}); falling back to the claude {role_name}",
                                requested.as_str()
                            )),
                        }
                    }
                }
            }
        }
    }

    /// Construct (or reuse the cached) backend for `kind`, WITHOUT any claude
    /// fallback. Shared by [`Self::select_backend`] — which layers the
    /// per-kind fallback policy on top — and [`Self::select_pool_candidate`],
    /// which must never fall back (see there).
    fn resolve_kind_backend(
        &mut self,
        kind: BackendKind,
        role: Role,
    ) -> Result<Arc<dyn AgentBackend>> {
        match kind {
            BackendKind::Claude => Ok(Arc::clone(&self.backend)),
            BackendKind::Codex => {
                if let Some(cached) = &self.codex_backend {
                    return Ok(Arc::clone(cached));
                }
                let binary = crate::backend_codex::discover_codex_binary(None)?;
                let backend: Arc<dyn AgentBackend> =
                    Arc::new(crate::backend_codex::CodexBackend::new(binary));
                self.codex_backend = Some(Arc::clone(&backend));
                Ok(backend)
            }
            BackendKind::Droid => {
                if let Some(cached) = &self.droid_backend {
                    return Ok(Arc::clone(cached));
                }
                let binary = crate::backend_droid::discover_droid_binary(None)?;
                let backend: Arc<dyn AgentBackend> =
                    Arc::new(crate::backend_droid::DroidBackend::new(binary));
                self.droid_backend = Some(Arc::clone(&backend));
                Ok(backend)
            }
            BackendKind::Kimi => {
                if let Some(cached) = &self.kimi_backend {
                    return Ok(Arc::clone(cached));
                }
                let binary = crate::backend_kimi::discover_kimi_binary(None)?;
                let backend: Arc<dyn AgentBackend> =
                    Arc::new(crate::backend_kimi::KimiBackend::new(binary));
                self.kimi_backend = Some(Arc::clone(&backend));
                Ok(backend)
            }
            BackendKind::Cursor => {
                if let Some(cached) = &self.cursor_backend {
                    return Ok(Arc::clone(cached));
                }
                let binary = crate::backend_cursor::discover_cursor_binary(None)?;
                let backend: Arc<dyn AgentBackend> =
                    Arc::new(crate::backend_cursor::CursorBackend::new(binary));
                self.cursor_backend = Some(Arc::clone(&backend));
                Ok(backend)
            }
            BackendKind::Local => {
                let role_cfg = self.state.config.role(role);
                // `config::validate` has already guaranteed base_url and
                // context_budget are present for a local-backed role; there
                // is no binary to probe and therefore no claude fallback.
                let base_url = role_cfg
                    .base_url
                    .clone()
                    .expect("validate guarantees base_url for backend = local");
                let temperature = role_cfg.temperature;
                let context_budget = role_cfg
                    .context_budget
                    .expect("validate guarantees context_budget for backend = local");
                let backend: Arc<dyn AgentBackend> = Arc::new(
                    crate::backend_local::LocalBackend::new(base_url, temperature, context_budget),
                );
                Ok(backend)
            }
            BackendKind::Acp => {
                let role_cfg = self.state.config.role(role);
                // `config::validate` has already guaranteed acp_command is
                // present for an acp-backed role (worker only). Like local,
                // there is no binary discovery: ACP defines no `--version`
                // convention, so the initialize handshake at session start
                // IS the probe — a non-ACP executable fails there, loudly,
                // and there is no claude fallback to hide that behind.
                let acp_command = role_cfg
                    .acp_command
                    .clone()
                    .expect("validate guarantees acp_command for backend = acp");
                let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_acp::AcpBackend::new(
                    acp_command,
                    role_cfg.acp_args.clone(),
                ));
                Ok(backend)
            }
        }
    }

    /// Select the backend for ONE dispatch-pool candidate (KRZ-303). Unlike
    /// [`Self::select_backend`] there is deliberately NO claude fallback: a
    /// pool whose unavailable candidate silently reran on claude would record
    /// two same-backend "candidates" — fake diversity, the exact opposite of
    /// the ticket's point (cross-harness divergence for scrutiny). An
    /// unavailable candidate backend errors here; the caller records that
    /// stream's terminal state and its siblings run unaffected.
    ///
    /// The returned cfg pins the WORKER role to the candidate's backend and
    /// (backend-normalized) model; everything else is the mission config.
    fn select_pool_candidate(&mut self, spec: &CandidateSpec) -> Result<SelectedBackend> {
        let kind = config::parse_backend(Some(&spec.backend)).map_err(|other| {
            EngineError::Config(format!(
                "workerCandidates entry names unknown backend {other:?} (config::validate \
                 should have rejected it at mission boundaries)"
            ))
        })?;
        if matches!(kind, BackendKind::Local | BackendKind::Acp) {
            return Err(EngineError::Config(format!(
                "workerCandidates entry backend {:?} is not supported in this pass \
                 (config::validate should have rejected it at mission boundaries)",
                spec.backend
            )));
        }
        let mut cfg = self.state.config.clone();
        cfg.worker.backend = Some(spec.backend.clone());
        cfg.worker.model = config::effective_model(Role::Worker, kind, &spec.model);
        let backend = self.resolve_kind_backend(kind, Role::Worker)?;
        Ok(SelectedBackend {
            backend,
            kind,
            cfg,
            fallback_reason: None,
        })
    }

    fn claude_fallback_cfg_for_role(&self, role: Role) -> MissionConfig {
        let mut cfg = self.state.config.clone();
        let fallback_model = match role {
            Role::Orchestrator | Role::ValidatorScrutiny => "opus",
            Role::Worker | Role::ValidatorFunctional => "sonnet",
        };
        match role {
            Role::Orchestrator => cfg.orchestrator.model = fallback_model.to_string(),
            Role::Worker => cfg.worker.model = fallback_model.to_string(),
            Role::ValidatorScrutiny => cfg.validator_scrutiny.model = fallback_model.to_string(),
            Role::ValidatorFunctional => {
                cfg.validator_functional.model = fallback_model.to_string()
            }
        }
        cfg
    }

    /// The worker HOME relocate-vs-inherit decision for this mission (mission
    /// m-165b6f, f-2-1), computed ONCE and cached in `self.worker_auth_verdict`.
    ///
    /// On the first call this drives a real trivial session via
    /// [`crate::auth_verify::verify_worker_auth`] against `self.backend` under a
    /// scratch candidate `HOME`/`CLAUDE_CONFIG_DIR` (seeded the same way a
    /// relocated worker's env would be); every subsequent call — across every
    /// worker this mission spawns, sequential or concurrent — returns the
    /// cached verdict without spawning another preflight session. If seeding
    /// the scratch candidate env fails (e.g. an unwritable temp dir), that is
    /// [`AuthVerdict::Inconclusive`] (fail-safe), same as the runner does for
    /// scratch-home seeding elsewhere.
    async fn worker_auth_verdict(&mut self) -> AuthVerdict {
        if let Some(verdict) = self.worker_auth_verdict {
            return verdict;
        }
        let real_home = std::env::var_os("HOME").map(PathBuf::from);
        let real_config_dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from);
        let scratch_root = crate::backend_claude::scratch_home_root(&format!(
            "preflight-{}",
            self.state.mission.id
        ));
        let verdict = match crate::backend_claude::seed_worker_scratch_home(
            &scratch_root,
            real_home.as_deref(),
            real_config_dir.as_deref(),
        ) {
            Ok((home, config_dir)) => {
                let mut candidate_env = HashMap::new();
                candidate_env.insert("HOME".to_string(), home.display().to_string());
                candidate_env.insert(
                    "CLAUDE_CONFIG_DIR".to_string(),
                    config_dir.display().to_string(),
                );
                crate::auth_verify::verify_worker_auth(self.backend.as_ref(), &candidate_env).await
            }
            Err(_) => AuthVerdict::Inconclusive,
        };
        self.worker_auth_verdict = Some(verdict);
        verdict
    }

    /// Test-only seam (mission m-165b6f, f-2-2): pre-seeds the cached
    /// worker-auth verdict so `MockBackend`-driven mission-flow tests don't
    /// have the live preflight (see [`Self::worker_auth_verdict`]) consume a
    /// `MockScript` meant for a real worker/validator session — the
    /// preflight and its verdict handling are covered directly by
    /// `auth_verify`'s own unit tests instead. Never call this outside
    /// tests: it bypasses the real auth-verification guarantee the
    /// preflight exists to provide.
    #[doc(hidden)]
    pub fn seed_worker_auth_verdict_for_test(&mut self, verdict: AuthVerdict) {
        self.worker_auth_verdict = Some(verdict);
    }

    /// Test hook: pre-seed a lazily-constructed per-kind backend cache so
    /// dispatch-pool tests can drive non-claude candidates with scripted
    /// [`crate::backend_mock::MockBackend`]s instead of real agent CLIs (the
    /// discovery probes read the host, which has no codex/droid/kimi binary
    /// under test). Never call this outside tests: it bypasses the real
    /// backend discovery the probe exists to perform.
    #[doc(hidden)]
    pub fn seed_kind_backend_for_test(
        &mut self,
        kind: BackendKind,
        backend: Arc<dyn AgentBackend>,
    ) {
        match kind {
            BackendKind::Codex => self.codex_backend = Some(backend),
            BackendKind::Droid => self.droid_backend = Some(backend),
            BackendKind::Kimi => self.kimi_backend = Some(backend),
            BackendKind::Cursor => self.cursor_backend = Some(backend),
            // claude is the engine's primary backend (injected at create);
            // local/acp have no probe cache to seed.
            BackendKind::Claude | BackendKind::Local | BackendKind::Acp => {}
        }
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

    /// Take (and clear) the reply text of the most recent orchestrator seed
    /// turn. `None` when no seed turn ran since the last take, or when its
    /// reply was trivially empty. Callers surface this BEFORE the turn's own
    /// output — the seed reply happened first in the conversation.
    pub fn take_seed_reply(&mut self) -> Option<String> {
        self.pending_seed_reply.take()
    }

    /// Approve a plan: normalize it, create the mission branch, write and
    /// commit `plan.json` (the engine writes and commits — the orchestrator
    /// never touches files, plan §4.4), and emit `plan.approved`.
    ///
    /// Worktree mode (M7 tier 1): the branch is created but never checked
    /// out in the primary tree; the commit instead happens in a short-lived
    /// integration worktree (`setup_mission_worktree`/`teardown_mission_worktree`,
    /// same helpers `run()` uses for the rest of the mission), so the primary
    /// checkout never moves off its starting branch. Checkout mode is
    /// unchanged: check out the branch in the primary tree and commit there.
    pub fn approve_plan(&mut self, mut plan: Plan) -> Result<()> {
        if self.state.mission.status != MissionStatus::Planning {
            return Err(EngineError::InvalidState(format!(
                "approve_plan requires Planning status, mission is {:?}",
                self.state.mission.status
            )));
        }
        if plan.milestones.is_empty() {
            return Err(EngineError::InvalidState(
                "plan has no milestones".to_string(),
            ));
        }
        if let Some(empty) = plan.milestones.iter().find(|m| m.features.is_empty()) {
            return Err(EngineError::InvalidState(format!(
                "plan milestone '{}' has no features",
                empty.title
            )));
        }

        // Workspace contract (D-A): validate the base-branch-owned
        // `.kranz/workspace.json` from the repo ROOT — never the mission
        // branch, so a mission cannot weaken the contract that judges it
        // (merge-gates ownership, same spirit). Missing ⇒ today's behavior
        // unchanged; present-but-invalid ⇒ fail closed, owner repo-setup,
        // before any branch/commit side effects below.
        let approval_contract =
            crate::workspace_contract::load_workspace_contract(&self.paths.repo_root)?;

        // Provider pin (D-B, ticket workspace-provider-pin-at-approval):
        // resolve the EFFECTIVE provider now — an unknown `workspace.provider`
        // name refuses approval HERE, before any branch/commit side effects
        // below (owner: operator), never a silent default on a misspelled
        // name. The pin event itself is emitted beside `plan.approved` —
        // AFTER the fallible git/commit steps, so a failed approve stays
        // event-free and retryable, and the log reads: contract validated →
        // provider pinned → plan approved.
        let workspace_pin = crate::workspace_provider::pin(
            &self.state.config.workspace,
            self.state.config.isolation(),
            approval_contract.as_ref(),
        )?;

        assign_assertion_ids(&mut plan.validation_contract);

        let calibration = cost::calibrate(&self.paths.repo_root);
        let estimate = cost::estimate(&plan, &self.state.config, &calibration.params);
        let estimate = cost::apply_shape(estimate, &plan, &calibration);
        validate_considered_alternatives(&plan, &estimate, &self.state.config)?;

        // Context-fit check (plan-feature-context-fit-check ticket): warn
        // when a feature looks bigger than one worker session — advisory
        // only (a decision event + the plan.md note rendered from it), never
        // a gate. Splitting is cheap here; respawns are expensive later.
        let fit_anchor = crate::plan_fit::corpus_fit_anchor(&self.paths.repo_root);
        let fit_warnings = crate::plan_fit::feature_fit_warnings(&plan, &fit_anchor);
        let fit_note = if fit_warnings.is_empty() {
            None
        } else {
            let note = crate::plan_fit::render_fit_note(&fit_warnings, &fit_anchor);
            self.emit_decision(
                &format!(
                    "context-fit check: {} feature(s) look bigger than one worker session",
                    fit_warnings.len()
                ),
                Some(note.clone()),
            )?;
            Some(note)
        };
        // repo-knowledge-store slice 1: research.md is soft-prompted over the
        // considered-alternatives threshold, not gated. Surface the gap in
        // telemetry so we can see (before hardening) how often over-threshold
        // drafts arrive without a research artifact.
        if self.pending_research.is_none()
            && considered_alternatives_requirement(&plan, &estimate, &self.state.config).is_some()
        {
            tracing::warn!(
                mission = %self.state.mission.id,
                "approving an over-threshold plan with no research.md (research is \
                 soft-prompted, not gated)"
            );
        }

        // Git first: if anything fails here, no event was emitted and
        // approve_plan can simply be retried.
        let base = self.state.mission.base_branch.clone();
        let branch = self.state.mission.mission_branch.clone();
        if !self.repo.branch_exists(&branch)? {
            self.repo.create_branch(&branch, Some(&base))?;
        }
        let worktree_mode = self.state.config.isolation() == WorkerIsolation::Worktree;
        if !worktree_mode {
            self.repo.checkout(&branch)?;
        }
        // Committing plan files onto the mission branch below does not move
        // the base branch ref, so resolving it anywhere in approve_plan pins
        // the base tip as of approval (plan §f-1-2: never re-resolve later —
        // that would reintroduce the moving-base-branch race this fixes).
        // Unaffected by worktree_mode: `base` is resolved against the primary
        // repo either way, and creating (but not checking out) the mission
        // branch never moves it.
        let base_sha = self.repo.rev_parse(&base)?;

        // Lint each `check: command` assertion against the untouched base
        // tree (M8 tier 1, feature f-1-2): at this point the working tree is
        // either still on `base` (worktree mode never checks out the mission
        // branch on the primary) or was just checked out onto a mission
        // branch freshly created FROM `base` above, with nothing committed
        // onto it yet — either way this is the pristine base. Never blocks
        // approval; only informs the operator and plan.md. Deliberately the
        // stricter `is_clean()` rather than `is_clean_tracked()`: this note
        // is advisory-only and a false positive (flagging an untracked
        // scratch file as "dirty") costs nothing, whereas `is_clean_tracked`
        // would silently ignore untracked-but-not-ignored files that could
        // still leak into a command assertion's output.
        let tree_clean_at_base = self.repo.is_clean()?;
        let contract_lint_report = contract_lint::run_contract_lint(
            &self.paths.repo_root,
            Some(&base_sha),
            &plan.validation_contract,
            tree_clean_at_base,
            &self.state.config.contract_env_passthrough,
        );

        // Named, deterministic contract-validation gates (ticket
        // contract-validation-gates.md): the defect classes behind the lint —
        // vacuous-filter, wrong-polarity, passes-on-base, env-sensitive —
        // evaluated through the gate plugin interface (gate.rs) so each
        // verdict carries its class name into the approval decision and
        // plan.md below. Static gates inspect the command text against the
        // (still pristine) repo root; passes-on-base graduates the lint
        // report. Advisory only, exactly like the lint: approval never
        // blocks on these.
        let gate_reports = contract_gates::contract_gate_reports(
            &plan.validation_contract,
            Some(&contract_lint_report),
            &self.paths.repo_root,
        );

        // Human-readable twin, committed alongside: reviewable in any git UI
        // and diffable across re-plans (plan.json stays the durable source).
        // The calibrated cost estimate is baked in here so the Reviewable
        // human queue gate (and any future surface reading plan.md) sees it
        // without recomputing it — `calibrate` never fails.
        let two_path = cost::estimate_two_path(estimate, &self.state.config, &calibration.params);
        let plan_md_body = render_plan_markdown(
            &plan,
            &self.state.mission,
            &estimate,
            two_path.as_ref(),
            fit_note.as_deref(),
            calibration.missions_used,
            &contract_lint_report,
            &gate_reports,
            &self.state.config.worker_candidates,
        );
        // research.md (repo-knowledge-store slice 1): the evidence the
        // orchestrator emitted with the plan, committed beside plan.md.
        let research_md = self
            .pending_research
            .as_ref()
            .map(|r| render_research_markdown(r, &self.state.mission.id));

        if worktree_mode {
            let (wt_path, wt_repo) = self.setup_mission_worktree()?;
            let commit_result = (|| -> Result<()> {
                let wt_paths = MissionPaths::new(wt_path.clone(), self.state.mission.id.clone());
                let plan_file = wt_paths.plan_file();
                if let Some(parent) = plan_file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&plan_file, serde_json::to_string_pretty(&plan)?)?;
                let plan_md = wt_paths.plan_md_file();
                std::fs::write(&plan_md, &plan_md_body)?;
                // Browsable catalog: date + goal-as-title + link per mission.
                // The canonical plan path stays stable; discovery lives here.
                let index = wt_paths.missions_dir().join("index.md");
                let index_body = upsert_mission_index(
                    &std::fs::read_to_string(&index).unwrap_or_default(),
                    &self.state.mission.id,
                    &plan.goal,
                    chrono::Utc::now().date_naive(),
                );
                std::fs::write(&index, index_body)?;
                let research_file = wt_paths.research_file();
                let mut to_commit: Vec<&Path> =
                    vec![plan_file.as_path(), plan_md.as_path(), index.as_path()];
                if let Some(body) = &research_md {
                    std::fs::write(&research_file, body)?;
                    to_commit.push(research_file.as_path());
                }
                wt_repo.commit_paths(
                    &to_commit,
                    &format!("[kranz] approved plan for {}", self.state.mission.id),
                )?;
                Ok(())
            })();
            self.teardown_mission_worktree();
            commit_result?;

            // Deliverable visibility (plan §f-2-3): the primary never checks
            // out the mission branch in worktree mode, so untracked twins in
            // the runtime dir are how operators (and reseed/digest/host
            // delete) read the approved plan without leaving the primary
            // checkout. Never committed here — the canonical copies live on
            // the mission branch above.
            let primary_plan = self.paths.plan_file();
            if let Some(parent) = primary_plan.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&primary_plan, serde_json::to_string_pretty(&plan)?)?;
            std::fs::write(self.paths.plan_md_file(), &plan_md_body)?;
            // Do NOT write missions/index.md on the primary: that catalog is
            // tracked on main in repos with merged missions, and a primary
            // rewrite would trip the worktree-mode cleanliness sweep (a
            // finding the worktree fix worker can never clear). Canonical
            // index lives on the mission branch above; REST/CLI read it from
            // there or from later merge.
            if let Some(body) = &research_md {
                std::fs::write(self.paths.research_file(), body)?;
            }
        } else {
            let plan_file = self.paths.plan_file();
            if let Some(parent) = plan_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&plan_file, serde_json::to_string_pretty(&plan)?)?;
            let plan_md = self.paths.plan_md_file();
            std::fs::write(&plan_md, &plan_md_body)?;
            let index = self.paths.missions_dir().join("index.md");
            let index_body = upsert_mission_index(
                &std::fs::read_to_string(&index).unwrap_or_default(),
                &self.state.mission.id,
                &plan.goal,
                chrono::Utc::now().date_naive(),
            );
            std::fs::write(&index, index_body)?;
            let research_file = self.paths.research_file();
            let mut to_commit: Vec<&Path> =
                vec![plan_file.as_path(), plan_md.as_path(), index.as_path()];
            if let Some(body) = &research_md {
                std::fs::write(&research_file, body)?;
                to_commit.push(research_file.as_path());
            }
            self.repo.commit_paths(
                &to_commit,
                &format!("[kranz] approved plan for {}", self.state.mission.id),
            )?;
        }
        self.pending_research = None;

        // Persist the approval-time estimate so the completion report reuses
        // this exact number (M1): recomputing it later would compare actual
        // cost against a value recalibrated on a since-changed corpus/config.
        self.persist_approved_estimate(&estimate)?;

        // The consent pin lands immediately before plan.approved (D-B/D-E):
        // contract validated → provider pinned → plan approved.
        self.emit(EventKind::WorkspaceProviderPinned {
            provider: workspace_pin.provider,
            template: workspace_pin.template,
            version: workspace_pin.version,
        })?;

        self.emit(EventKind::PlanApproved {
            plan,
            base_sha: Some(base_sha),
        })?;

        // First-class gate results (ticket gate-results-first-class-events,
        // KRZ-312): one gate.result event per evaluated approval gate, in
        // pipeline order. Emitted AFTER plan.approved, preserving the "Git
        // first" invariant above (no event lands until approval can no
        // longer fail) — a retried approve_plan therefore never double-
        // records a ladder. Record-only: the advisory posture is unchanged,
        // these events gate nothing.
        for kind in
            gate_results::gate_result_events(crate::gate::GateSurface::Approval, &gate_reports)
        {
            self.emit(kind)?;
        }

        // Fold the contract lint into an operator-facing decision (M8 tier 1,
        // feature f-1-2): suspects (already pass on the untouched base) get a
        // headline distinct from the benign base-expected-to-fail case, but
        // either way this only informs — approval above already succeeded.
        // The named gate verdicts (contract-validation-gates) ride the same
        // decision: failed defect classes are named in the headline, and the
        // full per-gate verdict block appends to the lint summary in the
        // detail. The existing headline text is preserved verbatim so
        // contract_health's lint counters keep classifying it.
        if !contract_lint_report.is_empty() {
            let suspect_count = contract_lint_report.suspects().len();
            let mut headline = if suspect_count > 0 {
                format!(
                    "contract lint: {suspect_count} author-bug suspect assertion(s) already \
                     pass on the untouched base — see plan.md"
                )
            } else {
                "contract lint: all command assertions correctly fail on the untouched base"
                    .to_string()
            };
            let failed_gates = contract_gates::failed_gate_names(&gate_reports);
            if !failed_gates.is_empty() {
                headline.push_str(&format!(
                    "; named contract gate(s) failed: {}",
                    failed_gates.join(", ")
                ));
            }
            let detail = format!(
                "{}\n\n{}",
                contract_lint_report.summary(),
                contract_gates::render_gate_verdicts(&gate_reports)
            );
            self.emit_decision(&headline, Some(detail))?;
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Mid-mission re-planning (roadmap M2)
    // -----------------------------------------------------------------------
    //
    // CONTRACT NOTE — what re-planning CAN and cannot express today.
    //
    // Re-planning a mission that is already Running/Blocked must NOT lose
    // completed work. The obvious approach — re-emit `plan.approved` with the
    // full revised plan — is unusable here: the reducer rebuilds `milestones`
    // from `plan.approved` wholesale (ms-<n>/f-<n>-<m> ids reassigned, every
    // status reset to Pending), which would clobber completed milestones and
    // features. And there is no first-class "add a milestone" or "revise the
    // plan" event in the contract (events.rs) — the ONLY event that adds work
    // is `fixfeature.created`, and it only appends a feature to an EXISTING
    // milestone.
    //
    // So re-planning is deliberately scoped to what the existing event
    // vocabulary can express honestly, on the FIRST not-yet-complete milestone
    // (the one work is actively flowing through):
    //   (i)  DROP a still-pending planned feature the revision removed
    //        (`feature.skipped`), and
    //   (ii) ADD a feature the revision introduced (`fixfeature.created`,
    //        origin=fix — the same mechanism validation fixes use).
    // Completed milestones/features and already-started features are left
    // untouched; the revision is rejected if it tries to alter them. The full
    // revised plan is committed as `revised-plan.md` for human review (the
    // engine writes + commits it, like plan.md), and an `orchestrator.decision`
    // records the revision so it appears in the replayed history and digest.
    //
    // What this CANNOT express (see contractChangeRequest below): adding a
    // brand-new milestone, reordering remaining milestones, or revising a
    // not-yet-started LATER milestone's feature set. Those need a first-class
    // `milestone.added` / `plan.revised` event.
    //
    // contractChangeRequest: add a `plan.revised { plan }` (or a narrower
    // `milestone.added { milestone }`) event whose reducer semantics MERGE the
    // revised remainder onto the existing milestones — preserving completed
    // milestones and their ids by title/order and only materializing genuinely
    // new milestones/features. That would let re-planning cover new and later
    // milestones, which the fixfeature-only subset here cannot.

    async fn propose_revision(&mut self, instructions: &str) -> Result<()> {
        if self.state.pending_revision.is_some() {
            self.emit_decision(
                "revision request ignored: a revised plan is already awaiting approval",
                Some(instructions.to_string()),
            )?;
            return Ok(());
        }
        let request = self
            .request_revised_plan_with_instructions(instructions)
            .await?;
        match request {
            PlanRequest::Ready(mut plan) => {
                assign_assertion_ids(&mut plan.validation_contract);
                let calibration = cost::calibrate(&self.paths.repo_root);
                let estimate = cost::estimate(&plan, &self.state.config, &calibration.params);
                let estimate = cost::apply_shape(estimate, &plan, &calibration);
                validate_considered_alternatives(&plan, &estimate, &self.state.config)?;
                if self.pending_research.is_none()
                    && considered_alternatives_requirement(&plan, &estimate, &self.state.config)
                        .is_some()
                {
                    tracing::warn!(
                        mission = %self.state.mission.id,
                        "revising to an over-threshold plan with no research.md (research is \
                         soft-prompted, not gated)"
                    );
                }
                validate_revised_plan_for_gate(&self.state.mission, &plan)?;
                let revision = self.state.latest_plan_revision + 1;
                self.emit(EventKind::PlanRevisionProposed {
                    revision,
                    plan,
                    instructions: instructions.trim().to_string(),
                })?;
                self.emit_decision(
                    &format!("revision {revision} proposed; awaiting approval"),
                    Some(format!(
                        "The run loop is parked until revision {revision} is approved or rejected."
                    )),
                )?;
            }
            PlanRequest::NotReady(reply) => {
                self.emit_decision(
                    "revision request needs more context",
                    Some(if reply.trim().is_empty() {
                        "orchestrator returned an empty not-ready reply".to_string()
                    } else {
                        reply
                    }),
                )?;
            }
            // The wrong-plan escalation is a DRAFT-stage channel: the revised
            // plan prompt never offers it and its parser never produces it.
            // Degrade to the not-ready path rather than panic if that ever
            // changes — the reason text is exactly what the operator needs.
            PlanRequest::WrongPlan { reason } => {
                self.emit_decision(
                    "revision request escalated: plan likely wrong",
                    Some(reason),
                )?;
            }
        }
        Ok(())
    }

    fn approve_pending_revision(&mut self, revision: u32) -> Result<()> {
        let pending = self.state.pending_revision.clone().ok_or_else(|| {
            EngineError::InvalidState("no pending revised plan to approve".to_string())
        })?;
        if pending.revision != revision {
            return Err(EngineError::InvalidState(format!(
                "pending revision is {}, not {revision}",
                pending.revision
            )));
        }
        validate_revised_plan_for_gate(&self.state.mission, &pending.plan)?;
        // Belt-and-suspenders: the reducer — not merely the gate — must accept
        // this revision. Dry-run the exact fold before any durable side effect,
        // so an unappliable PlanRevised can never be appended to the log (emit
        // appends before it folds; a failed fold on replay bricks the mission).
        reducer::dry_run_revised_plan(&self.state, &pending.plan, revision)?;
        self.commit_revised_plan_record(&pending.plan, revision)?;
        if self.state.mission.status == MissionStatus::Blocked {
            if let Some(mi) = first_incomplete(&self.state) {
                let milestone_id = self.state.mission.milestones[mi].id.clone();
                self.emit(EventKind::MilestoneUnblocked {
                    milestone_id,
                    reason: format!("revision {revision} approved"),
                    validator_guidance: None,
                })?;
            }
        }
        self.emit(EventKind::PlanRevised {
            revision,
            plan: pending.plan,
        })?;
        self.emit_decision(
            &format!("revision {revision} approved"),
            Some("plan.json and plan.md were rewritten; completed work remains frozen".to_string()),
        )?;
        Ok(())
    }

    fn reject_pending_revision(&mut self, revision: u32) -> Result<()> {
        let pending = self.state.pending_revision.as_ref().ok_or_else(|| {
            EngineError::InvalidState("no pending revised plan to reject".to_string())
        })?;
        if pending.revision != revision {
            return Err(EngineError::InvalidState(format!(
                "pending revision is {}, not {revision}",
                pending.revision
            )));
        }
        self.emit(EventKind::PlanRevisionRejected {
            revision,
            reason: "rejected by operator".to_string(),
        })?;
        self.emit_decision(
            &format!("revision {revision} rejected"),
            Some("mission will continue with the existing plan of record".to_string()),
        )?;
        Ok(())
    }

    /// Approve the parked grant for `command`: append `grant.approved` (the
    /// reducer extends `command_grants` extend-only and clears the pending
    /// request), so the milestone's validators re-run with the widened
    /// allow-set. The `command` echoed back by the operator must match the
    /// parked request — a stale approval for a different command is refused,
    /// not silently applied to whatever is parked now.
    fn approve_pending_grant(&mut self, command: &str) -> Result<()> {
        let pending = self.state.pending_grant_request.clone().ok_or_else(|| {
            EngineError::InvalidState("no pending grant request to approve".to_string())
        })?;
        if pending.command != command {
            return Err(EngineError::InvalidState(format!(
                "pending grant is {:?}, not {command:?}",
                pending.command
            )));
        }
        self.emit(EventKind::GrantApproved {
            kind: pending.kind,
            command: pending.command.clone(),
        })?;
        let (list_label, detail) = match pending.kind {
            GrantKind::Command => (
                "command grants",
                "the milestone's validators will re-run with the widened allow-set",
            ),
            GrantKind::TouchPath => (
                "touch set",
                "the milestone re-validates with the path inside the contract",
            ),
            GrantKind::WorkerDeny => (
                "worker deny exceptions",
                "the worker respawns with the deny rule lifted",
            ),
            GrantKind::Egress => (
                "egress grants",
                "the re-run's egress proxy allows the granted destination",
            ),
        };
        self.emit_decision(
            &format!(
                "grant approved: `{}` added to {list_label}",
                pending.command
            ),
            Some(detail.to_string()),
        )?;
        self.grant_requested_at = None;
        Ok(())
    }

    /// Deny the parked grant for `command`: append `grant.denied` (clears the
    /// pending request), then apply the kind's refusal semantics.
    ///
    /// - `Command` / `Egress`: block the milestone. The block is what stops the
    ///   run loop re-entering validation forever; without it, clearing the
    ///   pending request alone would let the next round re-request the same
    ///   grant.
    /// - `TouchPath` / `WorkerDeny`: do NOT block — the out-of-contract write
    ///   is a normal finding (and the still-denied worker command a normal
    ///   judgement), so let it flow on exactly as it did before those grants
    ///   existed. Instead, saturate the per-milestone grant counter so the next
    ///   round doesn't re-offer the same grant.
    ///
    /// The `Command` `MilestoneBlocked` emit is guarded on the milestone still
    /// existing: a concurrent plan revision can drop the parked (in-flight)
    /// milestone, and `MilestoneBlocked` for an unknown milestone fails its OWN
    /// reducer fold — which, because `emit` appends before it folds, would brick
    /// the mission on every future load (the deny-default timeout makes that
    /// automatic). If the milestone is gone, the revision already moved past it,
    /// so clearing the grant (`grant.denied`, which folds unconditionally) is
    /// enough — the run loop re-evaluates the revised plan.
    fn deny_pending_grant(&mut self, command: &str, reason: &str) -> Result<()> {
        let pending = self.state.pending_grant_request.clone().ok_or_else(|| {
            EngineError::InvalidState("no pending grant request to deny".to_string())
        })?;
        if pending.command != command {
            return Err(EngineError::InvalidState(format!(
                "pending grant is {:?}, not {command:?}",
                pending.command
            )));
        }
        self.emit(EventKind::GrantDenied {
            kind: pending.kind,
            command: pending.command.clone(),
            reason: reason.to_string(),
        })?;
        match pending.kind {
            GrantKind::Command | GrantKind::Egress => {
                let milestone_exists = self
                    .state
                    .mission
                    .milestones
                    .iter()
                    .any(|m| m.id == pending.milestone_id);
                if milestone_exists {
                    let boundary = match pending.kind {
                        GrantKind::Egress => "egress",
                        _ => "validator command",
                    };
                    self.emit(EventKind::MilestoneBlocked {
                        milestone_id: pending.milestone_id.clone(),
                        reason: format!("{boundary} denied: `{}` — {reason}", pending.command),
                    })?;
                }
            }
            GrantKind::TouchPath | GrantKind::WorkerDeny => {
                // No block: saturate the cap so the re-run stops re-offering and
                // the run flows on its normal path — TouchPath's out-of-contract
                // finding to convert_findings (fix/waive), WorkerDeny's still-
                // denied worker command to the normal judgement/respawn.
                //
                // Two bounded, fail-safe limitations of using the ephemeral
                // counter (vs a durable MilestoneBlocked) here:
                //  - Restart re-arm: the counter is process-local, so a crash
                //    during the re-run window loses the "already denied" memory
                //    and the deterministic trigger re-offers the grant once more.
                //    Safe (re-prompt, not a brick/loop) and bounded by the cap;
                //    a durable "denied" marker isn't worth the event-schema
                //    weight for a re-prompt.
                //  - Cap coupling: the counter is shared across grant kinds for
                //    this milestone, so a later denial of another kind in the
                //    SAME run won't be offered a grant (falls through). Fails
                //    closed; rare (multiple boundaries in one milestone-run).
                self.grant_requests
                    .insert(pending.milestone_id.clone(), self.grant_request_cap);
            }
        }
        self.emit_decision(
            &format!("grant denied: `{}`", pending.command),
            Some(reason.to_string()),
        )?;
        self.grant_requested_at = None;
        Ok(())
    }

    /// Answer an open structured question (ticket
    /// `structured-human-question-events`): append `question.answered`, which
    /// the reducer cross-checks against the parked projection, removes from
    /// it, and routes onto `pending_user_messages` — the answer reaches the
    /// running mission through the EXISTING user-message consult (and the
    /// blocked-milestone consult when blocked), never a new delivery
    /// mechanism.
    ///
    /// The cross-checks mirror the grant approve/deny discipline, so a
    /// stale, replayed, or mistyped answer can never land on a different
    /// question than the operator saw:
    /// - the id must name an OPEN question (a duplicate control file — the
    ///   crash-between-emit-and-acknowledge window — errors here, is noted,
    ///   and is acknowledged away, exactly like a duplicate grant decision);
    /// - an option INDEX answer must be in range and its text must match the
    ///   parked option verbatim (the surface resolved the index against the
    ///   same projection);
    /// - the answer text must be non-empty.
    ///
    /// The answer is scrubbed + capped at this write boundary: operator-typed
    /// text can still carry a pasted token, and the log is corpus-exported.
    fn answer_pending_question(
        &mut self,
        question_id: &str,
        answer: &str,
        option: Option<u32>,
    ) -> Result<()> {
        let pending = self
            .state
            .pending_questions
            .iter()
            .find(|q| q.question_id == question_id)
            .cloned()
            .ok_or_else(|| {
                EngineError::InvalidState(format!("no open question '{question_id}' to answer"))
            })?;
        if answer.trim().is_empty() {
            return Err(EngineError::InvalidState(
                "question answer must not be empty".to_string(),
            ));
        }
        if let Some(index) = option {
            let expected = pending.options.get(index as usize).ok_or_else(|| {
                EngineError::InvalidState(format!(
                    "question '{question_id}' has no option {index} (it offered {})",
                    pending.options.len()
                ))
            })?;
            if expected != answer {
                return Err(EngineError::InvalidState(format!(
                    "answer {answer:?} does not match option {index} ({expected:?}) of question '{question_id}'"
                )));
            }
        }
        self.emit(EventKind::QuestionAnswered {
            question_id: question_id.to_string(),
            answer: scrub::scrub_and_truncate(answer, ANSWER_TEXT_MAX),
            via: "answer-question".to_string(),
            option,
        })?;
        // NO success decision here (contrast the grant approve/deny paths):
        // an `orchestrator.decision` fold CONSUMES `pending_user_messages`,
        // which is exactly where the reducer just routed this answer — a
        // decision emitted now would eat the answer (and any other queued
        // operator message) before the consult can read it. The
        // `question.answered` event itself is the audit record; the tail and
        // both surfaces render it.
        Ok(())
    }

    /// Clear every open question matching `scope` (emit `question.cleared`)
    /// because it stopped being actionable — its milestone completed, or the
    /// mission ended with the ask still open. Keeps the pending-decision
    /// projection honest: a question whose decision is moot never lingers as
    /// a "your move" the operator can no longer act on. (An abandoned mission
    /// is the deliberate exception — the abandon path emits no events of its
    /// own, mirroring how a parked grant request also outlives it in state;
    /// every surface gates on an active mission.)
    fn clear_open_questions(
        &mut self,
        why: &str,
        scope: impl Fn(&PendingQuestion) -> bool,
    ) -> Result<()> {
        let ids: Vec<String> = self
            .state
            .pending_questions
            .iter()
            .filter(|q| scope(q))
            .map(|q| q.question_id.clone())
            .collect();
        for question_id in ids {
            self.emit(EventKind::QuestionCleared {
                question_id,
                why: why.to_string(),
            })?;
        }
        Ok(())
    }

    /// Park a `kind` grant for `target` (emit `GrantRequested`), returning
    /// `true`. Bounded by `grant_request_cap` per milestone: over the cap it
    /// emits an informational decision and returns `false` so the caller falls
    /// through to its normal path (this monotonic, never-reset counter is what
    /// bounds the park→approve→re-validate loop). `blocked_desc` is the
    /// human-readable "what was blocked" clause for the decision line.
    fn park_for_grant(
        &mut self,
        milestone_id: &str,
        kind: GrantKind,
        target: &str,
        blocked_desc: &str,
    ) -> Result<bool> {
        let prior = *self.grant_requests.get(milestone_id).unwrap_or(&0);
        if prior >= self.grant_request_cap {
            self.emit_decision(
                &format!(
                    "still blocked on `{target}` after {} grant request(s); not offering another",
                    self.grant_request_cap
                ),
                None,
            )?;
            return Ok(false);
        }
        self.grant_requests
            .insert(milestone_id.to_string(), prior + 1);
        self.emit(EventKind::GrantRequested {
            milestone_id: milestone_id.to_string(),
            kind,
            command: target.to_string(),
        })?;
        self.emit_decision(
            &format!("{blocked_desc}; parked for an operator grant decision"),
            None,
        )?;
        Ok(true)
    }

    /// If `outcome` was stopped by a grantable command denial, offer the
    /// operator the narrowest command grant and park, returning `true`. Only the
    /// first denied command is offered; a re-run surfaces the next. Non-command
    /// denials (Write/Edit/web — READ_ONLY_DENY, deny-wins) never populate
    /// `denied_commands`, so they don't reach here. Callers gate this on an
    /// UNTRUSTED outcome.
    fn maybe_park_for_grant(
        &mut self,
        milestone_id: &str,
        role: Role,
        outcome: &runner::RunOutcome,
    ) -> Result<bool> {
        let Some(command) = outcome.denied_commands.first().cloned() else {
            return Ok(false);
        };
        let desc = format!("{} validation blocked on `{command}`", role_label(role));
        self.park_for_grant(milestone_id, GrantKind::Command, &command, &desc)
    }

    /// If `outcome` was stopped by an egress-proxy denial, offer the operator
    /// an egress grant naming the refused destination and park, returning
    /// `true`. Mirrors [`Self::maybe_park_for_grant`]: only the FIRST denied
    /// destination is offered (a re-run surfaces the next), and callers gate
    /// this on an UNTRUSTED outcome. Approving extends `egress_grants`, which
    /// `runner::apply_egress_grants` folds into the re-run's proxy allowlist;
    /// denying blocks the milestone, same as a denied command grant. The
    /// target is scrubbed like a denied command before it is parked (the host
    /// string is model-influenced via what the run chose to connect to).
    fn maybe_park_for_egress_grant(
        &mut self,
        milestone_id: &str,
        role: Role,
        outcome: &runner::RunOutcome,
    ) -> Result<bool> {
        let Some(denial) = outcome.denied_egress.first() else {
            return Ok(false);
        };
        let target = scrub::scrub_and_truncate(
            &format!("{}:{}", denial.host, denial.port),
            MESSAGE_CONTENT_MAX,
        );
        let desc = format!(
            "{} validation blocked on egress to `{target}`",
            role_label(role)
        );
        self.park_for_grant(milestone_id, GrantKind::Egress, &target, &desc)
    }

    /// If the milestone's findings include a genuine out-of-contract write,
    /// offer the operator a touch-set grant for that path and park, returning
    /// `true`. Approving extends `touch_set` so the write is in-contract on
    /// re-validate; denying (or a timeout) lets the write flow to the normal
    /// fix/waive path. Bounded by the same per-milestone cap.
    ///
    /// Only the TRUSTED deterministic engine sweep (`ENGINE_RUN_ID`) can offer a
    /// touch grant — never a spawned validator that merely emitted a finding
    /// with the same class string. And only a genuinely GRANTABLE path is
    /// offered ([`contract_sweep::grantable_touch_path`]): the `FINDING_CLASS`
    /// string is shared by the primary-checkout sentinel and glob-compile-error
    /// findings, neither of which extending `touch_set` can resolve.
    fn maybe_park_for_touch_grant(
        &mut self,
        milestone_id: &str,
        findings: &[(String, Finding)],
    ) -> Result<bool> {
        let touch_set = &self.state.mission.touch_set;
        let Some(path) = findings
            .iter()
            .filter(|(run_id, _)| run_id.as_str() == crate::reducer::ENGINE_RUN_ID)
            .find_map(|(_, f)| contract_sweep::grantable_touch_path(f, touch_set))
            .map(str::to_string)
        else {
            return Ok(false);
        };
        let desc = format!("worker wrote `{path}` outside the touch-set");
        self.park_for_grant(milestone_id, GrantKind::TouchPath, &path, &desc)
    }

    /// If the worker's `outcome` was blocked by a deny rule, offer the operator
    /// a grant to LIFT that rule and park, returning `true`. The park discards
    /// this run's outcome, so either decision re-runs the worker when the run
    /// loop re-enters this still-Active feature. Approving adds the rule to
    /// `deny_exceptions` (subtracting it from the worker deny set) so the
    /// re-run has it lifted; deny/timeout leaves it in force and saturates the
    /// request cap, so the re-run's denial is not re-offered and flows to the
    /// normal judgement/respawn. Bounded by the same per-milestone cap.
    ///
    /// The grant TARGET is the deny RULE (e.g. `Bash(git push*)`), not the
    /// command — that is what `deny_exceptions` removes and what the operator is
    /// consenting to lift (coarser than one command, but deny-rule removal is
    /// inherently rule-granular). Only a command blocked by a liftable
    /// `Bash(...)` deny rule is offered; a hook denial or a non-Bash tool denial
    /// matches no rule and is not grantable this way.
    fn maybe_park_for_worker_deny_grant(
        &mut self,
        milestone_id: &str,
        outcome: &runner::RunOutcome,
    ) -> Result<bool> {
        let Some(command) = outcome.denied_commands.first().cloned() else {
            return Ok(false);
        };
        // The worker's CURRENT deny set (already-lifted rules removed) still
        // contains the rule that blocked this command.
        let profile = permissions::for_role(
            Role::Worker,
            &self.state.config,
            &[],
            &self.state.mission.command_grants,
            &self.state.mission.deny_exceptions,
        );
        let Some(rule) = permissions::matching_deny_rule(&command, &profile.disallowed_tools)
        else {
            return Ok(false);
        };
        let desc = format!("worker command `{command}` blocked by deny rule `{rule}`");
        self.park_for_grant(milestone_id, GrantKind::WorkerDeny, &rule, &desc)
    }

    /// Persist the approval-time cost estimate to the primary mission dir as
    /// gitignored runtime bookkeeping (see [`MissionPaths::estimate_file`]). The
    /// completion report reads it back so "estimated vs actual" reflects the
    /// number the operator actually approved, not one recomputed later.
    fn persist_approved_estimate(&self, estimate: &cost::CostEstimate) -> Result<()> {
        let path = self.paths.estimate_file();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(estimate)?)?;
        Ok(())
    }

    fn commit_revised_plan_record(&mut self, plan: &Plan, revision: u32) -> Result<()> {
        let calibration = cost::calibrate(&self.paths.repo_root);
        let estimate = cost::estimate(plan, &self.state.config, &calibration.params);
        let estimate = cost::apply_shape(estimate, plan, &calibration);
        self.persist_approved_estimate(&estimate)?;
        // Re-planning does not re-lint the contract against the base (the
        // base tree may no longer be pristine mid-mission); the section is
        // simply omitted here since `is_empty()` is true.
        let no_lint = contract_lint::ContractLintReport {
            results: Vec::new(),
            tree_clean_at_base: true,
        };
        let two_path = cost::estimate_two_path(estimate, &self.state.config, &calibration.params);
        let fit_anchor = crate::plan_fit::corpus_fit_anchor(&self.paths.repo_root);
        let fit_warnings = crate::plan_fit::feature_fit_warnings(plan, &fit_anchor);
        let fit_note = (!fit_warnings.is_empty())
            .then(|| crate::plan_fit::render_fit_note(&fit_warnings, &fit_anchor));
        let plan_md_body = render_plan_markdown(
            plan,
            &self.state.mission,
            &estimate,
            two_path.as_ref(),
            fit_note.as_deref(),
            calibration.missions_used,
            &no_lint,
            &[],
            &self.state.config.worker_candidates,
        );
        let revised_md_body = render_revised_plan_markdown(plan, &self.state.mission, &[], &[]);
        let research_md = self
            .pending_research
            .as_ref()
            .map(|r| render_research_markdown(r, &self.state.mission.id));
        let active_paths = self.active_paths();
        let plan_file = active_paths.plan_file();
        let plan_md = active_paths.plan_md_file();
        let revised_md = active_paths.mission_dir().join("revised-plan.md");
        if let Some(parent) = plan_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&plan_file, serde_json::to_string_pretty(plan)?)?;
        std::fs::write(&plan_md, &plan_md_body)?;
        std::fs::write(&revised_md, &revised_md_body)?;
        let index = active_paths.missions_dir().join("index.md");
        let index_body = upsert_mission_index(
            &std::fs::read_to_string(&index).unwrap_or_default(),
            &self.state.mission.id,
            &plan.goal,
            chrono::Utc::now().date_naive(),
        );
        std::fs::write(&index, index_body)?;
        let research_file = active_paths.research_file();
        let mut to_commit: Vec<&Path> = vec![
            plan_file.as_path(),
            plan_md.as_path(),
            revised_md.as_path(),
            index.as_path(),
        ];
        if let Some(body) = &research_md {
            std::fs::write(&research_file, body)?;
            to_commit.push(research_file.as_path());
        }
        self.active_repo().commit_paths(
            &to_commit,
            &format!(
                "[kranz] revised plan for {} (rev {revision})",
                self.state.mission.id
            ),
        )?;

        if self.active_tree.is_some() {
            let primary_plan_file = self.paths.plan_file();
            if let Some(parent) = primary_plan_file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&primary_plan_file, serde_json::to_string_pretty(plan)?)?;
            std::fs::write(self.paths.plan_md_file(), &plan_md_body)?;
            std::fs::write(
                self.paths.mission_dir().join("revised-plan.md"),
                revised_md_body,
            )?;
            if let Some(body) = &research_md {
                std::fs::write(self.paths.research_file(), body)?;
            }
        }
        self.pending_research = None;
        Ok(())
    }

    /// Apply a revised plan to a running or blocked mission (roadmap M2),
    /// preserving all completed work. See the contract note above for the full
    /// rationale and the honest scope of what this expresses.
    ///
    /// Validation (rejects with [`EngineError::InvalidState`]):
    /// - the mission must be Running or Blocked (re-planning a Planning mission
    ///   is [`Self::approve_plan`]; a terminal mission cannot be revised);
    /// - every already-Complete milestone must appear in the revised plan,
    ///   FIRST and in the same order, with its title and full feature set
    ///   (titles, specs, criteria) UNCHANGED — a dropped or altered completed
    ///   milestone is rejected.
    ///
    /// Application (existing events only): on the FIRST not-yet-complete
    /// milestone, pending planned features the revision drops are
    /// `feature.skipped`, and features the revision adds are appended via
    /// `fixfeature.created`. The full revised plan is written + committed as
    /// `revised-plan.md`, and an `orchestrator.decision` summarizes the change.
    pub fn approve_revised_plan(&mut self, plan: Plan) -> Result<()> {
        // State gate: re-planning is for live missions only.
        match self.state.mission.status {
            MissionStatus::Running | MissionStatus::Blocked => {}
            other => {
                return Err(EngineError::InvalidState(format!(
                    "approve_revised_plan requires a Running or Blocked mission, mission is {other:?}"
                )));
            }
        }
        if plan.milestones.is_empty() {
            return Err(EngineError::InvalidState(
                "revised plan has no milestones".to_string(),
            ));
        }

        // (1) The completed milestones, in current order, must be reproduced
        // unchanged and first in the revised plan.
        let completed: Vec<&Milestone> = self
            .state
            .mission
            .milestones
            .iter()
            .filter(|m| m.status == MilestoneStatus::Complete)
            .collect();
        for (i, done) in completed.iter().enumerate() {
            let revised = plan.milestones.get(i).ok_or_else(|| {
                EngineError::InvalidState(format!(
                    "revised plan drops completed milestone '{}' (must appear first, unchanged)",
                    done.title
                ))
            })?;
            if revised.title.trim() != done.title.trim() {
                return Err(EngineError::InvalidState(format!(
                    "revised plan milestone {} is '{}' but completed milestone '{}' must appear \
                     there unchanged",
                    i + 1,
                    revised.title,
                    done.title
                )));
            }
            if !completed_features_unchanged(done, revised) {
                return Err(EngineError::InvalidState(format!(
                    "revised plan alters the features of completed milestone '{}'",
                    done.title
                )));
            }
        }

        // (2) Locate the first not-yet-complete milestone (the active target)
        // and the revised milestone that positionally maps to it (the one right
        // after the completed prefix).
        let Some(target_mi) = self
            .state
            .mission
            .milestones
            .iter()
            .position(|m| m.status != MilestoneStatus::Complete)
        else {
            return Err(EngineError::InvalidState(
                "no incomplete milestone to revise (all milestones are complete)".to_string(),
            ));
        };
        // The revised milestone aligned with the target is at the target's
        // index (completed milestones occupy indices 0..completed.len(), and
        // the target is the first index past them = completed.len()).
        let revised_target = plan.milestones.get(target_mi).ok_or_else(|| {
            EngineError::InvalidState(
                "revised plan is missing the milestone that maps to the active one".to_string(),
            )
        })?;

        // (3) Diff the target milestone's features by title:
        //   - a still-Pending planned feature absent from the revision → skip;
        //   - a revised feature title absent from the milestone → add (fix).
        // Titles are compared trimmed/case-insensitively so trivial editorial
        // differences do not spuriously drop or duplicate a feature.
        let target = &self.state.mission.milestones[target_mi];
        let revised_titles: Vec<String> = revised_target
            .features
            .iter()
            .map(|f| norm_title(&f.title))
            .collect();
        let current_titles: Vec<String> = target
            .features
            .iter()
            .map(|f| norm_title(&f.title))
            .collect();

        let to_skip: Vec<String> = target
            .features
            .iter()
            .filter(|f| {
                f.status == FeatureStatus::Pending
                    && f.origin == FeatureOrigin::Plan
                    && !revised_titles.contains(&norm_title(&f.title))
            })
            .map(|f| f.id.clone())
            .collect();
        let to_add: Vec<PlanFeature> = revised_target
            .features
            .iter()
            .filter(|f| !current_titles.contains(&norm_title(&f.title)))
            .cloned()
            .collect();

        // (4) Write + commit the human-reviewable revised plan (the engine
        // writes and commits — the orchestrator never touches files, like
        // approve_plan). Git first: a failure here leaves no event emitted, so
        // approve_revised_plan can simply be retried.
        //
        // Worktree mode (M7 tier 1): this is called between `run()` calls, so
        // `self.active_tree` is None here — mirror `approve_plan`'s own
        // setup/teardown of a scratch integration worktree rather than
        // committing straight to the primary tree.
        let worktree_mode = self.state.config.isolation() == WorkerIsolation::Worktree;
        let revised_md_body =
            render_revised_plan_markdown(&plan, &self.state.mission, &to_skip, &to_add);
        if worktree_mode {
            let (wt_path, wt_repo) = self.setup_mission_worktree()?;
            let commit_result = (|| -> Result<()> {
                let wt_paths = MissionPaths::new(wt_path.clone(), self.state.mission.id.clone());
                let revised_md = wt_paths.mission_dir().join("revised-plan.md");
                if let Some(parent) = revised_md.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&revised_md, &revised_md_body)?;
                wt_repo.commit_paths(
                    &[revised_md.as_path()],
                    &format!("[kranz] revised plan for {}", self.state.mission.id),
                )?;
                Ok(())
            })();
            self.teardown_mission_worktree();
            commit_result?;

            // Untracked human-readable twin in the primary runtime dir, same
            // rationale as `approve_plan`'s `primary_plan_md` twin.
            let primary_revised_md = self.paths.mission_dir().join("revised-plan.md");
            if let Some(parent) = primary_revised_md.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&primary_revised_md, &revised_md_body)?;
        } else {
            let revised_md = self.paths.mission_dir().join("revised-plan.md");
            if let Some(parent) = revised_md.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&revised_md, &revised_md_body)?;
            self.repo.commit_paths(
                &[revised_md.as_path()],
                &format!("[kranz] revised plan for {}", self.state.mission.id),
            )?;
        }

        // (5) Record the revision, then apply the expressible subset.
        let target_id = target.id.clone();
        // Next re-plan cycle = 1 + the highest existing `<id>-replan-<c>-*`
        // cycle on this milestone, so repeated re-plans never mint colliding
        // ids (two re-plans without an intervening validation round would share
        // fix_cycles). The reducer also rejects duplicates as a backstop.
        let replan_prefix = format!("{target_id}-replan-");
        let replan_cycle = target
            .features
            .iter()
            .filter_map(|f| f.id.strip_prefix(&replan_prefix))
            .filter_map(|rest| rest.split('-').next())
            .filter_map(|c| c.parse::<u32>().ok())
            .max()
            .map_or(1, |m| m + 1);
        self.emit_decision(
            &format!(
                "re-plan for {target_id}: {} feature(s) dropped, {} added",
                to_skip.len(),
                to_add.len()
            ),
            Some(format!(
                "Revised plan committed to revised-plan.md. Dropped {} pending feature(s); \
                 added {} feature(s) to {target_id}. Completed milestones preserved unchanged.",
                to_skip.len(),
                to_add.len()
            )),
        )?;

        for feature_id in to_skip {
            self.emit(EventKind::FeatureSkipped {
                feature_id,
                reason: "dropped by mid-mission re-plan".to_string(),
            })?;
        }
        // Added features enter as fix-origin features on the target milestone —
        // the only event that can add a feature. Ids reuse the fix-feature
        // shape but on a "re-plan" cycle namespace so they never collide with
        // validation fix ids (which are ms-<id>-fix-<cycle>-<n>).
        for (i, pf) in to_add.into_iter().enumerate() {
            let feature = Feature {
                id: format!("{target_id}-replan-{replan_cycle}-{}", i + 1),
                title: scrub::scrub(&pf.title),
                spec: scrub::scrub(&pf.spec),
                validation_criteria: pf
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
                milestone_id: target_id.clone(),
                feature,
            })?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // run() — THE LOOP (plan §4.5)
    // -----------------------------------------------------------------------

    /// Drive the mission until it is Complete or Failed (returned), Blocked
    /// (returned so the user can intervene), or the process is killed (safe:
    /// the log is the source of truth). Paused missions loop in place,
    /// draining the control inbox, until a Resume arrives.
    /// NOTE on checkout lifetime: in CHECKOUT mode, run() leaves the checkout
    /// on the MISSION branch at terminal states deliberately — report.md/
    /// plan.md are committed there, and yanking the checkout back to base
    /// would make the mission's own artifacts vanish from the working tree at
    /// the exact moment the operator reads them. The dispatcher (`kranz work`)
    /// and `kranz draft` restore the operator's checkout at THEIR boundaries.
    ///
    /// In WORKTREE mode (M7 tier 1) the primary checkout never moves at all —
    /// plan.md/report.md are committed on the mission branch via the
    /// integration worktree (`approve_plan`/`write_mission_report`), and a
    /// human-readable, untracked twin of each is written straight to the
    /// primary runtime dir (`.kranz/missions/<id>/`) so an operator reading
    /// the primary checkout still sees them, without the primary ever leaving
    /// its starting branch.
    pub async fn run(&mut self) -> Result<MissionStatus> {
        if self.state.mission.status == MissionStatus::Planning {
            return Err(EngineError::InvalidState(
                "cannot run a mission whose plan is not approved".to_string(),
            ));
        }
        // A terminal mission (Complete/Failed/Abandoned) must never spawn
        // workers again — abandon exists precisely to STOP spend. Without this
        // gate, `kranz run` (or auto-selection, since the abandon event is the
        // newest log write) would resurrect a killed mission and pay for it.
        if is_terminal_status(self.state.mission.status) {
            return Err(EngineError::InvalidState(format!(
                "mission is already terminal ({:?}); nothing to run",
                self.state.mission.status
            )));
        }

        // WorkspaceProvider seam (design D-B, ticket workspace-provider-seam):
        // resolve the configured workspace.provider BEFORE any side effects —
        // an unknown provider name fails closed here, at run start, rather
        // than silently falling back to local. Arc-shared onto the engine so
        // validation_round can drive the golden-data reset-between-rounds
        // hook (design D-D) through the same seam. The additive
        // workspace.teardownMode (ticket workspace-idle-hibernate) validates
        // here too — an unknown mode fails closed before any spend, the same
        // backstop as provider resolution.
        let provider: Arc<dyn crate::workspace_provider::WorkspaceProvider> =
            crate::workspace_provider::resolve(&self.state.config.workspace)?.into();
        let teardown_mode = crate::workspace_provider::teardown_mode(&self.state.config.workspace)?;
        self.workspace_provider = Some(Arc::clone(&provider));

        // Pack contract (ticket pack-contract-gates-prompts): validate the
        // configured pack BEFORE any side effects — an invalid pack fails
        // closed here, at run start, the same backstop as provider
        // resolution above, rather than silently degrading to pack-less
        // behavior at the surfaces that consume it (the final gate, the
        // role-prompt builders). No packDir ⇒ None ⇒ byte-identical run.
        // The summary stays short (decision summaries are length-capped);
        // the full registration list rides in the detail.
        if let Some(pack) = crate::pack::load_for_config(&self.state.config, &self.paths.repo_root)
            .map_err(EngineError::Config)?
        {
            self.emit_decision(
                &format!(
                    "pack contract: pack `{}` (schema {}) registered: {} gate(s), \
                     {} prompt(s), {} checklist(s), {} artefact store(s)",
                    pack.name,
                    pack.schema,
                    pack.gates.len(),
                    pack.prompts.len(),
                    pack.checklists.len(),
                    pack.artefact_stores.len(),
                ),
                Some(pack.describe()),
            )?;
        }

        // Branch isolation: workers commit on the mission branch, never on
        // whatever branch the operator (or a previous mission/draft) left
        // checked out. Approval created and checked out the branch, but
        // nothing re-asserted it at run time — the first live `kranz work`
        // train committed three missions straight to main.
        //
        // Worktree mode (M7 tier 1): the PRIMARY checkout must never change
        // branches, so mission-branch work instead runs in a dedicated
        // integration worktree (`setup_mission_worktree`); `self.active_tree`
        // routes every mission-branch git op there for the rest of this run.
        let worktree_mode = self.state.config.isolation() == WorkerIsolation::Worktree;
        if worktree_mode {
            // Recorded BEFORE `setup_mission_worktree` (which never touches
            // the primary anyway) so the sweep's primary-checkout cleanliness
            // check has a baseline branch to compare against for this run.
            self.primary_branch_at_start = Some(self.repo.current_branch()?);
            let (path, wt_repo) = self.setup_mission_worktree()?;
            self.active_tree = Some((path, wt_repo));
        } else {
            let mission_branch = self.state.mission.mission_branch.clone();
            if self.repo.current_branch()? != mission_branch {
                if !self.repo.branch_exists(&mission_branch)? {
                    // A deleted branch is recreated at the pinned approval base.
                    let from = self
                        .state
                        .mission
                        .base_sha
                        .clone()
                        .unwrap_or_else(|| self.state.mission.base_branch.clone());
                    self.repo.create_branch(&mission_branch, Some(&from))?;
                }
                self.repo.checkout(&mission_branch)?;
                self.emit_decision(
                    &format!(
                        "run: re-asserted mission branch {mission_branch} (checkout had drifted)"
                    ),
                    None,
                )?;
            }
        }

        let result = self.run_loop(&*provider).await;

        // Provider teardown seam (design D-E, ticket
        // workspace-idle-hibernate): a TERMINAL run (Complete/Failed/
        // Abandoned) drives the configured workspace.teardownMode; a
        // non-terminal end (Blocked/Paused) Keeps so the mission can
        // resume; local-worktree is always Keep (effective_teardown_mode).
        // The event records the actual mode + outcome. Skipped when the
        // run errored: crash semantics, with the resume sweep owning
        // leftovers.
        if result.is_ok() {
            let run_terminal = matches!(&result, Ok(status) if is_terminal_status(*status));
            let mode = crate::workspace_provider::effective_teardown_mode(
                provider.kind(),
                run_terminal,
                teardown_mode,
            );
            self.teardown_workspace(&*provider, mode).await;
        }

        // Integration worktree lifetime: torn down once the mission reaches
        // a status the resume/reconcile path already accounts for (terminal,
        // or an error that ends this process) — never on Blocked/Paused,
        // where the mission may resume and wants its worktree intact
        // (a leaked one is reaped by `resume()`'s crash sweep regardless).
        if worktree_mode {
            let should_teardown = match &result {
                Ok(status) => is_terminal_status(*status),
                Err(_) => true,
            };
            if should_teardown {
                self.teardown_mission_worktree();
                self.active_tree = None;
            }
        }

        result
    }

    /// The §4.5 preflight + loop body of [`Self::run`], factored out so the
    /// caller can wrap it with integration-worktree setup/teardown (M7 tier 1)
    /// without duplicating every early-return site inside the loop.
    async fn run_loop(
        &mut self,
        provider: &dyn crate::workspace_provider::WorkspaceProvider,
    ) -> Result<MissionStatus> {
        // Environment preflight (roadmap M2): surface obvious missing
        // prerequisites of the contract commands as ONE advisory decision
        // before the first worker spawns. Never blocks — the contract gate at
        // completion stays authoritative. Emit one outcome on every run so a
        // later clean preflight durably supersedes an earlier warning.
        let issues = self.preflight();
        let summary = if issues.is_empty() {
            PREFLIGHT_CLEAR_SUMMARY.to_string()
        } else {
            format!(
                "preflight: {} issue(s): {}",
                issues.len(),
                issues
                    .iter()
                    .map(|i| format!("[{}] {}", i.severity, i.message))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        };
        self.emit_decision(&summary, None)?;

        // WorkspaceProvider seam drive (design D-B/D-C; ticket
        // workspace-provider-seam): provider.provision → provider.readiness
        // (= the workspace bootstrap + readiness gate) → workers. With a
        // workspace contract, bootstrap then readiness run in the execution
        // cwd BEFORE any worker/validator spawns — a failure BLOCKS the
        // mission (owner: repo-setup) instead of starting spend on a
        // half-ready app. Once per run() invocation; resume re-runs it
        // (idempotent-by-contract, see workspace_provider docs). No contract
        // ⇒ byte-identical behavior plus the additive workspace.provisioned
        // lifecycle event.
        if let Some(status) = self.provision_workspace(provider).await? {
            return Ok(status);
        }

        loop {
            // (a) drain the control inbox.
            self.drain_control().await?;

            match self.state.mission.status {
                MissionStatus::Complete => return Ok(MissionStatus::Complete),
                MissionStatus::Failed => return Ok(MissionStatus::Failed),
                // (b) paused: idle-drain until resumed. The mission may sit
                // here indefinitely, so age-flush any buffered deltas each
                // tick rather than waiting for the next lifecycle event.
                MissionStatus::Paused => {
                    self.log.flush_if_due()?;
                    tokio::time::sleep(PAUSE_POLL).await;
                    continue;
                }
                _ => {}
            }

            if self.state.pending_revision.is_some() {
                self.log.flush_if_due()?;
                tokio::time::sleep(PAUSE_POLL).await;
                continue;
            }

            // (c') capability-grant gate: a validator hit a command outside its
            // allow-set and parked the milestone for an operator decision.
            // Mirror the revision gate — a passive park drained by
            // `drain_control` (ApproveGrant/DenyGrant) — with a deny-default
            // timeout so an unanswered request fails closed. Routing the gate
            // here (not inside validation_round) keeps the park shallow: control
            // draining, pause, and the revision gate all still apply, and no
            // stale milestone index is held across the wait.
            if let Some(pending) = self.state.pending_grant_request.clone() {
                // Arm the clock on first observation — also covers a restart
                // that reloaded a durable pending request with no timestamp.
                let requested_at = *self
                    .grant_requested_at
                    .get_or_insert_with(std::time::Instant::now);
                if requested_at.elapsed() >= self.grant_request_timeout {
                    self.deny_pending_grant(
                        &pending.command,
                        "grant request timed out with no operator decision (deny-default)",
                    )?;
                    continue;
                }
                self.log.flush_if_due()?;
                tokio::time::sleep(PAUSE_POLL).await;
                continue;
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
                let start_sha = self.active_repo().head_sha()?;
                let milestone_id = self.state.mission.milestones[mi].id.clone();
                self.emit(EventKind::MilestoneStarted {
                    milestone_id,
                    start_sha,
                })?;
            }

            // Parallel-within-milestone (roadmap M3), STRICTLY gated: only when
            // the operator opted in (max_parallel_workers > 1) AND there is a
            // batch of ≥2 not-yet-started independent features to fan out. When
            // this returns true it drove a parallel batch and the loop
            // re-evaluates; false means "no parallel batch here" and execution
            // falls through to the byte-for-byte-unchanged sequential path.
            //
            // With max_parallel_workers == 1 this guard short-circuits before
            // any parallel code runs, so the sequential behaviour below is
            // exactly what it was pre-M3.
            if self.state.config.max_parallel_workers > 1 && self.try_parallel_batch(mi).await? {
                continue;
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
    async fn drain_control(&mut self) -> Result<()> {
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
                        self.emit_decision(&format!("config change ignored: {e}"), None)?;
                    } else {
                        self.emit(EventKind::ConfigChanged { patch })?;
                    }
                }
                ControlCommand::Msg { text, interrupt } => {
                    self.emit(EventKind::UserMessage { text, interrupt })?;
                }
                ControlCommand::RequestRevision { instructions } => {
                    if let Err(e) = self.propose_revision(&instructions).await {
                        tracing::warn!(error = %e, "revision request ignored");
                        self.emit(EventKind::OrchestratorDecision {
                            summary: format!("revision request ignored: {e}"),
                            detail: None,
                        })?;
                    }
                }
                ControlCommand::ApproveRevision { revision } => {
                    if let Err(e) = self.approve_pending_revision(revision) {
                        tracing::warn!(error = %e, revision, "revision approval ignored");
                        self.emit(EventKind::OrchestratorDecision {
                            summary: format!("revision {revision} approval ignored: {e}"),
                            detail: None,
                        })?;
                    }
                }
                ControlCommand::RejectRevision { revision } => {
                    if let Err(e) = self.reject_pending_revision(revision) {
                        tracing::warn!(error = %e, revision, "revision rejection ignored");
                        self.emit(EventKind::OrchestratorDecision {
                            summary: format!("revision {revision} rejection ignored: {e}"),
                            detail: None,
                        })?;
                    }
                }
                ControlCommand::ApproveGrant { command } => {
                    if let Err(e) = self.approve_pending_grant(&command) {
                        tracing::warn!(error = %e, command, "grant approval ignored");
                        self.emit(EventKind::OrchestratorDecision {
                            summary: format!("grant approval for `{command}` ignored: {e}"),
                            detail: None,
                        })?;
                    }
                }
                ControlCommand::DenyGrant { command, reason } => {
                    if let Err(e) = self.deny_pending_grant(&command, &reason) {
                        tracing::warn!(error = %e, command, "grant denial ignored");
                        self.emit(EventKind::OrchestratorDecision {
                            summary: format!("grant denial for `{command}` ignored: {e}"),
                            detail: None,
                        })?;
                    }
                }
                ControlCommand::AnswerQuestion {
                    question_id,
                    answer,
                    option,
                } => {
                    if let Err(e) = self.answer_pending_question(&question_id, &answer, option) {
                        tracing::warn!(error = %e, question_id, "question answer ignored");
                        self.emit(EventKind::OrchestratorDecision {
                            summary: format!("answer for question {question_id} ignored: {e}"),
                            detail: None,
                        })?;
                    }
                }
            }
            control::acknowledge(&self.paths, &path)?;
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
             {{\"action\":\"unblock-raise-cap\"|\"unblock-skip-findings\"|\"unblock-add-fix\"|\"skip-milestone\"|\"stay-blocked\",\"note\":\"string\",\"candidate\":null,\"validatorGuidance\":\"string (optional)\",\"fix\":{{\"title\":\"string\",\"spec\":\"string\",\"validationCriteria\":[\"string\"]}} (optional)}}\n\
             Use \"unblock-add-fix\" when validation fails for a mechanical reason a repair \
             worker should fix BEFORE re-validating (run cargo fmt, fix a doc/test lint) — \
             resuming validation unchanged would just fail again; include the fix object \
             describing the repair. When unblocking you may set validatorGuidance to \
             verbatim instructions for the next validator session (e.g. \"run cargo fmt \
             before the gate\", \"the a3 grep pattern is the problem\") — it is folded into \
             mission state and injected into the next validator task and its retry, even \
             across a process restart. When the milestone is parked on a dispatch-pool \
             judgement (the block reason names kranz/pool/* candidate branches) and the \
             user names a winning candidate, set \"candidate\" to its zero-based stream \
             index (the -c<i> branch suffix); leave it null when no candidate was chosen. \
             This only RECORDS the judgement — the engine never merges a candidate."
        );
        let (decision, text) = self.json_decision::<UnblockDecision>(&message).await?;
        // Conservative default (documented): stay blocked.
        let (action, note, candidate, validator_guidance, fix) = match decision {
            Some(d) => (
                d.action.trim().to_ascii_lowercase(),
                d.note,
                d.candidate,
                d.validator_guidance,
                d.fix,
            ),
            None => (
                "stay-blocked".to_string(),
                "unparseable unblock decision".to_string(),
                None,
                None,
                None,
            ),
        };
        self.emit_decision(
            &format!("unblock decision for {milestone_id}: {action}"),
            Some(text.clone()),
        )?;

        // Dispatch-pool resolution RECORD (KRZ-304): the judgement the pool
        // parked for lands here, BEFORE the unblock it rides on, so the log
        // reads record-then-move. Record-only — the match below is untouched.
        self.record_pool_resolutions(mi, &action, &note, candidate)?;

        match action.as_str() {
            "unblock-raise-cap" | "unblock-skip-findings" => {
                self.emit(EventKind::MilestoneUnblocked {
                    milestone_id,
                    reason: if note.is_empty() { action } else { note },
                    validator_guidance,
                })?;
                Ok(None)
            }
            "unblock-add-fix" => {
                // Operator-directed repair (a fmt pass, a doc/test lint): a
                // fresh repair feature runs BEFORE the next validation round
                // — resuming validation unchanged would just fail again. The
                // reducer's fix-cycle guard only increments from Validating
                // status, so this repair does not spend a fix cycle; it is
                // not validator-finding loop churn.
                let reason = if note.is_empty() {
                    action.clone()
                } else {
                    note.clone()
                };
                let fix = fix.unwrap_or_else(|| FixFeatureSpec {
                    title: format!("repair blocked {milestone_id}"),
                    spec: format!(
                        "Repair what blocks validation of {milestone_id} (operator-directed): {reason}"
                    ),
                    validation_criteria: Vec::new(),
                });
                self.emit(EventKind::MilestoneUnblocked {
                    milestone_id: milestone_id.clone(),
                    reason,
                    validator_guidance,
                })?;
                self.emit_fix_features(mi, vec![fix], "blocked-state repair", text)?;
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
                    validator_guidance: None,
                })?;
                let to_skip: Vec<String> = self.state.mission.milestones[mi]
                    .features
                    .iter()
                    .filter(|f| matches!(f.status, FeatureStatus::Pending | FeatureStatus::Active))
                    .map(|f| f.id.clone())
                    .collect();
                for feature_id in to_skip {
                    self.emit(EventKind::FeatureSkipped {
                        feature_id,
                        reason: "milestone skipped".to_string(),
                    })?;
                }
                // Structured human questions (ticket
                // structured-human-question-events): asks scoped to this
                // milestone are moot once it is skipped — clear them so the
                // pending-decision projection never shows an unanswerable
                // "your move".
                self.clear_open_questions("milestone skipped", |q| {
                    q.milestone_id.as_deref() == Some(milestone_id.as_str())
                })?;
                self.emit(EventKind::MilestoneCompleted {
                    milestone_id,
                    tag: None,
                })?;
                Ok(None)
            }
            _ => Ok(Some(MissionStatus::Blocked)),
        }
    }

    /// Dispatch-pool resolution RECORD (ticket `divergence-first-class-event`,
    /// KRZ-304): the pool parks its unit's milestone for a human judgement
    /// act (KRZ-303); this is where that judgement lands in the log. An
    /// operator steer that NAMES a candidate (`candidate` on the unblock
    /// decision) or that DISPOSES of the unit (skip-milestone) resolves it:
    /// append one `divergence.resolved` per unresolved pool unit of this
    /// milestone — which candidate (or none), why, decided by whom.
    ///
    /// RECORD ONLY: the resolution changes nothing about the mission's
    /// course. The engine never merges a candidate (the KRZ-303 freeze),
    /// an unblock-* action on a pool park simply re-parks via the
    /// re-dispatch guard, and agreement between models is a signal to log,
    /// never a criterion to trust — a unit is done when gates are green and
    /// no escalation is open, not when its streams stopped disagreeing.
    ///
    /// FIRST JUDGEMENT WINS: at most one resolution per unit — the
    /// reducer-folded `resolved_divergence_units` set is the durable memory
    /// (restart-safe), so a re-blocked-then-re-steered unit never accrues a
    /// second record; a changed mind after the record is a conversation
    /// (user.message), not a resolution amendment. A bare unblock that
    /// names no candidate and does not dispose of the unit records NOTHING
    /// — the judgement has not arrived, and the park continues honestly.
    fn record_pool_resolutions(
        &mut self,
        mi: usize,
        action: &str,
        note: &str,
        candidate: Option<u32>,
    ) -> Result<()> {
        let disposes = action == "skip-milestone";
        if candidate.is_none() && !disposes {
            return Ok(());
        }
        let units: Vec<String> = self.state.mission.milestones[mi]
            .features
            .iter()
            .map(|f| f.id.clone())
            .filter(|id| !self.state.resolved_divergence_units.contains(id))
            .filter(|id| {
                self.state
                    .runs
                    .values()
                    .any(|r| r.candidate.as_ref().is_some_and(|c| &c.unit == id))
            })
            .collect();
        for unit in units {
            let recorded: Vec<u32> = self
                .state
                .runs
                .values()
                .filter_map(|r| {
                    r.candidate
                        .as_ref()
                        .filter(|c| c.unit == unit)
                        .map(|c| c.index)
                })
                .collect();
            let base_reason = if note.is_empty() { action } else { note };
            // `selected` must name a stream that was actually recorded: an
            // out-of-range index folds to None with the discrepancy named in
            // the reason — the record never points at a candidate that does
            // not exist (a resolution of "none" is honest; a phantom is not).
            let (selected, reason) = match candidate {
                Some(i) if recorded.contains(&i) => (Some(i), base_reason.to_string()),
                Some(i) => (
                    None,
                    format!("{base_reason} (named candidate c{i} has no recorded stream)"),
                ),
                None => (None, base_reason.to_string()),
            };
            self.emit(EventKind::DivergenceResolved {
                unit,
                selected,
                reason,
                decided_by: "operator".to_string(),
            })?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Feature execution (f)
    // -----------------------------------------------------------------------

    /// Worker self-escalation (ticket `backend-routing-abstraction`, KRZ-331):
    /// when the finished worker's report carries an `escalation` reason,
    /// record the request as a `worker.escalated` event naming the SOURCE
    /// route (the executor capability class this session ran on, derived
    /// from the routed config exactly like every other tier read) and the
    /// TARGET route (the frontier advisor — `frontier`, the orchestrator
    /// role's frontier-floor-enforced model/endpoint).
    ///
    /// Deliberately RECORD-ONLY: the event changes no state (the reducer
    /// fold validates the run reference and nothing else), so the escalation
    /// can never bypass the floor's validator requirements, flip the
    /// executor tier, or spend the respawn budget. The advisor ACT already
    /// exists — the judgement turn that every caller invokes immediately
    /// after this helper reads the same report, escalation request included
    /// — so the request is layered on top of the deterministic floor, never
    /// a replacement for it, and no new session kind is invented here.
    fn emit_worker_escalation(
        &mut self,
        feature_id: &str,
        outcome: &runner::RunOutcome,
    ) -> Result<()> {
        let Some(report) = &outcome.report else {
            return Ok(());
        };
        let Some(reason) = &report.escalation else {
            return Ok(());
        };
        self.emit(EventKind::WorkerEscalated {
            run_id: outcome.run_id.clone(),
            feature_id: feature_id.to_string(),
            from: self.state.executor_tier(),
            to: ExecutorTier::Frontier,
            reason: reason.clone(),
        })?;
        Ok(())
    }

    /// Structured human questions (ticket `structured-human-question-events`):
    /// when the finished worker's report carries `questions` (the "ask the
    /// human" tool payload — text plus structured choices), open each as a
    /// `question.opened` event feeding the ONE pending-decision projection
    /// the dashboard and Slack render beside grants (the D-X channel
    /// unification: permission prompts stay on the grant flow, ticket
    /// underspecification stays on NeedsContext, blocked prose stays valid —
    /// this is never a parallel inbox for any of them).
    ///
    /// Deliberately NOT a park: opening a question gates nothing (contrast
    /// `park_for_grant`). The worker's own `result` drives the mission's
    /// course exactly as before — a worker that needs a human choice reports
    /// partial/fail and the normal judgement/blocked flow carries on, with
    /// the question riding alongside as structured context the operator can
    /// answer through the existing control path (the answer then reaches the
    /// mission via the user-message consult fold). A prose-only report (no
    /// `questions` key) emits nothing, so backends without a structured ask
    /// keep working byte-for-byte.
    ///
    /// Write discipline: the report text was credential-scrubbed at capture
    /// (runner.rs `final_text`); each field is scrubbed + truncated AGAIN at
    /// this write boundary (defense-in-depth, and the truncation cap only
    /// applies here), question/options counts are capped, and the question
    /// id is engine-minted from the folded `question_count` — never
    /// model-supplied, so one report's id can never shadow another's.
    fn emit_worker_questions(
        &mut self,
        milestone_id: &str,
        feature_id: &str,
        outcome: &runner::RunOutcome,
    ) -> Result<()> {
        let Some(report) = &outcome.report else {
            return Ok(());
        };
        let Some(questions) = &report.questions else {
            return Ok(());
        };
        for question in questions.iter().take(QUESTIONS_PER_REPORT_CAP) {
            if question.text.trim().is_empty() {
                continue;
            }
            // Minted from the CURRENT folded count; each emit below folds
            // immediately and bumps it, so the next iteration's id is fresh.
            let question_id = format!("q-{}", self.state.question_count + 1);
            self.emit(EventKind::QuestionOpened {
                question_id,
                role: Role::Worker,
                text: scrub::scrub_and_truncate(&question.text, QUESTION_TEXT_MAX),
                options: question
                    .options
                    .iter()
                    .take(QUESTION_OPTIONS_CAP)
                    .map(|o| scrub::scrub_and_truncate(o, QUESTION_OPTION_MAX))
                    .filter(|o| !o.trim().is_empty())
                    .collect(),
                run_id: Some(outcome.run_id.clone()),
                feature_id: Some(feature_id.to_string()),
                milestone_id: Some(milestone_id.to_string()),
            })?;
        }
        let dropped = questions.len().saturating_sub(QUESTIONS_PER_REPORT_CAP);
        if dropped > 0 {
            self.emit_decision(
                &format!(
                    "worker report carried {dropped} question(s) beyond the {QUESTIONS_PER_REPORT_CAP}-question cap; only the first {QUESTIONS_PER_REPORT_CAP} were opened",
                ),
                None,
            )?;
        }
        Ok(())
    }

    /// Run one feature to a terminal state: worker run(s) with interrupt
    /// wiring, the §4.4 dirty-tree discipline, an orchestrator judgement turn,
    /// and the bounded respawn loop.
    async fn run_feature(&mut self, mi: usize, fi: usize) -> Result<()> {
        // Heterogeneous dispatch pool (ticket heterogeneous-dispatch-pool,
        // KRZ-303): with >= 2 configured `workerCandidates` the unit fans out
        // to ALL of them concurrently and the mission parks for the human
        // judgement act — a strictly opt-in fork of this method. An empty
        // pool (or the validated-away 1-entry list) keeps the byte-for-byte
        // sequential path below.
        if self.state.config.worker_candidates.len() >= 2 {
            return self.run_feature_dispatch_pool(mi, fi).await;
        }
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
            let base_sha = self.state.mission.base_sha.clone();
            let grants = self.state.mission.command_grants.clone();
            let egress_grants = self.state.mission.egress_grants.clone();
            let deny_exceptions = self.state.mission.deny_exceptions.clone();
            let touch_set = self.state.mission.touch_set.clone();
            let pre_run_sha = self.active_repo().head_sha()?;

            // Interrupt wiring: a control watcher polls the inbox and fires
            // the notify on `Msg { interrupt: true }`; run_session aborts the
            // worker and the outcome comes back Partial.
            let cancel = Arc::new(Notify::new());
            let watcher = tokio::spawn(control::ControlWatcher::wait_for_interrupt(
                self.paths.clone(),
                INTERRUPT_POLL,
                Arc::clone(&cancel),
            ));
            let selected = self.select_backend(Role::Worker);
            if let Some(reason) = selected.fallback_reason.as_deref() {
                self.emit_decision(reason, None)?;
            }
            let selected_kind = selected.kind;
            let backend = Arc::clone(&selected.backend);
            let cfg = selected.cfg;
            // Once-per-mission cached decision (mission m-165b6f, f-2-1): the
            // preflight session is driven at most once per mission, not once
            // per worker spawn. It is Claude-specific; non-Claude workers do
            // not need a Claude auth probe before launch.
            let auth_verdict = if selected_kind == BackendKind::Claude {
                self.worker_auth_verdict().await
            } else {
                AuthVerdict::Inconclusive
            };
            // Worktree mode (M7 tier 1): the worker session's cwd is the
            // mission integration worktree, never the primary repo root.
            // Checkout mode keeps the exact `run_worker` call it always had.
            let outcome = if self.state.config.isolation() == WorkerIsolation::Worktree {
                let session_cwd = self.active_root().to_path_buf();
                runner::run_worker_in(
                    backend.as_ref(),
                    &mut self.log,
                    &self.paths,
                    &cfg,
                    &feature,
                    &goal,
                    &milestone_title,
                    guidance.as_deref(),
                    Some(cancel),
                    &session_cwd,
                    base_sha.as_deref(),
                    &grants,
                    &egress_grants,
                    &deny_exceptions,
                    auth_verdict,
                    &touch_set,
                )
                .await
            } else {
                runner::run_worker(
                    backend.as_ref(),
                    &mut self.log,
                    &self.paths,
                    &cfg,
                    &feature,
                    &goal,
                    &milestone_title,
                    guidance.as_deref(),
                    Some(cancel),
                    base_sha.as_deref(),
                    &grants,
                    &egress_grants,
                    &deny_exceptions,
                    auth_verdict,
                    &touch_set,
                )
                .await
            };
            watcher.abort();
            // Fold the runner's events into state even when the run errored
            // (worker.spawned may already be on disk).
            let caught = self.catch_up();
            let outcome = outcome?;
            caught?;

            // Interrupt (or any queued command) → events now, so the
            // judgement digest reflects them.
            self.drain_control().await?;

            // §4.4 dirty-tree discipline (applies to interrupted runs too).
            if !self.active_repo().is_clean()? && !self.resolve_dirty_tree(mi, &feature.id).await? {
                return Ok(()); // orchestrator chose fail-feature
            }
            let commits: Vec<String> = self
                .active_repo()
                .commits_between(&pre_run_sha, "HEAD")?
                .iter()
                .map(|c| format!("{} {}", c.sha, c.subject))
                .collect();
            let diff_stat = self
                .active_repo()
                .diff_stat(&pre_run_sha, "HEAD")
                .unwrap_or_default();

            // Worker-deny grant (grant-request-decision-flow): a worker command
            // blocked by a deny rule (deny-wins) can only be unblocked by
            // lifting the rule. Offer that grant and park BEFORE judging — after
            // the dirty-tree checkpoint above, so the worker's partial work is
            // preserved. Parking discards this run's outcome, so EITHER decision
            // re-runs the worker on re-entry: approve lifts the rule for the
            // re-run; deny/timeout keeps it in force and saturates the request
            // cap, so the re-run's denial is not re-offered and flows to the
            // normal judgement. Routed through the run-loop park gate (return
            // Ok) — never a deep park holding this `mi`/`fi`.
            //
            // Gated on a NON-successful outcome (mirrors the validator flow's
            // `!trusted` gate): a worker that hit a denial but still reported
            // `pass` worked around it, so eroding a guardrail on its behalf
            // would be a spurious prompt — and an approve would pointlessly
            // re-run an already-done feature.
            //
            // Budget coupling (bounded, fail-safe): the re-run is a fresh worker
            // spawn, so the reducer still charges `feature.respawns` — but the
            // judgement branch below subtracts `grant_respawns`, so grant-driven
            // re-runs do NOT deplete the `max_respawns` failure-retry budget
            // (they are bounded by `grant_request_cap` instead). The credit is
            // process-local: a restart drops it and re-couples the counters, so
            // pre-restart grant re-runs count against `max_respawns` again and
            // can fail the feature earlier than intended — fails closed, never
            // loops. Unique to WorkerDeny (Command/TouchPath re-run validation,
            // not a worker).
            if outcome.result != RunResult::Pass {
                let milestone_id = self.state.mission.milestones[mi].id.clone();
                if self.maybe_park_for_worker_deny_grant(&milestone_id, &outcome)? {
                    // This park re-runs the worker on re-entry (approve OR deny
                    // both re-run it); credit that respawn so it doesn't charge
                    // the failure-retry budget below.
                    *self.grant_respawns.entry(feature.id.clone()).or_insert(0) += 1;
                    return Ok(());
                }
            }

            // Worker self-escalation (KRZ-331): record the worker's request
            // for the frontier advisor BEFORE the judgement turn — the
            // advisor act — consumes it from the same report.
            self.emit_worker_escalation(&feature.id, &outcome)?;
            // Structured human questions (ticket
            // structured-human-question-events): open the report's "ask the
            // human" payload into the pending-decision projection — also
            // BEFORE the judgement turn, which reads the same report. Never
            // a park: the outcome drives the flow below unchanged.
            let milestone_id = self.state.mission.milestones[mi].id.clone();
            self.emit_worker_questions(&milestone_id, &feature.id, &outcome)?;

            match self
                .judge_worker_run(&feature.id, &outcome, &commits, &diff_stat)
                .await?
            {
                JudgementOutcome::Complete => {
                    self.emit(EventKind::FeatureCompleted {
                        feature_id: feature.id,
                        commits,
                    })?;
                    return Ok(());
                }
                JudgementOutcome::Failed(reason) => {
                    self.emit(EventKind::FeatureFailed {
                        feature_id: feature.id,
                        reason,
                    })?;
                    return Ok(());
                }
                JudgementOutcome::Respawn(new_guidance) => {
                    let respawns = self.state.mission.milestones[mi].features[fi].respawns;
                    // Don't let operator-approved deny-lift respawns eat the
                    // failure-retry budget: subtract them so `max_respawns`
                    // bounds only judgement-driven retries.
                    let grant_respawns = *self.grant_respawns.get(&feature.id).unwrap_or(&0);
                    if respawns.saturating_sub(grant_respawns) < self.state.config.max_respawns {
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

    // -----------------------------------------------------------------------
    // Heterogeneous dispatch pool (KRZ-303)
    // -----------------------------------------------------------------------

    /// Heterogeneous dispatch pool (ticket `heterogeneous-dispatch-pool`,
    /// KRZ-303; the positioning ADR's 2026-07-31 boundary gloss): run ONE
    /// unit of work (this feature) on all N configured `workerCandidates`
    /// backends concurrently — one git worktree per stream, reusing the M3
    /// wall-clock idiom — record every output as a SIBLING CANDIDATE tied to
    /// the unit, then park the milestone for the human judgement act.
    ///
    /// The ticket's three freeze properties, enforced HERE (not just
    /// documented):
    ///
    /// 1. CANDIDATES FOR JUDGEMENT, NEVER A WINNER. This path calls
    ///    `judge_worker_run` NOWHERE and emits no `feature.completed`: there
    ///    is no code path that selects, ranks, or merges a candidate. Every
    ///    stream gets its own run record ([`CandidateLink`]ed to the unit and
    ///    its sibling set) and its own branch
    ///    (`kranz/pool/<mission>/<feature>-c<i>`, KEPT — the branches ARE the
    ///    candidate deliverables the later judgement act inspects). The
    ///    unit's milestone is then BLOCKED: without a judgement surface (the
    ///    divergence follow-up ticket) the only honest terminal posture is to
    ///    park for a human.
    /// 2. DIVERGENCE FOR SCRUTINY, NEVER THROUGHPUT. The N streams all run
    ///    the SAME unit; nothing here fans out distinct work to go faster,
    ///    and a candidate whose backend is unavailable fails its own stream
    ///    loudly ([`Self::select_pool_candidate`] has no claude fallback)
    ///    rather than silently duplicating a sibling's backend.
    /// 3. COST MULTIPLIER IN CONSENT. `cost::estimate` multiplies worker runs
    ///    by N and plan.md's dispatch-pool section names N; each stream keeps
    ///    the per-run `maxBudgetUsd` cap, so worst-case spend is N × cap and
    ///    the approved estimate prices exactly that sum.
    ///
    /// Single-writer discipline mirrors the M3 batch: Phase A (serial,
    /// engine-owned writer) emits `feature.started` and forks the worktrees;
    /// Phase B (concurrent, no log access) runs the N sessions via a JoinSet,
    /// each BUFFERING its event kinds; Phase C (serial, candidate order)
    /// replays each stream's kinds through `emit`, stamping the
    /// [`CandidateLink`] onto its `worker.spawned`.
    ///
    /// FAILURE ISOLATION: one stream failing (backend-unavailable selection
    /// error, session spawn/run error, task panic) does NOT abort its
    /// siblings — every stream's terminal state is recorded in the pool
    /// decision's detail, and a run record exists for every stream that
    /// started. A stream that never started gets NO synthetic run record
    /// (fabricating one would be dishonest — no session, no transcript); its
    /// terminal state lives in the decision detail.
    ///
    /// NO respawn loop and no dirty-tree orchestrator turn: the sequential
    /// path's judgement-driven machinery is exactly the winner-selection the
    /// freeze forbids here. Per-worktree dirty trees are checkpoint-committed
    /// onto the candidate branch (M3 idiom) so every candidate's deliverable
    /// is its branch HEAD; a secret-scan refusal is recorded in the decision
    /// detail and that candidate's leftovers are discarded with its worktree.
    ///
    /// RE-DISPATCH GUARD: a feature that already has candidate-linked runs is
    /// NEVER fanned out again silently (each dispatch is N paid sessions) —
    /// the guard re-blocks the milestone with the same judgement-pending
    /// reason, which is also the crash-resume posture (a half-recorded
    /// candidate set parks instead of silently completing or re-running).
    async fn run_feature_dispatch_pool(&mut self, mi: usize, fi: usize) -> Result<()> {
        let feature = self.state.mission.milestones[mi].features[fi].clone();
        let milestone_id = self.state.mission.milestones[mi].id.clone();

        if feature.status == FeatureStatus::Pending {
            self.emit(EventKind::FeatureStarted {
                feature_id: feature.id.clone(),
            })?;
        }

        // The re-dispatch guard. Checked against the DURABLE record (any run
        // candidate-linked to this unit), so it holds across restarts.
        let already_dispatched = self
            .state
            .runs
            .values()
            .any(|r| r.candidate.as_ref().is_some_and(|c| c.unit == feature.id));
        if already_dispatched {
            // Re-block only when the milestone is not already parked —
            // re-emitting an identical milestone.blocked would just spam the
            // log on every resume poll.
            if self.state.mission.milestones[mi].status != MilestoneStatus::Blocked {
                let reason = self.pool_judgement_block_reason(&feature.id);
                self.emit(EventKind::MilestoneBlocked {
                    milestone_id,
                    reason,
                })?;
            }
            return Ok(());
        }

        let specs = self.state.config.worker_candidates.clone();
        let mission_id = self.state.mission.id.clone();
        let pre_run_sha = self.active_repo().head_sha()?;

        // Per-candidate worktree layout, built up front so the cleanup guard
        // sees every path even if a fork fails midway (M3 idiom).
        let workspaces: Vec<PoolWorkspace> = specs
            .into_iter()
            .enumerate()
            .map(|(index, spec)| PoolWorkspace {
                branch: format!("kranz/pool/{mission_id}/{}-c{index}", feature.id),
                path: pool_worktree_path(&self.paths.repo_root, &mission_id, &feature.id, index),
                spec,
            })
            .collect();

        // The fallible body is wrapped so the worktree-DIR cleanup runs on
        // every exit — mirroring run_parallel_batch's cleanup guard, with one
        // deliberate difference: candidate BRANCHES are never deleted by the
        // engine. They ARE the recorded deliverables a judging human inspects
        // (and a future judgement act consumes); resume()'s leak sweep
        // deletes only the dirs for the same reason.
        //
        // `preserve` comes back from the inner body with the indices of
        // candidates whose worktree could not even be INSPECTED (12th-pass
        // review, P2): an inspection error must never be read as a clean
        // tree and reaped with a possibly dirty deliverable inside, so the
        // guard skips those dirs. (Their branches were never deletable
        // anyway; resume()'s operator-initiated leak sweep still reaps by
        // path shape — the failure record names the path while it survives.)
        let mut preserve: Vec<usize> = Vec::new();
        let pool_result = self
            .run_dispatch_pool_inner(mi, &feature, &pre_run_sha, &workspaces, &mut preserve)
            .await;

        for (idx, ws) in workspaces.iter().enumerate() {
            if preserve.contains(&idx) {
                continue;
            }
            if let Err(e) = self.repo.remove_worktree(&ws.path) {
                tracing::warn!(path = %ws.path.display(), error = %e, "pool worktree cleanup failed");
            }
        }
        if let Err(e) = self.repo.prune_worktrees() {
            tracing::warn!(error = %e, "pool worktree prune failed");
        }

        pool_result
    }

    /// The `milestone.blocked` reason a dispatch-pool unit parks with
    /// (KRZ-303): names the unit, the recorded candidate count against N, and
    /// WHY the mission stops here — selection is a human judgement act (the
    /// divergence follow-up surfaces it); the engine never picks a winner.
    fn pool_judgement_block_reason(&self, feature_id: &str) -> String {
        let recorded = self
            .state
            .runs
            .values()
            .filter(|r| r.candidate.as_ref().is_some_and(|c| c.unit == feature_id))
            .count();
        let n = self.state.config.worker_candidates.len();
        format!(
            "dispatch pool: {recorded}/{n} candidate stream(s) recorded for unit {feature_id}; \
             every output is a candidate for judgement — the engine never selects or merges a \
             winner (KRZ-303), and the judgement surface lands with the divergence follow-up \
             ticket. Inspect the candidate branches (kranz/pool/*); to proceed without \
             judging, skip the milestone."
        )
    }

    /// Append the unit's divergence/agreement record (ticket
    /// `divergence-first-class-event`, KRZ-304): compare every RECORDED
    /// candidate stream's branch tree and emit one `divergence.noted`
    /// naming the unit and the candidates (run id + branch + backend +
    /// tree hash) with the verdict. Called from the pool dispatch after
    /// every stream's checkpoint commit, so each branch HEAD IS the
    /// candidate deliverable the hash pins.
    ///
    /// **Agreement between models is a signal to log, never a criterion to
    /// trust.** Identical trees emit the same kind with `diverged: false`
    /// and change NOTHING about the mission's course — the milestone parks
    /// for judgement either way, no gate is consulted or skipped on the
    /// verdict, and a unit is done when gates are green and no escalation
    /// is open, not when streams stop disagreeing.
    ///
    /// Only candidates with a run record AND a successful Phase C
    /// inspection (`inspected`) are compared: a stream that never started
    /// has no candidate diff, and a candidate whose worktree inspection
    /// FAILED (the failed-and-preserved posture) still has a run record but
    /// its branch carries rejected/untouched bytes — counting either would
    /// fabricate agreement (or divergence) out of a failure (13th-pass
    /// review, P2: eligibility was previously inferred from the run record
    /// alone). The pool decision's detail names failed streams verbatim
    /// instead. With fewer than two eligible candidates there is nothing to
    /// compare and NO event is appended (a one-stream "agreement" would be
    /// vacuous). The crash-resume re-dispatch guard never calls here: a
    /// half-recorded candidate set parks without a comparison rather than
    /// fabricating one from incomplete streams.
    fn emit_pool_divergence_record(
        &mut self,
        feature: &Feature,
        workspaces: &[PoolWorkspace],
        inspected: &[usize],
    ) -> Result<()> {
        let mut candidates: Vec<DivergenceCandidate> = Vec::new();
        for (index, ws) in workspaces.iter().enumerate() {
            if !inspected.contains(&index) {
                continue;
            }
            let run = self.state.runs.values().find(|r| {
                r.candidate
                    .as_ref()
                    .is_some_and(|c| c.unit == feature.id && c.index == index as u32)
            });
            let Some(run) = run else { continue };
            // Read-only probe on the shared refs (worktree isolation
            // untouched): the tree hash anchors the verdict to exact bytes.
            let tree = self.repo.rev_parse(&format!("{}^{{tree}}", ws.branch))?;
            candidates.push(DivergenceCandidate {
                run_id: run.id.clone(),
                branch: ws.branch.clone(),
                backend: ws.spec.backend.clone(),
                tree,
            });
        }
        if candidates.len() < 2 {
            return Ok(());
        }
        let diverged = candidates.iter().any(|c| c.tree != candidates[0].tree);
        self.emit(EventKind::DivergenceNoted {
            unit: feature.id.clone(),
            candidates,
            diverged,
        })?;
        Ok(())
    }

    /// Fallible body of [`Self::run_feature_dispatch_pool`] (the caller's
    /// worktree-dir cleanup guard runs regardless of how this returns).
    ///
    /// `preserve` collects the indices of candidates whose worktree inspection
    /// failed at the Phase C checkpoint (12th-pass review, P2): the caller's
    /// cleanup guard skips reaping those dirs so the unverified bytes survive
    /// for human inspection. Populated as the failures happen, so even a
    /// later `?` return cannot lose a preservation decision already made.
    async fn run_dispatch_pool_inner(
        &mut self,
        mi: usize,
        feature: &Feature,
        pre_run_sha: &str,
        workspaces: &[PoolWorkspace],
        preserve: &mut Vec<usize>,
    ) -> Result<()> {
        let milestone_id = self.state.mission.milestones[mi].id.clone();
        let n = workspaces.len();

        // --- Phase A (serial, single-writer): fork every candidate worktree
        // off the mission branch tip. `feature.started` was already emitted by
        // the caller before the re-dispatch guard.
        for ws in workspaces {
            self.repo.add_worktree(&ws.path, &ws.branch, pre_run_sha)?;
        }

        // Resolve every candidate's backend BEFORE spawning: a candidate
        // whose backend cannot be constructed becomes a recorded stream
        // failure — never a batch abort, and NEVER a silent claude fallback
        // (a same-backend duplicate would fake the diversity that is the
        // pool's entire point).
        let mut selected: Vec<Option<SelectedBackend>> = Vec::with_capacity(n);
        let mut stream_errors: Vec<Option<String>> = (0..n).map(|_| None).collect();
        for (idx, ws) in workspaces.iter().enumerate() {
            match self.select_pool_candidate(&ws.spec) {
                Ok(selection) => selected.push(Some(selection)),
                Err(e) => {
                    stream_errors[idx] = Some(e.to_string());
                    selected.push(None);
                }
            }
        }

        // The claude auth probe is once-per-mission and meaningful only for
        // the claude backend; compute it BEFORE any concurrent task spawns
        // when ANY selected candidate is claude-backed (mirrors the M3
        // batch's pre-spawn probe), then hand it to claude streams only.
        let any_claude = selected
            .iter()
            .flatten()
            .any(|s| s.kind == BackendKind::Claude);
        let auth_verdict = if any_claude {
            self.worker_auth_verdict().await
        } else {
            AuthVerdict::Inconclusive
        };

        // --- Phase B (CONCURRENT, no log access): run every stream at once.
        // Mirrors the M3 batch: each task buffers its kinds and returns them
        // with its RunOutcome; nothing touches the shared log. The tracker
        // records the wall-clock overlap for the pool decision (and tests).
        let goal = self.state.mission.goal.clone();
        let milestone_title = self.state.mission.milestones[mi].title.clone();
        let base_sha = self.state.mission.base_sha.clone();
        let grants = self.state.mission.command_grants.clone();
        let egress_grants = self.state.mission.egress_grants.clone();
        let deny_exceptions = self.state.mission.deny_exceptions.clone();
        let touch_set = self.state.mission.touch_set.clone();
        let tracker = ConcurrencyTracker::new();

        let mut set: tokio::task::JoinSet<(usize, BufferedRunResult)> = tokio::task::JoinSet::new();
        for (idx, ws) in workspaces.iter().enumerate() {
            let Some(selection) = selected[idx].take() else {
                continue; // selection error already recorded for this stream
            };
            let verdict = if selection.kind == BackendKind::Claude {
                auth_verdict
            } else {
                AuthVerdict::Inconclusive
            };
            let backend = selection.backend;
            let cfg = selection.cfg;
            let paths = self.paths.clone();
            let feature = feature.clone();
            let goal = goal.clone();
            let milestone_title = milestone_title.clone();
            let ws_path = ws.path.clone();
            let guard = tracker.clone();
            let base_sha = base_sha.clone();
            let grants = grants.clone();
            let egress_grants = egress_grants.clone();
            let deny_exceptions = deny_exceptions.clone();
            let touch_set = touch_set.clone();
            set.spawn(async move {
                let _live = guard.enter(); // count this session as live
                let result = runner::run_worker_in_buffered(
                    backend.as_ref(),
                    &paths,
                    &cfg,
                    &feature,
                    &goal,
                    &milestone_title,
                    None,
                    &ws_path,
                    base_sha.as_deref(),
                    &grants,
                    &egress_grants,
                    &deny_exceptions,
                    verdict,
                    &touch_set,
                )
                .await;
                (idx, result)
            });
        }

        // Collect per-stream results keyed by candidate index. UNLIKE the M3
        // batch there is no batch-level error: one stream's failure is
        // recorded against that stream and the survivors still replay — a
        // failed candidate must never abort its siblings (KRZ-303).
        let mut buffered: Vec<Option<(Vec<EventKind>, runner::RunOutcome)>> =
            (0..n).map(|_| None).collect();
        let mut panic_note: Option<String> = None;
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((idx, Ok(result))) => buffered[idx] = Some(result),
                Ok((idx, Err(e))) => stream_errors[idx] = Some(e.to_string()),
                Err(e) => {
                    panic_note = panic_note.or(Some(format!("pool worker task panicked: {e}")));
                }
            }
        }
        // A panicked task carries no index; any stream that produced neither
        // a result nor an error was spawned but never returned (selection
        // errors already populated `stream_errors`), so the panic becomes
        // its recorded terminal state.
        for (idx, slot) in stream_errors.iter_mut().enumerate() {
            if buffered[idx].is_none() && slot.is_none() {
                *slot = Some(
                    panic_note
                        .clone()
                        .unwrap_or_else(|| "stream ended without a result".to_string()),
                );
            }
        }
        let peak = tracker.peak();

        // --- Phase C (serial, single-writer, candidate order): replay each
        // stream's buffered kinds — stamping the sibling linkage onto its
        // worker.spawned — then checkpoint-commit its worktree so the
        // candidate branch HEAD is the deliverable.
        let mut lines: Vec<String> = Vec::with_capacity(n);
        // The indices whose Phase C inspection SUCCEEDED (13th-pass review,
        // P2): the divergence comparison below must compare only VERIFIED
        // candidate bytes — a failed-and-preserved candidate still has a run
        // record, but its branch carries rejected/untouched bytes, and
        // comparing those would fabricate an agreement (or divergence) out
        // of an inspection failure.
        let mut inspected: Vec<usize> = Vec::with_capacity(n);
        for (idx, ws) in workspaces.iter().enumerate() {
            match buffered[idx].take() {
                Some((events, outcome)) => {
                    let link = CandidateLink {
                        unit: feature.id.clone(),
                        index: idx as u32,
                        count: n as u32,
                        backend: ws.spec.backend.clone(),
                    };
                    for mut kind in events {
                        if let EventKind::WorkerSpawned { candidate, .. } = &mut kind {
                            *candidate = Some(link.clone());
                        }
                        self.emit(kind)?;
                    }
                    self.log.flush()?;

                    // Checkpoint any stream output on the candidate branch (in
                    // its worktree), exactly the M3 worktree idiom: a dirty
                    // deliverable is committed here rather than run through
                    // the sequential dirty-tree turn; a secret-scan refusal is
                    // recorded (never silently dropped) and the leftovers go
                    // away with the worktree dir.
                    //
                    // The worktree is HOSTILE (12th-pass review, P1): the
                    // stream that just ran in it could plant `core.fsmonitor`,
                    // `core.hooksPath`, or a hook in its git metadata, which
                    // the checkpoint's own status/commit would then EXECUTE
                    // with the engine's ambient privileges. The handle runs
                    // hooks/fsmonitor-disabled — the same countermeasure the
                    // validator-fingerprint and gated-merge paths use
                    // (`GitRepo::with_hooks_disabled`).
                    //
                    // And inspection is LOAD-BEARING (12th-pass, P2): an
                    // inspection ERROR must never be read as "clean" or "0
                    // commits" — that reaped the worktree with a possibly
                    // dirty deliverable inside. Any failure to open, inspect,
                    // or query the worktree fails the candidate honestly —
                    // recorded exactly where stream failures are recorded —
                    // and PRESERVES its worktree dir + branch (the cleanup
                    // guard skips the index), so the bytes survive for a
                    // human. Only a COMMIT-time failure stays a dispatch
                    // error (`?`): the tree was inspectable by then, so that
                    // is a real git failure, not hostile metadata.
                    let inspection: Result<(GitRepo, bool)> = (|| {
                        let wt_repo = GitRepo::open(&ws.path)?.with_hooks_disabled()?;
                        wt_repo.ensure_identity()?;
                        let clean = wt_repo.is_clean()?;
                        Ok((wt_repo, clean))
                    })();
                    let (wt_repo, clean) = match inspection {
                        Ok(pair) => pair,
                        Err(error) => {
                            preserve.push(idx);
                            lines.push(pool_inspection_failure_line(idx, n, ws, &error));
                            continue;
                        }
                    };
                    let mut note = String::new();
                    if !clean {
                        match wt_repo.commit_dirty_paths(
                            &contract_sweep::pool_checkpoint_commit_message(&feature.id, idx),
                        )? {
                            crate::git_ops::CheckpointOutcome::Committed(_) => {}
                            crate::git_ops::CheckpointOutcome::RefusedBySecretScan { detail } => {
                                note = format!(
                                    "; dirty-tree checkpoint refused by secret scan ({detail})"
                                );
                            }
                        }
                    }
                    let commits = match wt_repo.commits_between(pre_run_sha, "HEAD") {
                        Ok(commits) => commits.len(),
                        Err(error) => {
                            preserve.push(idx);
                            lines.push(pool_inspection_failure_line(idx, n, ws, &error));
                            continue;
                        }
                    };
                    lines.push(format!(
                        "- candidate {idx}/{}: `{}` / `{}` → branch `{}` — run {:?}, {} commit(s){}",
                        n - 1,
                        ws.spec.backend,
                        ws.spec.model,
                        ws.branch,
                        outcome.result,
                        commits,
                        note
                    ));
                    // Inspection succeeded end to end (open, identify,
                    // status, commit query): this candidate's bytes are
                    // verified and it MAY join the divergence comparison.
                    inspected.push(idx);
                }
                None => {
                    let err = stream_errors[idx]
                        .clone()
                        .unwrap_or_else(|| "stream produced no run record".to_string());
                    lines.push(format!(
                        "- candidate {idx}/{}: `{}` / `{}` — stream failed, no run record: {err}",
                        n - 1,
                        ws.spec.backend,
                        ws.spec.model
                    ));
                }
            }
        }

        // The divergence/agreement record (KRZ-304): emitted while every
        // inspected candidate branch HEAD is final (checkpoints committed
        // above) and BEFORE the park, so the judgement the milestone waits
        // on has a first-class handle. Only successfully inspected
        // candidates participate (13th-pass, P2). Record-only — the park
        // below is unchanged whether the streams diverged or agreed.
        self.emit_pool_divergence_record(feature, workspaces, &inspected)?;

        // One first-class decision record for the dispatch: the candidate
        // table AND the freeze statements, so the replayed history shows what
        // was produced and why nothing was picked. The N and peak numbers in
        // the summary let tests assert the fan-out and the overlap.
        self.emit_decision(
            &format!(
                "dispatch pool: unit {} fanned out to {n} candidates (peak {peak} concurrent) \
                 — candidates for judgement, no winner selected",
                feature.id
            ),
            Some(format!(
                "Heterogeneous dispatch (KRZ-303): unit `{}` ran on {n} backends concurrently, \
                 one worktree per stream. Every output below is a CANDIDATE FOR JUDGEMENT tied \
                 to the unit — the engine never selects, ranks, or merges a winner; selection \
                 is the human judgement act the divergence follow-up surfaces. The claimed \
                 value is divergence for scrutiny, not throughput. Cost: the approved estimate \
                 priced all {n} streams (the per-mission budget applies to the sum).\n\n{}",
                feature.id,
                lines.join("\n")
            )),
        )?;

        // Park the milestone for the human judgement act (freeze property 1:
        // no code path completes the unit from a candidate).
        let reason = self.pool_judgement_block_reason(&feature.id);
        self.emit(EventKind::MilestoneBlocked {
            milestone_id,
            reason,
        })?;
        Ok(())
    }

    /// Dirty tree after a worker run: ask the orchestrator (JSON), defaulting
    /// to commit-as-is (deterministic, documented). Returns `false` when the
    /// feature was failed instead — by the orchestrator's own decision, or
    /// because the checkpoint's secret scan refused the commit (which also
    /// blocks milestone `mi`: the refused content stays dirty in the shared
    /// sequential tree, so running further features would only cascade the
    /// same refusal onto them).
    async fn resolve_dirty_tree(&mut self, mi: usize, feature_id: &str) -> Result<bool> {
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
            None => (
                "commit-as-is".to_string(),
                "unparseable dirty-tree decision".to_string(),
            ),
        };
        self.emit_decision(
            &format!("dirty tree after {feature_id}: {action}"),
            Some(text),
        )?;
        if action == "fail-feature" {
            self.emit(EventKind::FeatureFailed {
                feature_id: feature_id.to_string(),
                reason: if note.is_empty() {
                    "dirty tree; orchestrator failed the feature".into()
                } else {
                    note
                },
            })?;
            return Ok(false);
        }
        let outcome = self
            .active_repo()
            .commit_dirty_paths(&contract_sweep::checkpoint_commit_message(feature_id))?;
        match outcome {
            crate::git_ops::CheckpointOutcome::Committed(_) => Ok(true),
            crate::git_ops::CheckpointOutcome::RefusedBySecretScan { detail } => {
                // A scan refusal is a policy decision, not a git failure:
                // propagating it would error the whole run, and the tree is
                // still dirty on resume, so the mission would wedge re-hitting
                // the same refusal. Record it and fail the FEATURE instead —
                // with an audit trail, and the leftover tree plus the
                // refusal's allowlist guidance as the operator's cleanup cue.
                // Real git failures still `?` out above.
                self.emit_decision(
                    &format!("dirty tree after {feature_id}: checkpoint refused by secret scan"),
                    Some(detail.clone()),
                )?;
                self.emit(EventKind::FeatureFailed {
                    feature_id: feature_id.to_string(),
                    reason: format!("dirty-tree checkpoint refused by secret scan: {detail}"),
                })?;
                // Then BLOCK the milestone: the refused content is still
                // sitting uncommitted in the SHARED sequential working tree
                // (nothing was staged or committed), so every later feature
                // in this milestone would trip its own dirty-tree turn,
                // re-hit the SAME refusal, and be failed with a reason naming
                // THIS feature's leak — a cascade of misattributed failures
                // against a poisoned tree. Blocking routes resume through the
                // normal blocked flow (`handle_blocked`: the run returns
                // Blocked, no tight loop) until the operator cleans or
                // allowlists the named paths. The parallel path needs no
                // such guard: its checkpoints run in per-feature worktrees
                // that are torn down with the batch.
                let dirty = self
                    .active_repo()
                    .dirty_paths()?
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                let milestone_id = self.state.mission.milestones[mi].id.clone();
                self.emit(EventKind::MilestoneBlocked {
                    milestone_id,
                    reason: format!(
                        "dirty-tree checkpoint for {feature_id} refused by secret scan; the \
                         working tree still holds the refused content — clean or allowlist \
                         these paths, then resume: {dirty}"
                    ),
                })?;
                Ok(false)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Parallel-within-milestone execution (roadmap M3)
    // -----------------------------------------------------------------------
    //
    // HONEST SCOPE (documented deliberately):
    //
    // * Gated behind `max_parallel_workers > 1`. With the default (1) NONE of
    //   this code runs and the sequential loop is byte-for-byte unchanged.
    // * Only NOT-YET-STARTED, Pending, PLAN-origin features are eligible.
    //   Fix-origin features, respawn candidates (Active), and everything after
    //   the first parallel batch fall through to the sequential path — the
    //   respawn/dirty-tree/judgement machinery there is the tested core and is
    //   never duplicated here.
    // * One orchestrator decision turn marks the INDEPENDENT subset and the
    //   MERGE ORDER (lenient parse + one retry + conservative default =
    //   all-sequential, i.e. no parallel batch). At most N run concurrently.
    // * Each independent feature runs its worker IN ITS OWN GIT WORKTREE on a
    //   per-feature branch off the milestone-start sha (real filesystem
    //   isolation). Branches merge into the mission branch SEQUENTIALLY in the
    //   declared order via merge_no_ff.
    // * CONFLICT HANDLING — the SAFE subset: a conflicting merge is aborted
    //   (git leaves a clean tree) and the feature is FAILED with a clear
    //   reason. Synthesizing a conflict-resolution fix-feature was judged too
    //   risky to land safely against the current event set (it would have to
    //   reopen a milestone mid-batch and thread both worktrees' reports), so it
    //   is deferred; see contractChangeRequest.
    // * A cleanup GUARD removes every per-feature worktree and its branch at
    //   the end of the batch — success or failure, panic or early return — so
    //   no worktree is ever leaked.
    // * The event log stays single-writer AND the N worker claude sessions
    //   OVERLAP in wall-clock (roadmap M3 "done when"). The batch runs in three
    //   phases (see run_parallel_batch_inner): Phase A emits feature.started +
    //   forks worktrees serially; Phase B runs all N worker sessions CONCURRENTLY
    //   via a JoinSet, each BUFFERING its event kinds (run_worker_in_buffered)
    //   and touching no log; Phase C replays each worker's buffered kinds through
    //   the engine's single-writer emit, then judges + merges, serially, in the
    //   declared order. Only the engine ever appends (Phases A/C are &mut self,
    //   one at a time; Phase B appends nothing), so seq stays monotonic and
    //   contiguous while the sessions themselves ran at the same time. The peak
    //   wall-clock overlap is recorded in the batch summary decision.

    /// Try to run a parallel batch for milestone `mi`. Returns `Ok(true)` when
    /// a batch ran (the loop should re-evaluate) and `Ok(false)` when there was
    /// nothing to parallelize (execution falls through to the sequential path).
    ///
    /// Only fires with ≥2 not-yet-started Pending/Plan features the
    /// orchestrator judges independent; otherwise `false`.
    async fn try_parallel_batch(&mut self, mi: usize) -> Result<bool> {
        // Candidate features: not-yet-started (Pending), plan-origin, and no
        // worker has ever run against them (worker_runs empty — a belt-and-
        // braces guard so a resumed mission never re-forks a started feature).
        let candidates: Vec<(String, usize)> = self.state.mission.milestones[mi]
            .features
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                f.status == FeatureStatus::Pending
                    && f.origin == FeatureOrigin::Plan
                    && f.worker_runs.is_empty()
            })
            .map(|(fi, f)| (f.id.clone(), fi))
            .collect();
        if candidates.len() < 2 {
            return Ok(false); // nothing to fan out — sequential handles it
        }

        // Ask the orchestrator which candidates are independent + merge order.
        let cap = self.state.config.max_parallel_workers as usize;
        let candidate_ids: Vec<String> = candidates.iter().map(|(id, _)| id.clone()).collect();
        let batch = self.plan_parallel_batch(mi, &candidate_ids).await?;

        // Map the chosen ids back to feature indices, in the declared merge
        // order, keeping only known candidate ids and capping at N. Fewer than
        // two after all filtering → not worth a batch, fall through.
        let index_of = |id: &str| {
            candidates
                .iter()
                .find(|(cid, _)| cid == id)
                .map(|(_, fi)| *fi)
        };
        let mut chosen: Vec<(String, usize)> = Vec::new();
        for id in &batch {
            if chosen.len() >= cap {
                break;
            }
            if let Some(fi) = index_of(id) {
                if !chosen.iter().any(|(cid, _)| cid == id) {
                    chosen.push((id.clone(), fi));
                }
            }
        }
        if chosen.len() < 2 {
            return Ok(false);
        }

        self.run_parallel_batch(mi, &chosen).await?;
        Ok(true)
    }

    /// The parallelization decision turn (roadmap M3): put the candidate
    /// feature ids to the orchestrator and get back the independent subset plus
    /// the merge order. Lenient parse + one retry; the conservative default on
    /// an unparseable/empty answer is "no independent features" (an empty Vec),
    /// which makes [`Self::try_parallel_batch`] fall through to sequential.
    async fn plan_parallel_batch(
        &mut self,
        mi: usize,
        candidate_ids: &[String],
    ) -> Result<Vec<String>> {
        let milestone_id = self.state.mission.milestones[mi].id.clone();
        let listed = self.state.mission.milestones[mi]
            .features
            .iter()
            .filter(|f| candidate_ids.contains(&f.id))
            .map(|f| format!("- [{}] {}: {}", f.id, f.title, f.spec.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        let message = format!(
            "Milestone {milestone_id} has these not-yet-started features. Decide which are \
             INDEPENDENT of one another — safe to implement concurrently in separate git \
             worktrees without touching the same files or depending on each other's output — \
             and the ORDER their branches should merge back. Conservative is correct: if two \
             features might touch the same code, do NOT call them independent. It is fine to \
             mark none or only some independent.\n\nFEATURES:\n{listed}\n\nRespond with ONLY \
             this JSON:\n\
             {{\"independent\":[\"featureId\",...],\"mergeOrder\":[\"featureId\",...],\"summary\":\"string\"}}"
        );
        let (decision, text): (Option<ParallelDecision>, String) =
            self.json_decision::<ParallelDecision>(&message).await?;
        let decision = decision.unwrap_or_default();

        // Keep only ids that are real candidates; de-dupe. The merge order is
        // the declared order restricted to the independent set, then any
        // independent id the orchestrator forgot to order, appended in plan
        // (candidate) order — so every independent feature gets a defined slot.
        let independent: Vec<String> = decision
            .independent
            .iter()
            .filter(|id| candidate_ids.contains(id))
            .cloned()
            .collect();
        let mut order: Vec<String> = Vec::new();
        for id in decision.merge_order.iter().chain(independent.iter()) {
            if independent.contains(id) && !order.contains(id) {
                order.push(id.clone());
            }
        }

        let summary = if decision.summary.is_empty() {
            format!("parallelization: {} independent feature(s)", order.len())
        } else {
            decision.summary
        };
        self.emit_decision(
            &format!("parallel plan for {milestone_id}: {summary}"),
            Some(text),
        )?;
        Ok(order)
    }

    /// Run one parallel batch (roadmap M3): fork a worktree per chosen feature,
    /// run its worker there, then merge the per-feature branches into the
    /// mission branch in the given (declared) order. A cleanup guard removes
    /// every worktree + branch on the way out, whatever happens.
    ///
    /// `chosen` is `(feature_id, feature_index)` in merge order.
    async fn run_parallel_batch(&mut self, mi: usize, chosen: &[(String, usize)]) -> Result<()> {
        let milestone_id = self.state.mission.milestones[mi].id.clone();
        let start_sha = self.state.mission.milestones[mi]
            .start_sha
            .clone()
            .ok_or_else(|| {
                EngineError::InvalidState(format!(
                    "milestone {milestone_id} started a parallel batch without a start sha"
                ))
            })?;
        let mission_branch = self.state.mission.mission_branch.clone();

        // Per-feature worktree layout, built up front so the cleanup guard sees
        // every path/branch even if a spawn fails midway.
        let workspaces: Vec<ParallelWorkspace> = chosen
            .iter()
            .map(|(feature_id, _fi)| ParallelWorkspace {
                feature_id: feature_id.clone(),
                branch: format!("kranz/wt/{}/{}", self.state.mission.id, feature_id),
                path: parallel_worktree_path(
                    &self.paths.repo_root,
                    &self.state.mission.id,
                    feature_id,
                ),
            })
            .collect();

        // The whole batch is wrapped so we can ALWAYS clean up worktrees, even
        // on an error return. `batch_result` carries the fallible body's error
        // to re-raise after cleanup.
        //
        // `preserve` comes back from the inner body with the indices of
        // features whose worktree could not even be INSPECTED at the Phase C
        // checkpoint (12th-pass review, P2): an inspection error must never
        // be read as a clean tree and reaped with a possibly dirty
        // deliverable inside, so the guard skips BOTH the worktree dir and
        // its branch for those. (resume()'s operator-initiated crash sweep
        // still reaps by path shape — the failure record names the path
        // while it survives.)
        let mut preserve: Vec<usize> = Vec::new();
        let batch_result = self
            .run_parallel_batch_inner(mi, &start_sha, &mission_branch, &workspaces, &mut preserve)
            .await;

        // Cleanup guard: remove every worktree + branch we created, except
        // the preserved inspection failures. Best-effort
        // and idempotent (remove_worktree/delete_branch_force tolerate absence);
        // a cleanup failure is logged, never allowed to mask the batch outcome.
        for (idx, ws) in workspaces.iter().enumerate() {
            if preserve.contains(&idx) {
                continue;
            }
            if let Err(e) = self.repo.remove_worktree(&ws.path) {
                tracing::warn!(path = %ws.path.display(), error = %e, "worktree cleanup failed");
            }
            if let Err(e) = self.repo.delete_branch_force(&ws.branch) {
                tracing::warn!(branch = %ws.branch, error = %e, "worktree branch cleanup failed");
            }
        }
        if let Err(e) = self.repo.prune_worktrees() {
            tracing::warn!(error = %e, "worktree prune failed");
        }

        batch_result
    }

    /// Set up the mission integration worktree (M7 tier 1 primitive): ensures
    /// the mission branch exists, then checks it out into a dedicated
    /// worktree at [`mission_worktree_path`] — WITHOUT touching the primary
    /// checkout's current branch.
    ///
    /// Called from `run()` when `workerIsolation = worktree`, which routes
    /// mission-branch mutations through the returned worktree for the run.
    fn setup_mission_worktree(&self) -> Result<(PathBuf, GitRepo)> {
        let mission_branch = self.state.mission.mission_branch.clone();
        if !self.repo.branch_exists(&mission_branch)? {
            let from = self
                .state
                .mission
                .base_sha
                .clone()
                .unwrap_or_else(|| self.state.mission.base_branch.clone());
            self.repo.create_branch(&mission_branch, Some(&from))?;
        }

        let path = mission_worktree_path(&self.paths.repo_root, &self.state.mission.id);
        // Idempotent: a stale integration worktree from a prior crash must be
        // gone before checking the branch out again (git refuses to check the
        // same branch out twice).
        let _ = self.repo.remove_worktree(&path);
        let _ = self
            .repo
            .remove_worktree(&legacy_mission_worktree_path(&self.state.mission.id));
        let _ = self.repo.prune_worktrees();

        self.repo.add_worktree_checkout(&path, &mission_branch)?;
        let wt_repo = GitRepo::open(&path)?;
        Ok((path, wt_repo))
    }

    /// Tear down the mission integration worktree created by
    /// [`Self::setup_mission_worktree`]. Best-effort and idempotent, mirroring
    /// the parallel-batch cleanup guard: failures are logged, never fatal.
    fn teardown_mission_worktree(&self) {
        let path = mission_worktree_path(&self.paths.repo_root, &self.state.mission.id);
        if let Err(e) = self.repo.remove_worktree(&path) {
            tracing::warn!(path = %path.display(), error = %e, "mission worktree cleanup failed");
        }
        if let Err(e) = self.repo.prune_worktrees() {
            tracing::warn!(error = %e, "mission worktree prune failed");
        }
    }

    /// Fallible body of [`Self::run_parallel_batch`] (the caller's cleanup guard
    /// runs regardless of how this returns).
    ///
    /// WALL-CLOCK OVERLAP, SINGLE-WRITER PRESERVED (roadmap M3). The batch runs
    /// in three phases so the N worker *claude sessions* overlap in wall-clock
    /// while the events.jsonl single-writer / monotonic-seq invariant still
    /// holds:
    ///
    ///   Phase A (serial, engine-owned writer): emit `feature.started` for each
    ///     Pending feature and create its worktree off the milestone-start sha.
    ///   Phase B (CONCURRENT, no log/engine access): run every feature's worker
    ///     session at once via a `JoinSet`, each BUFFERING its event kinds
    ///     (`run_worker_in_buffered`) rather than touching the shared log. Only
    ///     the claude sessions and per-run transcript files (distinct files) are
    ///     live here; nothing appends to events.jsonl.
    ///   Phase C (serial, engine-owned writer, in declared merge order): replay
    ///     each worker's buffered kinds through the engine's single-writer
    ///     `emit`, then judge + commit + merge exactly as the sequential-merge
    ///     code did — so appends stay serialized and seq stays contiguous.
    ///
    /// Because only the engine appends (Phases A and C are `&mut self`, one at a
    /// time; Phase B appends nothing), invariant (a) SINGLE WRITER holds. A
    /// crash during Phase B loses only buffered-but-unwritten worker events —
    /// acceptable: the whole batch re-runs on resume and its worktree branches
    /// are swept by `resume()`. A crash during Phase C leaves a log the resume
    /// path recovers from (any half-emitted feature is re-forked, its stale
    /// worktree/branch swept). This is gated behind `max_parallel_workers > 1`;
    /// the sequential path never reaches here.
    ///
    /// `preserve` collects the indices of workspaces whose Phase C checkpoint
    /// found the worktree UNINSPECTABLE (12th-pass review, P2): the caller's
    /// cleanup guard skips reaping those worktree dirs and branches so the
    /// unverified bytes survive for human inspection.
    async fn run_parallel_batch_inner(
        &mut self,
        mi: usize,
        start_sha: &str,
        mission_branch: &str,
        workspaces: &[ParallelWorkspace],
        preserve: &mut Vec<usize>,
    ) -> Result<()> {
        let milestone_id = self.state.mission.milestones[mi].id.clone();
        let mut merged_ok: usize = 0;
        let mut conflicts: usize = 0;
        let mut resolutions: usize = 0;

        // --- Phase A (serial, single-writer): feature.started + worktrees ----
        // Emit feature.started through the engine's own writer and fork every
        // worktree off the milestone-start sha, up front, in workspace order.
        // Doing this before any session runs keeps the ONLY log appends in this
        // phase engine-serial, and gives the cleanup guard every path even if a
        // later phase fails.
        for ws in workspaces {
            let (mwi, fwi) = self.locate_feature(&ws.feature_id)?;
            if self.state.mission.milestones[mwi].features[fwi].status == FeatureStatus::Pending {
                self.emit(EventKind::FeatureStarted {
                    feature_id: ws.feature_id.clone(),
                })?;
            }
            self.repo.add_worktree(&ws.path, &ws.branch, start_sha)?;
        }

        // --- Phase B (CONCURRENT, no log access): run every worker session ---
        // Snapshot each worker's inputs, then run all sessions at once. Each
        // buffers its kinds and returns them with its RunOutcome; NONE touches
        // the shared log. A shared peak-concurrency tracker records how many
        // sessions were live simultaneously so the batch summary can prove the
        // overlap (and tests can assert it).
        let goal = self.state.mission.goal.clone();
        let milestone_title = self.state.mission.milestones[mi].title.clone();
        let base_sha = self.state.mission.base_sha.clone();
        let grants = self.state.mission.command_grants.clone();
        let egress_grants = self.state.mission.egress_grants.clone();
        let deny_exceptions = self.state.mission.deny_exceptions.clone();
        let touch_set = self.state.mission.touch_set.clone();
        let tracker = ConcurrencyTracker::new();
        let selected = self.select_backend(Role::Worker);
        if let Some(reason) = selected.fallback_reason.as_deref() {
            self.emit_decision(reason, None)?;
        }
        let selected_kind = selected.kind;
        let worker_backend = Arc::clone(&selected.backend);
        let cfg = selected.cfg;
        // Once-per-mission cached decision (mission m-165b6f, f-2-1): computed
        // here, BEFORE any concurrent worker task is spawned, so every worker
        // in this batch shares the exact same decision and the preflight
        // session never races itself.
        let auth_verdict = if selected_kind == BackendKind::Claude {
            self.worker_auth_verdict().await
        } else {
            AuthVerdict::Inconclusive
        };

        let mut set: tokio::task::JoinSet<(usize, BufferedRunResult)> = tokio::task::JoinSet::new();
        for (idx, ws) in workspaces.iter().enumerate() {
            let (mwi, fwi) = self.locate_feature(&ws.feature_id)?;
            let feature = self.state.mission.milestones[mwi].features[fwi].clone();
            let backend = Arc::clone(&worker_backend);
            let paths = self.paths.clone();
            let cfg = cfg.clone();
            let goal = goal.clone();
            let milestone_title = milestone_title.clone();
            let ws_path = ws.path.clone();
            let guard = tracker.clone();
            let base_sha = base_sha.clone();
            let grants = grants.clone();
            let egress_grants = egress_grants.clone();
            let deny_exceptions = deny_exceptions.clone();
            let touch_set = touch_set.clone();
            set.spawn(async move {
                let _live = guard.enter(); // count this session as live
                let result = runner::run_worker_in_buffered(
                    backend.as_ref(),
                    &paths,
                    &cfg,
                    &feature,
                    &goal,
                    &milestone_title,
                    None,
                    &ws_path,
                    base_sha.as_deref(),
                    &grants,
                    &egress_grants,
                    &deny_exceptions,
                    auth_verdict,
                    &touch_set,
                )
                .await;
                (idx, result)
            });
        }

        // Collect results, keyed by workspace index so Phase C can process them
        // in the DECLARED merge order regardless of completion order.
        let mut buffered: Vec<Option<(Vec<EventKind>, runner::RunOutcome)>> =
            (0..workspaces.len()).map(|_| None).collect();
        let mut join_err: Option<EngineError> = None;
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((idx, Ok(result))) => buffered[idx] = Some(result),
                Ok((_, Err(e))) => join_err = join_err.or(Some(e)),
                Err(e) => {
                    join_err = join_err.or(Some(EngineError::Backend(format!(
                        "parallel worker task panicked: {e}"
                    ))));
                }
            }
        }
        // A session error/panic aborts the batch AFTER every task has been
        // joined (the JoinSet is drained above, so no worker is left running).
        // The caller's cleanup guard still sweeps every worktree/branch, and a
        // re-run of the batch on resume retries cleanly.
        if let Some(e) = join_err {
            return Err(e);
        }
        let peak = tracker.peak();

        // --- Phase C (serial, single-writer, DECLARED merge order) -----------
        // Replay each worker's buffered kinds through the engine's own writer,
        // then judge + commit + merge exactly as the sequential-merge code did.
        let mut worker_ok: Vec<WorktreeDisposition> = Vec::with_capacity(workspaces.len());
        for (idx, ws) in workspaces.iter().enumerate() {
            let (events, outcome) = buffered[idx]
                .take()
                .expect("every non-errored workspace has a buffered result");
            let disposition = self
                .append_and_judge_worktree(ws, &milestone_id, start_sha, events, &outcome)
                .await?;
            // An uninspectable worktree keeps its bytes (12th-pass review,
            // P2): the caller's cleanup guard skips its dir AND branch.
            if matches!(disposition, WorktreeDisposition::InspectionFailed) {
                preserve.push(idx);
            }
            worker_ok.push(disposition);
        }

        // (2) Merge the per-feature branches into the mission branch in the
        // declared order. Clean → keep the feature's commits + feature.completed;
        // conflict (aborted, clean tree) → feature.failed PLUS a resolution
        // fix-feature (below); a worker that failed in its worktree →
        // feature.failed without attempting a merge.
        for (ws, disposition) in workspaces.iter().zip(&worker_ok) {
            let feature_id = ws.feature_id.clone();
            match disposition {
                WorktreeDisposition::Ready => {}
                WorktreeDisposition::NotReady => {
                    self.emit(EventKind::FeatureFailed {
                        feature_id,
                        reason: "worker run did not complete in its parallel worktree".to_string(),
                    })?;
                    continue;
                }
                WorktreeDisposition::InspectionFailed => {
                    // Named separately from a plain worker failure: the bytes
                    // were never verified, and they SURVIVE (the cleanup
                    // guard skips this worktree + branch) so a human can
                    // inspect what the engine could not (12th-pass review).
                    self.emit(EventKind::FeatureFailed {
                        feature_id,
                        reason: format!(
                            "worktree inspection failed after the run; worktree and branch {} \
                             are preserved for inspection (see the checkpoint decision record)",
                            ws.branch
                        ),
                    })?;
                    continue;
                }
            }
            let pre_merge_sha = self.active_repo().head_sha()?;
            match self.active_repo().merge_no_ff(&ws.branch)? {
                crate::git_ops::MergeOutcome::Clean => {
                    let commits: Vec<String> = self
                        .active_repo()
                        .commits_between(&pre_merge_sha, "HEAD")?
                        .iter()
                        .map(|c| format!("{} {}", c.sha, c.subject))
                        .collect();
                    self.emit(EventKind::FeatureCompleted {
                        feature_id,
                        commits,
                    })?;
                    merged_ok += 1;
                }
                crate::git_ops::MergeOutcome::Conflict { files } => {
                    conflicts += 1;
                    // Fail the conflicting feature (its worktree branch is
                    // discarded by the cleanup guard) …
                    let files_note = if files.is_empty() {
                        String::new()
                    } else {
                        format!(" (conflicting files: {})", files.join(", "))
                    };
                    // Snapshot the original before feature.failed flips its
                    // status — the resolution spec quotes its title/spec.
                    let original = self.state.mission.milestones
                        [self.locate_feature(&feature_id)?.0]
                        .features
                        .iter()
                        .find(|f| f.id == feature_id)
                        .cloned();
                    self.emit(EventKind::FeatureFailed {
                        feature_id: feature_id.clone(),
                        reason: format!(
                            "parallel merge of {} into {mission_branch} conflicted and was \
                             aborted{files_note}; a resolution feature re-does this work on \
                             the merged branch",
                            ws.branch
                        ),
                    })?;
                    // … and ALSO synthesize a conflict-resolution fix-feature
                    // on the SAME (still-Active) milestone so the milestone can
                    // be RESOLVED rather than merely losing the feature. It runs
                    // SEQUENTIALLY on the next loop iteration (first_incomplete
                    // picks up the Active milestone; next_feature the new
                    // Pending fix feature) — no worktree, straight on the
                    // mission branch, so it cannot conflict again. The
                    // infinite-chain guard (synthesize_conflict_resolution
                    // returns None for a `-conflict-` id) means a resolution
                    // that ITSELF conflicts would not spawn another; in this
                    // Plan-origin batch that never arises, so the emit always
                    // fires here.
                    if let Some(original) = original {
                        let existing = &self.state.mission.milestones
                            [self.locate_feature(&feature_id)?.0]
                            .features;
                        if let Some(resolution) = synthesize_conflict_resolution(
                            &milestone_id,
                            &original,
                            &files,
                            existing,
                        ) {
                            // Belt and braces: the strings are model-derived
                            // (the original feature's title/spec) and land
                            // verbatim in a fixfeature.created event.
                            let feature = Feature {
                                title: scrub::scrub(&resolution.title),
                                spec: scrub::scrub(&resolution.spec),
                                validation_criteria: resolution
                                    .validation_criteria
                                    .iter()
                                    .map(|c| scrub::scrub(c))
                                    .collect(),
                                ..resolution
                            };
                            self.emit(EventKind::FixFeatureCreated {
                                milestone_id: milestone_id.clone(),
                                feature,
                            })?;
                            resolutions += 1;
                        }
                    }
                }
                crate::git_ops::MergeOutcome::RefusedPreMerge { detail } => {
                    conflicts += 1;
                    // A pre-MERGE_HEAD refusal (e.g. an untracked file in the
                    // way) is not a content conflict, so there is nothing for
                    // a resolution feature to re-implement — just fail the
                    // feature with git's verbatim detail.
                    self.emit(EventKind::FeatureFailed {
                        feature_id: feature_id.clone(),
                        reason: format!(
                            "parallel merge of {} into {mission_branch} was refused by git \
                             before it started: {detail}",
                            ws.branch
                        ),
                    })?;
                }
            }
        }

        // (3) One summarizing orchestrator.decision for the batch (existing
        // event vocabulary only). Names the conflict→resolution outcome AND the
        // peak wall-clock overlap (how many worker sessions ran at once) so both
        // appear in the replayed history/digest — and so tests can assert the
        // sessions actually overlapped without touching the mock backend.
        self.emit_decision(
            &format!(
                "parallel: {} workers (peak {} concurrent), merged {} branches, {} conflicts \
                 -> {} resolution features ({milestone_id})",
                workspaces.len(),
                peak,
                merged_ok,
                conflicts,
                resolutions
            ),
            None,
        )?;
        Ok(())
    }

    /// Phase C for one feature (roadmap M3): append the worker's BUFFERED event
    /// kinds through the engine's single-writer `emit`, checkpoint-commit its
    /// worktree, and judge the run. Returns [`WorktreeDisposition::Ready`] when
    /// the work is ready to merge; any other variant fails the feature (and
    /// `InspectionFailed` additionally preserves the worktree + branch).
    ///
    /// `buffered` is exactly the `worker.spawned` / `worker.message` /
    /// `worker.completed` kinds `run_worker_in_buffered` collected while the
    /// session ran concurrently in Phase B (plus any `hook.gate.fired`
    /// records folded at session end, KRZ-302) — replaying them here,
    /// serially, through `emit` is what keeps events.jsonl single-writer
    /// with contiguous seq even though the sessions overlapped.
    /// `feature.started` was already emitted in Phase A.
    ///
    /// Deliberately does NOT respawn: the parallel batch is best-effort per the
    /// honest subset. A non-complete judgement fails the feature (its branch is
    /// discarded by the cleanup guard); the sequential path — with its full
    /// respawn/dirty-tree machinery — remains the way a feature gets retried.
    async fn append_and_judge_worktree(
        &mut self,
        ws: &ParallelWorkspace,
        milestone_id: &str,
        start_sha: &str,
        buffered: Vec<EventKind>,
        outcome: &runner::RunOutcome,
    ) -> Result<WorktreeDisposition> {
        // Replay the buffered run kinds through the engine's own single writer,
        // in the order the session produced them. `emit` folds each into state
        // (worker.spawned → the run is registered on the feature, etc.), so no
        // separate catch_up is needed — but flush any throttled deltas so a
        // later log read sees them.
        for kind in buffered {
            self.emit(kind)?;
        }
        self.log.flush()?;

        // A GitRepo rooted at the worktree, for its own dirty-tree/commit ops.
        // The worktree is HOSTILE (12th-pass review, P1): the worker that ran
        // in it could plant `core.fsmonitor`, `core.hooksPath`, or a hook in
        // its git metadata, which the checkpoint's own status/commit would
        // then EXECUTE with the engine's ambient privileges. The handle runs
        // hooks/fsmonitor-disabled — the same countermeasure the
        // validator-fingerprint and gated-merge paths use
        // (`GitRepo::with_hooks_disabled`).
        //
        // Inspection is LOAD-BEARING (12th-pass, P2): an inspection ERROR
        // must never be read as "clean" (the old `unwrap_or(true)`) or "0
        // commits" (`unwrap_or_default()`) — the cleanup guard would then
        // reap the worktree with a possibly dirty deliverable inside. Any
        // failure to open, identify, inspect, or query the worktree fails the
        // feature honestly and PRESERVES its bytes. Only a COMMIT-time
        // failure stays a batch error (`?`): the tree was inspectable by
        // then, so that is a real git failure, not hostile metadata.
        let wt_repo = match GitRepo::open(&ws.path).and_then(|repo| repo.with_hooks_disabled()) {
            Ok(repo) => repo,
            Err(error) => return self.record_uninspectable_worktree(ws, &error),
        };
        if let Err(error) = wt_repo.ensure_identity() {
            return self.record_uninspectable_worktree(ws, &error);
        }

        // Commit any worker output on the per-feature branch (in the worktree)
        // so the merge carries it. The worker session's own commits (if any)
        // already landed on the branch; a dirty tree is checkpoint-committed
        // here rather than run through the sequential dirty-tree turn — the
        // parallel subset keeps its worktree self-contained. A real git
        // failure `?`-aborts the batch (the caller's cleanup guard still
        // reaps every non-preserved worktree); a secret-scan refusal is
        // recorded below, so dirty deliverables are never silently dropped
        // before judgement.
        let clean = match wt_repo.is_clean() {
            Ok(clean) => clean,
            Err(error) => return self.record_uninspectable_worktree(ws, &error),
        };
        if !clean {
            match wt_repo.commit_dirty_paths(
                &contract_sweep::parallel_checkpoint_commit_message(&ws.feature_id),
            )? {
                crate::git_ops::CheckpointOutcome::Committed(_) => {}
                crate::git_ops::CheckpointOutcome::RefusedBySecretScan { detail } => {
                    // Same policy refusal as the sequential dirty-tree turn:
                    // record it and report the run not-ready-to-merge — the
                    // caller fails the feature, and the batch cleanup guard
                    // discards the worktree along with its secret-bearing
                    // leftovers.
                    self.emit_decision(
                        &format!(
                            "parallel checkpoint for {}: refused by secret scan",
                            ws.feature_id
                        ),
                        Some(detail),
                    )?;
                    return Ok(WorktreeDisposition::NotReady);
                }
            }
        }

        // Judge the run against the worktree's own commit range (start_sha..HEAD
        // in the worktree — the branch was forked at start_sha).
        let commits: Vec<String> = match wt_repo.commits_between(start_sha, "HEAD") {
            Ok(commits) => commits
                .iter()
                .map(|c| format!("{} {}", c.sha, c.subject))
                .collect(),
            Err(error) => return self.record_uninspectable_worktree(ws, &error),
        };
        let diff_stat = wt_repo.diff_stat(start_sha, "HEAD").unwrap_or_default();
        // Worker self-escalation (KRZ-331): same record-only emission as the
        // sequential path, before the judgement turn consumes the report.
        self.emit_worker_escalation(&ws.feature_id, outcome)?;
        // Structured human questions (ticket
        // structured-human-question-events): same projection open as the
        // sequential path — never a park.
        self.emit_worker_questions(milestone_id, &ws.feature_id, outcome)?;
        match self
            .judge_worker_run(&ws.feature_id, outcome, &commits, &diff_stat)
            .await?
        {
            JudgementOutcome::Complete => Ok(WorktreeDisposition::Ready),
            // Respawn/Failed both mean "not ready to merge" in the parallel
            // subset (no respawn here); the feature is failed by the caller.
            JudgementOutcome::Failed(_) | JudgementOutcome::Respawn(_) => {
                Ok(WorktreeDisposition::NotReady)
            }
        }
    }

    /// Record an uninspectable parallel worktree (12th-pass review, P2): the
    /// failure lands in a decision record — where the batch's other
    /// checkpoint failures (e.g. a secret-scan refusal) are recorded — and
    /// the caller marks the feature failed AND preserves the worktree dir +
    /// branch. An inspection error must never be read as a clean tree whose
    /// bytes the cleanup guard may reap.
    fn record_uninspectable_worktree(
        &mut self,
        ws: &ParallelWorkspace,
        error: &EngineError,
    ) -> Result<WorktreeDisposition> {
        self.emit_decision(
            &format!(
                "parallel checkpoint for {}: worktree inspection failed",
                ws.feature_id
            ),
            Some(format!(
                "{error} — the feature is failed honestly and its worktree dir + branch are \
                 PRESERVED for inspection (an inspection error is never a clean, reapable tree)"
            )),
        )?;
        Ok(WorktreeDisposition::InspectionFailed)
    }

    /// Locate a feature by id, returning `(milestone_index, feature_index)`.
    fn locate_feature(&self, feature_id: &str) -> Result<(usize, usize)> {
        for (mi, ms) in self.state.mission.milestones.iter().enumerate() {
            if let Some(fi) = ms.features.iter().position(|f| f.id == feature_id) {
                return Ok((mi, fi));
            }
        }
        Err(EngineError::InvalidState(format!(
            "parallel batch references unknown feature '{feature_id}'"
        )))
    }

    // -----------------------------------------------------------------------
    // Validation round (g)
    // -----------------------------------------------------------------------

    /// The cleared env contract `command` assertions run with
    /// (agent-env-clear): a per-mission scratch HOME under the gitignored
    /// `runs/` dir, the minimal allowlist, toolchain caches, and exactly the
    /// operator's `contractEnvPassthrough` names — ambient secrets never
    /// reach a contract command. The passthrough application is recorded as
    /// a decision (names only, never values) so the escape hatch is always
    /// audible in the event log.
    fn contract_command_env(&mut self, base_sha: Option<&str>) -> Result<HashMap<String, String>> {
        let passthrough = self.state.config.contract_env_passthrough.clone();
        if !passthrough.is_empty() {
            self.emit_decision(
                "contract env passthrough applied",
                Some(format!(
                    "contractEnvPassthrough names copied from ambient into the contract \
                     command env (values never logged): {}",
                    passthrough.join(", ")
                )),
            )?;
        }
        let scratch = self.paths.runs_dir().join("contract-home");
        Ok(crate::agent_env::contract_command_env(
            &scratch,
            base_sha,
            &passthrough,
        ))
    }

    /// The sandbox posture engine-run gate commands execute under (ticket
    /// engine-gates-sandbox-wrapped): the worker role's resolved wrap —
    /// process profile or, with `provider: container`, the mission container
    /// (ticket container-gate-wrapper) — with the gate's cwd (`root`, the
    /// active tree) as the writable root and the mission's
    /// `runs/contract-home` as the private scratch — the same shape the
    /// contract env already points HOME/TMPDIR/CARGO_HOME at, so no env
    /// change is needed on this path. `enforce: off` resolves to
    /// [`crate::command_exec::GateSandbox::Disabled`], today's exact
    /// behavior; an enforced posture is recorded as a decision so the wrap
    /// is audible in the event log. Resolution failures (linux without
    /// `bwrap`, an unsupported platform, `provider: container` with no
    /// runtime on PATH) fail closed, mirroring session resolution.
    fn gate_sandbox(&mut self, root: &std::path::Path) -> Result<crate::command_exec::GateSandbox> {
        let resolution = crate::command_exec::resolve_gate_sandbox(
            &self.state.config.worker.sandbox,
            root,
            &self.paths.mission_dir(),
            &self.paths.runs_dir().join("contract-home"),
            &self.paths.runs_dir(),
        )?;
        match &resolution.note {
            Some(note) => {
                self.emit_decision("engine-run gates NOT sandbox-wrapped", Some(note.clone()))?
            }
            None if resolution.sandbox.enforce() != crate::types::SandboxEnforce::Off => self
                .emit_decision(
                    "engine-run gates sandbox-wrapped",
                    Some(format!(
                        "validation/final-gate commands execute inside the resolved worker \
                         sandbox wrap (provider:{}, enforce:{}): writes limited to the gate \
                         tree plus the contract scratch; mission metadata write-denies and \
                         authority read-denies apply as they do to agent sessions",
                        self.state.config.worker.sandbox.provider.as_str(),
                        self.state.config.worker.sandbox.enforce.as_str()
                    )),
                )?,
            // enforce: off — today's posture exactly; no new event noise.
            None => {}
        }
        Ok(resolution.sandbox)
    }

    /// Run the contract's command assertions engine-side and render the
    /// captured results for the functional validator's task (validator
    /// repair 3/5): the validator judges verbatim PASS/FAIL evidence instead
    /// of authoring shell — the m-9e4ef3 failure mode (improvised compounds,
    /// pipes, lost exit codes, accidental backgrounding, Monitors). Returns
    /// None when the contract has no command assertions.
    ///
    /// `env` is the commands' COMPLETE (cleared) environment, built by the
    /// caller via [`Self::contract_command_env`]; `sandbox` is the resolved
    /// gate wrap from [`Self::gate_sandbox`] ([`crate::command_exec::GateSandbox::Disabled`]
    /// reproduces the pre-wrap behavior exactly).
    ///
    /// Deliberately an associated function WITHOUT a self receiver: a `&self`
    /// receiver is captured by the async future for its whole lifetime, and
    /// `&MissionEngine` is not Send (MissionEngine is not Sync), which would
    /// make run()'s future non-Send for spawn-based drivers.
    async fn run_contract_commands_for_validation(
        contract: &[Assertion],
        root: &std::path::Path,
        env: &HashMap<String, String>,
        sandbox: &crate::command_exec::GateSandbox,
    ) -> Option<String> {
        let command_assertions: Vec<(String, Option<String>)> = contract
            .iter()
            .filter(|a| a.check == AssertionCheck::Command)
            .map(|a| (a.id.clone(), a.command.clone()))
            .collect();
        if command_assertions.is_empty() {
            return None;
        }
        let mut rendered = String::new();
        for (id, command) in command_assertions {
            match command.as_deref() {
                Some(command) => {
                    let (ok, output) =
                        run_shell_command_sandboxed(root, command, env, sandbox).await;
                    let verdict = if ok { "PASS" } else { "FAIL" };
                    let tail = scrub::scrub(&output);
                    rendered.push_str(&format!("- [{id}] `{command}` → {verdict}\n{tail}\n"));
                }
                None => rendered.push_str(&format!(
                    "- [{id}] (check=command but no command — cannot run)\n"
                )),
            }
        }
        Some(rendered)
    }

    /// Milestone validation: scrutiny then functional validators (v1:
    /// sequential; each skippable by config). Findings go to the conversion
    /// turn, where the orchestrator turns each into a fix feature or waives
    /// it; no findings — or all findings waived — means a tag + completion.
    async fn validation_round(&mut self, mi: usize) -> Result<()> {
        let milestone_id = self.state.mission.milestones[mi].id.clone();
        self.emit(EventKind::MilestoneValidating {
            milestone_id: milestone_id.clone(),
        })?;

        // Golden-data reset between rounds (design D-D): when the workspace
        // contract's data block opts in (`resetBetweenRounds`) and declares
        // a reset hook, re-seed the dataset BEFORE any validator spawn so
        // every round judges the same baseline. A reset failure Blocks with
        // the owned gate shape — never a validator finding.
        if self.run_data_reset_between_rounds().await? {
            return Ok(());
        }

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

        // Engine-run contract commands (validator repair 3/5): executed once
        // here — bounded, process-tree-killed, scrubbed, in the cleared
        // contract env — and handed to the functional validator as
        // authoritative evidence.
        let contract_results = if roles.contains(&Role::ValidatorFunctional) {
            let contract = self.state.mission.validation_contract.clone();
            let base_sha = self.state.mission.base_sha.clone();
            let root = self.active_root().to_path_buf();
            let env = self.contract_command_env(base_sha.as_deref())?;
            let gate_sandbox = self.gate_sandbox(&root)?;
            Self::run_contract_commands_for_validation(&contract, &root, &env, &gate_sandbox).await
        } else {
            None
        };

        for role in roles {
            let milestone = self.state.mission.milestones[mi].clone();
            let contract = self.state.mission.validation_contract.clone();
            let base_sha = self.state.mission.base_sha.clone();
            let grants = self.state.mission.command_grants.clone();
            let egress_grants = self.state.mission.egress_grants.clone();
            let worker_commands = worker_commands_for_milestone(&self.state, &milestone);

            let selected = self.select_backend(role);
            if let Some(reason) = selected.fallback_reason.as_deref() {
                self.emit_decision(reason, None)?;
            }
            let selected_kind = selected.kind;
            let backend = Arc::clone(&selected.backend);
            let cfg = selected.cfg;

            // Validator snapshot (the follow-up to ticket
            // validator-immutability-proof): the validator never sees the
            // real checkout — it runs in a throwaway copy (HEAD + the
            // worker's uncommitted diff, warmed target/) that is discarded
            // with the session. The fingerprint on the REAL checkout stays
            // as a tripwire: with isolation in place it should never drift.
            let fingerprint =
                validator_integrity::CheckoutFingerprint::capture(self.active_repo())?;
            let Some(snapshot) = self.validator_snapshot(&milestone_id, role)? else {
                return Ok(());
            };
            let session_cwd = snapshot.path().to_path_buf();
            // Mandatory containment (ticket validator-mandatory-containment):
            // resolved per spawn so the posture decision lands next to the
            // session it covers; `enforce: off` no longer runs the validator
            // bare where the platform and backend can contain it.
            let validator_sandbox =
                self.validator_containment(role, selected_kind, &cfg, &session_cwd)?;
            let outcome = runner::run_validator_in(
                backend.as_ref(),
                &mut self.log,
                &self.paths,
                &cfg,
                role,
                &milestone,
                &contract,
                &start_sha,
                None,
                &session_cwd,
                base_sha.as_deref(),
                &grants,
                &egress_grants,
                &worker_commands,
                milestone.validator_guidance.as_deref(),
                contract_results.as_deref(),
                validator_sandbox,
            )
            .await;
            let caught = self.catch_up();
            let mut outcome = outcome?;
            caught?;

            // Tripwire on the REAL checkout: any drift across the session
            // means the isolation itself failed — fail the round honestly,
            // before the grant-park/retry machinery.
            if self.fail_on_validator_tamper(&milestone_id, role, &outcome.run_id, &fingerprint)? {
                return Ok(());
            }
            // Validator-set index flags inside the snapshot (4th-pass
            // review): any flag present was set by the validator.
            if self.fail_on_snapshot_index_flags(
                &milestone_id,
                role,
                &outcome.run_id,
                &snapshot,
                &fingerprint.head,
            )? {
                return Ok(());
            }
            // Discard the primary snapshot before any retry builds its own:
            // one warm target/ copy at a time.
            drop(snapshot);

            // Bounded (exactly one retry) runtime fallback: a validator run
            // that did not produce a trusted pass is retried once with the
            // injected Claude backend. A crashed/aborted validator must never
            // collapse into "no findings" and green-light validation.
            //
            // `retried_on_claude` records whether THIS verdict came from the
            // claude retry: confirm-on-pass (below) keys on the verdict being
            // the LOCAL primary's own — a retried verdict is already frontier,
            // so confirming it would judge frontier by frontier.
            let mut retried_on_claude = false;
            if !validator_outcome_trusted(&outcome) {
                // Capability-boundary check (grant-request-decision-flow),
                // gated on the UNTRUSTED outcome: a validator stopped by a
                // command outside its allow-set is a grantable allow-set MISS
                // (validators carry no blanket Bash; `command_grants` fold into
                // their allow-set as `Bash(<cmd>*)` patterns, so extending the
                // grants genuinely unblocks the re-run — unlike a worker
                // deny-rule/hook denial, where deny wins). Offer the narrowest
                // grant and park BEFORE burning the retry (same allow-set). The
                // !trusted gate matters: a validator that hit an incidental
                // denial but still produced a trusted PASS must NOT park, or a
                // later deny would wrongly block a milestone that actually
                // passed.
                if self.maybe_park_for_grant(&milestone_id, role, &outcome)? {
                    return Ok(());
                }
                // Egress grant (3.3b): same boundary, network side — a sandboxed
                // validator whose proxy refused a destination parks for an
                // egress grant BEFORE the retry (approve extends `egress_grants`,
                // which the re-run's proxy allowlist picks up). Checked after the
                // command grant: one boundary per park, the re-run surfaces the
                // next.
                if self.maybe_park_for_egress_grant(&milestone_id, role, &outcome)? {
                    return Ok(());
                }
                self.emit_decision(
                    &format!(
                        "{} {} run did not produce a trusted validator report ({}); retrying once with \
                         the claude {}",
                        selected_kind.as_str(),
                        role_label(role),
                        run_outcome_summary(&outcome),
                        role_label(role)
                    ),
                    None,
                )?;
                let retry_cfg = self.claude_fallback_cfg_for_role(role);
                let retry_backend = Arc::clone(&self.backend);
                // The retry is a fresh validator session: its own throwaway
                // snapshot (the real checkout provably untouched by the
                // primary — the isolation guarantees it, the tripwire
                // verifies it) and its own before/after tripwire pair.
                let retry_fingerprint =
                    validator_integrity::CheckoutFingerprint::capture(self.active_repo())?;
                let Some(retry_snapshot) = self.validator_snapshot(&milestone_id, role)? else {
                    return Ok(());
                };
                let retry_session_cwd = retry_snapshot.path().to_path_buf();
                // The retry runs on the injected claude backend — the one
                // backend that always honors the containment wrap.
                let retry_validator_sandbox = self.validator_containment(
                    role,
                    BackendKind::Claude,
                    &retry_cfg,
                    &retry_session_cwd,
                )?;
                let retry_outcome = runner::run_validator_in(
                    retry_backend.as_ref(),
                    &mut self.log,
                    &self.paths,
                    &retry_cfg,
                    role,
                    &milestone,
                    &contract,
                    &start_sha,
                    None,
                    &retry_session_cwd,
                    base_sha.as_deref(),
                    &grants,
                    &egress_grants,
                    &worker_commands,
                    milestone.validator_guidance.as_deref(),
                    contract_results.as_deref(),
                    retry_validator_sandbox,
                )
                .await;
                let caught = self.catch_up();
                outcome = retry_outcome?;
                caught?;
                retried_on_claude = true;

                if self.fail_on_validator_tamper(
                    &milestone_id,
                    role,
                    &outcome.run_id,
                    &retry_fingerprint,
                )? {
                    return Ok(());
                }
                if self.fail_on_snapshot_index_flags(
                    &milestone_id,
                    role,
                    &outcome.run_id,
                    &retry_snapshot,
                    &retry_fingerprint.head,
                )? {
                    return Ok(());
                }
                drop(retry_snapshot);

                // A denial the runner could only read on the Claude retry (a
                // Codex/Droid primary whose events don't map to a command, or a
                // primary that failed some other way) surfaces its grant here,
                // so those backends aren't silently un-grantable.
                if !validator_outcome_trusted(&outcome)
                    && self.maybe_park_for_grant(&milestone_id, role, &outcome)?
                {
                    return Ok(());
                }
                if !validator_outcome_trusted(&outcome)
                    && self.maybe_park_for_egress_grant(&milestone_id, role, &outcome)?
                {
                    return Ok(());
                }
            }

            if !validator_outcome_trusted(&outcome) {
                let reason = format!(
                    "{} validation did not produce a trusted report after retry: {}",
                    role_label(role),
                    run_outcome_summary(&outcome)
                );
                self.emit_decision(&reason, None)?;
                self.emit(EventKind::MilestoneBlocked {
                    milestone_id,
                    reason,
                })?;
                return Ok(());
            }

            let report = outcome
                .validator_report
                .expect("trusted validator outcome must carry a report");

            // Confirm-on-pass (ticket local-inference-validator-guarded,
            // KRZ-206b; review addendum §4): a LOCAL functional verdict never
            // greens a gate alone. The executor-escalation valve catches
            // executor FAILURES but not validator MISSES — a weak local
            // validator that wrongly PASSES bad work is not a failure, so
            // without this confirmation the "no silent green" promise rests
            // on an unmeasured model. Every local PASS on a contract-command
            // assertion (and any all-clean local report, which would green
            // judgment too) is re-judged by a frontier functional session
            // BEFORE the round may complete, regardless of any spot-check
            // sampling rate; a local FAIL is trusted without confirmation —
            // failures are visible (they cost a fix cycle), misses are the
            // danger, and the asymmetry is deliberate. The confirmations
            // land in the event store as `validation.confirm`, which IS the
            // local-vs-frontier miss-rate ground truth (the ticket's start
            // precondition: the mechanism is the measurement).
            if role == Role::ValidatorFunctional
                && selected_kind == BackendKind::Local
                && !retried_on_claude
            {
                let local_subjects: std::collections::HashSet<&str> =
                    report.findings.iter().map(|f| f.subject.as_str()).collect();
                let has_command_assertions =
                    contract.iter().any(|a| a.check == AssertionCheck::Command);
                let passed_command_ids: Vec<String> = contract
                    .iter()
                    .filter(|a| a.check == AssertionCheck::Command)
                    .map(|a| a.id.clone())
                    .filter(|id| !local_subjects.contains(id.as_str()))
                    .collect();
                let needs_confirm = !passed_command_ids.is_empty()
                    // A contract with no command assertions hands the local
                    // session pure judgment; an all-clean report there would
                    // green the gate on local judgment alone, which the
                    // guarded role split forbids — confirm it exactly like a
                    // command-assertion PASS.
                    || (!has_command_assertions && report.findings.is_empty());
                if needs_confirm {
                    match self
                        .confirm_local_functional_pass(
                            &milestone_id,
                            role,
                            &milestone,
                            &contract,
                            &start_sha,
                            base_sha.as_deref(),
                            &grants,
                            &egress_grants,
                            &worker_commands,
                            contract_results.as_deref(),
                            &outcome.run_id,
                            &report,
                            &passed_command_ids,
                        )
                        .await?
                    {
                        Some(disagreements) => findings.extend(disagreements),
                        // The round blocked honestly (an untrusted
                        // confirmation or a tripwire) — never green on an
                        // unconfirmed local PASS.
                        None => return Ok(()),
                    }
                }
            }

            for finding in report.findings {
                findings.push((outcome.run_id.clone(), finding));
            }
        }

        // Engine-computed out-of-contract-write sweep (M7 tier 1, feature
        // f-1-2): deterministic, side-effect-free, runs alongside the spawned
        // validator sessions above. Attributed to the reserved engine run id,
        // exactly like `final_gate`'s synthesized findings.
        for finding in self.out_of_contract_sweep(&start_sha)? {
            findings.push((crate::reducer::ENGINE_RUN_ID.to_string(), finding));
        }

        // Touch-set grant (grant-request-decision-flow): an out-of-contract
        // write can be resolved by extending the touch_set instead of fixing or
        // waiving it. Offer the operator that grant and park BEFORE recording
        // the findings (so a re-validation on approve doesn't double-emit them):
        // approve extends touch_set and re-validates clean; deny/timeout
        // saturates the cap and lets the write flow to the fix/waive path below.
        if self.maybe_park_for_touch_grant(&milestone_id, &findings)? {
            return Ok(());
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
            // Structured human questions (ticket
            // structured-human-question-events): asks scoped to this
            // milestone are moot once it completes — clear them out of the
            // pending-decision projection.
            self.clear_open_questions("milestone completed", |q| {
                q.milestone_id.as_deref() == Some(milestone_id.as_str())
            })?;
            self.emit(EventKind::MilestoneCompleted { milestone_id, tag })?;
            return Ok(());
        }

        // The conversion turn runs even with the fix-cycle cap exhausted:
        // the cap bounds fix ROUNDS, not the orchestrator's right to judge
        // findings — an all-waived answer completes the milestone where the
        // old flow would have blocked on trivia.
        let findings: Vec<Finding> = findings.into_iter().map(|(_, f)| f).collect();
        match self.convert_findings(&milestone_id, &findings).await? {
            // validation_round findings never carry class=="command-assertion",
            // so convert_findings' escape-hatch guard makes this practically
            // unreachable here; handle it defensively rather than panic.
            FindingsConversion::Escalate { escalations, .. } => {
                let subjects = escalations
                    .iter()
                    .map(|e| e.subject.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.emit(EventKind::MilestoneBlocked {
                    milestone_id,
                    reason: format!(
                        "orchestrator marked finding(s) {subjects} as author-broken command \
                         assertions, but this validation round has none — escalating to \
                         operator rather than fixing or waiving."
                    ),
                })?;
            }
            FindingsConversion::Waive { waived } => {
                self.emit_waive_decision(&waived)?;
                let tag = self.tag_milestone(&milestone_id);
                // Structured human questions: same clear-on-complete as the
                // findings-empty path above.
                self.clear_open_questions("milestone completed", |q| {
                    q.milestone_id.as_deref() == Some(milestone_id.as_str())
                })?;
                self.emit(EventKind::MilestoneCompleted { milestone_id, tag })?;
            }
            FindingsConversion::Fix {
                specs,
                summary,
                text,
            } => {
                if self.fix_cycle_exhausted(mi) {
                    if self.escalate_or_block(&milestone_id)? {
                        self.emit_fix_features(mi, specs, &summary, text)?;
                        return Ok(());
                    }
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

    /// Confirm-on-pass for a LOCAL functional verdict (ticket
    /// `local-inference-validator-guarded`, KRZ-206b): re-run the functional
    /// validator on the FRONTIER tier — the injected claude backend with the
    /// same fallback config the untrusted-retry path uses — against the same
    /// milestone, contract, and engine-captured command evidence, in its own
    /// throwaway snapshot with the same before/after tripwires as any
    /// validator session.
    ///
    /// The comparison fails CLOSED: every frontier finding on a subject the
    /// local report passed is a recorded miss (the `validation.confirm`
    /// event — the local-vs-frontier miss-rate ground truth) and is returned
    /// for the round's findings, so the frontier verdict stands. A frontier
    /// finding on a subject the local report already failed is NOT a miss
    /// (both tiers fail it; the local FAIL was already trusted — failures
    /// are visible, misses are the danger).
    ///
    /// Returns `Ok(Some(disagreements))` when a trusted confirmation ran
    /// (an empty vec means the frontier tier agreed with every local PASS),
    /// `Ok(None)` when the round BLOCKED honestly: an untrusted confirmation
    /// never greens the gate — the local PASS simply has no verdict until a
    /// frontier session can judge it (mirroring the untrusted-after-retry
    /// block). Deliberately no grant-park or second retry here: the operator
    /// unblocks with a grant or guidance, and the re-validation re-runs both
    /// the local verdict and its confirmation.
    #[allow(clippy::too_many_arguments)]
    async fn confirm_local_functional_pass(
        &mut self,
        milestone_id: &str,
        role: Role,
        milestone: &Milestone,
        contract: &[Assertion],
        start_sha: &str,
        base_sha: Option<&str>,
        grants: &[String],
        egress_grants: &[String],
        worker_commands: &[String],
        contract_results: Option<&str>,
        local_run_id: &str,
        local_report: &ValidatorReport,
        passed_command_ids: &[String],
    ) -> Result<Option<Vec<(String, Finding)>>> {
        self.emit_decision(
            &format!(
                "local {} passed {} contract command assertion(s); running the frontier \
                 confirmation before any green (confirm-on-pass, KRZ-206b — a local PASS \
                 never greens the gate alone)",
                role_label(role),
                passed_command_ids.len()
            ),
            None,
        )?;
        let confirm_cfg = self.claude_fallback_cfg_for_role(role);
        let confirm_backend = Arc::clone(&self.backend);
        // The confirmation is a fresh validator session: its own throwaway
        // snapshot (the real checkout provably untouched by the local
        // primary — the isolation guarantees it, the tripwire verifies it)
        // and its own before/after tripwire pair.
        let fingerprint = validator_integrity::CheckoutFingerprint::capture(self.active_repo())?;
        let Some(snapshot) = self.validator_snapshot(milestone_id, role)? else {
            return Ok(None);
        };
        let session_cwd = snapshot.path().to_path_buf();
        // The confirmation runs on the injected claude backend — the one
        // backend that always honors the containment wrap.
        let validator_sandbox =
            self.validator_containment(role, BackendKind::Claude, &confirm_cfg, &session_cwd)?;
        let outcome = runner::run_validator_in(
            confirm_backend.as_ref(),
            &mut self.log,
            &self.paths,
            &confirm_cfg,
            role,
            milestone,
            contract,
            start_sha,
            None,
            &session_cwd,
            base_sha,
            grants,
            egress_grants,
            worker_commands,
            milestone.validator_guidance.as_deref(),
            contract_results,
            validator_sandbox,
        )
        .await;
        let caught = self.catch_up();
        let outcome = outcome?;
        caught?;

        // Same tripwires as the primary: any drift across the confirmation
        // session means the isolation itself failed — fail the round
        // honestly, before the verdict comparison.
        if self.fail_on_validator_tamper(milestone_id, role, &outcome.run_id, &fingerprint)? {
            return Ok(None);
        }
        if self.fail_on_snapshot_index_flags(
            milestone_id,
            role,
            &outcome.run_id,
            &snapshot,
            &fingerprint.head,
        )? {
            return Ok(None);
        }
        drop(snapshot);

        if !validator_outcome_trusted(&outcome) {
            let reason = format!(
                "frontier confirmation of the local {} PASS did not produce a trusted \
                 report ({}); the local verdict cannot green the gate unconfirmed",
                role_label(role),
                run_outcome_summary(&outcome)
            );
            self.emit_decision(&reason, None)?;
            self.emit(EventKind::MilestoneBlocked {
                milestone_id: milestone_id.to_string(),
                reason,
            })?;
            return Ok(None);
        }

        let confirm_report = outcome
            .validator_report
            .expect("trusted validator outcome must carry a report");
        let local_subjects: std::collections::HashSet<&str> = local_report
            .findings
            .iter()
            .map(|f| f.subject.as_str())
            .collect();
        // A miss is a frontier finding on a subject the local report did NOT
        // fail — the local tier passed it and the frontier tier caught it.
        let disagreements: Vec<Finding> = confirm_report
            .findings
            .into_iter()
            .filter(|f| !local_subjects.contains(f.subject.as_str()))
            .collect();
        let disagreement_subjects: std::collections::HashSet<&str> =
            disagreements.iter().map(|f| f.subject.as_str()).collect();
        let confirmed: Vec<String> = passed_command_ids
            .iter()
            .filter(|id| !disagreement_subjects.contains(id.as_str()))
            .cloned()
            .collect();
        if !disagreements.is_empty() {
            self.emit_decision(
                &format!(
                    "local validator MISS: the frontier confirmation overturned {} local \
                     PASS verdict(s) ({}) — failing closed to the frontier verdict; the \
                     miss is recorded on validation.confirm (the local-vs-frontier \
                     miss-rate ground truth)",
                    disagreements.len(),
                    disagreements
                        .iter()
                        .map(|f| f.subject.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None,
            )?;
        }
        self.emit(EventKind::ValidationConfirm {
            milestone_id: milestone_id.to_string(),
            local_run_id: local_run_id.to_string(),
            confirm_run_id: outcome.run_id.clone(),
            confirmed,
            disagreements: disagreements.clone(),
        })?;
        Ok(Some(
            disagreements
                .into_iter()
                .map(|f| (outcome.run_id.clone(), f))
                .collect(),
        ))
    }

    /// Mandatory validator containment resolution (ticket
    /// `validator-mandatory-containment`): the wrap every validator session
    /// gets regardless of the role's `sandbox.enforce` — the decision matrix
    /// lives in [`crate::sandbox::resolve_validator_containment`]. Surfaces
    /// the posture as an orchestrator decision per spawn: the LOUD
    /// degradation note when the platform or the selected backend cannot
    /// contain (deliberately NOT fail-closed — the ticket names that as a
    /// later operator decision; snapshot isolation plus the after-fingerprint
    /// tripwire still apply), and the positive note when the mandatory wrap
    /// contains a session whose `enforce: off` would previously have run
    /// bare. A resolution Err is the role's own fail-closed posture
    /// (enforcement requested but unhonorable here) — unchanged.
    fn validator_containment(
        &mut self,
        role: Role,
        kind: BackendKind,
        cfg: &MissionConfig,
        session_cwd: &std::path::Path,
    ) -> Result<Option<crate::sandbox::ResolvedSandbox>> {
        // The real checkout roots the validator must not read: the tree the
        // snapshot was taken from (the active tree — the integration
        // worktree in worktree mode), plus the primary checkout when they
        // differ (the snapshot lives under the primary's `.kranz`, so the
        // read-deny carve-outs keep it — and the shared git dir —
        // reachable).
        let mut deny_roots = vec![self.active_root().to_path_buf()];
        if !deny_roots.contains(&self.paths.repo_root) {
            deny_roots.push(self.paths.repo_root.clone());
        }
        let containment = crate::sandbox::resolve_validator_containment(
            &cfg.role(role).sandbox,
            kind,
            session_cwd,
            &self.paths.mission_dir(),
            &deny_roots,
        )?;
        match &containment.note {
            Some(note) => self.emit_decision(
                "validator session NOT sandbox-contained",
                Some(note.clone()),
            )?,
            None if containment.sandbox.is_some()
                && cfg.role(role).sandbox.enforce == crate::types::SandboxEnforce::Off =>
            {
                self.emit_decision(
                    "validator session sandbox-contained (mandatory)",
                    Some(format!(
                        "enforce:off no longer leaves the {} unwrapped: writes are limited to \
                         the throwaway snapshot plus the session-private scratch, the real \
                         checkout's source tree is read-denied (the shared git objects/refs \
                         the inspection needs stay readable), and mission metadata \
                         write-denies plus authority read-denies apply as they do to any \
                         session (ticket validator-mandatory-containment)",
                        role_label(role)
                    )),
                )?
            }
            None => {}
        }
        Ok(containment.sandbox)
    }

    /// Build the per-session validator snapshot (module
    /// [`crate::validator_snapshot`]) under the mission's gitignored `runs/`
    /// scratch and emit the `validation.snapshot` audit event (path,
    /// target-copy tier, creation cost). A creation failure BLOCKS the round
    /// honestly — decision + `milestone.blocked` naming the error — rather
    /// than falling back to the real checkout: this hardening exists
    /// precisely to keep validators out of it (fail-closed, mirroring
    /// `resolve_sandbox_or_refuse`). Returns `None` when the round blocked.
    fn validator_snapshot(
        &mut self,
        milestone_id: &str,
        role: Role,
    ) -> Result<Option<validator_snapshot::ValidatorSnapshot>> {
        let kind = match role {
            Role::ValidatorScrutiny => "scrutiny",
            Role::ValidatorFunctional => "functional",
            other => {
                return Err(EngineError::InvalidState(format!(
                    "validator snapshot requested for non-validator role {other:?}"
                )))
            }
        };
        let path = self
            .paths
            .runs_dir()
            .join(format!("validator-snapshot-{kind}"));
        match validator_snapshot::ValidatorSnapshot::create(self.active_repo(), &path) {
            Ok(snapshot) => {
                self.emit(EventKind::ValidationSnapshot {
                    milestone_id: milestone_id.to_string(),
                    role,
                    path: snapshot.path().display().to_string(),
                    target_tier: snapshot.target_tier().as_str().to_string(),
                    creation_ms: snapshot.creation().as_millis() as u64,
                    detail: snapshot.detail().map(str::to_string),
                })?;
                Ok(Some(snapshot))
            }
            Err(err) => {
                let reason = format!(
                    "could not create the {} snapshot ({err}); validators never run \
                     against the real checkout, so the round blocks honestly",
                    role_label(role)
                );
                self.emit_decision(&reason, None)?;
                self.emit(EventKind::MilestoneBlocked {
                    milestone_id: milestone_id.to_string(),
                    reason,
                })?;
                Ok(None)
            }
        }
    }

    /// The after-side of the validator tripwire (module
    /// [`validator_integrity`]): re-fingerprint the REAL session checkout
    /// after a validator session that ran in a throwaway snapshot
    /// ([`crate::validator_snapshot`]). With isolation in place the real
    /// checkout should be byte-identical across the session, so any drift
    /// now means the ISOLATION itself failed (a validator escaped its
    /// snapshot, or shared git refs were moved) — fail the round honestly:
    /// emit `validator.tamper` (recording WHAT changed) and block the
    /// milestone. Never a retry, never a finding the orchestrator's
    /// conversion turn could waive. Returns `true` when the round failed
    /// (caller returns immediately).
    fn fail_on_validator_tamper(
        &mut self,
        milestone_id: &str,
        role: Role,
        run_id: &str,
        before: &validator_integrity::CheckoutFingerprint,
    ) -> Result<bool> {
        let after = validator_integrity::CheckoutFingerprint::capture(self.active_repo())?;
        let Some(drift) = before.drift(&after) else {
            return Ok(false);
        };
        self.emit(EventKind::ValidatorTamper {
            milestone_id: milestone_id.to_string(),
            run_id: run_id.to_string(),
            role,
            head_before: drift.head_before.clone(),
            head_after: drift.head_after.clone(),
            appeared: drift.appeared.clone(),
            resolved: drift.resolved.clone(),
            git_metadata_changed: drift.git_metadata_changed,
            git_metadata_fields: drift.git_metadata_fields.clone(),
        })?;
        let reason = format!(
            "{} session escaped its snapshot: the REAL checkout drifted ({}); \
             the tripwire firing means the validator isolation itself failed, \
             so the round fails honestly",
            role_label(role),
            drift.summary()
        );
        self.emit_decision(&reason, None)?;
        self.emit(EventKind::MilestoneBlocked {
            milestone_id: milestone_id.to_string(),
            reason,
        })?;
        Ok(true)
    }

    /// The snapshot-side half of the tamper gate (4th-pass review):
    /// `skip-worktree`/`assume-unchanged` flags hide modifications from git
    /// while the files on disk still drive the verdict — and the snapshot's
    /// teardown erases the evidence. The snapshot builds with a fresh,
    /// flag-free index, so any flag present after the session was set by
    /// the validator: emit `validator.tamper` and block, same as a real-
    /// checkout tripwire drift. Returns `true` when the round failed.
    fn fail_on_snapshot_index_flags(
        &mut self,
        milestone_id: &str,
        role: Role,
        run_id: &str,
        snapshot: &validator_snapshot::ValidatorSnapshot,
        head: &str,
    ) -> Result<bool> {
        let validator_flags = snapshot.validator_set_index_flags()?;
        if validator_flags.is_empty() {
            return Ok(false);
        }
        self.emit(EventKind::ValidatorTamper {
            milestone_id: milestone_id.to_string(),
            run_id: run_id.to_string(),
            role,
            head_before: head.to_string(),
            head_after: head.to_string(),
            appeared: validator_flags.clone(),
            resolved: Vec::new(),
            git_metadata_changed: false,
            git_metadata_fields: Vec::new(),
        })?;
        let reason = format!(
            "{} session set skip-worktree/assume-unchanged flags in its \
             snapshot ({}); hidden modifications would corrupt the verdict, \
             so the round fails honestly",
            role_label(role),
            validator_flags.join(", ")
        );
        self.emit_decision(&reason, None)?;
        self.emit(EventKind::MilestoneBlocked {
            milestone_id: milestone_id.to_string(),
            reason,
        })?;
        Ok(true)
    }

    /// Engine-computed out-of-contract-write sweep (M7 tier 1, feature
    /// f-1-2): deterministic, read-only, no LLM validator involved. Compares
    /// worker-authored paths changed since `milestone_start_sha` against the
    /// mission's declared `touch_set`, and — in worktree mode — asserts the
    /// primary checkout stayed clean and on its original branch. Findings use
    /// `class = "out-of-contract-write"` and flow through the same
    /// `convert_findings` path as validator findings (see `final_gate` for
    /// the identical engine-synthesized-finding pattern).
    fn out_of_contract_sweep(&self, milestone_start_sha: &str) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();

        let touch_set = &self.state.mission.touch_set;
        let repo = self.active_repo();
        let commits = repo.commits_between(milestone_start_sha, "HEAD")?;
        let mission_id = self.state.mission.id.clone();

        // Attribute each changed path to the commit that made it, via a
        // per-commit diff against its own FIRST parent (see
        // `commit_changed_paths` — chaining consecutive range entries would
        // interleave merge parents and invent paths a commit never touched).
        // Engine/meta commits are skipped entirely so their paths never enter
        // the candidate set, even when outside the touch-set — but only when
        // the commit's own paths PROVE it is one: a subject template alone is
        // spoofable by a worker's `git commit` ("[kranz] mission report
        // cleanup"), so a template-subject commit touching anything beyond
        // mission-record metadata is swept like any other worker commit
        // (contract_sweep::is_meta_commit_with_paths).
        let mut changes: Vec<(String, CommitInfo)> = Vec::new();
        let mut worker_commit_count = 0usize;
        for commit in &commits {
            let paths = commit_changed_paths(repo, &commit.sha)?;
            if contract_sweep::is_meta_commit_with_paths(&commit.subject, &mission_id, &paths) {
                continue;
            }
            worker_commit_count += 1;
            for path in paths {
                if !contract_sweep::is_meta_path(&mission_id, &path) {
                    changes.push((path, commit.clone()));
                }
            }
        }

        if touch_set.is_empty() {
            // Advisory-off: do not emit a finding (that would force an extra
            // convert_findings turn and desync mock/scripted missions). Log
            // loudly when workers landed commits so operators still see the gap.
            if worker_commit_count > 0 {
                tracing::warn!(
                    worker_commits = worker_commit_count,
                    "out-of-contract-write path sweep is advisory-off: mission has no \
                     declared touchSet but worker commits landed"
                );
            } else {
                tracing::info!(
                    "out-of-contract-write path sweep is advisory-off: mission has no declared touchSet"
                );
            }
        } else {
            let attributed: Vec<contract_sweep::AttributedChange> = changes
                .iter()
                .map(|(path, commit)| contract_sweep::AttributedChange { path, commit })
                .collect();
            findings.extend(contract_sweep::path_findings(touch_set, &attributed));
        }

        // Primary-checkout cleanliness only asserts anything in worktree
        // mode: in checkout mode the primary IS the active repo, and it is
        // expected to be on the mission branch while work is in progress.
        if let Some(branch_at_start) = &self.primary_branch_at_start {
            // Tracked-only: the primary root always carries the engine's own
            // untracked mission housekeeping files (events.jsonl, state.json,
            // runs/, control/ — see paths.rs) regardless of worktree mode.
            // Those are gitignored in this repo but not guaranteed to be in
            // every host repo, so a full `is_clean` would false-positive on
            // ordinary engine operation; only a TRACKED change means a
            // worker/validator session actually wrote into the primary.
            let is_clean = self.repo.is_clean_tracked()?;
            let current_branch = self.repo.current_branch()?;
            if let Some(finding) =
                contract_sweep::primary_checkout_finding(is_clean, &current_branch, branch_at_start)
            {
                findings.push(finding);
            }
        }

        Ok(findings)
    }

    /// Annotated milestone tag; a pre-existing tag (milestone re-completed
    /// after final-gate fixes) downgrades to `None` rather than failing the
    /// mission.
    fn tag_milestone(&self, milestone_id: &str) -> Option<String> {
        let name = format!("kranz/{}/{}", self.state.mission.id, milestone_id);
        match self.active_repo().tag(&name, "kranz milestone complete") {
            Ok(()) => Some(name),
            Err(e) => {
                tracing::warn!(tag = %name, error = %e, "milestone tag failed; completing untagged");
                None
            }
        }
    }

    // -----------------------------------------------------------------------
    // Final contract gate (h)
    // -----------------------------------------------------------------------

    /// All milestones complete: run every `command` assertion ourselves and
    /// put `agent-judgement` assertions to the orchestrator. Failures become
    /// findings on the last milestone. Command-assertion findings are
    /// **non-waivable** (a RED cargo test must not become COMPLETE by model
    /// discretion) but remain **fixable** through [`Self::convert_findings`] —
    /// the orchestrator may emit fix features or, if the fix-cycle cap is
    /// spent, the mission blocks. Agent-judgement / synthesized findings may
    /// still be waived.
    /// Returns `Some(status)` to end `run()`, `None` to continue the loop.
    async fn final_gate(&mut self) -> Result<Option<MissionStatus>> {
        if self.state.mission.status != MissionStatus::Validating {
            self.emit(EventKind::MissionValidating {})?;
        }

        // Deterministic non-emptiness safety net (feature f-2-2): a mission
        // whose deliverable diff against the pinned base is empty (no
        // non-meta feature commits on the mission branch) must terminate
        // Failed, independent of and BEFORE any contract assertion — a green
        // contract can never override an empty deliverable. This runs
        // first, ahead of the command/agent-judgement assertions below.
        let base = match self.state.mission.base_sha.as_deref() {
            Some(sha) if !sha.is_empty() => sha.to_string(),
            _ => self.state.mission.base_branch.clone(),
        };
        // The meta exemption is path-verified, not subject-only: a worker
        // titling its commit "[kranz] mission report cleanup" while touching
        // real files must still count as a deliverable, or a forged subject
        // could hide worker writes from this gate (and desync it from the
        // path sweep, which applies the same check — see
        // contract_sweep::is_meta_commit_with_paths). Each commit is diffed
        // against its own FIRST parent (`commit_changed_paths`), never the
        // previous range entry — chaining interleaves merge parents and can
        // fail a genuine meta commit's path check, inflating the count.
        let commits = self.active_repo().commits_between(&base, "HEAD")?;
        let gate_mission_id = self.state.mission.id.clone();
        let mut non_meta_commit_count = 0usize;
        // The union of paths the mission diff touches — the `whenPaths`
        // scoping input for pack gates below (merge-gate idiom: a scoped
        // gate runs when at least one changed path sits under a prefix).
        let mut changed_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
        for commit in &commits {
            let paths = commit_changed_paths(self.active_repo(), &commit.sha)?;
            if !contract_sweep::is_meta_commit_with_paths(&commit.subject, &gate_mission_id, &paths)
            {
                non_meta_commit_count += 1;
            }
            changed_paths.extend(paths);
        }
        if non_meta_commit_count == 0 {
            // Same mission-end clear as complete_mission: no open question
            // may outlive the mission in the pending-decision projection.
            self.clear_open_questions("mission failed", |_| true)?;
            self.emit(EventKind::MissionFailed {
                reason: format!(
                    "no deliverable commits landed on the mission branch: \
                     {base}..HEAD contains 0 feature commits (only engine/meta \
                     commits). Refusing to COMPLETE on an empty deliverable diff."
                ),
            })?;
            return Ok(Some(MissionStatus::Failed));
        }

        let contract = self.state.mission.validation_contract.clone();
        let mut findings: Vec<Finding> = Vec::new();
        // agent-env-clear: command assertions run with a CLEARED environment
        // (minimal allowlist + scratch HOME + toolchain caches + any
        // contractEnvPassthrough names) — ambient secrets never reach them.
        let gate_base_sha = self.state.mission.base_sha.clone();
        let env = self.contract_command_env(gate_base_sha.as_deref())?;
        // engine-gates-sandbox-wrapped: the same commands then run under the
        // resolved worker sandbox profile (a no-op Disabled wrap when
        // enforce == off). Resolved ONCE for the whole final gate — every
        // assertion below shares the gate tree + contract scratch shape.
        let gate_root = self.active_root().to_path_buf();
        let gate_sandbox = self.gate_sandbox(&gate_root)?;

        // command assertions — engine-run (design.md: the hard gate).
        for assertion in contract
            .iter()
            .filter(|a| a.check == AssertionCheck::Command)
        {
            let Some(command) = assertion.command.as_deref() else {
                findings.push(Finding {
                    subject: assertion.id.clone(),
                    severity: "critical".to_string(),
                    evidence: "assertion has check=command but no command".to_string(),
                    suggested_fix: String::new(),
                    class: String::new(),
                });
                continue;
            };
            let (ok, output) =
                run_shell_command_sandboxed(&gate_root, command, &env, &gate_sandbox).await;
            if !ok {
                findings.push(Finding {
                    subject: assertion.id.clone(),
                    severity: "critical".to_string(),
                    evidence: scrub::scrub(&format!("command failed: {command}\n{output}")),
                    suggested_fix: String::new(),
                    class: "command-assertion".to_string(),
                });
            }
        }

        // Named contract gates at the final-gate surface (ticket
        // contract-validation-gates.md): the static, deterministic classes
        // (vacuous-filter, wrong-polarity, env-sensitive) re-checked against
        // the ACTIVE tree — e.g. a negated grep whose target is still absent
        // here passed vacuously, and its green contributes nothing. The
        // passes-on-base gate is absent: the work has landed, so passing on
        // base is now expected. Advisory only, same posture as at approval:
        // the command outcomes and findings above are unchanged — this
        // records the named verdicts so a vacuously-green contract is
        // visible in the event log instead of silently trusted.
        //
        // ONE shared pipeline (ticket pack-contract-gates-prompts): the
        // engine floor gates register FIRST and the configured pack's
        // deterministic gates register AFTER — registration order is the
        // evaluation order within the deterministic section (gate.rs), so a
        // pack can add to the floor but never precede, displace, or replace
        // it (a pack gate named like a floor gate was already refused at
        // load). Pack gate commands run FIRST, before the pipeline exists:
        // Gate::evaluate is synchronous (and the pipeline is not Send, so it
        // must never be held across an await) while the engine's bounded
        // shell runner is async — each gate captures its command's outcome
        // (same cleared contract env and active root as the contract
        // assertions above) and the pipeline still owns ordering and
        // reporting — see pack.rs's module docs. Same advisory posture as
        // the floor: a failing pack gate is recorded, never blocking.
        let pack = crate::pack::load_for_config(&self.state.config, &self.paths.repo_root)
            .map_err(EngineError::Config)?;
        let mut pack_gates: Vec<crate::pack::PackGate> = Vec::new();
        if let Some(pack) = &pack {
            let changed_paths: Vec<String> = changed_paths.into_iter().collect();
            for decl in pack.gates_for_paths(&changed_paths) {
                let (ok, output) =
                    run_shell_command_sandboxed(&gate_root, &decl.command, &env, &gate_sandbox)
                        .await;
                pack_gates.push(crate::pack::PackGate::from_run(
                    &decl.name,
                    &decl.command,
                    ok,
                    output,
                ));
            }
        }
        // The pipeline is scoped to this block: it is not Send (Box<dyn
        // Gate>), so it must be fully dropped before the next await below.
        let (floor_reports, pack_reports) = {
            let mut pipeline = crate::gate::GatePipeline::new();
            contract_gates::register_contract_gates(
                &mut pipeline,
                &contract,
                None,
                self.active_root(),
            );
            let floor_gate_count = pipeline.len();
            for gate in pack_gates {
                pipeline.register(Box::new(gate));
            }
            let final_gate_reports = pipeline.evaluate();
            let (floor, pack) = final_gate_reports.split_at(floor_gate_count);
            (floor.to_vec(), pack.to_vec())
        };
        // First-class gate results (ticket gate-results-first-class-events,
        // KRZ-312): one gate.result event per evaluation, recorded for the
        // WHOLE ladder — floor then pack, concatenated back into pipeline
        // (registration) order so the per-section indices the helper assigns
        // run continuously across both. Unconditional, pass or fail: a
        // gate's silent green is exactly as invisible as its failure, and
        // replay reconstructs the ladder from these events alone. Record-
        // only; the advisory posture of both floors is unchanged.
        let ladder: Vec<crate::gate::GateReport> = floor_reports
            .iter()
            .chain(pack_reports.iter())
            .cloned()
            .collect();
        for kind in gate_results::gate_result_events(crate::gate::GateSurface::FinalGate, &ladder) {
            self.emit(kind)?;
        }
        let failed_gates = contract_gates::failed_gate_names(&floor_reports);
        if !failed_gates.is_empty() {
            self.emit_decision(
                &format!(
                    "contract gates (final gate): named gate(s) failed: {} — advisory only; \
                     command outcomes and findings above are unchanged",
                    failed_gates.join(", ")
                ),
                Some(contract_gates::render_gate_verdicts(&floor_reports)),
            )?;
        }
        // The pack's verdicts are recorded whenever a configured pack had
        // applicable gates — pass or fail, since a pack's silent green is
        // exactly as invisible as its failure would be. No pack ⇒ no
        // decision ⇒ byte-identical behavior.
        if let Some(pack) = &pack {
            if !pack_reports.is_empty() {
                let failed_pack = contract_gates::failed_gate_names(&pack_reports);
                let summary = if failed_pack.is_empty() {
                    format!(
                        "pack `{}` gates (final gate): {} deterministic gate(s) passed — advisory only",
                        pack.name,
                        pack_reports.len()
                    )
                } else {
                    format!(
                        "pack `{}` gates (final gate): named gate(s) failed: {} — advisory only; \
                         command outcomes and findings above are unchanged",
                        pack.name,
                        failed_pack.join(", ")
                    )
                };
                self.emit_decision(
                    &summary,
                    Some(contract_gates::render_verdict_block(
                        &format!("pack `{}` gates:", pack.name),
                        &pack_reports,
                    )),
                )?;
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
            self.complete_mission().await?;
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

        // Command assertions are non-waivable but still fixable: send every
        // finding through convert_findings, then refuse an all-waive that
        // covers any command-assertion subject (synthesize fixes instead).
        let command_subjects: std::collections::HashSet<String> = findings
            .iter()
            .filter(|f| f.class == "command-assertion")
            .map(|f| f.subject.clone())
            .collect();
        let all_findings = findings;
        match self
            .convert_findings(&last_milestone_id, &all_findings)
            .await?
        {
            FindingsConversion::Escalate { escalations, text } => {
                let subjects = escalations
                    .iter()
                    .map(|e| e.subject.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let detail = escalations
                    .iter()
                    .map(|e| {
                        let evidence = all_findings
                            .iter()
                            .find(|f| f.subject == e.subject)
                            .map(|f| f.evidence.as_str())
                            .unwrap_or("");
                        format!("- {}: {}\n  evidence: {}", e.subject, e.diagnosis, evidence)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.emit_decision(
                    &format!(
                        "final-gate command assertion(s) {subjects} appear author-broken \
                         (false negative); escalating to the operator with evidence attached"
                    ),
                    Some(format!("{detail}\n\n{text}")),
                )?;
                self.emit(EventKind::MilestoneBlocked {
                    milestone_id: last_milestone_id,
                    reason: format!(
                        "contract command assertion(s) {subjects} appear buggy (false \
                         negative) — command still fails but the requirement is verified met; \
                         evidence attached. Escalating to operator (gate-repair is a human \
                         decision, not a fix cycle)."
                    ),
                })?;
                Ok(None)
            }
            FindingsConversion::Waive { waived }
                if waived
                    .iter()
                    .all(|w| !command_subjects.contains(w.subject.as_str())) =>
            {
                self.emit_waive_decision(&waived)?;
                // Report AFTER the waive decision (so the gate waiver is in
                // the replayed history) and BEFORE mission.completed.
                self.complete_mission().await?;
                Ok(Some(MissionStatus::Complete))
            }
            FindingsConversion::Waive { waived } => {
                // Model waived a command assertion — refuse. Fix every
                // command-classified finding the waive covered (and any
                // other unwaived remainder is already handled by convert
                // synthesizing; here the waive emptied the set, so rebuild
                // from command findings only).
                let refuse_note = waived
                    .iter()
                    .filter(|w| command_subjects.contains(w.subject.as_str()))
                    .map(|w| w.subject.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.emit_decision(
                    &format!(
                        "refused waive of final-gate command assertion(s): {refuse_note}; synthesizing fix feature(s)"
                    ),
                    Some(
                        waived
                            .iter()
                            .map(|w| format!("- {}: {}", w.subject, w.reason))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                )?;
                let command_only: Vec<&Finding> = all_findings
                    .iter()
                    .filter(|f| command_subjects.contains(&f.subject))
                    .collect();
                let specs = synthesize_fix_specs(command_only);
                if self.fix_cycle_exhausted(li) && !self.escalate_or_block(&last_milestone_id)? {
                    self.emit(EventKind::MilestoneBlocked {
                        milestone_id: last_milestone_id,
                        reason: format!(
                            "{} final-gate command assertion(s) failed but the fix-cycle cap ({}) is reached",
                            specs.len(),
                            self.state.config.max_fix_cycles_per_milestone
                        ),
                    })?;
                    return Ok(None);
                }
                self.emit(EventKind::MilestoneValidating {
                    milestone_id: last_milestone_id,
                })?;
                self.emit_fix_features(
                    li,
                    specs,
                    &format!("fix non-waivable command assertion(s): {refuse_note}"),
                    refuse_note,
                )?;
                Ok(None)
            }
            FindingsConversion::Fix {
                specs,
                summary,
                text,
            } => {
                if self.fix_cycle_exhausted(li) && !self.escalate_or_block(&last_milestone_id)? {
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
                            all_findings.len(),
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

    // -----------------------------------------------------------------------
    // Completion report (roadmap M1)
    // -----------------------------------------------------------------------

    /// Complete the mission: capture at most one cross-mission lesson, note
    /// whether one was captured (or NONE) on the event feed, then write and
    /// commit the completion report — with the lesson files (if any) folded
    /// into the SAME report commit — and finally emit `mission.completed`.
    ///
    /// Both callers (findings-empty and all-waived at the final gate) must
    /// go through this single path so the capture turn runs exactly once,
    /// only at completion. Every step here is best-effort: a capture or
    /// commit failure must never prevent `mission.completed` from being
    /// emitted for a mission that already passed its final gate.
    async fn complete_mission(&mut self) -> Result<()> {
        let lesson_paths = self.capture_lesson().await;
        match &lesson_paths {
            Some(paths) => self.emit_decision(
                &format!("cross-mission lesson captured ({} file(s))", paths.len()),
                None,
            )?,
            None => self.emit_decision("no cross-mission lesson captured", None)?,
        }
        self.write_mission_report(lesson_paths);
        // Structured human questions (ticket
        // structured-human-question-events): questions do not park the run
        // loop, so an ask can still be open here — a completed mission makes
        // every one moot. Clear them so the projection never shows a
        // "your move" on a mission that can no longer act on it.
        self.clear_open_questions("mission completed", |_| true)?;
        self.emit(EventKind::MissionCompleted {})?;
        Ok(())
    }

    /// Write, commit, and index the mission completion report.
    ///
    /// Best-effort BY DESIGN: the report is derived data, regenerable from
    /// the event log at any time, so a render/write/git failure here must
    /// never strand a mission that just passed its final gate — every error
    /// is downgraded to a warning and the caller proceeds to emit
    /// `mission.completed` regardless. `extra_paths` (e.g. a captured lesson
    /// + its index) are folded into the same report commit when present.
    fn write_mission_report(&mut self, extra_paths: Option<Vec<PathBuf>>) {
        if let Err(e) = self.try_write_mission_report(extra_paths) {
            tracing::warn!(error = %e, "mission report failed; completing the mission without it");
        }
    }

    /// Fallible body of [`Self::write_mission_report`]: render `report.md`
    /// from the (flushed) event log, write it beside plan.md, add a report
    /// link to this mission's line in `missions/index.md`, and commit both
    /// (plus any `extra_paths`) in one `[kranz] mission report for <id>`
    /// commit.
    ///
    /// Worktree mode (M7 tier 1): this runs inside `run()`, so `active_paths`/
    /// `active_repo` already route to the integration worktree — the report,
    /// index update, and any extra paths (e.g. a captured lesson) are written
    /// and committed there, never in the primary tree. A human-readable
    /// report.md twin is also written (untracked) to the primary runtime dir
    /// so it stays readable without leaving the primary checkout.
    fn try_write_mission_report(&mut self, extra_paths: Option<Vec<PathBuf>>) -> Result<()> {
        // Flush buffered stream deltas so the replayed history is complete.
        self.log.flush()?;
        let events = EventLog::read_events(&self.paths.events_file())?;
        let plan: Plan = serde_json::from_str(&self.plan_json()?)?;
        // Prefer the estimate persisted at approval so "estimated vs actual"
        // compares against the exact number the operator approved (M1). Missions
        // approved before estimate.json existed fall back to a calibrated
        // recompute (still better than the old default-params number).
        let estimate = std::fs::read_to_string(self.paths.estimate_file())
            .ok()
            .and_then(|s| serde_json::from_str::<cost::CostEstimate>(&s).ok())
            .unwrap_or_else(|| {
                let calibration = cost::calibrate(&self.paths.repo_root);
                cost::apply_shape(
                    cost::estimate(&plan, &self.state.config, &calibration.params),
                    &plan,
                    &calibration,
                )
            });
        // Workspace contract presence line (D-H): read from the repo root
        // (base-branch-owned). Approval already validated it, so a load or
        // parse failure here (e.g. edited invalid mid-mission) must not fail
        // report writing — degrade to the "no workspace contract" line.
        let workspace_contract =
            crate::workspace_contract::load_workspace_contract(&self.paths.repo_root)
                .ok()
                .flatten();
        let report = render_mission_report(
            &self.state,
            &events,
            &plan,
            &estimate,
            self.active_root(),
            workspace_contract.as_ref(),
        );

        let active_paths = self.active_paths();
        let report_file = active_paths.mission_dir().join("report.md");
        if let Some(parent) = report_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&report_file, &report)?;

        // Index line: append " · [report](<id>/report.md)" to this mission's
        // entry; the line format is otherwise kept stable (see
        // upsert_mission_index). A missing index or line is tolerated — the
        // report itself is the deliverable.
        let index = active_paths.missions_dir().join("index.md");
        let mut commit: Vec<&std::path::Path> = vec![report_file.as_path()];
        let index_changed = match std::fs::read_to_string(&index) {
            Ok(existing) => {
                let updated = mark_mission_index_report(&existing, &self.state.mission.id);
                let changed = updated != existing;
                if changed {
                    std::fs::write(&index, updated)?;
                }
                changed
            }
            Err(_) => false,
        };
        if index_changed {
            commit.push(index.as_path());
        }
        let extra_paths = extra_paths.unwrap_or_default();
        commit.extend(extra_paths.iter().map(PathBuf::as_path));
        let metadata = KranzCommitMetadata {
            mission_id: self.state.mission.id.clone(),
            cost_usd: self.state.total_cost_usd,
            tokens: self.state.totals.clone(),
        };
        let message = with_kranz_trailers(
            &format!("[kranz] mission report for {}", self.state.mission.id),
            &metadata,
        );
        self.active_repo().commit_paths(&commit, &message)?;

        if self.state.config.isolation() == WorkerIsolation::Worktree {
            let primary_report = self.paths.mission_dir().join("report.md");
            if let Some(parent) = primary_report.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&primary_report, &report)?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Planning-seed context (lessons + knowledge)
    // -----------------------------------------------------------------------

    /// Provenance-filtered lessons MANIFEST for a planning seed: only lessons
    /// whose file was ADDED (in the current branch's reachable history) by a
    /// `[kranz] mission report` commit carrying a matching `Kranz-Mission`
    /// trailer reach the prompt. This keeps a worker-dropped or otherwise
    /// arbitrary file in `.kranz/lessons/` from injecting text into a future
    /// planner. Bodies are no longer inlined here (see the ticket
    /// lessons-manifest-body-split); a mechanically pre-selected few arrive
    /// through a separate path. The git check runs per listed lesson, bounded
    /// to the manifest cap — negligible at planning frequency.
    fn render_lessons_for_planning(&self) -> Option<String> {
        let repo = &self.repo;
        crate::lessons::render_lessons_manifest(&self.paths.repo_root, &|filename: &str| {
            lesson_provenance_clean(repo, filename)
        })
    }

    /// Ranked ≤4 KiB `docs/knowledge/` block for planning / revised-planning
    /// seeds (slice 2 / D-C). Separate budget from lessons. Missing vault →
    /// `None` (planning continues).
    pub(crate) fn render_knowledge_for_planning(&self) -> Option<String> {
        let ticket_body = Ticket::slug_for_mission(&self.paths.repo_root, &self.state.mission.id)
            .and_then(|slug| {
                std::fs::read_to_string(Ticket::md_path(&self.paths.repo_root, &slug)).ok()
            });
        let changed = self.knowledge_changed_files();
        knowledge::render_knowledge_for_planning(
            &self.paths.repo_root,
            &KnowledgeQuery {
                goal: &self.state.mission.goal,
                ticket_body: ticket_body.as_deref(),
                touch_hints: &self.state.mission.touch_set,
                changed_files: &changed,
            },
        )
    }

    /// Best-effort `base_sha..HEAD` path list for knowledge tier-3 overlap.
    /// Empty during early planning (no base pin yet) or on git errors.
    fn knowledge_changed_files(&self) -> Vec<String> {
        let Some(base) = self.state.mission.base_sha.as_deref() else {
            return Vec::new();
        };
        let Ok(head) = self.repo.head_sha() else {
            return Vec::new();
        };
        self.repo.changed_paths(base, &head).unwrap_or_default()
    }

    /// Append knowledge (then lessons) onto a planning seed. Order and
    /// separate budgets are load-bearing (ticket
    /// repo-knowledge-ranked-brief-injection).
    fn append_planning_context(&self, seed: &mut String) {
        if let Some(block) = self.render_knowledge_for_planning() {
            seed.push_str("\n\n");
            seed.push_str(&block);
        }
        if let Some(index) = self.render_lessons_for_planning() {
            seed.push_str("\n\n");
            seed.push_str(&index);
        }
    }

    // -----------------------------------------------------------------------
    // Orchestrator session management (i)
    // -----------------------------------------------------------------------

    /// One orchestrator turn with re-seed resilience: ensure the session,
    /// send the digest-prefixed message, pump to the turn's `Result`. If the
    /// session dies mid-turn, re-seed once and retry; two consecutive
    /// failures → [`EngineError::Backend`].
    pub(crate) async fn orch_turn(&mut self, message: &str) -> Result<String> {
        if self.state.config.backend_kind(Role::Orchestrator) != BackendKind::Claude {
            return self.orch_single_shot_turn(message).await;
        }

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

    /// One orchestrator turn through a single-shot backend (Codex/Droid).
    ///
    /// The default Claude path remains the long-lived streaming session above.
    /// Non-Claude backends do not support `send_user_message`, so each
    /// orchestrator turn is a fresh single-shot session grounded by the same
    /// digest the streaming path prepends to every turn.
    async fn orch_single_shot_turn(&mut self, message: &str) -> Result<String> {
        let selected = self.select_backend(Role::Orchestrator);
        if let Some(reason) = selected.fallback_reason.as_deref() {
            self.emit_decision(reason, None)?;
        }
        let backend = Arc::clone(&selected.backend);
        let cfg = selected.cfg;
        let role_cfg = cfg.role(Role::Orchestrator).clone();

        let mut vars: HashMap<&str, String> = HashMap::new();
        vars.insert(
            "turnBudget",
            cfg.worker
                .max_turns
                .map(|n| n.to_string())
                .unwrap_or_else(|| "a reasonable number of".to_string()),
        );
        let system_prompt = prompts::render(prompts::text(Role::Orchestrator), &vars);

        let prompt = if self.state.mission.status == MissionStatus::Planning {
            let mut seed = format!(
                "MISSION GOAL:\n{}\n\nYou are in the planning phase. Interrogate the \
                 goal and the repository (read-only), ask the user sharp questions if \
                 anything material is ambiguous, then propose the validation contract, \
                 milestones and features. Do not emit the plan JSON until asked.",
                self.state.mission.goal
            );
            self.append_planning_context(&mut seed);
            format!("{seed}\n\nUSER TURN:\n{message}")
        } else {
            format!("{}\n\n{}", digest::render(&self.state), message)
        };

        let mut spec = SessionSpec {
            cwd: self.paths.repo_root.clone(),
            prompt: PromptMode::SingleShot(prompt),
            append_system_prompt: Some(system_prompt),
            model: role_cfg.model.clone(),
            effort: role_cfg.reasoning_effort.clone(),
            session_id: uuid::Uuid::new_v4().to_string(),
            resume: None,
            permission_mode: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            tools: cfg.role(Role::Orchestrator).tools.clone(),
            writable: false,
            settings_json: None,
            json_schema: None,
            max_budget_usd: role_cfg.max_budget_usd,
            max_turns: role_cfg.max_turns,
            env: HashMap::new(),
            sandbox: None,
        };
        permissions::apply(
            permissions::for_role(Role::Orchestrator, &cfg, &[], &[], &[]),
            &mut spec,
        );

        let orch_count = self
            .state
            .runs
            .values()
            .filter(|r| r.role == Role::Orchestrator)
            .count();
        let run_id = format!("orch-{}", orch_count + 1);
        let run_meta = runner::RunMeta {
            run_id,
            role: Role::Orchestrator,
            feature_id: None,
            milestone_id: None,
            model: role_cfg.model,
            prompt_hash: prompts::hash(Role::Orchestrator),
        };
        let outcome = runner::run_session(
            backend.as_ref(),
            spec,
            &mut self.log,
            &self.paths,
            run_meta,
            None,
        )
        .await;
        let caught = self.catch_up();
        let outcome = outcome?;
        caught?;
        if outcome.result == RunResult::Fail {
            return Err(EngineError::Backend(format!(
                "orchestrator single-shot turn failed: {}",
                outcome.final_text
            )));
        }
        Ok(outcome.final_text)
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
            let mut seed = format!(
                "MISSION GOAL:\n{}\n\nYou are in the planning phase. Interrogate the \
                 goal and the repository (read-only), ask the user sharp questions if \
                 anything material is ambiguous, then propose the validation contract, \
                 milestones and features. Do not emit the plan JSON until asked.",
                self.state.mission.goal
            );
            self.append_planning_context(&mut seed);
            (seed, None)
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
                    let mut seed = format!(
                        "MISSION GOAL:\n{}\n\nYou are in the planning phase; a previous \
                         planning conversation was lost. Re-establish context from the \
                         repository (read-only), then continue shaping the validation \
                         contract, milestones and features with the user. Do not emit \
                         the plan JSON until asked.",
                        self.state.mission.goal
                    );
                    self.append_planning_context(&mut seed);
                    seed
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
            tools: cfg.role(Role::Orchestrator).tools.clone(),
            writable: false,
            settings_json: None,
            json_schema: None,
            max_budget_usd: role_cfg.max_budget_usd,
            max_turns: role_cfg.max_turns,
            env: HashMap::new(),
            sandbox: None,
        };
        permissions::apply(
            permissions::for_role(Role::Orchestrator, &cfg, &[], &[], &[]),
            &mut spec,
        );

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
            candidate: None,
            sdk_session_id: sdk_session_id.clone(),
            model: role_cfg.model,
            quant: "n/a".to_string(),
            weight_hash: None,
            prompt_hash: prompts::hash(Role::Orchestrator),
            transcript_path: MissionPaths::transcript_rel(&run_id),
        })?;

        self.orch = Some(session);
        self.orch_session_id = Some(sdk_session_id);
        self.orch_run_id = Some(run_id);
        self.orch_transcript = Some(transcript);

        // The seed is a full turn (the backend sends the streaming initial
        // prompt as the first user message); consume its Result so every
        // later send/pump pair stays aligned. The reply is captured (already
        // scrubbed by pump_turn) rather than discarded: the planning seed's
        // answer routinely ends with questions the user must see.
        match self.pump_turn().await {
            Ok(ack) => {
                if !ack.trim().is_empty() {
                    self.pending_seed_reply = Some(ack);
                }
                Ok(())
            }
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
                AgentEvent::Result {
                    text,
                    is_error,
                    usage,
                    cost_usd,
                    ..
                } => {
                    // Per-turn accounting: streaming sessions emit one Result
                    // per injected turn (design.md), so each becomes one
                    // worker.completed carrying that turn's usage — totals
                    // accumulate in the reducer.
                    self.emit(EventKind::WorkerCompleted {
                        run_id: run_id.clone(),
                        result: if is_error {
                            RunResult::Fail
                        } else {
                            RunResult::Pass
                        },
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
                    let turn_text = if text.trim().is_empty() {
                        texts.join("\n")
                    } else {
                        text
                    };
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
            AgentEvent::ToolUse { tool, summary, .. } => ("tool-use", format!("{tool}: {summary}")),
            AgentEvent::ToolResult {
                tool,
                denied,
                summary,
                ..
            } => {
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
                    considered_alternatives: None,
                    command_grants: mission.command_grants.clone(),
                    touch_set: mission.touch_set.clone(),
                };
                Ok(serde_json::to_string_pretty(&plan)?)
            }
        }
    }
}

fn validator_outcome_trusted(outcome: &runner::RunOutcome) -> bool {
    outcome.result == RunResult::Pass && outcome.validator_report.is_some()
}

pub(crate) fn run_outcome_summary(outcome: &runner::RunOutcome) -> String {
    format!(
        "result={:?}, exit={}, deniedToolResults={}",
        outcome.result,
        session_exit_summary(&outcome.exit),
        outcome.denied_count
    )
}

fn session_exit_summary(exit: &SessionExit) -> String {
    match exit {
        SessionExit::Completed => "completed".to_string(),
        SessionExit::Aborted => "aborted".to_string(),
        SessionExit::Failed(message) => format!("failed: {}", tail_chars(message, 240)),
    }
}

/// One feature's slot in a parallel batch (roadmap M3): the feature it runs,
/// its per-feature branch, and the worktree directory that branch is checked
/// out in. Built up front so the cleanup guard can always find every worktree.
struct ParallelWorkspace {
    feature_id: String,
    /// Per-feature branch (`kranz/wt/<mission>/<feature>`), off the milestone
    /// start sha, merged into the mission branch on success.
    branch: String,
    /// Absolute worktree directory the branch is checked out in.
    path: PathBuf,
}

/// How one parallel worktree's Phase C ended (12th-pass review). The merge
/// loop treats every non-`Ready` variant as "fail the feature", but an
/// inspection failure additionally PRESERVES the worktree + branch — the
/// cleanup guard must not reap bytes the checkpoint never verified.
enum WorktreeDisposition {
    /// Judged complete: merge the branch.
    Ready,
    /// Not ready to merge (a secret-scan policy refusal or a non-complete
    /// judgement): the feature fails and the cleanup guard reaps as before.
    NotReady,
    /// The worktree could not be opened, inspected, or queried: the feature
    /// fails AND its index lands in the batch's `preserve` set, so the
    /// cleanup guard keeps the worktree dir and branch for human inspection.
    InspectionFailed,
}

/// Result of one buffered parallel worker session (roadmap M3): the event
/// kinds it collected (to be replayed by the engine's single writer) plus its
/// [`runner::RunOutcome`], or the error that aborted the session.
type BufferedRunResult = Result<(Vec<EventKind>, runner::RunOutcome)>;

/// One candidate stream's slot in a dispatch pool (KRZ-303): the configured
/// backend/model pairing, its branch, and the worktree directory that branch
/// is checked out in. Mirrors [`ParallelWorkspace`] with one deliberate
/// difference: pool branches (`kranz/pool/<mission>/<feature>-c<index>`) are
/// NEVER deleted by the engine — they are the candidate deliverables a
/// judging human inspects; only the worktree dirs are reaped. The candidate
/// index is the slot's position in the `workspaces` vec itself (built in
/// `workerCandidates` order), so it is not duplicated here.
struct PoolWorkspace {
    /// Per-candidate branch, off the mission branch tip at dispatch.
    branch: String,
    /// Absolute worktree directory the branch is checked out in.
    path: PathBuf,
    /// The configured candidate this stream runs.
    spec: CandidateSpec,
}

/// Tracks how many parallel worker sessions were live at once (roadmap M3),
/// so the batch can prove real wall-clock overlap. Cheap and lock-free: each
/// session bumps the live count on entry and records the running peak, then
/// decrements on exit. Cloning shares the same counters (an `Arc` inside).
#[derive(Clone)]
struct ConcurrencyTracker {
    live: Arc<std::sync::atomic::AtomicUsize>,
    peak: Arc<std::sync::atomic::AtomicUsize>,
}

/// RAII guard: a live session while held; decrements the live count on drop.
struct ConcurrencyGuard {
    live: Arc<std::sync::atomic::AtomicUsize>,
}

impl ConcurrencyTracker {
    fn new() -> Self {
        ConcurrencyTracker {
            live: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            peak: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Mark a session live for the returned guard's lifetime, updating the peak.
    fn enter(&self) -> ConcurrencyGuard {
        use std::sync::atomic::Ordering;
        let now = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        ConcurrencyGuard {
            live: Arc::clone(&self.live),
        }
    }

    /// The greatest number of sessions ever live simultaneously.
    fn peak(&self) -> usize {
        self.peak.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for ConcurrencyGuard {
    fn drop(&mut self) {
        self.live.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Infix marking a conflict-RESOLUTION fix-feature id (`<ms>-conflict-<n>`).
/// A feature whose id already contains this must never spawn ANOTHER
/// resolution — the guard against an infinite conflict→resolution chain.
const CONFLICT_INFIX: &str = "-conflict-";

/// Synthesize the conflict-RESOLUTION fix-feature for a parallel-merge
/// conflict (roadmap M3). When a per-feature branch fails to merge, its own
/// commits are discarded (the branch is thrown away by the cleanup guard) and
/// the feature is FAILED — but the work still needs doing on top of the
/// now-merged mission branch. This builds the resolution feature that redoes
/// it: a Fix-origin, Pending feature the sequential loop picks up on the next
/// iteration (no worktree, straight on the mission branch, so it cannot
/// conflict again).
///
/// - Id shape `<milestone_id>-conflict-<n>`, where `n` is 1 + the count of
///   features on the milestone whose id already contains [`CONFLICT_INFIX`]
///   (namespaced so repeated conflicts in one batch never collide, mirroring
///   the replan-id fix).
/// - Spec carries the ORIGINAL feature's title and spec, the conflicting file
///   list, and a note that earlier features in this milestone already merged
///   (so the worker redoes the work COMPATIBLY on the current branch).
///
/// Returns `None` — the infinite-chain guard — when `original.id` already
/// contains [`CONFLICT_INFIX`]: a resolution feature that itself conflicts
/// must NOT spawn a resolution-of-a-resolution. (In practice only Plan-origin
/// `f-<m>-<n>` features enter a parallel batch, so the guard is belt-and-
/// braces; it is enforced here so the property holds wherever this is called.)
///
/// Pure and deterministic; the caller scrubs at the emit boundary as usual.
pub fn synthesize_conflict_resolution(
    milestone_id: &str,
    original: &Feature,
    conflict_files: &[String],
    existing_features: &[Feature],
) -> Option<Feature> {
    if original.id.contains(CONFLICT_INFIX) {
        return None;
    }
    let n = existing_features
        .iter()
        .filter(|f| f.id.contains(CONFLICT_INFIX))
        .count()
        + 1;
    let files = if conflict_files.is_empty() {
        "(git named no specific files)".to_string()
    } else {
        conflict_files.join(", ")
    };
    let spec = format!(
        "Re-implement the feature \"{title}\" ON TOP OF the current mission branch, which \
         already contains the other features from this milestone that merged first. The \
         original attempt ran in an isolated worktree and its branch FAILED to merge back \
         (conflicting files: {files}); those commits were discarded. Redo the work \
         compatibly with what is now on the branch — read the current state of the \
         conflicting files first, then apply the change so it no longer conflicts.\n\n\
         ORIGINAL FEATURE SPEC:\n{spec}",
        title = original.title.trim(),
        spec = original.spec.trim(),
    );
    Some(Feature {
        id: format!("{milestone_id}{CONFLICT_INFIX}{n}"),
        title: format!("Resolve merge conflict: {}", original.title.trim()),
        spec,
        validation_criteria: original.validation_criteria.clone(),
        origin: FeatureOrigin::Fix,
        status: FeatureStatus::Pending,
        worker_runs: Vec::new(),
        commits: Vec::new(),
        respawns: 0,
    })
}

/// Absolute worktree directory for one feature of one mission (roadmap M3).
/// Lives under the system temp dir — OUTSIDE the repo working tree, so a
/// worktree is never mistaken for mission content — namespaced by mission +
/// feature so concurrent batches never collide.
fn parallel_worktree_path(
    repo_root: &std::path::Path,
    mission_id: &str,
    feature_id: &str,
) -> PathBuf {
    // Feature ids are `f-<m>-<n>` / `ms-<id>-...` — filesystem-safe already,
    // but replace anything unexpected defensively.
    let safe: String = feature_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    std::env::temp_dir().join(format!(
        "kranz-wt-{}-{mission_id}-{safe}",
        repo_worktree_namespace(repo_root)
    ))
}

/// Absolute directory for one mission's INTEGRATION worktree (M7 tier 1):
/// the single worktree, checked out to the mission branch, that all
/// mission-branch mutations run in when `workerIsolation = worktree`. Lives
/// under the same temp-dir base as [`parallel_worktree_path`], namespaced
/// with a `_integration` suffix that no real feature id can produce (feature
/// ids never start with `_`), so it never collides with a per-feature path.
pub fn mission_worktree_path(repo_root: &std::path::Path, mission_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "kranz-wt-{}-{mission_id}-_integration",
        repo_worktree_namespace(repo_root)
    ))
}

/// Absolute worktree directory for one dispatch-pool candidate stream
/// (KRZ-303). Same temp-dir base and repo namespacing as
/// [`parallel_worktree_path`], with a distinct `kranz-pool-` prefix so
/// candidate worktrees are mechanically and visually distinct from M3
/// per-feature worktrees (resume()'s sweeps key off each path shape: M3
/// branches die with their worktrees; pool BRANCHES are kept — only pool
/// dirs are reaped).
fn pool_worktree_path(
    repo_root: &std::path::Path,
    mission_id: &str,
    feature_id: &str,
    index: usize,
) -> PathBuf {
    // Same defensive sanitization as parallel_worktree_path.
    let safe: String = feature_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    std::env::temp_dir().join(format!(
        "kranz-pool-{}-{mission_id}-{safe}-c{index}",
        repo_worktree_namespace(repo_root)
    ))
}

/// The dispatch-pool decision line for a candidate whose worktree could not
/// even be INSPECTED at the checkpoint (12th-pass review, P2). Recorded
/// exactly where stream failures are recorded in the pool decision detail,
/// and the caller additionally pushes the candidate's index into `preserve`
/// so the cleanup guard skips reaping its worktree dir (its branch is never
/// deleted regardless) — an inspection error must never destroy deliverable
/// bytes the engine never got to verify.
fn pool_inspection_failure_line(
    idx: usize,
    n: usize,
    ws: &PoolWorkspace,
    error: &EngineError,
) -> String {
    format!(
        "- candidate {idx}/{}: `{}` / `{}` → branch `{}` — worktree inspection failed: {error} \
         (candidate FAILED; worktree dir and branch preserved for inspection)",
        n - 1,
        ws.spec.backend,
        ws.spec.model,
        ws.branch
    )
}

/// Stable, non-secret repository namespace for process-global temporary
/// worktree paths. Mission ids are repository-local, so the repository root
/// must participate in every worktree identity at the host boundary.
fn repo_worktree_namespace(repo_root: &std::path::Path) -> String {
    let canonical = canonical_root(repo_root.to_path_buf());
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Pre-M8 worktree locations, retained only so crash recovery can reap a
/// worktree left behind by an older kranz process after an upgrade.
fn legacy_parallel_worktree_path(mission_id: &str, feature_id: &str) -> PathBuf {
    let safe: String = feature_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    std::env::temp_dir().join(format!("kranz-wt-{mission_id}-{safe}"))
}

fn legacy_mission_worktree_path(mission_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kranz-wt-{mission_id}-_integration"))
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

fn role_label(role: Role) -> &'static str {
    match role {
        Role::Orchestrator => "orchestrator",
        Role::Worker => "worker",
        Role::ValidatorScrutiny => "scrutiny validator",
        Role::ValidatorFunctional => "functional validator",
    }
}

/// Index of the first milestone (in plan order) that is not Complete.
pub(crate) fn first_incomplete(state: &MissionState) -> Option<usize> {
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

/// git's well-known empty-tree object id (SHA-1 object format — the only
/// format the engine's throwaway and host repos use today): the `from` side
/// when diffing a parentless commit, whose whole tree is what it introduced.
const EMPTY_TREE_SHA: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Paths changed by the commit `sha` relative to its own FIRST parent.
///
/// Per-commit attribution must never chain consecutive entries of a
/// `commits_between` list: `from..to` interleaves merge parents, so adjacent
/// entries are not parent-child and a chained diff invents paths the commit
/// never touched — inflating the final gate's deliverable count and creating
/// spurious out-of-contract sweep findings (false-positive direction only).
/// A merge commit diffs against its first parent, i.e. what the merge itself
/// landed on the mission branch.
///
/// A parentless commit (reachable only via a merged orphan history — a
/// milestone range never STARTS at one) diffs against the empty tree:
/// everything it contains is exactly what it introduced. A real git failure
/// still surfaces, because the fallback runs the same plumbing.
fn commit_changed_paths(repo: &GitRepo, sha: &str) -> Result<Vec<String>> {
    match repo.changed_paths(&format!("{sha}^"), sha) {
        Ok(paths) => Ok(paths),
        Err(_) => repo.changed_paths(EMPTY_TREE_SHA, sha),
    }
}

/// De-duplicated, first-seen-order commands run by this milestone's workers,
/// gathered from each feature's `worker_runs` reports so validators can
/// re-run what workers already cited as evidence.
pub(crate) fn worker_commands_for_milestone(
    state: &MissionState,
    milestone: &Milestone,
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut commands = Vec::new();
    for feature in &milestone.features {
        for run_id in &feature.worker_runs {
            let Some(run) = state.runs.get(run_id) else {
                continue;
            };
            let Some(report) = &run.report else {
                continue;
            };
            for command in &report.commands_run {
                if seen.insert(command.clone()) {
                    commands.push(command.clone());
                }
            }
        }
    }
    commands
}

/// First non-empty line of a text (decision summaries).
pub(crate) fn first_nonempty_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Canonicalize the repo root when possible (macOS tempdirs are symlinks
/// under /var → /private/var; git pathspec matching needs the real path).
pub(crate) fn canonical_root(root: PathBuf) -> PathBuf {
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
        let mut text = "# kranz engine bookkeeping — never part of mission commits\n".to_string();
        for rule in crate::paths::KRANZ_GITIGNORE_RULES {
            text.push_str(rule);
            text.push('\n');
        }
        std::fs::write(&file, text)?;
    }
    Ok(())
}

/// Pre-flight a `config.changed` patch: the merged result must deserialize
/// and validate, or the event must not be appended (the reducer would poison
/// every future fold of the log).
fn preview_config_patch(current: &MissionConfig, patch: &serde_json::Value) -> Result<()> {
    config::apply_validated_patch(current, patch).map(|_| ())
}

// ---------------------------------------------------------------------------
// Unit tests for the tricky pure helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::judgement::lesson_orch_script;
    use crate::preflight::DroidEnvGuard;

    // -----------------------------------------------------------------------
    // Mission integration worktree primitive (M7 tier 1, feature f-1-2)
    // -----------------------------------------------------------------------

    /// Whether a `git worktree list` entry refers to the same directory as a
    /// Rust-canonicalized path. `list_worktrees` yields forward-slash paths
    /// with no verbatim prefix on every platform, whereas
    /// `std::fs::canonicalize` returns a `\\?\C:\...` backslash path on
    /// Windows — a raw `Path` equality never matches there. Normalizing both
    /// sides (unify separators, strip a leading `\\?\` verbatim prefix, and —
    /// on Windows only, where the filesystem is case-insensitive — lowercase)
    /// makes them comparable without another filesystem round-trip.
    fn worktree_entry_is(listed: &str, canonical: &std::path::Path) -> bool {
        fn norm(s: &str) -> String {
            let unified = s.replace('\\', "/");
            let stripped = unified.strip_prefix("//?/").unwrap_or(&unified);
            if cfg!(windows) {
                stripped.to_ascii_lowercase()
            } else {
                stripped.to_string()
            }
        }
        norm(listed) == norm(&canonical.to_string_lossy())
    }

    /// `setup_mission_worktree` creates the integration worktree on the
    /// mission branch WITHOUT moving the primary checkout off `main`, and
    /// `teardown_mission_worktree` removes it (proven via `list_worktrees`).
    #[test]
    fn setup_and_teardown_mission_worktree_round_trip() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        let mission_id = engine.state.mission.id.clone();
        let mission_branch = engine.state.mission.mission_branch.clone();

        let (path, wt_repo) = engine.setup_mission_worktree().expect("setup");
        assert_eq!(path, mission_worktree_path(&root, &mission_id));
        assert!(path.exists(), "integration worktree dir must exist");

        // The mission branch now exists and is checked out in the new
        // worktree...
        assert!(engine.repo.branch_exists(&mission_branch).unwrap());
        assert_eq!(wt_repo.current_branch().unwrap(), mission_branch);

        // ...while the PRIMARY checkout never moved off main.
        assert_eq!(engine.repo.current_branch().unwrap(), "main");

        let listed = engine.repo.list_worktrees().unwrap();
        let canon_path = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        assert!(
            listed.iter().any(|p| worktree_entry_is(p, &canon_path)),
            "integration worktree not in list_worktrees: {listed:?}"
        );

        engine.teardown_mission_worktree();
        let after = engine.repo.list_worktrees().unwrap();
        assert!(
            !after.iter().any(|p| worktree_entry_is(p, &canon_path)),
            "integration worktree still listed after teardown: {after:?}"
        );
        assert!(!path.exists(), "integration worktree dir must be gone");
    }

    // -----------------------------------------------------------------------
    // emit-never-poisons-log: fold-validate before append
    // -----------------------------------------------------------------------

    /// Build an engine on a throwaway repo and return it with its events path.
    fn emit_test_engine(root: &std::path::Path) -> (MissionEngine, std::path::PathBuf) {
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let engine =
            MissionEngine::create(backend, root, "goal", MissionConfig::default()).unwrap();
        let events_path = engine.paths.events_file();
        (engine, events_path)
    }

    /// An emit whose fold would fail must append NOTHING: the log stays
    /// byte-identical, the state is untouched, and the error surfaces to the
    /// caller. This is the m-83d1ed wedge class — appended-then-unfoldable —
    /// closed at emit time.
    #[test]
    fn emit_never_appends_an_unfoldable_event() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (mut engine, events_path) = emit_test_engine(&root);
        let log_before = std::fs::read(&events_path).unwrap();
        let state_before = serde_json::to_string(&engine.state).unwrap();

        // mission.created is fold-valid ONLY as the log's first event — and
        // this log already has one (create() wrote it). Re-emitting the REAL
        // first event's own kind is the simplest guaranteed unfoldable emit.
        let first_line = std::fs::read_to_string(&events_path)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .to_string();
        let first_event: Event = serde_json::from_str(&first_line).unwrap();
        let result = engine.emit(first_event.kind);

        let err = result.expect_err("a fold-invalid emit must be rejected");
        assert!(
            err.to_string().contains("only valid as the first event"),
            "unexpected error: {err}"
        );
        assert_eq!(
            std::fs::read(&events_path).unwrap(),
            log_before,
            "a rejected emit must leave the log byte-identical"
        );
        assert_eq!(
            serde_json::to_string(&engine.state).unwrap(),
            state_before,
            "a rejected emit must leave the state untouched"
        );
    }

    /// The happy path keeps its exact-once shape under the new pre-fold: one
    /// valid emit appends exactly one event, folds it (last_seq +1), and
    /// refreshes the snapshot to match.
    #[test]
    fn emit_never_appends_valid_emit_lands_exactly_once() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (mut engine, events_path) = emit_test_engine(&root);
        let lines_before = std::fs::read_to_string(&events_path)
            .unwrap()
            .lines()
            .count();
        let seq_before = engine.state.last_seq;

        engine
            .emit(EventKind::OrchestratorDecision {
                summary: "a fold-valid decision".to_string(),
                detail: None,
            })
            .expect("a fold-valid emit must land");

        let lines_after = std::fs::read_to_string(&events_path)
            .unwrap()
            .lines()
            .count();
        assert_eq!(lines_after, lines_before + 1, "exactly one event appended");
        assert_eq!(
            engine.state.last_seq,
            seq_before + 1,
            "the event folded exactly once"
        );
        let snapshot: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(engine.paths.state_file()).unwrap())
                .unwrap();
        assert_eq!(
            snapshot["lastSeq"].as_u64().unwrap(),
            seq_before + 1,
            "the snapshot reflects the fold"
        );
    }

    // -----------------------------------------------------------------------
    // Out-of-contract-write sweep (M7 tier 1, feature f-1-2)
    // -----------------------------------------------------------------------

    /// End-to-end: a real commit outside the declared touch-set produces
    /// exactly one out-of-contract-write finding; a commit inside it produces
    /// none.
    #[test]
    fn out_of_contract_sweep_flags_path_outside_touch_set() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        engine.state.mission.touch_set = vec!["src/**".to_string()];
        let start_sha = engine.repo.head_sha().unwrap();

        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("widget.rs"), "// in contract\n").unwrap();
        std::fs::write(root.join("oops.md"), "out of contract\n").unwrap();
        engine.repo.add_all_and_commit("[f-1] add widget").unwrap();

        let findings = engine.out_of_contract_sweep(&start_sha).unwrap();
        assert_eq!(findings.len(), 1, "findings: {findings:?}");
        assert_eq!(findings[0].class, contract_sweep::FINDING_CLASS);
        assert_eq!(findings[0].subject, "oops.md");
    }

    /// An empty (undeclared) touch-set skips the path sweep (advisory-off):
    /// no out-of-contract-write path findings, even for a path that would
    /// otherwise be flagged. Operators still get a warn log when worker
    /// commits landed.
    #[test]
    fn out_of_contract_sweep_empty_touch_set_is_advisory_off() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        assert!(engine.state.mission.touch_set.is_empty());
        let start_sha = engine.repo.head_sha().unwrap();

        std::fs::write(root.join("anything.md"), "whatever\n").unwrap();
        engine
            .repo
            .add_all_and_commit("[f-1] add anything")
            .unwrap();

        let findings = engine.out_of_contract_sweep(&start_sha).unwrap();
        assert!(findings.is_empty(), "findings: {findings:?}");
    }

    /// A `[kranz]`-authored commit that touches a path outside the touch-set
    /// (e.g. the approved-plan commit writing plan.json) is never flagged.
    #[test]
    fn out_of_contract_sweep_engine_commit_exempt() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        engine.state.mission.touch_set = vec!["src/**".to_string()];
        let start_sha = engine.repo.head_sha().unwrap();

        let mission_id = engine.state.mission.id.clone();
        let plan_dir = root.join(".kranz").join("missions").join(&mission_id);
        std::fs::create_dir_all(&plan_dir).unwrap();
        std::fs::write(plan_dir.join("plan.json"), "{}\n").unwrap();
        engine
            .repo
            .add_all_and_commit(&format!("[kranz] approved plan for {mission_id}"))
            .unwrap();

        let findings = engine.out_of_contract_sweep(&start_sha).unwrap();
        assert!(findings.is_empty(), "findings: {findings:?}");
    }

    /// A worker commit that SPOOFS an engine meta subject ("[kranz] mission
    /// report cleanup" matches the "[kranz] mission report" template) but
    /// touches a real file outside the touch-set is still swept: the meta
    /// exemption is path-verified, never subject-only.
    #[test]
    fn out_of_contract_sweep_flags_spoofed_meta_subject_commit() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        engine.state.mission.touch_set = vec!["src/**".to_string()];
        let start_sha = engine.repo.head_sha().unwrap();

        std::fs::write(root.join("smuggled.md"), "out of contract\n").unwrap();
        engine
            .repo
            .add_all_and_commit("[kranz] mission report cleanup")
            .unwrap();

        let findings = engine.out_of_contract_sweep(&start_sha).unwrap();
        assert_eq!(findings.len(), 1, "findings: {findings:?}");
        assert_eq!(findings[0].subject, "smuggled.md");
        assert_eq!(findings[0].class, contract_sweep::FINDING_CLASS);
    }

    /// A merge commit inside the milestone range must not create spurious
    /// findings: each commit is diffed against its own FIRST parent, never
    /// chained through the `commits_between` list (which interleaves merge
    /// parents, so adjacent entries are not parent-child). Regression shape:
    /// a genuine engine meta commit lands on the mission branch while a
    /// worker commit lands on a side branch; the chained diff compared the
    /// meta commit against the SIDE branch's tip, saw the worker's file,
    /// failed the meta exemption's path check, and flagged the meta commit's
    /// own research.md (mission-record, but not in `meta_paths`) as an
    /// out-of-contract write.
    #[test]
    fn out_of_contract_sweep_merge_commit_yields_no_spurious_finding() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        engine.state.mission.touch_set = vec!["src/**".to_string()];
        let start_sha = engine.repo.head_sha().unwrap();
        let mission_id = engine.state.mission.id.clone();

        // Side branch off the milestone start: one worker commit, entirely
        // inside the touch-set.
        engine.repo.create_branch("side", None).unwrap();
        engine.repo.checkout("side").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("widget.rs"), "// in contract\n").unwrap();
        engine.repo.add_all_and_commit("[f-1] add widget").unwrap();

        // Meanwhile a genuine engine meta commit lands on main.
        engine.repo.checkout("main").unwrap();
        let record_dir = root.join(".kranz").join("missions").join(&mission_id);
        std::fs::create_dir_all(&record_dir).unwrap();
        std::fs::write(record_dir.join("research.md"), "evidence\n").unwrap();
        engine
            .repo
            .add_all_and_commit(&format!("[kranz] approved plan for {mission_id}"))
            .unwrap();

        // A real merge commit inside the range.
        assert_eq!(
            engine.repo.merge_no_ff("side").unwrap(),
            crate::git_ops::MergeOutcome::Clean
        );

        let findings = engine.out_of_contract_sweep(&start_sha).unwrap();
        assert!(
            findings.is_empty(),
            "first-parent attribution must not invent findings across merge parents: {findings:?}"
        );
    }

    /// A dirty primary checkout in worktree mode yields a critical
    /// `primary-checkout` finding.
    #[test]
    fn primary_checkout_sweep_dirty_primary_flags_critical_finding() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        let start_sha = engine.repo.head_sha().unwrap();

        let (path, wt_repo) = engine.setup_mission_worktree().unwrap();
        engine.active_tree = Some((path, wt_repo));
        engine.primary_branch_at_start = Some("main".to_string());

        // Dirty the PRIMARY checkout's TRACKED content (not the worktree):
        // an untracked file wouldn't count (see `is_clean_tracked`), since
        // the engine's own housekeeping files are legitimately untracked
        // there in every worktree-mode run.
        std::fs::write(root.join("README.md"), "should never change\n").unwrap();

        let findings = engine.out_of_contract_sweep(&start_sha).unwrap();
        let primary_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.subject == "primary-checkout")
            .collect();
        assert_eq!(primary_findings.len(), 1, "findings: {findings:?}");
        assert_eq!(primary_findings[0].severity, "critical");

        engine.teardown_mission_worktree();
    }

    /// A clean, unmoved primary checkout in worktree mode yields no finding.
    #[test]
    fn primary_checkout_sweep_clean_primary_yields_no_finding() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        let start_sha = engine.repo.head_sha().unwrap();

        let (path, wt_repo) = engine.setup_mission_worktree().unwrap();
        engine.active_tree = Some((path, wt_repo));
        engine.primary_branch_at_start = Some("main".to_string());

        let findings = engine.out_of_contract_sweep(&start_sha).unwrap();
        assert!(
            !findings.iter().any(|f| f.subject == "primary-checkout"),
            "findings: {findings:?}"
        );

        engine.teardown_mission_worktree();
    }

    /// A mission integration worktree left behind by a crashed engine (never
    /// torn down) is reaped by `resume()`'s crash-recovery sweep, the same
    /// way per-feature worktrees are (orchestrator.rs:~408-414, M7 tier 1).
    #[test]
    fn resume_reaps_leaked_integration_worktree() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let engine =
            MissionEngine::create(backend.clone(), &root, "goal", MissionConfig::default())
                .unwrap();
        let mission_id = engine.state.mission.id.clone();

        let (path, _wt_repo) = engine.setup_mission_worktree().expect("setup");
        assert_eq!(path, mission_worktree_path(&root, &mission_id));
        assert!(path.exists(), "integration worktree dir must exist");

        let listed = engine.repo.list_worktrees().unwrap();
        let canon_path = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        assert!(
            listed.iter().any(|p| worktree_entry_is(p, &canon_path)),
            "integration worktree not in list_worktrees before crash: {listed:?}"
        );

        // Simulate a crash: drop the engine WITHOUT tearing down the
        // integration worktree, releasing the single-writer lock so resume()
        // can re-acquire it.
        drop(engine);

        let resumed = MissionEngine::resume(backend, &root, &mission_id, LockForce::No)
            .expect("resume should reap the leaked integration worktree and succeed");

        let after = resumed.repo.list_worktrees().unwrap();
        assert!(
            !after.iter().any(|p| worktree_entry_is(p, &canon_path)),
            "integration worktree still listed after resume: {after:?}"
        );
        assert!(
            !path.exists(),
            "integration worktree dir must be pruned after resume"
        );
    }

    /// `mission_worktree_path` never collides with a per-feature
    /// `parallel_worktree_path`, even for an adversarial feature id.
    #[test]
    fn mission_worktree_path_does_not_collide_with_feature_paths() {
        let mission_id = "m-collide-test";
        let repo_root = std::path::Path::new("/tmp/repo-a");
        let integration = mission_worktree_path(repo_root, mission_id);
        for feature_id in ["f-1-1", "f-1-2", "ms-collide-test-1"] {
            assert_ne!(
                integration,
                parallel_worktree_path(repo_root, mission_id, feature_id),
                "collided with feature id {feature_id:?}"
            );
        }
    }

    #[test]
    fn duplicate_mission_ids_in_different_repos_have_distinct_worktree_paths() {
        let mission_id = "m-same-id";
        assert_ne!(
            mission_worktree_path(std::path::Path::new("/tmp/repo-a"), mission_id),
            mission_worktree_path(std::path::Path::new("/tmp/repo-b"), mission_id),
        );
        assert_ne!(
            parallel_worktree_path(std::path::Path::new("/tmp/repo-a"), mission_id, "f-1-1",),
            parallel_worktree_path(std::path::Path::new("/tmp/repo-b"), mission_id, "f-1-1",),
        );
    }

    // -----------------------------------------------------------------------
    // Scrutiny backend selection (f-2-2)
    // -----------------------------------------------------------------------

    /// Serializes tests that mutate process-global env vars (`HOME`, `PATH`,
    /// `KRANZ_CODEX_BIN`) to force [`crate::backend_codex::discover_codex_binary`]
    /// to fail, regardless of whatever codex install happens to sit on the
    /// host running the suite.
    static CODEX_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard: points `KRANZ_CODEX_BIN` at a path that cannot exist, so
    /// codex discovery misses. Since `KRANZ_CODEX_BIN` is an exclusive
    /// override (see `discover_codex_binary`), this alone makes codex
    /// deterministically "absent" without touching `PATH`/`HOME` — other
    /// tests that shell out to `git` in parallel are unaffected. Restores the
    /// previous value on drop, including on panic, so a failed assertion
    /// never leaks a poisoned environment into later tests.
    struct CodexEnvGuard {
        prev_bin: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl CodexEnvGuard {
        fn engage() -> Self {
            let lock = CODEX_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let prev_bin = std::env::var_os("KRANZ_CODEX_BIN");
            std::env::set_var(
                "KRANZ_CODEX_BIN",
                "/nonexistent/kranz-test-codex-binary-absent",
            );
            CodexEnvGuard {
                prev_bin,
                _lock: lock,
            }
        }
    }

    impl Drop for CodexEnvGuard {
        fn drop(&mut self) {
            match self.prev_bin.take() {
                Some(v) => std::env::set_var("KRANZ_CODEX_BIN", v),
                None => std::env::remove_var("KRANZ_CODEX_BIN"),
            }
        }
    }

    /// Default config never selects a non-Claude backend: `select_backend`
    /// must hand back the injected backend untouched for every role and never
    /// emit a fallback decision (there is nothing to fall back from).
    #[test]
    fn default_role_backends_are_claude() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
        let _ = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&root)
            .output();
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(&root)
            .output();
        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&root)
            .output();
        std::fs::write(root.join("README.md"), "seed\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&root)
            .output();
        let _ = std::process::Command::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(&root)
            .output();

        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine =
            MissionEngine::create(backend.clone(), &root, "goal", MissionConfig::default())
                .expect("create engine");

        let before = EventLog::read_events(&engine.paths.events_file()).expect("read events");

        for role in [
            Role::Orchestrator,
            Role::Worker,
            Role::ValidatorScrutiny,
            Role::ValidatorFunctional,
        ] {
            let selected = engine.select_backend(role);
            assert!(
                selected.fallback_reason.is_none(),
                "default config must not fall back for {role:?}"
            );
            assert_eq!(selected.kind, BackendKind::Claude);
            assert!(
                Arc::ptr_eq(&selected.backend, &backend),
                "default config must select the injected backend for {role:?}"
            );
            assert_eq!(
                selected.cfg.role(role).model,
                MissionConfig::default().role(role).model
            );
        }

        let after = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert_eq!(
            before.len(),
            after.len(),
            "select_backend must not emit any event on the claude-default path"
        );
    }

    /// `validatorScrutiny.backend = "codex"` with no codex binary reachable:
    /// preflight must warn, the run loop's fallback decision must land in the
    /// event log, and the scrutiny validator must still run — through the
    /// injected (mock) backend, never silently skipped.
    #[tokio::test]
    async fn codex_absent_loud_fallback() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };

        let mut cfg = MissionConfig::default();
        cfg.validator_scrutiny.backend = Some("codex".to_string());
        cfg.skip_functional = true;

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            crate::backend_mock::MockScript::single_shot_json(&serde_json::json!({
                "findings": [],
                "summary": "clean"
            })),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg).expect("create engine");
        engine.state.mission.milestones.push(Milestone {
            id: "ms-1".to_string(),
            title: "m".to_string(),
            features: vec![],
            status: MilestoneStatus::Active,
            fix_cycles: 0,
            start_sha: Some("HEAD".to_string()),
            validator_guidance: None,
        });

        let env_guard = CodexEnvGuard::engage();

        let issues = engine.preflight();
        assert!(
            issues
                .iter()
                .any(|i| i.severity == "warn" && i.message.contains("codex")),
            "expected a codex preflight warning, got {issues:?}"
        );

        engine
            .validation_round(0)
            .await
            .expect("validation round must complete through the mock fallback, not error");

        drop(env_guard);

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events.jsonl");
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                EventKind::OrchestratorDecision { summary, .. }
                    if summary.contains("codex") && summary.contains("not available")
            )),
            "expected a loud fallback decision recorded in the event log; got {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        let started = mock.started_specs();
        assert_eq!(
            started.len(),
            1,
            "the scrutiny validator must still run exactly once, through the injected backend"
        );
    }

    /// `validatorScrutiny.backend = "droid"` with no droid binary reachable:
    /// preflight must warn, the run loop's fallback decision must land in the
    /// event log, and the scrutiny validator must still run — through the
    /// injected (mock) backend, never silently skipped.
    #[tokio::test]
    async fn droid_absent_loud_fallback() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };

        let mut cfg = MissionConfig::default();
        cfg.validator_scrutiny.backend = Some("droid".to_string());
        cfg.skip_functional = true;

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            crate::backend_mock::MockScript::single_shot_json(&serde_json::json!({
                "findings": [],
                "summary": "clean"
            })),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg).expect("create engine");
        engine.state.mission.milestones.push(Milestone {
            id: "ms-1".to_string(),
            title: "m".to_string(),
            features: vec![],
            status: MilestoneStatus::Active,
            fix_cycles: 0,
            start_sha: Some("HEAD".to_string()),
            validator_guidance: None,
        });

        let env_guard = DroidEnvGuard::engage();

        let issues = engine.preflight();
        assert!(
            issues
                .iter()
                .any(|i| i.severity == "warn" && i.message.contains("droid")),
            "expected a droid preflight warning, got {issues:?}"
        );

        engine
            .validation_round(0)
            .await
            .expect("validation round must complete through the mock fallback, not error");

        drop(env_guard);

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events.jsonl");
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                EventKind::OrchestratorDecision { summary, .. }
                    if summary.contains("droid") && summary.contains("not available")
            )),
            "expected a loud fallback decision recorded in the event log; got {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        let started = mock.started_specs();
        assert_eq!(
            started.len(),
            1,
            "the scrutiny validator must still run exactly once, through the injected backend"
        );
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
            validator_guidance: None,
        };
        assert_eq!(
            next_feature(&ms),
            Some(3),
            "Active (crashed) before Pending"
        );
        let mut done = ms.clone();
        done.features[3].status = FeatureStatus::Complete;
        done.features[4].status = FeatureStatus::Complete;
        assert_eq!(next_feature(&done), None);
    }

    #[test]
    fn worker_commands_for_milestone_dedupes_across_feature_reports() {
        let report = WorkerReport {
            result: RunResult::Pass,
            summary: String::new(),
            files_touched: vec![],
            tests_added: vec![],
            test_evidence: String::new(),
            dependencies_added: vec![],
            known_gaps: vec![],
            commits: vec![],
            commands_run: vec!["gc lint".to_string(), "gc lint".to_string()],
            escalation: None,
            questions: None,
        };
        let run = WorkerRun {
            id: "run-1".to_string(),
            role: Role::Worker,
            feature_id: Some("f1".to_string()),
            milestone_id: None,
            candidate: None,
            sdk_session_id: "sdk-1".to_string(),
            model: "m".to_string(),
            quant: "n/a".to_string(),
            weight_hash: None,
            started_at: chrono::Utc::now(),
            ended_at: None,
            tokens: TokenUsage::default(),
            cost_usd: None,
            transcript_path: "t.jsonl".to_string(),
            result: Some(RunResult::Pass),
            report: Some(report),
            prompt_hash: "h".to_string(),
        };
        let feature = Feature {
            id: "f1".to_string(),
            title: String::new(),
            spec: String::new(),
            validation_criteria: vec![],
            origin: FeatureOrigin::Plan,
            status: FeatureStatus::Complete,
            worker_runs: vec!["run-1".to_string()],
            commits: vec![],
            respawns: 0,
        };
        let milestone = Milestone {
            id: "ms-1".to_string(),
            title: String::new(),
            features: vec![feature],
            status: MilestoneStatus::Active,
            fix_cycles: 0,
            start_sha: None,
            validator_guidance: None,
        };
        let mut runs = std::collections::BTreeMap::new();
        runs.insert("run-1".to_string(), run);
        let state = MissionState {
            mission: Mission {
                id: "m-1".to_string(),
                goal: String::new(),
                validation_contract: vec![],
                milestones: vec![milestone.clone()],
                status: MissionStatus::Running,
                created_at: chrono::Utc::now(),
                base_branch: "main".to_string(),
                base_sha: None,
                mission_branch: "kranz/mission-m-1".to_string(),
                command_grants: vec![],
                touch_set: vec![],
                deny_exceptions: vec![],
                egress_grants: vec![],
            },
            runs,
            totals: TokenUsage::default(),
            total_cost_usd: 0.0,
            pending_user_messages: vec![],
            recent_decisions: vec![],
            config: MissionConfig::default(),
            latest_plan_revision: 0,
            pending_revision: None,
            pending_grant_request: None,
            pending_questions: vec![],
            question_count: 0,
            last_seq: 0,
            escalated_milestones: 0,
            local_executor_milestones: 0,
            workspace_provider: None,
            workspace_pin: None,
            workspace_lifecycle: None,
            resolved_divergence_units: std::collections::BTreeSet::new(),
        };

        assert_eq!(
            worker_commands_for_milestone(&state, &milestone),
            vec!["gc lint".to_string()]
        );
    }

    #[test]
    fn first_nonempty_line_skips_blanks() {
        assert_eq!(first_nonempty_line("\n\n  hello\nworld"), "hello");
        assert_eq!(first_nonempty_line(""), "");
    }

    #[test]
    fn preview_config_patch_rejects_invalid() {
        let cfg = MissionConfig::default();
        // 9 is out of the 1..=8 range M3 allows, so the patch must be rejected.
        let bad = serde_json::json!({ "maxParallelWorkers": 9 });
        assert!(preview_config_patch(&cfg, &bad).is_err());
        let below_floor = serde_json::json!({ "worker": { "model": "haiku" } });
        assert!(preview_config_patch(&cfg, &below_floor).is_err());
        let good = serde_json::json!({
            "worker": { "model": "haiku" },
            "allowBelowDefaultWorkerModel": true
        });
        assert!(preview_config_patch(&cfg, &good).is_ok());
    }

    #[tokio::test]
    async fn invalid_drain_time_config_patch_emits_an_audit_decision() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        control::enqueue(
            &engine.paths,
            &ControlCommand::ConfigChange {
                patch: serde_json::json!({ "worker": { "model": "haiku" } }),
            },
        )
        .unwrap();

        engine.drain_control().await.unwrap();

        assert!(
            engine
                .state
                .recent_decisions
                .iter()
                .any(|decision| decision.contains("config change ignored")),
            "invalid command must leave an operator-visible audit receipt"
        );
        assert!(control::drain(&engine.paths).unwrap().is_empty());
    }

    /// `contract_env(None)` must yield no KRANZ_BASE_SHA key at all (not an
    /// empty-string value) — locks in the None case for the final gate.
    ///
    /// This asserts directly on the map rather than spawning a subprocess:
    /// `.envs()` overlays onto the inherited process env without clearing
    /// it, so a subprocess-based check would pass or fail depending on
    /// whether KRANZ_BASE_SHA happens to be set in the ambient environment
    /// (e.g. because the engine's own final gate set it for this mission),
    /// which is exactly the false-CRITICAL failure mode this test exists to
    /// prevent.
    #[test]
    fn no_base_sha_means_no_gate_env_var() {
        let env = runner::contract_env(None);
        assert!(
            !env.contains_key("KRANZ_BASE_SHA"),
            "None base_sha must not define KRANZ_BASE_SHA in the gate env"
        );
    }

    /// F2: while `run()` idles in the `MissionStatus::Paused` poll branch, a
    /// buffered stream delta must age out to disk on its own — no further
    /// lifecycle event, no resume — proving the loop actually calls
    /// `EventLog::flush_if_due` on its `PAUSE_POLL` tick rather than only on
    /// the next `append`/`flush`/drop.
    #[tokio::test(flavor = "multi_thread")]
    async fn paused_idle_loop_age_flushes_buffered_delta() {
        let ok = std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("skipping test: git is not on PATH");
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {:?}", out);
        };
        if !std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            run(&["init"]);
            run(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        }
        run(&["config", "user.name", "test"]);
        run(&["config", "user.email", "test@example.com"]);
        std::fs::write(dir.path().join("README.md"), "seed\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-m", "seed"]);
        let root = std::fs::canonicalize(dir.path()).expect("canonicalize repo root");

        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let cfg = MissionConfig {
            event_stream_throttle_ms: 10,
            worker_isolation: WorkerIsolation::Checkout,
            ..MissionConfig::default()
        };
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg).expect("create engine");

        // Force the idle-Paused branch and buffer a stream delta directly
        // (bypassing any lifecycle path that would flush it immediately).
        engine.state.mission.status = MissionStatus::Paused;
        engine
            .log
            .append(EventKind::WorkerMessage {
                run_id: "test-run".to_string(),
                tag: "text".to_string(),
                content: "buffered delta".to_string(),
            })
            .expect("buffer a stream delta");

        let paths = engine.paths.clone();
        let before = EventLog::read_events(&paths.events_file()).expect("read events.jsonl");
        assert!(
            !before
                .iter()
                .any(|e| matches!(&e.kind, EventKind::WorkerMessage { .. })),
            "delta must still be buffered, not yet on disk"
        );

        let handle = tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(5), engine.run()).await;
        });

        // PAUSE_POLL is 300ms and the throttle above is 10ms, so a couple of
        // idle ticks are more than enough for flush_if_due to drain it.
        // Checked BEFORE aborting the task: EventLog's Drop also flushes, so
        // reading only after abort would pass even without the fix under test.
        // Poll to a deadline instead of a fixed sleep: loaded CI runners
        // (windows-latest) slip fixed delays and flaked this at 700ms.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut flushed = false;
        while std::time::Instant::now() < deadline {
            let events = EventLog::read_events(&paths.events_file()).expect("read events.jsonl");
            if events.iter().any(|e| {
                matches!(&e.kind, EventKind::WorkerMessage { content, .. } if content == "buffered delta")
            }) {
                flushed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        handle.abort();

        assert!(
            flushed,
            "idle Paused loop must age-flush the buffered delta to disk without a lifecycle event"
        );
    }

    // -----------------------------------------------------------------------
    // Lesson capture (roadmap: cross-mission learning)
    // -----------------------------------------------------------------------

    /// A throwaway git repo (seeded, `main` branch), or `None` (with a skip
    /// note) when `git` is not on PATH.
    pub(crate) fn lessons_test_repo() -> Option<(tempfile::TempDir, PathBuf)> {
        let git_ok = std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !git_ok {
            eprintln!("skipping test: git is not on PATH");
            return None;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {:?}", out);
        };
        if !std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            run(&["init"]);
            run(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        }
        run(&["config", "user.name", "test"]);
        run(&["config", "user.email", "test@example.com"]);
        std::fs::write(dir.path().join("README.md"), "seed\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-m", "seed"]);
        let root = std::fs::canonicalize(dir.path()).expect("canonicalize repo root");
        Some((dir, root))
    }

    #[tokio::test]
    async fn non_pass_worker_outcome_cannot_complete_from_pass_report() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let report = serde_json::json!({
            "result": "pass",
            "summary": "I passed before the process died",
            "filesTouched": [],
            "testsAdded": [],
            "testEvidence": "",
            "dependenciesAdded": [],
            "knownGaps": [],
            "commits": [],
            "commandsRun": []
        });
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            crate::backend_mock::MockScript::single_shot("auth ok"),
            crate::backend_mock::MockScript::single_shot_json(&report)
                .with_exit(SessionExit::Aborted),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let cfg = MissionConfig {
            max_respawns: 0,
            worker_isolation: WorkerIsolation::Checkout,
            ..MissionConfig::default()
        };
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg).unwrap();
        engine.state.mission.milestones.push(Milestone {
            id: "ms-1".to_string(),
            title: "m".to_string(),
            features: vec![Feature {
                id: "f-1-1".to_string(),
                title: "f".to_string(),
                spec: "s".to_string(),
                validation_criteria: vec![],
                origin: FeatureOrigin::Plan,
                status: FeatureStatus::Pending,
                worker_runs: vec![],
                commits: vec![],
                respawns: 0,
            }],
            status: MilestoneStatus::Active,
            fix_cycles: 0,
            start_sha: Some(engine.repo.head_sha().unwrap()),
            validator_guidance: None,
        });

        engine.run_feature(0, 0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::FeatureFailed { feature_id, .. } if feature_id == "f-1-1")),
            "non-pass runner outcome must fail/respawn, not complete: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::FeatureCompleted { feature_id, .. } if feature_id == "f-1-1")),
            "stale pass report must not complete the feature"
        );
        assert_eq!(
            mock.started_specs().len(),
            2,
            "only the auth preflight and worker should run; no orchestrator judgement turn"
        );
    }

    #[tokio::test]
    async fn failed_validator_without_report_blocks_validation() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            crate::backend_mock::MockScript::single_shot("not json")
                .with_exit(SessionExit::Failed("validator crashed".to_string())),
            crate::backend_mock::MockScript::single_shot("still not json")
                .with_exit(SessionExit::Failed("validator crashed again".to_string())),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let cfg = MissionConfig {
            skip_functional: true,
            worker_isolation: WorkerIsolation::Checkout,
            ..MissionConfig::default()
        };
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg).unwrap();
        engine.state.mission.milestones.push(Milestone {
            id: "ms-1".to_string(),
            title: "m".to_string(),
            features: vec![],
            status: MilestoneStatus::Active,
            fix_cycles: 0,
            start_sha: Some(engine.repo.head_sha().unwrap()),
            validator_guidance: None,
        });

        engine.validation_round(0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        let validator_spawns = events
            .iter()
            .filter(|e| {
                matches!(
                    &e.kind,
                    EventKind::WorkerSpawned { role, .. } if *role == Role::ValidatorScrutiny
                )
            })
            .count();
        assert_eq!(validator_spawns, 2, "validator must be retried once");
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneBlocked { milestone_id, reason } if milestone_id == "ms-1" && reason.contains("trusted report"))),
            "failed validator must block validation, not count as clean: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneCompleted { milestone_id, .. } if milestone_id == "ms-1")),
            "failed validator with no report must not complete the milestone"
        );
    }

    // ---------------------------------------------------------------------------
    // validator immutability: snapshot isolation + tripwire
    // (ticket validator-immutability-proof and its snapshot follow-up)
    // ---------------------------------------------------------------------------

    /// A validator report claiming a clean pass.
    fn clean_validator_script() -> crate::backend_mock::MockScript {
        crate::backend_mock::MockScript::single_shot_json(&serde_json::json!({
            "findings": [],
            "summary": "no findings"
        }))
    }

    fn single_milestone_engine(
        backend: Arc<dyn AgentBackend>,
        root: &std::path::Path,
    ) -> MissionEngine {
        let cfg = MissionConfig {
            skip_functional: true,
            worker_isolation: WorkerIsolation::Checkout,
            ..MissionConfig::default()
        };
        let mut engine = MissionEngine::create(backend, root, "goal", cfg).unwrap();
        engine.state.mission.milestones.push(Milestone {
            id: "ms-1".to_string(),
            title: "m".to_string(),
            features: vec![],
            status: MilestoneStatus::Active,
            fix_cycles: 0,
            start_sha: Some(engine.repo.head_sha().unwrap()),
            validator_guidance: None,
        });
        engine
    }

    /// A clean validator round passes the identity assertion: no
    /// `validator.tamper` event, the milestone completes — and the session
    /// ran in the throwaway snapshot, audited by a `validation.snapshot`
    /// event (removed once the round is done).
    #[tokio::test]
    async fn clean_validator_round_passes_immutability_assertion() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            clean_validator_script(),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine = single_milestone_engine(backend, &root);

        engine.validation_round(0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::ValidatorTamper { .. })),
            "clean round must not emit validator.tamper: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneCompleted { milestone_id, .. } if milestone_id == "ms-1")),
            "clean round completes the milestone: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        // The session ran in the snapshot, never the real checkout…
        let expected = engine.paths.runs_dir().join("validator-snapshot-scrutiny");
        let specs = mock.started_specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].cwd, expected,
            "the validator session cwd IS the snapshot"
        );
        // …the event audits path/tier (no target/ in this repo → absent)…
        let snapshot_event = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::ValidationSnapshot {
                    milestone_id,
                    role,
                    path,
                    target_tier,
                    ..
                } if milestone_id == "ms-1" => Some((*role, path.clone(), target_tier.clone())),
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!(
                    "expected validation.snapshot on the log: {:?}",
                    events.iter().map(|e| &e.kind).collect::<Vec<_>>()
                )
            });
        assert_eq!(snapshot_event.0, Role::ValidatorScrutiny);
        assert_eq!(snapshot_event.1, expected.display().to_string());
        assert_eq!(snapshot_event.2, "absent");
        // …and the snapshot is gone once the round is done.
        assert!(
            !expected.exists(),
            "the snapshot is discarded after the round"
        );
        assert!(validator_snapshot_leftovers(&engine).is_empty());
    }

    /// `validator-snapshot*` dirs left under runs/ — the leak the RAII
    /// guard must prevent, asserted empty after every kind of round.
    fn validator_snapshot_leftovers(engine: &MissionEngine) -> Vec<String> {
        std::fs::read_dir(engine.paths.runs_dir())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("validator-snapshot"))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The whole point of the snapshot: a validator that edits a TRACKED
    /// file writes into the THROWAWAY copy — the real checkout is
    /// byte-untouched, the tripwire stays silent, and the round's outcome is
    /// decided by the snapshot session's verdict (a clean pass completes).
    #[tokio::test]
    async fn validator_writes_land_in_snapshot_not_the_real_checkout() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        // The script claims a clean pass WHILE editing the tracked README —
        // the "alter tests to manufacture a pass" shape. With isolation the
        // edit is discarded with the snapshot; only the verdict crosses back.
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            clean_validator_script().writes_file("README.md", "tampered\n"),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine = single_milestone_engine(backend, &root);

        engine.validation_round(0).await.unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).unwrap(),
            "seed\n",
            "the validator's edit never reached the real checkout"
        );
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::ValidatorTamper { .. })),
            "an isolated write is not drift — the tripwire must stay silent: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneCompleted { milestone_id, .. } if milestone_id == "ms-1")),
            "the round is decided by the snapshot session's verdict: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(validator_snapshot_leftovers(&engine).is_empty());
        assert_eq!(mock.started_specs().len(), 1);
    }

    /// Mandatory containment (ticket `validator-mandatory-containment`): with
    /// the default `enforce: off` a validation round STILL wraps the
    /// validator where the platform and backend can contain it — the
    /// pre-resolved sandbox reaches the session spec with the snapshot as
    /// the writable root, the real checkout as the read-deny root, and the
    /// session-private scratch pinned — the posture is recorded as an
    /// orchestrator decision, the round completes, and the after-fingerprint
    /// tripwire stays armed as defense-in-depth (never the only net). Where
    /// the platform cannot contain (no bwrap, no Seatbelt), the round still
    /// completes and the LOUD degradation note is recorded instead — never
    /// silently bare.
    #[tokio::test]
    async fn validator_containment_wraps_enforce_off_round_and_records_posture() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            clean_validator_script(),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine = single_milestone_engine(backend, &root);

        engine.validation_round(0).await.unwrap();

        // The round completes regardless of the containment posture…
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneCompleted { milestone_id, .. } if milestone_id == "ms-1")),
            "the contained (or loudly degraded) round still completes: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        // …and the after-fingerprint remains — defense-in-depth, not the
        // only net: the tripwire ran and stayed silent on a clean round.
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::ValidatorTamper { .. })),
            "the tripwire stays armed: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        let specs = mock.started_specs();
        assert_eq!(specs.len(), 1);
        let decisions: Vec<&str> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::OrchestratorDecision { summary, .. } => Some(summary.as_str()),
                _ => None,
            })
            .collect();

        let containable = cfg!(target_os = "macos")
            || (cfg!(target_os = "linux") && crate::sandbox::command_available("bwrap"));
        if containable {
            let sandbox = specs[0]
                .sandbox
                .as_ref()
                .expect("enforce: off no longer leaves the validator unwrapped");
            let expected_cwd = engine.paths.runs_dir().join("validator-snapshot-scrutiny");
            assert_eq!(
                sandbox.inputs.session_cwd, expected_cwd,
                "the snapshot is the writable root"
            );
            assert_eq!(
                sandbox.inputs.tmpdir,
                crate::backend_claude::scratch_home_root(&specs[0].session_id),
                "the writable scratch is pinned to THIS session's private root"
            );
            assert_eq!(
                sandbox.inputs.validator_read_deny_roots,
                vec![engine.paths.repo_root.clone()],
                "checkout mode: the real checkout is the single read-deny root"
            );
            assert!(
                sandbox.inputs.extra_write.is_empty(),
                "no operator extraWrite widening under the mandatory wrap"
            );
            assert!(
                decisions
                    .iter()
                    .any(|s| s.contains("sandbox-contained (mandatory)")),
                "the contained posture is recorded per round: {decisions:?}"
            );
            // The generated profile read-denies the real tree's contents
            // (string-level; the applied sandbox-exec/bwrap probes live in
            // crate::sandbox's tests).
            let profile = crate::sandbox::generate_profile(&sandbox.inputs);
            let readme = format!("(literal \"{}\")", root.join("README.md").display());
            assert!(
                profile.contains(&readme),
                "the real checkout's source files are read-denied:\n{profile}"
            );
            let git_dir = format!("\"{}\"", root.join(".git").display());
            assert!(
                !profile.contains(&git_dir),
                "the shared git dir stays readable (the inspection surface):\n{profile}"
            );
        } else {
            assert!(
                specs[0].sandbox.is_none(),
                "an uncontainable platform runs degraded — never silently wrapped"
            );
            assert!(
                decisions
                    .iter()
                    .any(|s| s.contains("NOT sandbox-contained")),
                "the LOUD degradation note is recorded per round: {decisions:?}"
            );
        }
        assert!(validator_snapshot_leftovers(&engine).is_empty());
    }

    /// A validator that commits inside its session moves only the
    /// SNAPSHOT's detached HEAD: the real checkout's HEAD is unchanged, the
    /// commit is discarded with the snapshot, and the round completes on
    /// the verdict.
    #[tokio::test]
    async fn validator_commit_moves_only_the_snapshot_head() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            clean_validator_script()
                .writes_file("sneaky.rs", "fn sneaky() {}\n")
                .commits_all("validator's unreviewed commit"),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = single_milestone_engine(backend, &root);
        let head_before = engine.repo.head_sha().unwrap();

        engine.validation_round(0).await.unwrap();

        assert_eq!(
            engine.repo.head_sha().unwrap(),
            head_before,
            "the validator's commit moved only the snapshot HEAD"
        );
        assert!(
            !root.join("sneaky.rs").exists(),
            "the committed file never landed in the real checkout"
        );
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::ValidatorTamper { .. })),
            "a snapshot-local commit is not drift: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneCompleted { milestone_id, .. } if milestone_id == "ms-1")),
            "the round completes on the verdict: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(validator_snapshot_leftovers(&engine).is_empty());
    }

    /// The tripwire: if the REAL checkout drifts across a validator session
    /// anyway (here: the mock seam writes through an absolute path, out of
    /// its snapshot), the isolation itself has failed — `validator.tamper`
    /// fires, the milestone blocks, no retry, no completion.
    #[tokio::test]
    async fn real_checkout_drift_trips_the_tripwire() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        // writes_file joins the path to the session cwd; an ABSOLUTE path
        // replaces it (std::path::Path::join), so this write escapes the
        // snapshot and lands in the real checkout — the isolation-failure
        // shape the tripwire exists to catch.
        let escape = root.join("README.md").to_string_lossy().into_owned();
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            clean_validator_script().writes_file(escape, "tampered\n"),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = single_milestone_engine(backend, &root);

        engine.validation_round(0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        let tamper = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::ValidatorTamper {
                    milestone_id,
                    appeared,
                    ..
                } if milestone_id == "ms-1" => Some(appeared.clone()),
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!(
                    "expected validator.tamper on the log: {:?}",
                    events.iter().map(|e| &e.kind).collect::<Vec<_>>()
                )
            });
        assert!(
            tamper.iter().any(|entry| entry.contains("README.md")),
            "tamper event names the drifted file: {tamper:?}"
        );
        assert!(
            events.iter().any(|e| matches!(&e.kind, EventKind::MilestoneBlocked { milestone_id, reason } if milestone_id == "ms-1" && reason.contains("escaped its snapshot"))),
            "the block reason names the isolation failure: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneCompleted { milestone_id, .. } if milestone_id == "ms-1")),
            "a round whose isolation failed must not complete the milestone"
        );
        let validator_spawns = events
            .iter()
            .filter(|e| {
                matches!(
                    &e.kind,
                    EventKind::WorkerSpawned { role, .. } if *role == Role::ValidatorScrutiny
                )
            })
            .count();
        assert_eq!(
            validator_spawns, 1,
            "tripwire drift is not retried — the round fails on the spot"
        );
        assert!(
            validator_snapshot_leftovers(&engine).is_empty(),
            "the snapshot is discarded even on the tamper early-return"
        );
    }

    /// Teardown on a FAILED round: an untrusted primary and retry each get
    /// their own snapshot, the milestone blocks honestly, and no snapshot
    /// dir survives either session.
    #[tokio::test]
    async fn snapshot_removed_after_untrusted_round() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            crate::backend_mock::MockScript::single_shot("not json")
                .with_exit(SessionExit::Failed("validator crashed".to_string())),
            crate::backend_mock::MockScript::single_shot("still not json")
                .with_exit(SessionExit::Failed("validator crashed again".to_string())),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine = single_milestone_engine(backend, &root);

        engine.validation_round(0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert!(
            events.iter().any(|e| matches!(&e.kind, EventKind::MilestoneBlocked { milestone_id, reason } if milestone_id == "ms-1" && reason.contains("trusted report"))),
            "the untrusted round blocks: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        let specs = mock.started_specs();
        assert_eq!(specs.len(), 2, "primary + one retry");
        let expected = engine.paths.runs_dir().join("validator-snapshot-scrutiny");
        assert!(
            specs.iter().all(|s| s.cwd == expected),
            "both the primary and the retry ran in snapshots: {:?}",
            specs.iter().map(|s| s.cwd.clone()).collect::<Vec<_>>()
        );
        let snapshot_events = events
            .iter()
            .filter(|e| matches!(&e.kind, EventKind::ValidationSnapshot { .. }))
            .count();
        assert_eq!(snapshot_events, 2, "one snapshot event per session");
        assert!(
            validator_snapshot_leftovers(&engine).is_empty(),
            "no snapshot survives the failed round"
        );
    }

    /// Gate artifact churn is not drift: writes under a gitignored path
    /// (target/) never reach the porcelain tripwire — and with the snapshot
    /// they land in the throwaway copy anyway — so the round passes.
    #[tokio::test]
    async fn validator_ignored_artifact_churn_passes_round() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        // gitignore target/ (as every Rust checkout does) before the engine
        // pins the milestone start sha.
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&root)
            .output()
            .expect("git add");
        std::process::Command::new("git")
            .args(["commit", "-m", "gitignore target"])
            .current_dir(&root)
            .output()
            .expect("git commit");

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            clean_validator_script().writes_file("target/debug/build-output.txt", "obj"),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = single_milestone_engine(backend, &root);

        engine.validation_round(0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::ValidatorTamper { .. })),
            "ignored-artifact churn must not trip the assertion: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneCompleted { milestone_id, .. } if milestone_id == "ms-1")),
            "round with only ignored churn completes: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
    }

    // ---------------------------------------------------------------------------
    // feature f-2-2: fix-cycle-cap escalation valve
    // ---------------------------------------------------------------------------

    fn tier_escalation_finding_script(subject: &str) -> crate::backend_mock::MockScript {
        crate::backend_mock::MockScript::single_shot_json(&serde_json::json!({
            "findings": [{
                "subject": subject,
                "severity": "major",
                "evidence": format!("{subject} evidence"),
                "suggestedFix": format!("fix {subject}")
            }],
            "summary": "found an issue"
        }))
    }

    fn tier_escalation_fix_reply() -> String {
        serde_json::json!({
            "fixFeatures": [{
                "title": "fix issue",
                "spec": "resolve the validation finding",
                "validationCriteria": ["finding resolved"]
            }],
            "waived": [],
            "summary": "1 fix feature(s)"
        })
        .to_string()
    }

    /// The long-lived streaming orchestrator session: one init/ready pair,
    /// then one `fixFeatures` reply per validation round (rounds share the
    /// session — only the very first `start()` call spawns it).
    fn tier_escalation_orch_script(rounds: usize) -> crate::backend_mock::MockScript {
        use crate::backend_mock::{mock_init, mock_result_text, mock_text};
        let reply = tier_escalation_fix_reply();
        crate::backend_mock::MockScript::streaming(vec![
            mock_init("orch-session"),
            mock_result_text("ready"),
        ])
        .responding(
            (0..rounds)
                .map(|_| vec![mock_text(&reply), mock_result_text(&reply)])
                .collect(),
        )
    }

    /// A cap-exhausted milestone whose executor is on the local tier
    /// escalates to frontier instead of blocking — and escalation is
    /// one-shot: the SAME milestone hitting the cap again (now on the
    /// frontier tier) blocks exactly like the pre-escalation behaviour.
    #[tokio::test]
    async fn tier_escalation_replaces_block_and_is_one_shot_per_mission() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let mut cfg = MissionConfig {
            skip_functional: true,
            max_fix_cycles_per_milestone: 2,
            ..MissionConfig::default()
        };
        cfg.worker.backend = Some("local".to_string());
        cfg.worker.base_url = Some("http://localhost:8080".to_string());
        cfg.worker.context_budget = Some(8192);
        cfg.allow_below_default_worker_model = true;

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            tier_escalation_finding_script("part 1 works"),
            tier_escalation_orch_script(2),
            tier_escalation_finding_script("part 1 works again"),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg).expect("create engine");
        engine.state.mission.milestones.push(Milestone {
            id: "ms-1".to_string(),
            title: "m".to_string(),
            features: vec![],
            status: MilestoneStatus::Active,
            fix_cycles: 2,
            start_sha: Some(engine.repo.head_sha().unwrap()),
            validator_guidance: None,
        });
        assert_eq!(engine.state.executor_tier(), ExecutorTier::Local);

        // Round 1: cap already spent (fix_cycles=2, cap=2) → escalate, not block.
        engine.validation_round(0).await.unwrap();

        assert_eq!(
            engine.state.executor_tier(),
            ExecutorTier::Frontier,
            "escalation must flip the executor tier"
        );
        assert_eq!(engine.state.mission.milestones[0].fix_cycles, 0);
        assert_ne!(
            engine.state.mission.milestones[0].status,
            MilestoneStatus::Blocked
        );
        assert_ne!(engine.state.mission.status, MissionStatus::Blocked);

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::TierEscalated { milestone_id, .. } if milestone_id == "ms-1")),
            "expected tier.escalated: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneBlocked { .. })),
            "must not block when escalating: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::FixFeatureCreated { .. })),
            "escalation must continue on to fix features: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        // Round 2: same milestone hits the cap again, but the tier is now
        // Frontier — escalation is one-shot, so this must block as before.
        engine.state.mission.milestones[0].status = MilestoneStatus::Active;
        engine.state.mission.milestones[0].fix_cycles = 2;
        engine.validation_round(0).await.unwrap();

        assert_eq!(engine.state.executor_tier(), ExecutorTier::Frontier);
        assert_eq!(
            engine.state.mission.milestones[0].status,
            MilestoneStatus::Blocked
        );

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(&e.kind, EventKind::TierEscalated { .. }))
                .count(),
            1,
            "escalation must happen at most once per mission: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events.iter().any(|e| matches!(&e.kind, EventKind::MilestoneBlocked { milestone_id, .. } if milestone_id == "ms-1")),
            "second cap hit on the (now) frontier tier must block: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
    }

    // ---------------------------------------------------------------------------
    // Confirm-on-pass: the guarded local functional validator
    // (ticket local-inference-validator-guarded, KRZ-206b)
    // ---------------------------------------------------------------------------

    /// A chat-completions body whose single message carries `report` — the
    /// stub local endpoint's answer to every request (one local verdict per
    /// test). Drives a REAL [`crate::backend_local::LocalBackend`], so the
    /// local functional verdict travels the same HTTP seam as in production.
    fn local_stub_body(report: serde_json::Value) -> String {
        serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": report.to_string()}}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 10}
        })
        .to_string()
    }

    /// A single-milestone engine whose FUNCTIONAL validator is local-backed
    /// (the stub endpoint at `base_url`); scrutiny is skipped so the only
    /// validator in play is the functional role under test. One command
    /// assertion (`true` — a deterministic engine-side PASS) gives the local
    /// verdict a mechanical check to pass.
    fn local_functional_engine(
        backend: Arc<dyn AgentBackend>,
        root: &std::path::Path,
        base_url: String,
    ) -> MissionEngine {
        let mut cfg = MissionConfig {
            skip_scrutiny: true,
            worker_isolation: WorkerIsolation::Checkout,
            ..MissionConfig::default()
        };
        cfg.validator_functional.backend = Some("local".to_string());
        cfg.validator_functional.base_url = Some(base_url);
        cfg.validator_functional.context_budget = Some(100_000);
        let mut engine = MissionEngine::create(backend, root, "goal", cfg).unwrap();
        engine.state.mission.validation_contract = vec![Assertion {
            id: "a1".to_string(),
            statement: "the build passes".to_string(),
            check: AssertionCheck::Command,
            command: Some("true".to_string()),
        }];
        engine.state.mission.milestones.push(Milestone {
            id: "ms-1".to_string(),
            title: "m".to_string(),
            features: vec![],
            status: MilestoneStatus::Active,
            fix_cycles: 0,
            start_sha: Some(engine.repo.head_sha().unwrap()),
            validator_guidance: None,
        });
        engine
    }

    /// KRZ-206b pin: a local functional PASS on a contract-command assertion
    /// NEVER greens the round alone — the frontier confirmation runs first,
    /// and only its agreement completes the milestone. The comparison is
    /// recorded on `validation.confirm`: the local-vs-frontier miss-rate
    /// ground truth lives in the event store.
    #[tokio::test]
    async fn guarded_local_validator_pass_triggers_frontier_confirm_before_green() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (base_url, requests, _received) = crate::backend_local::tests::spawn_stub(
            "HTTP/1.1 200 OK",
            local_stub_body(serde_json::json!({"findings": [], "summary": "clean"})),
        )
        .await;
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            clean_validator_script(), // the frontier confirmation: agrees
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine = local_functional_engine(backend, &root, base_url);

        engine.validation_round(0).await.unwrap();

        // Exactly one LOCAL session (the primary — one HTTP request) and
        // exactly one FRONTIER session (the confirmation — one mock start,
        // on the claude fallback model, never another local call).
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        let specs = mock.started_specs();
        assert_eq!(specs.len(), 1, "only the confirmation runs on the mock");
        assert_eq!(
            specs[0].model, "sonnet",
            "the confirmation is the FRONTIER functional session"
        );

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        let confirm_seq = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::ValidationConfirm {
                    milestone_id,
                    local_run_id,
                    confirm_run_id,
                    confirmed,
                    disagreements,
                } if milestone_id == "ms-1" => {
                    assert_eq!(confirmed, &vec!["a1".to_string()]);
                    assert!(disagreements.is_empty());
                    assert_ne!(local_run_id, confirm_run_id);
                    Some(e.seq)
                }
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!(
                    "validation.confirm must land on the log: {:?}",
                    events.iter().map(|e| &e.kind).collect::<Vec<_>>()
                )
            });
        let completed_seq = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::MilestoneCompleted { milestone_id, .. } if milestone_id == "ms-1" => {
                    Some(e.seq)
                }
                _ => None,
            })
            .expect("an agreed confirmation completes the milestone");
        assert!(
            confirm_seq < completed_seq,
            "the confirmation must land BEFORE the green: confirm seq {confirm_seq}, \
             completed seq {completed_seq}"
        );
    }

    /// KRZ-206b pin: local PASS vs frontier FAIL is a recorded miss and fails
    /// CLOSED — the frontier finding stands as a round finding, the milestone
    /// does NOT complete, and the finding flows to the fix path.
    #[tokio::test]
    async fn guarded_local_validator_disagreement_fails_closed_to_frontier() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (base_url, _requests, _received) = crate::backend_local::tests::spawn_stub(
            "HTTP/1.1 200 OK",
            local_stub_body(serde_json::json!({"findings": [], "summary": "clean"})),
        )
        .await;
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            // The frontier confirmation disagrees: a1 is failing.
            tier_escalation_finding_script("a1"),
            // The conversion turn answers the finding with a fix feature.
            tier_escalation_orch_script(1),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = local_functional_engine(backend, &root, base_url);

        engine.validation_round(0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        // The miss is recorded (the measurement): nothing confirmed, one
        // disagreement on a1.
        let (confirmed, disagreements) = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::ValidationConfirm {
                    milestone_id,
                    confirmed,
                    disagreements,
                    ..
                } if milestone_id == "ms-1" => Some((confirmed.clone(), disagreements.clone())),
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!(
                    "validation.confirm must land on the log: {:?}",
                    events.iter().map(|e| &e.kind).collect::<Vec<_>>()
                )
            });
        assert!(
            confirmed.is_empty(),
            "a1 was overturned — no check stays confirmed: {confirmed:?}"
        );
        assert_eq!(disagreements.len(), 1);
        assert_eq!(disagreements[0].subject, "a1");
        // ...and it FAILED CLOSED: the frontier verdict became a round
        // finding (no silent green), never a completion.
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::ValidationFinding { milestone_id, finding, .. } if milestone_id == "ms-1" && finding.subject == "a1")),
            "the disagreement must fail closed as a validation.finding: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneCompleted { milestone_id, .. } if milestone_id == "ms-1")),
            "a disagreed PASS must not complete the milestone"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::FixFeatureCreated { .. })),
            "the failed-closed finding flows to the fix path: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
    }

    /// KRZ-206b pin (the deliberate asymmetry): a local FAIL is trusted
    /// WITHOUT a frontier confirmation — failures are visible (they cost a
    /// fix cycle); misses are the danger. No confirmation session runs and
    /// no `validation.confirm` lands.
    #[tokio::test]
    async fn guarded_local_validator_local_fail_is_trusted_without_confirmation() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (base_url, requests, _received) = crate::backend_local::tests::spawn_stub(
            "HTTP/1.1 200 OK",
            local_stub_body(serde_json::json!({
                "findings": [{
                    "subject": "a1",
                    "severity": "critical",
                    "evidence": "the local validator sees a1 failing"
                }],
                "summary": "a1 fails"
            })),
        )
        .await;
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            // The only mock session: the conversion turn's fix-feature reply.
            tier_escalation_orch_script(1),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine = local_functional_engine(backend, &root, base_url);

        engine.validation_round(0).await.unwrap();

        // One local session (the primary), and the ONLY mock session is the
        // conversion orchestrator — no frontier validator ever ran.
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            mock.started_specs().len(),
            1,
            "only the conversion orchestrator runs on the mock — no confirmation"
        );

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::ValidationConfirm { .. })),
            "a local FAIL triggers no confirmation: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::ValidationFinding { milestone_id, finding, .. } if milestone_id == "ms-1" && finding.subject == "a1")),
            "the local FAIL is trusted as a round finding: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneCompleted { milestone_id, .. } if milestone_id == "ms-1")),
            "a failed round must not complete the milestone"
        );
    }

    /// KRZ-206b pin: an untrusted confirmation fails CLOSED — the round
    /// blocks rather than greening an unconfirmed local PASS.
    #[tokio::test]
    async fn guarded_local_validator_untrusted_confirmation_blocks_instead_of_greening() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (base_url, _requests, _received) = crate::backend_local::tests::spawn_stub(
            "HTTP/1.1 200 OK",
            local_stub_body(serde_json::json!({"findings": [], "summary": "clean"})),
        )
        .await;
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            // The frontier confirmation crashes without a report.
            crate::backend_mock::MockScript::single_shot("not json")
                .with_exit(SessionExit::Failed("confirm crashed".to_string())),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = local_functional_engine(backend, &root, base_url);

        engine.validation_round(0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneBlocked { milestone_id, reason } if milestone_id == "ms-1" && reason.contains("cannot green the gate unconfirmed"))),
            "an untrusted confirmation blocks honestly: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::ValidationConfirm { .. })),
            "no comparison record without a trusted confirmation: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneCompleted { milestone_id, .. } if milestone_id == "ms-1")),
            "an unconfirmed local PASS must never complete the milestone"
        );
    }

    /// Companion to the escalation test: a mission whose executor is already
    /// on the frontier tier still blocks at the fix-cycle cap — the guard
    /// only changes behaviour while the executor is Local.
    #[tokio::test]
    async fn frontier_tier_still_blocks_at_fix_cycle_cap() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let cfg = MissionConfig {
            skip_functional: true,
            max_fix_cycles_per_milestone: 2,
            ..MissionConfig::default()
        };

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            tier_escalation_finding_script("part 1 works"),
            tier_escalation_orch_script(1),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg).expect("create engine");
        engine.state.mission.milestones.push(Milestone {
            id: "ms-1".to_string(),
            title: "m".to_string(),
            features: vec![],
            status: MilestoneStatus::Active,
            fix_cycles: 2,
            start_sha: Some(engine.repo.head_sha().unwrap()),
            validator_guidance: None,
        });
        assert_eq!(engine.state.executor_tier(), ExecutorTier::Frontier);

        engine.validation_round(0).await.unwrap();

        assert_eq!(engine.state.executor_tier(), ExecutorTier::Frontier);
        assert_eq!(
            engine.state.mission.milestones[0].status,
            MilestoneStatus::Blocked
        );

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::TierEscalated { .. })),
            "frontier tier must never escalate: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events.iter().any(|e| matches!(&e.kind, EventKind::MilestoneBlocked { milestone_id, .. } if milestone_id == "ms-1")),
            "frontier tier must still block at the cap: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
    }

    /// Escalating the executor must never touch the validator role configs —
    /// validators stay on the frontier tier throughout, per the mission's
    /// D-X decision.
    #[tokio::test]
    async fn validator_stays_frontier_after_worker_tier_escalates() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let mut cfg = MissionConfig {
            skip_functional: true,
            max_fix_cycles_per_milestone: 2,
            ..MissionConfig::default()
        };
        cfg.worker.backend = Some("local".to_string());
        cfg.worker.base_url = Some("http://localhost:8080".to_string());
        cfg.worker.context_budget = Some(8192);
        cfg.allow_below_default_worker_model = true;

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            tier_escalation_finding_script("part 1 works"),
            tier_escalation_orch_script(1),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg).expect("create engine");
        engine.state.mission.milestones.push(Milestone {
            id: "ms-1".to_string(),
            title: "m".to_string(),
            features: vec![],
            status: MilestoneStatus::Active,
            fix_cycles: 2,
            start_sha: Some(engine.repo.head_sha().unwrap()),
            validator_guidance: None,
        });

        assert_ne!(
            engine.state.config.validator_scrutiny.backend.as_deref(),
            Some("local")
        );
        assert_ne!(
            engine.state.config.validator_functional.backend.as_deref(),
            Some("local")
        );

        engine.validation_round(0).await.unwrap();

        assert_eq!(engine.state.executor_tier(), ExecutorTier::Frontier);
        assert_ne!(
            engine.state.config.validator_scrutiny.backend.as_deref(),
            Some("local"),
            "validator scrutiny must stay off the local backend after escalation"
        );
        assert_ne!(
            engine.state.config.validator_functional.backend.as_deref(),
            Some("local"),
            "validator functional must stay off the local backend after escalation"
        );
        assert_eq!(
            engine.state.config.backend_kind(Role::ValidatorScrutiny),
            BackendKind::Claude
        );
    }

    // -----------------------------------------------------------------------
    // Worker self-escalation to the frontier advisor
    // (ticket backend-routing-abstraction, KRZ-331)
    // -----------------------------------------------------------------------

    /// The escalation event names the SOURCE route the escalating worker ran
    /// on and the TARGET advisor route — and the emission is record-only:
    /// the validator route and the executor tier are byte-identical after it
    /// (a worker escalation can never bypass the floor's validator
    /// requirements; the tier flip is tier.escalated's job, and that is
    /// orchestrator-initiated only).
    #[tokio::test]
    async fn routing_abstraction_escalation_event_names_source_and_target_routes() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let mut cfg = MissionConfig {
            worker_isolation: WorkerIsolation::Checkout,
            ..MissionConfig::default()
        };
        cfg.worker.backend = Some("local".to_string());
        cfg.worker.base_url = Some("http://localhost:8080".to_string());
        cfg.worker.context_budget = Some(8192);
        cfg.allow_below_default_worker_model = true;

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg).expect("create engine");
        assert_eq!(engine.state.executor_tier(), ExecutorTier::Local);

        // A recorded worker run to escalate from (the fold validates the run
        // reference as a corruption guard, so the run must exist).
        engine
            .emit(EventKind::WorkerSpawned {
                run_id: "r-1".to_string(),
                role: Role::Worker,
                feature_id: None,
                milestone_id: None,
                candidate: None,
                sdk_session_id: "s-1".to_string(),
                model: "m".to_string(),
                quant: "n/a".to_string(),
                weight_hash: None,
                prompt_hash: "h".to_string(),
                transcript_path: "runs/r-1.jsonl".to_string(),
            })
            .unwrap();

        let outcome = |escalation: Option<&str>| runner::RunOutcome {
            run_id: "r-1".to_string(),
            session_id: "s-1".to_string(),
            result: RunResult::Pass,
            usage: TokenUsage::default(),
            cost_usd: None,
            final_text: String::new(),
            report: Some(WorkerReport {
                result: RunResult::Pass,
                summary: "s".to_string(),
                files_touched: vec![],
                tests_added: vec![],
                test_evidence: String::new(),
                dependencies_added: vec![],
                known_gaps: vec![],
                commits: vec![],
                commands_run: vec![],
                escalation: escalation.map(|s| s.to_string()),
                questions: None,
            }),
            validator_report: None,
            exit: SessionExit::Completed,
            denied_count: 0,
            denied_commands: vec![],
            denied_egress: vec![],
        };

        let validators_before = (
            engine.state.config.validator_scrutiny.clone(),
            engine.state.config.validator_functional.clone(),
        );
        engine
            .emit_worker_escalation(
                "f-1-1",
                &outcome(Some("spec ambiguity beyond my confidence")),
            )
            .unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        let recorded: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::WorkerEscalated {
                    run_id,
                    feature_id,
                    from,
                    to,
                    reason,
                } => Some((
                    run_id.clone(),
                    feature_id.clone(),
                    *from,
                    *to,
                    reason.clone(),
                )),
                _ => None,
            })
            .collect();
        assert_eq!(
            recorded.len(),
            1,
            "exactly one worker.escalated: {events:?}"
        );
        let (run_id, feature_id, from, to, reason) = &recorded[0];
        assert_eq!(run_id, "r-1");
        assert_eq!(feature_id, "f-1-1");
        assert_eq!(
            *from,
            ExecutorTier::Local,
            "the source route is the tier the worker session ran on"
        );
        assert_eq!(
            *to,
            ExecutorTier::Frontier,
            "the target route is the frontier advisor"
        );
        assert_eq!(reason, "spec ambiguity beyond my confidence");

        // Record-only: the floor is untouched.
        assert_eq!(engine.state.config.validator_scrutiny, validators_before.0);
        assert_eq!(
            engine.state.config.validator_functional,
            validators_before.1
        );
        assert_eq!(
            engine.state.executor_tier(),
            ExecutorTier::Local,
            "a worker escalation never flips the executor tier"
        );

        // No escalation requested (or no report at all) ⇒ no event.
        engine
            .emit_worker_escalation("f-1-1", &outcome(None))
            .unwrap();
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(&e.kind, EventKind::WorkerEscalated { .. }))
                .count(),
            1,
            "a report without an escalation request must not record one"
        );
    }

    // -----------------------------------------------------------------------
    // Structured human questions (ticket structured-human-question-events)
    // -----------------------------------------------------------------------

    /// Engine with an approved one-milestone/one-feature plan, ms-1 started,
    /// and worker run r-1 spawned on f-1-1 — the context refs a
    /// `question.opened` can name (the fold validates them).
    #[cfg(test)]
    fn question_events_engine() -> Option<(tempfile::TempDir, MissionEngine)> {
        let (_dir, root) = lessons_test_repo()?;
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine = MissionEngine::create(
            backend,
            &root,
            "goal",
            MissionConfig {
                worker_isolation: WorkerIsolation::Checkout,
                ..MissionConfig::default()
            },
        )
        .expect("create engine");
        engine
            .approve_plan(Plan {
                goal: "g".into(),
                validation_contract: vec![],
                milestones: vec![PlanMilestone {
                    title: "m".into(),
                    features: vec![PlanFeature {
                        title: "f".into(),
                        spec: "s".into(),
                        validation_criteria: vec![],
                    }],
                }],
                considered_alternatives: None,
                command_grants: vec![],
                touch_set: vec![],
            })
            .expect("approve plan");
        engine
            .emit(EventKind::MilestoneStarted {
                milestone_id: "ms-1".to_string(),
                start_sha: "sha-1".to_string(),
            })
            .unwrap();
        engine
            .emit(EventKind::WorkerSpawned {
                run_id: "r-1".to_string(),
                role: Role::Worker,
                feature_id: Some("f-1-1".to_string()),
                milestone_id: None,
                candidate: None,
                sdk_session_id: "s-1".to_string(),
                model: "m".to_string(),
                quant: "n/a".to_string(),
                weight_hash: None,
                prompt_hash: "h".to_string(),
                transcript_path: "runs/r-1.jsonl".to_string(),
            })
            .unwrap();
        Some((_dir, engine))
    }

    /// A worker outcome whose report carries the given questions (and no
    /// escalation) — the "ask the human" payload the run path hands
    /// [`MissionEngine::emit_worker_questions`].
    #[cfg(test)]
    fn question_outcome(
        questions: Option<Vec<crate::types::ReportQuestion>>,
    ) -> runner::RunOutcome {
        runner::RunOutcome {
            run_id: "r-1".to_string(),
            session_id: "s-1".to_string(),
            result: RunResult::Partial,
            usage: TokenUsage::default(),
            cost_usd: None,
            final_text: String::new(),
            report: Some(WorkerReport {
                result: RunResult::Partial,
                summary: "blocked on a human choice".to_string(),
                files_touched: vec![],
                tests_added: vec![],
                test_evidence: String::new(),
                dependencies_added: vec![],
                known_gaps: vec![],
                commits: vec![],
                commands_run: vec![],
                escalation: None,
                questions,
            }),
            validator_report: None,
            exit: SessionExit::Completed,
            denied_count: 0,
            denied_commands: vec![],
            denied_egress: vec![],
        }
    }

    #[test]
    fn question_events_worker_report_opens_pending_decision_projection() {
        let Some((_dir, mut engine)) = question_events_engine() else {
            return;
        };
        engine
            .emit_worker_questions(
                "ms-1",
                "f-1-1",
                &question_outcome(Some(vec![
                    crate::types::ReportQuestion {
                        text: "Which storage engine should the cache use?".to_string(),
                        options: vec!["sqlite".to_string(), "in-memory".to_string()],
                    },
                    crate::types::ReportQuestion {
                        text: "What should the flag be called?".to_string(),
                        options: vec![],
                    },
                ])),
            )
            .unwrap();

        let pending = &engine.state.pending_questions;
        assert_eq!(pending.len(), 2, "both asks parked: {pending:?}");
        assert_eq!(engine.state.question_count, 2);
        // Engine-minted ids, per-mission monotonic — never model-supplied.
        assert_eq!(pending[0].question_id, "q-1");
        assert_eq!(pending[1].question_id, "q-2");
        assert_eq!(pending[0].options, vec!["sqlite", "in-memory"]);
        assert!(pending[1].options.is_empty(), "empty options = free text");
        for q in pending {
            assert_eq!(q.role, Role::Worker);
            assert_eq!(q.run_id.as_deref(), Some("r-1"));
            assert_eq!(q.feature_id.as_deref(), Some("f-1-1"));
            assert_eq!(q.milestone_id.as_deref(), Some("ms-1"));
        }
        // The events landed in the log (the replay source of truth).
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(&e.kind, EventKind::QuestionOpened { .. }))
                .count(),
            2
        );
        // Opening a question parks NOTHING (contrast the grant park).
        assert!(engine.state.pending_grant_request.is_none());
        assert_eq!(engine.state.mission.status, MissionStatus::Running);
    }

    #[test]
    fn question_events_caps_truncate_and_scrub_at_write() {
        let Some((_dir, mut engine)) = question_events_engine() else {
            return;
        };
        // A token the anthropic-api-key rule flags (same shape as the scrub
        // tests' anchor): model-authored text must never reach the log. The
        // secret leads the over-long strings so truncation (which follows the
        // scrub) can't cut it away first — the redaction marker must be what
        // survives.
        const SECRET: &str = "sk-ant-api03-ScrubNofollowTestValue1";
        let long_text = format!("{SECRET}{}", "x".repeat(600));
        let questions: Vec<crate::types::ReportQuestion> = (0..6)
            .map(|i| crate::types::ReportQuestion {
                text: if i == 0 {
                    long_text.clone()
                } else {
                    format!("question {i}")
                },
                options: (0..6)
                    .map(|o| {
                        if o == 0 {
                            format!("{SECRET}{}", "y".repeat(200))
                        } else {
                            format!("option {o}")
                        }
                    })
                    .collect(),
            })
            .collect();
        engine
            .emit_worker_questions("ms-1", "f-1-1", &question_outcome(Some(questions)))
            .unwrap();

        // The 4-question cap: first four opened, the rest dropped WITH an
        // operator-visible note (never silently).
        assert_eq!(engine.state.pending_questions.len(), 4);
        assert!(
            engine
                .state
                .recent_decisions
                .iter()
                .any(|d| d.contains("beyond the 4-question cap")),
            "the drop is narrated: {:?}",
            engine.state.recent_decisions
        );
        let first = &engine.state.pending_questions[0];
        assert!(
            first.text.chars().count() <= 500 + "… [truncated]".len(),
            "text capped: {} chars",
            first.text.chars().count()
        );
        assert_eq!(first.options.len(), 4, "options capped");
        assert!(
            first.options[0].chars().count() <= 100 + "… [truncated]".len(),
            "option text capped: {} chars",
            first.options[0].chars().count()
        );
        // Scrubbed at write: the secret shape appears NOWHERE in the log.
        let raw = std::fs::read_to_string(engine.paths.events_file()).expect("read log");
        assert!(
            !raw.contains(SECRET),
            "model-authored secret must be scrubbed from events.jsonl"
        );
        assert!(raw.contains("[REDACTED]"), "redaction marker present");
    }

    /// Prose fallback (ticket structured-human-question-events): a report
    /// without a `questions` key — every backend without a structured ask —
    /// opens nothing and the mission flows exactly as before.
    #[test]
    fn question_events_prose_only_report_opens_nothing() {
        let Some((_dir, mut engine)) = question_events_engine() else {
            return;
        };
        engine
            .emit_worker_questions("ms-1", "f-1-1", &question_outcome(None))
            .unwrap();
        assert!(engine.state.pending_questions.is_empty());
        assert_eq!(engine.state.question_count, 0);
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::QuestionOpened { .. })),
            "no question events for a prose-only report"
        );
        // Some questions but an empty list behaves the same.
        engine
            .emit_worker_questions("ms-1", "f-1-1", &question_outcome(Some(vec![])))
            .unwrap();
        assert!(engine.state.pending_questions.is_empty());
    }

    /// End-to-end through the EXISTING control path: an `answer-question`
    /// control file drains to `question.answered`, which routes the answer
    /// onto the user-message consult — and a replayed (duplicate) answer
    /// file is noted and swallowed, never a brick.
    #[tokio::test]
    async fn question_events_answer_reaches_mission_via_control_drain() {
        let Some((_dir, mut engine)) = question_events_engine() else {
            return;
        };
        engine
            .emit_worker_questions(
                "ms-1",
                "f-1-1",
                &question_outcome(Some(vec![crate::types::ReportQuestion {
                    text: "Which storage engine?".to_string(),
                    options: vec!["sqlite".to_string(), "in-memory".to_string()],
                }])),
            )
            .unwrap();

        control::enqueue(
            &engine.paths,
            &ControlCommand::AnswerQuestion {
                question_id: "q-1".to_string(),
                answer: "sqlite".to_string(),
                option: Some(0),
            },
        )
        .unwrap();
        engine.drain_control().await.unwrap();

        assert!(engine.state.pending_questions.is_empty());
        assert_eq!(engine.state.pending_user_messages.len(), 1);
        assert!(
            engine.state.pending_user_messages[0].contains("sqlite"),
            "the answer reached the consult path: {:?}",
            engine.state.pending_user_messages
        );
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        let answered: Vec<_> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::QuestionAnswered {
                    question_id,
                    answer,
                    via,
                    option,
                } => Some((question_id.clone(), answer.clone(), via.clone(), *option)),
                _ => None,
            })
            .collect();
        assert_eq!(answered.len(), 1);
        assert_eq!(answered[0].0, "q-1");
        assert_eq!(answered[0].1, "sqlite");
        assert_eq!(answered[0].2, "answer-question");
        assert_eq!(answered[0].3, Some(0));

        // Duplicate answer (the crash-between-emit-and-acknowledge window):
        // noted for the operator, swallowed — never a brick, and NEVER a
        // second question.answered (the note's decision fold may consume the
        // replayed consult line, the pre-existing queue semantics every user
        // message shares; the durable answer stays in the log either way).
        control::enqueue(
            &engine.paths,
            &ControlCommand::AnswerQuestion {
                question_id: "q-1".to_string(),
                answer: "sqlite".to_string(),
                option: Some(0),
            },
        )
        .unwrap();
        engine.drain_control().await.unwrap();
        assert!(
            engine
                .state
                .recent_decisions
                .iter()
                .any(|d| d.contains("answer for question q-1 ignored")),
            "the duplicate is narrated: {:?}",
            engine.state.recent_decisions
        );
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(&e.kind, EventKind::QuestionAnswered { .. }))
                .count(),
            1,
            "the duplicate never lands a second question.answered"
        );
        assert!(control::drain(&engine.paths).unwrap().is_empty());
    }

    /// The answer cross-checks (engine-side, mirroring the grant echo
    /// discipline): unknown id, empty answer, out-of-range option, and
    /// option text that doesn't match the parked question are all refused
    /// BEFORE any event lands.
    #[test]
    fn question_events_answer_validation_refuses_stale_answers() {
        let Some((_dir, mut engine)) = question_events_engine() else {
            return;
        };
        engine
            .emit_worker_questions(
                "ms-1",
                "f-1-1",
                &question_outcome(Some(vec![crate::types::ReportQuestion {
                    text: "Which storage engine?".to_string(),
                    options: vec!["sqlite".to_string(), "in-memory".to_string()],
                }])),
            )
            .unwrap();
        let seq_before = engine.state.last_seq;

        assert!(engine
            .answer_pending_question("q-nope", "sqlite", None)
            .is_err());
        assert!(engine.answer_pending_question("q-1", "   ", None).is_err());
        assert!(engine
            .answer_pending_question("q-1", "sqlite", Some(9))
            .is_err());
        assert!(engine
            .answer_pending_question("q-1", "in-memory", Some(0))
            .is_err());
        assert_eq!(
            engine.state.last_seq, seq_before,
            "a refused answer appends nothing"
        );
        assert_eq!(engine.state.pending_questions.len(), 1);

        // Free text on an optioned question (the "Other" path) IS accepted.
        engine
            .answer_pending_question("q-1", "postgres, actually", None)
            .unwrap();
        assert!(engine.state.pending_questions.is_empty());

        // The answer is scrubbed + capped at the write boundary too: an
        // operator pasting a token into an over-long answer lands redacted
        // and truncated in the corpus-exported log.
        const SECRET: &str = "sk-ant-api03-ScrubNofollowTestValue1";
        engine
            .emit_worker_questions(
                "ms-1",
                "f-1-1",
                &question_outcome(Some(vec![crate::types::ReportQuestion {
                    text: "Another?".to_string(),
                    options: vec![],
                }])),
            )
            .unwrap();
        let long_answer = format!("{SECRET}{}", "z".repeat(600));
        engine
            .answer_pending_question("q-2", &long_answer, None)
            .unwrap();
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        let answers: Vec<String> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::QuestionAnswered { answer, .. } => Some(answer.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(answers.len(), 2, "both answers recorded");
        let answer = &answers[1];
        assert!(!answer.contains(SECRET), "answer redacted at write");
        assert!(answer.contains("[REDACTED]"));
        assert!(
            answer.chars().count() <= 500 + "… [truncated]".len(),
            "answer capped: {} chars",
            answer.chars().count()
        );
    }

    /// The clear sweep: milestone-scoped clears remove only that milestone's
    /// asks; a mission-end clear removes them all — the projection never
    /// shows an unanswerable "your move".
    #[test]
    fn question_events_clear_open_questions_scopes() {
        let Some((_dir, mut engine)) = question_events_engine() else {
            return;
        };
        engine
            .emit_worker_questions(
                "ms-1",
                "f-1-1",
                &question_outcome(Some(vec![
                    crate::types::ReportQuestion {
                        text: "first".to_string(),
                        options: vec![],
                    },
                    crate::types::ReportQuestion {
                        text: "second".to_string(),
                        options: vec![],
                    },
                ])),
            )
            .unwrap();
        assert_eq!(engine.state.pending_questions.len(), 2);

        // Milestone scope: only ms-1's asks clear. (Both opens here are
        // ms-1-scoped, so one remains after a foreign milestone's sweep.)
        engine
            .clear_open_questions("milestone completed", |q| {
                q.milestone_id.as_deref() == Some("ms-2")
            })
            .unwrap();
        assert_eq!(
            engine.state.pending_questions.len(),
            2,
            "foreign scope clears nothing"
        );
        engine
            .clear_open_questions("milestone completed", |q| {
                q.milestone_id.as_deref() == Some("ms-1")
            })
            .unwrap();
        assert!(engine.state.pending_questions.is_empty());

        // Mission-end scope: everything clears.
        engine
            .emit_worker_questions(
                "ms-1",
                "f-1-1",
                &question_outcome(Some(vec![crate::types::ReportQuestion {
                    text: "third".to_string(),
                    options: vec![],
                }])),
            )
            .unwrap();
        engine
            .clear_open_questions("mission completed", |_| true)
            .unwrap();
        assert!(engine.state.pending_questions.is_empty());
        // Ids are never reused across clears (the folded count only grows).
        assert_eq!(engine.state.question_count, 3);
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(&e.kind, EventKind::QuestionCleared { .. }))
                .count(),
            3
        );
    }

    /// End-to-end through the worker run path: a worker report carrying an
    /// `escalation` reason records the `worker.escalated` event BEFORE the
    /// judgement turn — the frontier advisor act — that consumes the same
    /// report, and the mission's floor is otherwise byte-identical: the
    /// feature completes on the judgement, the executor tier never flips,
    /// and the validator configs are untouched.
    #[tokio::test]
    async fn routing_abstraction_worker_escalation_reaches_advisor_leaving_floor_untouched() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let report = serde_json::json!({
            "result": "pass",
            "summary": "built it; flagged an approach call for advice",
            "filesTouched": [],
            "testsAdded": [],
            "testEvidence": "cargo test: ok",
            "dependenciesAdded": [],
            "knownGaps": [],
            "commits": [],
            "commandsRun": [],
            "escalation": "chose the retry policy arbitrarily — wants frontier advice"
        });
        let judgement =
            serde_json::json!({"decision": "complete", "guidance": "", "summary": "advice: policy is fine"})
                .to_string();
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            crate::backend_mock::MockScript::single_shot_json(&report),
            // The long-lived orchestrator session: init/ready, then the
            // judgement verdict for this run.
            {
                use crate::backend_mock::{mock_init, mock_result_text, mock_text};
                crate::backend_mock::MockScript::streaming(vec![
                    mock_init("orch-session"),
                    mock_result_text("ready"),
                ])
                .responding(vec![vec![
                    mock_text(&judgement),
                    mock_result_text(&judgement),
                ]])
            },
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let cfg = MissionConfig {
            worker_isolation: WorkerIsolation::Checkout,
            ..MissionConfig::default()
        };
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg).unwrap();
        engine.seed_worker_auth_verdict_for_test(AuthVerdict::Authenticated);
        engine.state.mission.milestones.push(Milestone {
            id: "ms-1".to_string(),
            title: "m".to_string(),
            features: vec![Feature {
                id: "f-1-1".to_string(),
                title: "f".to_string(),
                spec: "s".to_string(),
                validation_criteria: vec![],
                origin: FeatureOrigin::Plan,
                status: FeatureStatus::Pending,
                worker_runs: vec![],
                commits: vec![],
                respawns: 0,
            }],
            status: MilestoneStatus::Active,
            fix_cycles: 0,
            start_sha: Some(engine.repo.head_sha().unwrap()),
            validator_guidance: None,
        });
        let validators_before = (
            engine.state.config.validator_scrutiny.clone(),
            engine.state.config.validator_functional.clone(),
        );

        engine.run_feature(0, 0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        let escalated: Vec<&Event> = events
            .iter()
            .filter(|e| matches!(&e.kind, EventKind::WorkerEscalated { .. }))
            .collect();
        assert_eq!(
            escalated.len(),
            1,
            "exactly one worker.escalated: {events:?}"
        );
        match &escalated[0].kind {
            EventKind::WorkerEscalated {
                feature_id,
                from,
                to,
                reason,
                ..
            } => {
                assert_eq!(feature_id, "f-1-1");
                assert_eq!(*from, ExecutorTier::Frontier);
                assert_eq!(*to, ExecutorTier::Frontier);
                assert_eq!(
                    reason,
                    "chose the retry policy arbitrarily — wants frontier advice"
                );
            }
            _ => unreachable!(),
        }

        // The record lands BEFORE the advisor act that consumes the request.
        let judgement_seq = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::OrchestratorDecision { summary, .. }
                    if summary.starts_with("judgement for f-1-1") =>
                {
                    Some(e.seq)
                }
                _ => None,
            })
            .expect("the judgement decision must be recorded");
        assert!(
            escalated[0].seq < judgement_seq,
            "the escalation is recorded before the judgement that advises on it"
        );

        // The floor is unaffected: the feature completed on the judgement
        // (escalation neither blocks nor short-circuits), the executor tier
        // never flipped, and the validator route is byte-identical.
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::FeatureCompleted { feature_id, .. } if feature_id == "f-1-1")),
            "the feature completes on the judgement: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert_eq!(engine.state.executor_tier(), ExecutorTier::Frontier);
        assert_eq!(engine.state.config.validator_scrutiny, validators_before.0);
        assert_eq!(
            engine.state.config.validator_functional,
            validators_before.1
        );
    }

    // -----------------------------------------------------------------------
    // Lessons index injection into planning seeds
    // -----------------------------------------------------------------------

    fn seed_lesson_for_index(root: &std::path::Path, id: &str, first_line: &str) {
        let lessons_dir = root.join(".kranz").join("lessons");
        std::fs::create_dir_all(&lessons_dir).unwrap();
        std::fs::write(
            lessons_dir.join(format!("{id}.md")),
            format!("{first_line}\n"),
        )
        .unwrap();
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(lessons_dir.join("index.md"))
            .unwrap();
        f.write_all(format!("- {id}.md · {first_line}\n").as_bytes())
            .unwrap();
        // Commit the lesson through a genuine report commit so it passes the
        // manifest's git-history provenance check (added by a
        // `[kranz] mission report` commit with a matching Kranz-Mission
        // trailer) — the real capture flow, mirrored for the test.
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        git(&["add", ".kranz/lessons"]);
        git(&[
            "commit",
            "-m",
            &format!("[kranz] mission report for {id}\n\nKranz-Mission: {id}"),
        ]);
    }

    fn streaming_seed(spec: &SessionSpec) -> &str {
        match &spec.prompt {
            PromptMode::Streaming(seed) => seed.as_str(),
            other => panic!("expected a streaming prompt, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn planning_seed_injects_lessons_index() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        seed_lesson_for_index(
            &root,
            "m01",
            "Always check the plan for a base_branch override.",
        );

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            lesson_orch_script("ready"),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        assert_eq!(engine.state.mission.status, MissionStatus::Planning);

        engine
            .ensure_orchestrator()
            .await
            .expect("ensure orchestrator");

        let specs = mock.started_specs();
        assert_eq!(specs.len(), 1);
        let seed = streaming_seed(&specs[0]);
        assert!(seed.contains("m01.md"));
        assert!(seed.contains("Always check the plan for a base_branch override."));
        assert!(seed.contains("## Lessons from past missions in this repo"));
    }

    /// Ticket pin (lessons-manifest-body-split): a lesson file dropped into
    /// `.kranz/lessons/` OUTSIDE the engine's commit flow (no `[kranz] mission
    /// report` commit introduced it) must never reach a planning prompt. This
    /// exercises the provenance filter end-to-end, not just its logic — a
    /// revert to an unfiltered render would fail here.
    #[tokio::test]
    async fn planning_seed_omits_a_dropped_lesson_without_provenance() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        // Write the file + index entry but DO NOT commit it (an arbitrary drop).
        let lessons_dir = root.join(".kranz").join("lessons");
        std::fs::create_dir_all(&lessons_dir).unwrap();
        std::fs::write(lessons_dir.join("m-drop.md"), "INJECTED PAYLOAD\n").unwrap();
        std::fs::write(
            lessons_dir.join("index.md"),
            "- m-drop.md · INJECTED PAYLOAD\n",
        )
        .unwrap();

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            lesson_orch_script("ready"),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        assert_eq!(engine.state.mission.status, MissionStatus::Planning);

        engine
            .ensure_orchestrator()
            .await
            .expect("ensure orchestrator");

        let specs = mock.started_specs();
        assert_eq!(specs.len(), 1);
        let seed = streaming_seed(&specs[0]);
        assert!(
            !seed.contains("INJECTED PAYLOAD") && !seed.contains("m-drop.md"),
            "an uncommitted lesson must be filtered out: {seed}"
        );
        assert!(
            !seed.contains("Lessons from past missions"),
            "with no provenance-clean lessons, no lessons block is injected: {seed}"
        );
    }

    #[tokio::test]
    async fn planning_seed_unchanged_without_lessons() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            lesson_orch_script("ready"),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        assert_eq!(engine.state.mission.status, MissionStatus::Planning);

        engine
            .ensure_orchestrator()
            .await
            .expect("ensure orchestrator");

        let specs = mock.started_specs();
        assert_eq!(specs.len(), 1);
        let seed = streaming_seed(&specs[0]);
        assert!(!seed.contains("Lessons from past missions"));
    }

    #[tokio::test]
    async fn resume_ack_seed_never_carries_lessons_index() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        seed_lesson_for_index(
            &root,
            "m01",
            "Always check the plan for a base_branch override.",
        );

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            lesson_orch_script("ready"),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        // Simulate a known previous sdk session so ensure_orchestrator takes
        // the resume-ack path instead of a fresh planning seed.
        engine.orch_session_id = Some("prev-session".to_string());

        engine
            .ensure_orchestrator()
            .await
            .expect("ensure orchestrator");

        let specs = mock.started_specs();
        assert_eq!(specs.len(), 1);
        let seed = streaming_seed(&specs[0]);
        assert!(seed.contains("The engine resumed this orchestrator session"));
        assert!(!seed.contains("Lessons from past missions"));
    }

    #[tokio::test]
    async fn non_planning_reseed_never_carries_lessons_index() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        seed_lesson_for_index(
            &root,
            "m01",
            "Always check the plan for a base_branch override.",
        );

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            lesson_orch_script("ready"),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        engine.state.mission.status = MissionStatus::Running;

        engine
            .ensure_orchestrator()
            .await
            .expect("ensure orchestrator");

        let specs = mock.started_specs();
        assert_eq!(specs.len(), 1);
        let seed = streaming_seed(&specs[0]);
        assert!(!seed.contains("Lessons from past missions"));
    }

    // -----------------------------------------------------------------------
    // Codex scrutiny integration (f-2-3): a stubbed `codex exec --json`
    // binary drives real ValidatorReport findings into the fix-cycle
    // machinery, priced with the codex table. No real API spend: everything
    // comes from a POSIX shell stub streaming the committed fixture.
    // -----------------------------------------------------------------------

    /// Writes an executable POSIX shell stub that stands in for the real
    /// `codex` CLI closely enough to drive [`crate::backend_codex::CodexBackend`]:
    /// `--version` prints a plausible version string and any `exec ...`
    /// invocation streams the committed fixture JSONL to stdout, exiting 0.
    /// Not portable to windows-latest (no `/bin/sh`), hence `cfg(unix)`.
    #[cfg(unix)]
    fn write_codex_stub() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = std::fs::canonicalize(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/codex_exec_scrutiny.jsonl"),
        )
        .expect("fixture exists");
        let script_path = dir.path().join("codex-stub.sh");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'codex-cli 0.0.0-test'\n  exit 0\nfi\ncat '{}'\nexit 0\n",
                fixture.display()
            ),
        )
        .expect("write stub script");
        let mut perms = std::fs::metadata(&script_path)
            .expect("stat stub script")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&script_path, perms).expect("chmod stub script");
        (dir, script_path)
    }

    /// Like [`write_codex_stub`] but the stub's JSONL has no `agent_message`
    /// item at all — only a `thread.started` and a `turn.completed` with
    /// `usage` — so `parse_validator_report` returns `None` even though the
    /// stub exits 0. Models a codex run that completed but never emitted a
    /// parseable report (e.g. auth/network hiccup mid-turn).
    #[cfg(unix)]
    fn write_codex_stub_no_report() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = std::fs::canonicalize(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/codex_exec_scrutiny_no_report.jsonl"),
        )
        .expect("fixture exists");
        let script_path = dir.path().join("codex-stub-no-report.sh");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'codex-cli 0.0.0-test'\n  exit 0\nfi\ncat '{}'\nexit 0\n",
                fixture.display()
            ),
        )
        .expect("write stub script");
        let mut perms = std::fs::metadata(&script_path)
            .expect("stat stub script")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&script_path, perms).expect("chmod stub script");
        (dir, script_path)
    }

    /// RAII guard: points `KRANZ_CODEX_BIN` at a working stub so
    /// `discover_codex_binary` deterministically resolves it as the FIRST
    /// candidate, regardless of whatever real `codex` install happens to sit
    /// on the host running the suite. Unlike [`CodexEnvGuard`], `HOME`/`PATH`
    /// are left untouched — validation contract commands may still need git
    /// on PATH, and the stub wins over PATH lookups either way. Serialized on
    /// the same [`CODEX_ENV_LOCK`] so it never races the other codex-env
    /// tests.
    #[cfg(unix)]
    struct CodexStubEnvGuard {
        prev_bin: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    #[cfg(unix)]
    impl CodexStubEnvGuard {
        fn engage(stub: &std::path::Path) -> Self {
            let lock = CODEX_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let prev_bin = std::env::var_os("KRANZ_CODEX_BIN");
            std::env::set_var("KRANZ_CODEX_BIN", stub);
            CodexStubEnvGuard {
                prev_bin,
                _lock: lock,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for CodexStubEnvGuard {
        fn drop(&mut self) {
            match self.prev_bin.take() {
                Some(v) => std::env::set_var("KRANZ_CODEX_BIN", v),
                None => std::env::remove_var("KRANZ_CODEX_BIN"),
            }
        }
    }

    /// One conversion-turn reply (§4.5 g) converting every finding into `n`
    /// fix features.
    #[cfg(unix)]
    fn codex_fix_features_reply(n: usize) -> String {
        let features: Vec<serde_json::Value> = (1..=n)
            .map(|i| {
                serde_json::json!({
                    "title": format!("fix issue {i}"),
                    "spec": format!("resolve validation finding {i}"),
                    "validationCriteria": [format!("finding {i} resolved")]
                })
            })
            .collect();
        serde_json::json!({ "fixFeatures": features, "summary": format!("{n} fix feature(s)") })
            .to_string()
    }

    #[cfg(unix)]
    fn codex_scrutiny_cfg() -> MissionConfig {
        let mut cfg = MissionConfig::default();
        cfg.validator_scrutiny.backend = Some("codex".to_string());
        cfg.skip_functional = true;
        cfg
    }

    // Every caller is a `cfg(unix)` stub-backend test (like its sibling
    // `codex_scrutiny_cfg`); ungated it is dead code under windows clippy.
    #[cfg(unix)]
    fn codex_scrutiny_milestone() -> Milestone {
        Milestone {
            id: "ms-1".to_string(),
            title: "m".to_string(),
            features: vec![],
            status: MilestoneStatus::Active,
            fix_cycles: 0,
            start_sha: Some("HEAD".to_string()),
            validator_guidance: None,
        }
    }

    /// The stub codex's ValidatorReport findings (>=1, per the fixture) fold
    /// into the run loop through the normal machinery: `validation.finding`
    /// events, an orchestrator conversion turn, and a `fixfeature.created`
    /// event that lands the fix feature in state — exactly like a claude
    /// scrutiny run's findings would. Also asserts the run actually went
    /// through codex (codex model on the spawn event, no fallback decision).
    #[cfg(unix)]
    #[tokio::test]
    async fn codex_scrutiny_findings_flow() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (_stub_dir, stub_path) = write_codex_stub();

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            lesson_orch_script(&codex_fix_features_reply(1)),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = MissionEngine::create(backend, &root, "goal", codex_scrutiny_cfg())
            .expect("create engine");
        engine
            .state
            .mission
            .milestones
            .push(codex_scrutiny_milestone());

        let env_guard = CodexStubEnvGuard::engage(&stub_path);
        engine
            .validation_round(0)
            .await
            .expect("validation round must complete through the stub codex backend");
        drop(env_guard);

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events.jsonl");

        assert!(
            !events.iter().any(|e| matches!(
                &e.kind,
                EventKind::OrchestratorDecision { summary, .. }
                    if summary.contains("codex") && summary.contains("not available")
            )),
            "codex must not have fallen back to claude: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                EventKind::WorkerSpawned { role, model, .. }
                    if *role == Role::ValidatorScrutiny && model == cost::DEFAULT_CODEX_MODEL
            )),
            "expected the scrutiny run spawned with the codex model: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::ValidationFinding { .. })),
            "expected the stub codex's findings as validation.finding events: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::FixFeatureCreated { .. })),
            "expected findings converted into a fix feature: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            engine.state().mission.milestones[0]
                .features
                .iter()
                .any(|f| f.origin == FeatureOrigin::Fix),
            "fix feature must be folded into mission state"
        );
    }

    /// The codex validator run's cost/tokens are priced with the codex table
    /// and land in mission totals: the run's recorded `cost_usd` equals
    /// `cost::usage_cost_usd(usage, DEFAULT_CODEX_MODEL)` for the fixture's
    /// token usage, and `total_cost_usd` increases by exactly that amount.
    #[cfg(unix)]
    #[tokio::test]
    async fn codex_validator_cost_in_totals() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (_stub_dir, stub_path) = write_codex_stub();

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            lesson_orch_script(&codex_fix_features_reply(1)),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = MissionEngine::create(backend, &root, "goal", codex_scrutiny_cfg())
            .expect("create engine");
        engine
            .state
            .mission
            .milestones
            .push(codex_scrutiny_milestone());
        assert_eq!(engine.state().total_cost_usd, 0.0, "totals start at zero");

        let env_guard = CodexStubEnvGuard::engage(&stub_path);
        engine
            .validation_round(0)
            .await
            .expect("validation round must complete through the stub codex backend");
        drop(env_guard);

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events.jsonl");
        let (usage, cost_usd) = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::WorkerCompleted {
                    tokens, cost_usd, ..
                } => Some((tokens.clone(), *cost_usd)),
                _ => None,
            })
            .expect("expected a worker.completed event for the codex scrutiny run");

        let expected = cost::usage_cost_usd(&usage, cost::DEFAULT_CODEX_MODEL);
        assert!(expected > 0.0, "expected nonzero codex-priced cost");
        assert_eq!(
            cost_usd,
            Some(expected),
            "the run's recorded cost_usd must equal codex pricing for its usage"
        );

        // Mission totals fold in every run's cost (including the mock
        // orchestrator conversion turn), so isolate the codex run's
        // contribution by summing every worker.completed cost_usd recorded
        // and checking the total accounts for exactly that sum — with the
        // codex-priced `expected` amount as one addend (asserted above).
        let all_runs_cost: f64 = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::WorkerCompleted { cost_usd, .. } => *cost_usd,
                _ => None,
            })
            .sum();
        assert!(
            all_runs_cost >= expected,
            "total run cost ({all_runs_cost}) must include the codex-priced run cost ({expected})"
        );
        assert_eq!(
            engine.state().total_cost_usd,
            all_runs_cost,
            "mission totals must equal the sum of every run's recorded cost, codex included"
        );
    }

    /// A codex scrutiny run that exits 0 but never emits a parseable
    /// `ValidatorReport` (usage present, no `agent_message`) must trigger the
    /// bounded runtime-retry fallback exactly once: a loud
    /// `orchestrator.decision` naming the retry, a second `ValidatorScrutiny`
    /// run actually executed against the claude (mock) backend, and that
    /// retry's findings folded into a fix feature like any other scrutiny
    /// run's would.
    #[cfg(unix)]
    #[tokio::test]
    async fn codex_scrutiny_no_report_falls_back_to_claude_once() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (_stub_dir, stub_path) = write_codex_stub_no_report();

        let retry_report = serde_json::json!({
            "findings": [{
                "subject": "retry-finding",
                "severity": "major",
                "evidence": "claude retry scrutiny run found this after codex produced no report",
                "suggestedFix": "address it"
            }],
            "summary": "one finding from the claude retry run"
        });
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            crate::backend_mock::MockScript::single_shot_json(&retry_report),
            lesson_orch_script(&codex_fix_features_reply(1)),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = MissionEngine::create(backend, &root, "goal", codex_scrutiny_cfg())
            .expect("create engine");
        engine
            .state
            .mission
            .milestones
            .push(codex_scrutiny_milestone());

        let env_guard = CodexStubEnvGuard::engage(&stub_path);
        engine
            .validation_round(0)
            .await
            .expect("validation round must complete via the claude retry fallback");
        drop(env_guard);

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events.jsonl");

        let retry_decisions: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    &e.kind,
                    EventKind::OrchestratorDecision { summary, .. }
                        if summary.contains("retrying once with the claude scrutiny validator")
                )
            })
            .collect();
        assert_eq!(
            retry_decisions.len(),
            1,
            "expected exactly one loud retry decision: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        let scrutiny_spawns = events
            .iter()
            .filter(|e| {
                matches!(
                    &e.kind,
                    EventKind::WorkerSpawned { role, .. } if *role == Role::ValidatorScrutiny
                )
            })
            .count();
        assert_eq!(
            scrutiny_spawns,
            2,
            "expected the initial codex run plus one claude retry run: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::FixFeatureCreated { .. })),
            "expected the claude retry's findings converted into a fix feature: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            engine.state().mission.milestones[0]
                .features
                .iter()
                .any(|f| f.origin == FeatureOrigin::Fix),
            "fix feature from the retry's findings must be folded into mission state"
        );
    }

    // -----------------------------------------------------------------------
    // Droid scrutiny integration (f-2-3): a stubbed `droid exec -o json`
    // binary drives real ValidatorReport findings into the fix-cycle
    // machinery, priced with the droid (Fireworks GLM) table. No real API
    // spend: everything comes from a POSIX shell stub streaming the
    // committed fixture, mirroring the codex integration tests above.
    // -----------------------------------------------------------------------

    /// Writes an executable POSIX shell stub that stands in for the real
    /// `droid` CLI closely enough to drive
    /// [`crate::backend_droid::DroidBackend`]: `--version` prints a
    /// plausible version string and any `exec ...` invocation streams the
    /// committed fixture (a single JSON result object) to stdout, exiting 0.
    /// Not portable to windows-latest (no `/bin/sh`), hence `cfg(unix)`.
    #[cfg(unix)]
    fn write_droid_stub() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = std::fs::canonicalize(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/droid_exec_scrutiny.json"),
        )
        .expect("fixture exists");
        let script_path = dir.path().join("droid-stub.sh");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'droid-cli 0.0.0-test'\n  exit 0\nfi\ncat '{}'\nexit 0\n",
                fixture.display()
            ),
        )
        .expect("write stub script");
        let mut perms = std::fs::metadata(&script_path)
            .expect("stat stub script")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&script_path, perms).expect("chmod stub script");
        (dir, script_path)
    }

    /// Like [`write_droid_stub`] but the stub cats
    /// `droid_exec_scrutiny_no_report.json` — a result object with an empty
    /// `result` string — so `parse_validator_report` returns `None` even
    /// though the stub exits 0. Models a droid run that completed but never
    /// emitted a parseable report.
    #[cfg(unix)]
    fn write_droid_stub_no_report() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let fixture = std::fs::canonicalize(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/droid_exec_scrutiny_no_report.json"),
        )
        .expect("fixture exists");
        let script_path = dir.path().join("droid-stub-no-report.sh");
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'droid-cli 0.0.0-test'\n  exit 0\nfi\ncat '{}'\nexit 0\n",
                fixture.display()
            ),
        )
        .expect("write stub script");
        let mut perms = std::fs::metadata(&script_path)
            .expect("stat stub script")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&script_path, perms).expect("chmod stub script");
        (dir, script_path)
    }

    /// RAII guard: points `KRANZ_DROID_BIN` at a working stub so
    /// `discover_droid_binary` deterministically resolves it as the FIRST
    /// (exclusive) candidate, regardless of whatever real `droid` install
    /// happens to sit on the host running the suite. Serialized on the same
    /// [`crate::preflight::DROID_ENV_LOCK`] used by the other
    /// `KRANZ_DROID_BIN`-mutating tests so they never race each other.
    #[cfg(unix)]
    struct DroidStubEnvGuard {
        prev_bin: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    #[cfg(unix)]
    impl DroidStubEnvGuard {
        fn engage(stub: &std::path::Path) -> Self {
            let lock = crate::preflight::DROID_ENV_LOCK
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let prev_bin = std::env::var_os("KRANZ_DROID_BIN");
            std::env::set_var("KRANZ_DROID_BIN", stub);
            DroidStubEnvGuard {
                prev_bin,
                _lock: lock,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for DroidStubEnvGuard {
        fn drop(&mut self) {
            match self.prev_bin.take() {
                Some(v) => std::env::set_var("KRANZ_DROID_BIN", v),
                None => std::env::remove_var("KRANZ_DROID_BIN"),
            }
        }
    }

    #[cfg(unix)]
    fn droid_scrutiny_cfg() -> MissionConfig {
        let mut cfg = MissionConfig::default();
        cfg.validator_scrutiny.backend = Some("droid".to_string());
        cfg.skip_functional = true;
        cfg
    }

    #[cfg(unix)]
    #[test]
    fn select_backend_routes_each_role_and_normalizes_default_models() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (_codex_stub_dir, codex_stub) = write_codex_stub();
        let (_droid_stub_dir, droid_stub) = write_droid_stub();

        let mut cfg = MissionConfig::default();
        cfg.orchestrator.backend = Some("droid".to_string());
        cfg.orchestrator.model = "claude-fable-5".to_string();
        cfg.worker.backend = Some("codex".to_string());
        cfg.validator_scrutiny.backend = Some("codex".to_string());
        cfg.validator_functional.backend = Some("droid".to_string());
        cfg.validator_functional.model = "claude-fable-5".to_string();

        let mock: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine =
            MissionEngine::create(mock.clone(), &root, "goal", cfg).expect("create engine");

        let codex_guard = CodexStubEnvGuard::engage(&codex_stub);
        let droid_guard = DroidStubEnvGuard::engage(&droid_stub);

        let worker = engine.select_backend(Role::Worker);
        assert_eq!(worker.kind, BackendKind::Codex);
        assert_eq!(worker.cfg.worker.model, cost::DEFAULT_CODEX_MODEL);
        assert!(
            !Arc::ptr_eq(&worker.backend, &mock),
            "worker should route to the codex backend"
        );

        let scrutiny = engine.select_backend(Role::ValidatorScrutiny);
        assert_eq!(scrutiny.kind, BackendKind::Codex);
        assert_eq!(
            scrutiny.cfg.validator_scrutiny.model,
            cost::DEFAULT_CODEX_MODEL
        );

        let functional = engine.select_backend(Role::ValidatorFunctional);
        assert_eq!(functional.kind, BackendKind::Droid);
        assert_eq!(functional.cfg.validator_functional.model, "claude-fable-5");

        let orchestrator = engine.select_backend(Role::Orchestrator);
        assert_eq!(orchestrator.kind, BackendKind::Droid);
        assert_eq!(orchestrator.cfg.orchestrator.model, "claude-fable-5");

        drop(droid_guard);
        drop(codex_guard);
    }

    #[test]
    fn local_select_routes_worker_to_local_backend() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };

        let mut cfg = MissionConfig::default();
        cfg.worker.backend = Some("local".to_string());
        cfg.worker.base_url = Some("http://127.0.0.1:9/v1".to_string());
        cfg.worker.context_budget = Some(8192);
        cfg.worker.temperature = Some(0.2);
        cfg.allow_below_default_worker_model = true;

        let mock: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine =
            MissionEngine::create(mock.clone(), &root, "goal", cfg).expect("create engine");

        let worker = engine.select_backend(Role::Worker);
        assert_eq!(worker.kind, BackendKind::Local);
        assert!(
            worker.fallback_reason.is_none(),
            "local selection must never fall back to claude"
        );
        assert!(
            !Arc::ptr_eq(&worker.backend, &mock),
            "worker should route to the local backend, not the injected claude backend"
        );
    }

    /// The stub droid's ValidatorReport findings (>=1, per the fixture) fold
    /// into the run loop through the normal machinery: `validation.finding`
    /// events, an orchestrator conversion turn, and a `fixfeature.created`
    /// event that lands the fix feature in state — exactly like a claude
    /// scrutiny run's findings would. Also asserts the run actually went
    /// through droid (droid model on the spawn event, no fallback decision).
    #[cfg(unix)]
    #[tokio::test]
    async fn droid_scrutiny_findings_flow() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (_stub_dir, stub_path) = write_droid_stub();

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            lesson_orch_script(&codex_fix_features_reply(1)),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = MissionEngine::create(backend, &root, "goal", droid_scrutiny_cfg())
            .expect("create engine");
        engine
            .state
            .mission
            .milestones
            .push(codex_scrutiny_milestone());

        let env_guard = DroidStubEnvGuard::engage(&stub_path);
        engine
            .validation_round(0)
            .await
            .expect("validation round must complete through the stub droid backend");
        drop(env_guard);

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events.jsonl");

        assert!(
            !events.iter().any(|e| matches!(
                &e.kind,
                EventKind::OrchestratorDecision { summary, .. }
                    if summary.contains("droid") && summary.contains("not available")
            )),
            "droid must not have fallen back to claude: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            !events.iter().any(|e| matches!(
                &e.kind,
                EventKind::OrchestratorDecision { summary, .. }
                    if summary.contains("retrying once")
            )),
            "droid must not have triggered the runtime retry fallback: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                EventKind::WorkerSpawned { role, model, .. }
                    if *role == Role::ValidatorScrutiny && model == cost::DEFAULT_DROID_MODEL
            )),
            "expected the scrutiny run spawned with the droid model: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::ValidationFinding { .. })),
            "expected the stub droid's findings as validation.finding events: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::FixFeatureCreated { .. })),
            "expected findings converted into a fix feature: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            engine.state().mission.milestones[0]
                .features
                .iter()
                .any(|f| f.origin == FeatureOrigin::Fix),
            "fix feature must be folded into mission state"
        );
    }

    /// The droid validator run's cost is priced with the droid (Fireworks
    /// GLM) table: the run's recorded `cost_usd` equals
    /// `cost::usage_cost_usd(usage, DEFAULT_DROID_MODEL)` for the fixture's
    /// token usage.
    #[cfg(unix)]
    #[tokio::test]
    async fn droid_scrutiny_run_priced_with_droid_table() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (_stub_dir, stub_path) = write_droid_stub();

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            lesson_orch_script(&codex_fix_features_reply(1)),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = MissionEngine::create(backend, &root, "goal", droid_scrutiny_cfg())
            .expect("create engine");
        engine
            .state
            .mission
            .milestones
            .push(codex_scrutiny_milestone());

        let env_guard = DroidStubEnvGuard::engage(&stub_path);
        engine
            .validation_round(0)
            .await
            .expect("validation round must complete through the stub droid backend");
        drop(env_guard);

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events.jsonl");
        let (usage, cost_usd) = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::WorkerCompleted {
                    tokens, cost_usd, ..
                } => Some((tokens.clone(), *cost_usd)),
                _ => None,
            })
            .expect("expected a worker.completed event for the droid scrutiny run");

        let expected = cost::usage_cost_usd(&usage, cost::DEFAULT_DROID_MODEL);
        assert!(expected > 0.0, "expected nonzero droid-priced cost");
        assert_eq!(
            cost_usd,
            Some(expected),
            "the run's recorded cost_usd must equal droid pricing for its usage"
        );
    }

    /// A droid scrutiny run that exits 0 but never emits a parseable
    /// `ValidatorReport` (empty `result` string) must trigger the bounded
    /// runtime-retry fallback exactly once: a loud `orchestrator.decision`
    /// naming the retry, mentioning "droid" and "retrying once", and that
    /// retry actually ran on the injected claude (mock) backend.
    #[cfg(unix)]
    #[tokio::test]
    async fn droid_runtime_retry_falls_back_to_claude() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (_stub_dir, stub_path) = write_droid_stub_no_report();

        let retry_report = serde_json::json!({
            "findings": [{
                "subject": "retry-finding",
                "severity": "major",
                "evidence": "claude retry scrutiny run found this after droid produced no report",
                "suggestedFix": "address it"
            }],
            "summary": "one finding from the claude retry run"
        });
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            crate::backend_mock::MockScript::single_shot_json(&retry_report),
            lesson_orch_script(&codex_fix_features_reply(1)),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine = MissionEngine::create(backend, &root, "goal", droid_scrutiny_cfg())
            .expect("create engine");
        engine
            .state
            .mission
            .milestones
            .push(codex_scrutiny_milestone());

        let env_guard = DroidStubEnvGuard::engage(&stub_path);
        engine
            .validation_round(0)
            .await
            .expect("validation round must complete via the claude retry fallback");
        drop(env_guard);

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events.jsonl");

        let retry_decisions: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    &e.kind,
                    EventKind::OrchestratorDecision { summary, .. }
                        if summary.contains("droid") && summary.contains("retrying once")
                )
            })
            .collect();
        assert_eq!(
            retry_decisions.len(),
            1,
            "expected exactly one loud retry decision naming droid: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        let scrutiny_spawns = events
            .iter()
            .filter(|e| {
                matches!(
                    &e.kind,
                    EventKind::WorkerSpawned { role, .. } if *role == Role::ValidatorScrutiny
                )
            })
            .count();
        assert_eq!(
            scrutiny_spawns,
            2,
            "expected the initial droid run plus one claude retry run: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        assert_eq!(
            mock.started_specs().len(),
            2,
            "the injected claude/mock backend must have started once for the retry \
             validator run and once for the fix-feature conversion turn"
        );

        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::FixFeatureCreated { .. })),
            "expected the claude retry's findings converted into a fix feature: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            engine.state().mission.milestones[0]
                .features
                .iter()
                .any(|f| f.origin == FeatureOrigin::Fix),
            "fix feature from the retry's findings must be folded into mission state"
        );
    }

    // -----------------------------------------------------------------------
    // Kimi scrutiny integration (f-4-1): a stubbed `kimi -p --output-format
    // stream-json` binary drives real ValidatorReport findings into the
    // fix-cycle machinery. No real API spend: everything comes from a POSIX
    // shell stub streaming a committed fixture, mirroring the droid
    // integration tests above.
    // -----------------------------------------------------------------------

    /// Synthetic kimi stream-json wire payload for a scrutiny run whose
    /// terminal assistant line is a parseable `ValidatorReport` JSON blob
    /// (mirrors the shape captured in the committed probe fixture
    /// `tests/fixtures/kimi_exec_scrutiny.jsonl`). Hand-authored harness
    /// scaffolding, not a probe capture, so it lives inline rather than as a
    /// separate fixture file.
    #[cfg(unix)]
    const KIMI_STUB_REPORT_JSONL: &str = concat!(
        r#"{"role":"assistant","content":"{\"findings\":[{\"subject\":\"assertion-3-retry-cap\",\"severity\":\"minor\",\"evidence\":\"MAX_RETRIES is defined as 3 in crates/engine/src/orchestrator.rs:42, matching the claimed retry cap.\",\"suggestedFix\":\"\"},{\"subject\":\"assertion-7-error-logging\",\"severity\":\"major\",\"evidence\":\"No structured log call found around the retry loop in orchestrator.rs; failures are silently swallowed instead of logged.\",\"suggestedFix\":\"Add a warn! log with the attempt number and error before each retry.\"}],\"summary\":\"Retry cap is correctly enforced at 3; missing structured logging on retry is the only material gap found.\"}"}"#,
        "\n",
        r#"{"role":"meta","type":"session.resume_hint","session_id":"c3d4e5f6-7a8b-4c9d-8e0f-1a2b3c4d5e6f","command":"kimi -r c3d4e5f6-7a8b-4c9d-8e0f-1a2b3c4d5e6f","content":"To resume this session: kimi -r c3d4e5f6-7a8b-4c9d-8e0f-1a2b3c4d5e6f"}"#,
        "\n"
    );

    /// Like [`KIMI_STUB_REPORT_JSONL`] but the terminal assistant text is
    /// plain prose, not JSON, so `parse_validator_report` returns `None`
    /// even though the stub exits 0. Models a kimi run that completed but
    /// never emitted a parseable report.
    #[cfg(unix)]
    const KIMI_STUB_NO_REPORT_JSONL: &str = concat!(
        r#"{"role":"assistant","content":"Done reviewing, nothing structured to report."}"#,
        "\n",
        r#"{"role":"meta","type":"session.resume_hint","session_id":"d4e5f6a7-8b9c-4d0e-9f1a-2b3c4d5e6f7a","command":"kimi -r d4e5f6a7-8b9c-4d0e-9f1a-2b3c4d5e6f7a","content":"To resume this session: kimi -r d4e5f6a7-8b9c-4d0e-9f1a-2b3c4d5e6f7a"}"#,
        "\n"
    );

    /// Writes an executable POSIX shell stub that stands in for the real
    /// `kimi` CLI closely enough to drive
    /// [`crate::backend_kimi::KimiBackend`]: `--version` prints a plausible
    /// version string and any `-p ...` invocation streams `payload` to
    /// stdout, exiting 0. Not portable to windows-latest (no `/bin/sh`),
    /// hence `cfg(unix)`.
    #[cfg(unix)]
    fn write_kimi_stub_with_payload(
        script_name: &str,
        payload_name: &str,
        payload: &str,
    ) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let payload_path = dir.path().join(payload_name);
        std::fs::write(&payload_path, payload).expect("write inline payload");
        let script_path = dir.path().join(script_name);
        std::fs::write(
            &script_path,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  echo 'kimi-cli 0.0.0-test'\n  exit 0\nfi\ncat '{}'\nexit 0\n",
                payload_path.display()
            ),
        )
        .expect("write stub script");
        let mut perms = std::fs::metadata(&script_path)
            .expect("stat stub script")
            .permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&script_path, perms).expect("chmod stub script");
        (dir, script_path)
    }

    #[cfg(unix)]
    fn write_kimi_stub() -> (tempfile::TempDir, PathBuf) {
        write_kimi_stub_with_payload(
            "kimi-stub.sh",
            "kimi_exec_scrutiny_report.jsonl",
            KIMI_STUB_REPORT_JSONL,
        )
    }

    /// Like [`write_kimi_stub`] but the stub cats [`KIMI_STUB_NO_REPORT_JSONL`].
    #[cfg(unix)]
    fn write_kimi_stub_no_report() -> (tempfile::TempDir, PathBuf) {
        write_kimi_stub_with_payload(
            "kimi-stub-no-report.sh",
            "kimi_exec_scrutiny_report_no_report.jsonl",
            KIMI_STUB_NO_REPORT_JSONL,
        )
    }

    /// RAII guard: points `KRANZ_KIMI_BIN` at a working stub so
    /// `discover_kimi_binary` deterministically resolves it as the FIRST
    /// (exclusive) candidate, regardless of whatever real `kimi` install
    /// happens to sit on the host running the suite. Serialized on
    /// [`crate::backend_kimi::KIMI_ENV_LOCK`] — the SAME mutex the
    /// `backend_kimi` discovery tests lock — so these tests never race
    /// against each other, even though they live in different source files.
    #[cfg(unix)]
    struct KimiStubEnvGuard {
        prev_bin: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    #[cfg(unix)]
    impl KimiStubEnvGuard {
        fn engage(stub: &std::path::Path) -> Self {
            let lock = crate::backend_kimi::KIMI_ENV_LOCK
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let prev_bin = std::env::var_os("KRANZ_KIMI_BIN");
            std::env::set_var("KRANZ_KIMI_BIN", stub);
            KimiStubEnvGuard {
                prev_bin,
                _lock: lock,
            }
        }
    }

    #[cfg(unix)]
    impl Drop for KimiStubEnvGuard {
        fn drop(&mut self) {
            match self.prev_bin.take() {
                Some(v) => std::env::set_var("KRANZ_KIMI_BIN", v),
                None => std::env::remove_var("KRANZ_KIMI_BIN"),
            }
        }
    }

    #[cfg(unix)]
    fn kimi_scrutiny_cfg() -> MissionConfig {
        let mut cfg = MissionConfig::default();
        cfg.validator_scrutiny.backend = Some("kimi".to_string());
        cfg.skip_functional = true;
        cfg
    }

    /// The stub kimi's ValidatorReport findings (>=1, per the fixture) fold
    /// into the run loop through the normal machinery: `validation.finding`
    /// events, an orchestrator conversion turn, and a `fixfeature.created`
    /// event that lands the fix feature in state — exactly like a claude or
    /// droid scrutiny run's findings would. Also asserts the run actually
    /// went through kimi (kimi model on the spawn event, no fallback
    /// decision).
    #[cfg(unix)]
    #[tokio::test]
    async fn kimi_scrutiny_findings_flow() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (_stub_dir, stub_path) = write_kimi_stub();

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            lesson_orch_script(&codex_fix_features_reply(1)),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = MissionEngine::create(backend, &root, "goal", kimi_scrutiny_cfg())
            .expect("create engine");
        engine
            .state
            .mission
            .milestones
            .push(codex_scrutiny_milestone());

        let env_guard = KimiStubEnvGuard::engage(&stub_path);
        engine
            .validation_round(0)
            .await
            .expect("validation round must complete through the stub kimi backend");
        drop(env_guard);

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events.jsonl");

        assert!(
            !events.iter().any(|e| matches!(
                &e.kind,
                EventKind::OrchestratorDecision { summary, .. }
                    if summary.contains("kimi") && summary.contains("not available")
            )),
            "kimi must not have fallen back to claude: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            !events.iter().any(|e| matches!(
                &e.kind,
                EventKind::OrchestratorDecision { summary, .. }
                    if summary.contains("retrying once")
            )),
            "kimi must not have triggered the runtime retry fallback: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                EventKind::WorkerSpawned { role, model, .. }
                    if *role == Role::ValidatorScrutiny && model == cost::DEFAULT_KIMI_MODEL
            )),
            "expected the scrutiny run spawned with the kimi model: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::ValidationFinding { .. })),
            "expected the stub kimi's findings as validation.finding events: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::FixFeatureCreated { .. })),
            "expected findings converted into a fix feature: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            engine.state().mission.milestones[0]
                .features
                .iter()
                .any(|f| f.origin == FeatureOrigin::Fix),
            "fix feature must be folded into mission state"
        );

        let scrutiny_run = engine
            .state()
            .runs
            .values()
            .find(|r| r.role == Role::ValidatorScrutiny)
            .expect("expected a recorded scrutiny run");
        assert_eq!(
            scrutiny_run.model,
            cost::DEFAULT_KIMI_MODEL,
            "the scrutiny run's recorded model must attribute it to BackendKind::Kimi"
        );
    }

    /// The kimi validator run's cost is priced with the kimi table: the run's
    /// recorded `cost_usd` equals `cost::usage_cost_usd(usage,
    /// DEFAULT_KIMI_MODEL)` for the fixture's (zero) token usage — kimi has
    /// no usage field on the wire, so this is effectively the Meterless
    /// floor, but it must still be priced through the kimi table rather than
    /// left unset.
    #[cfg(unix)]
    #[tokio::test]
    async fn kimi_scrutiny_run_priced_with_kimi_table() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (_stub_dir, stub_path) = write_kimi_stub();

        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            lesson_orch_script(&codex_fix_features_reply(1)),
        ]));
        let backend: Arc<dyn AgentBackend> = mock;
        let mut engine = MissionEngine::create(backend, &root, "goal", kimi_scrutiny_cfg())
            .expect("create engine");
        engine
            .state
            .mission
            .milestones
            .push(codex_scrutiny_milestone());

        let env_guard = KimiStubEnvGuard::engage(&stub_path);
        engine
            .validation_round(0)
            .await
            .expect("validation round must complete through the stub kimi backend");
        drop(env_guard);

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events.jsonl");
        let (usage, cost_usd) = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::WorkerCompleted {
                    tokens, cost_usd, ..
                } => Some((tokens.clone(), *cost_usd)),
                _ => None,
            })
            .expect("expected a worker.completed event for the kimi scrutiny run");

        let expected = cost::usage_cost_usd(&usage, cost::DEFAULT_KIMI_MODEL);
        assert_eq!(
            cost_usd,
            Some(expected),
            "the run's recorded cost_usd must equal kimi pricing for its usage"
        );
    }

    /// A kimi scrutiny run that exits 0 but never emits a parseable
    /// `ValidatorReport` (plain-prose final text) must trigger the bounded
    /// runtime-retry fallback exactly once: a loud `orchestrator.decision`
    /// naming the retry, mentioning "kimi" and "retrying once", and that
    /// retry actually ran on the injected claude (mock) backend.
    #[cfg(unix)]
    #[tokio::test]
    async fn kimi_runtime_retry_falls_back_to_claude() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let (_stub_dir, stub_path) = write_kimi_stub_no_report();

        let retry_report = serde_json::json!({
            "findings": [{
                "subject": "retry-finding",
                "severity": "major",
                "evidence": "claude retry scrutiny run found this after kimi produced no report",
                "suggestedFix": "address it"
            }],
            "summary": "one finding from the claude retry run"
        });
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            crate::backend_mock::MockScript::single_shot_json(&retry_report),
            lesson_orch_script(&codex_fix_features_reply(1)),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine = MissionEngine::create(backend, &root, "goal", kimi_scrutiny_cfg())
            .expect("create engine");
        engine
            .state
            .mission
            .milestones
            .push(codex_scrutiny_milestone());

        let env_guard = KimiStubEnvGuard::engage(&stub_path);
        engine
            .validation_round(0)
            .await
            .expect("validation round must complete via the claude retry fallback");
        drop(env_guard);

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events.jsonl");

        let retry_decisions: Vec<_> = events
            .iter()
            .filter(|e| {
                matches!(
                    &e.kind,
                    EventKind::OrchestratorDecision { summary, .. }
                        if summary.contains("kimi") && summary.contains("retrying once")
                )
            })
            .collect();
        assert_eq!(
            retry_decisions.len(),
            1,
            "expected exactly one loud retry decision naming kimi: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        let scrutiny_spawns = events
            .iter()
            .filter(|e| {
                matches!(
                    &e.kind,
                    EventKind::WorkerSpawned { role, .. } if *role == Role::ValidatorScrutiny
                )
            })
            .count();
        assert_eq!(
            scrutiny_spawns,
            2,
            "expected the initial kimi run plus one claude retry run: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );

        assert_eq!(
            mock.started_specs().len(),
            2,
            "the injected claude/mock backend must have started once for the retry \
             validator run and once for the fix-feature conversion turn"
        );

        assert!(
            events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::FixFeatureCreated { .. })),
            "expected the claude retry's findings converted into a fix feature: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        assert!(
            engine.state().mission.milestones[0]
                .features
                .iter()
                .any(|f| f.origin == FeatureOrigin::Fix),
            "fix feature from the retry's findings must be folded into mission state"
        );
    }

    // -----------------------------------------------------------------------
    // Heterogeneous dispatch pool (ticket heterogeneous-dispatch-pool,
    // KRZ-303; the positioning ADR's 2026-07-31 boundary gloss)
    // -----------------------------------------------------------------------

    fn dispatch_pool_report(summary: &str) -> serde_json::Value {
        serde_json::json!({
            "result": "pass",
            "summary": summary,
            "filesTouched": [],
            "testsAdded": [],
            "testEvidence": "",
            "dependenciesAdded": [],
            "knownGaps": [],
            "commits": [],
            "commandsRun": []
        })
    }

    fn dispatch_pool_cfg() -> MissionConfig {
        MissionConfig {
            worker_isolation: WorkerIsolation::Checkout,
            worker_candidates: vec![
                CandidateSpec {
                    backend: "claude".into(),
                    model: "sonnet".into(),
                },
                CandidateSpec {
                    backend: "codex".into(),
                    model: "gpt-5-codex".into(),
                },
            ],
            ..MissionConfig::default()
        }
    }

    fn dispatch_pool_milestone(engine: &MissionEngine) -> Milestone {
        Milestone {
            id: "ms-1".to_string(),
            title: "m".to_string(),
            features: vec![Feature {
                id: "f-1-1".to_string(),
                title: "f".to_string(),
                spec: "s".to_string(),
                validation_criteria: vec![],
                origin: FeatureOrigin::Plan,
                status: FeatureStatus::Pending,
                worker_runs: vec![],
                commits: vec![],
                respawns: 0,
            }],
            status: MilestoneStatus::Active,
            fix_cycles: 0,
            start_sha: Some(engine.repo.head_sha().unwrap()),
            validator_guidance: None,
        }
    }

    /// A passing single-shot worker script that leaves `path` dirty in its
    /// session worktree (so the pool checkpoint has a deliverable to commit).
    fn dispatch_pool_pass_script(
        summary: &str,
        path: &str,
        contents: &str,
    ) -> crate::backend_mock::MockScript {
        crate::backend_mock::MockScript::single_shot_json(&dispatch_pool_report(summary))
            .writes_file(path, contents)
    }

    /// Acceptance hint 1: one brief to two mock backends yields two sibling
    /// run records linked to one unit id, each in its own worktree — and the
    /// freeze holds: no winner, no completion, the mission parks for the
    /// human judgement act.
    #[tokio::test]
    async fn dispatch_pool_two_backends_yield_sibling_candidates() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let claude_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("claude candidate", "claude.txt", "claude was here"),
        ]));
        let codex_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("codex candidate", "codex.txt", "codex was here"),
        ]));
        let backend: Arc<dyn AgentBackend> = claude_mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", dispatch_pool_cfg()).unwrap();
        engine.seed_worker_auth_verdict_for_test(AuthVerdict::Authenticated);
        engine.seed_kind_backend_for_test(BackendKind::Codex, codex_mock.clone());
        let pre_run_sha = engine.repo.head_sha().unwrap();
        engine
            .state
            .mission
            .milestones
            .push(dispatch_pool_milestone(&engine));

        engine.run_feature(0, 0).await.unwrap();

        let mission_id = engine.mission_id().to_string();
        let state = engine.state();
        // Two sibling run records, each candidate-linked to the one unit id.
        let mut linked: Vec<&WorkerRun> = state
            .runs
            .values()
            .filter(|r| r.role == Role::Worker && r.candidate.is_some())
            .collect();
        linked.sort_by_key(|r| r.candidate.as_ref().unwrap().index);
        assert_eq!(linked.len(), 2, "expected two candidate-linked run records");
        assert_eq!(
            linked[0].candidate,
            Some(CandidateLink {
                unit: "f-1-1".to_string(),
                index: 0,
                count: 2,
                backend: "claude".to_string(),
            })
        );
        assert_eq!(
            linked[1].candidate,
            Some(CandidateLink {
                unit: "f-1-1".to_string(),
                index: 1,
                count: 2,
                backend: "codex".to_string(),
            })
        );
        // Each stream ran in its OWN worktree (the M3 isolation idiom), and
        // the worktree dirs are reaped afterwards while the branches persist.
        let claude_specs = claude_mock.started_specs();
        let codex_specs = codex_mock.started_specs();
        assert_eq!(claude_specs.len(), 1, "claude stream ran exactly once");
        assert_eq!(codex_specs.len(), 1, "codex stream ran exactly once");
        let c0_path = pool_worktree_path(&root, &mission_id, "f-1-1", 0);
        let c1_path = pool_worktree_path(&root, &mission_id, "f-1-1", 1);
        assert_eq!(claude_specs[0].cwd, c0_path);
        assert_eq!(codex_specs[0].cwd, c1_path);
        assert_ne!(c0_path, c1_path, "streams must not share a worktree");
        assert!(
            !c0_path.exists() && !c1_path.exists(),
            "worktree dirs are reaped after the dispatch; branches carry the deliverables"
        );
        // The candidate branches are kept, each carrying its stream's
        // checkpointed deliverable (the mock's dirty write).
        for (index, file) in [(0usize, "claude.txt"), (1usize, "codex.txt")] {
            let branch = format!("kranz/pool/{mission_id}/f-1-1-c{index}");
            assert!(
                engine.repo.branch_exists(&branch).unwrap(),
                "candidate branch {branch} must be kept for judgement"
            );
            let commits = engine.repo.commits_between(&pre_run_sha, &branch).unwrap();
            assert_eq!(
                commits.len(),
                1,
                "candidate {index} branch carries exactly its checkpoint commit"
            );
            let shown = engine
                .repo
                .show_file(&branch, file)
                .expect("git show works")
                .expect("candidate branch carries the stream's file");
            let shown = String::from_utf8(shown).unwrap();
            assert!(shown.contains("was here"), "{file} on {branch}: {shown}");
        }
        // Siblings are one logical dispatch: no respawn budget charged.
        let feature = &state.mission.milestones[0].features[0];
        assert_eq!(feature.worker_runs.len(), 2);
        assert_eq!(feature.respawns, 0);

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        // The dispatch decision names N and proves the wall-clock overlap.
        let decision = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::OrchestratorDecision { summary, detail }
                    if summary.starts_with("dispatch pool:") =>
                {
                    Some((summary.clone(), detail.clone().unwrap_or_default()))
                }
                _ => None,
            })
            .expect("a dispatch pool decision must be recorded");
        assert!(
            decision
                .0
                .contains("unit f-1-1 fanned out to 2 candidates (peak 2 concurrent)"),
            "decision names N and the overlap: {}",
            decision.0
        );
        assert!(
            decision.1.contains("CANDIDATE FOR JUDGEMENT")
                && decision
                    .1
                    .contains("divergence for scrutiny, not throughput"),
            "the decision detail states the freeze properties: {}",
            decision.1
        );
        // Both terminal states recorded (both passed here).
        let completed: Vec<RunResult> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::WorkerCompleted { result, .. } => Some(*result),
                _ => None,
            })
            .collect();
        assert_eq!(completed, vec![RunResult::Pass, RunResult::Pass]);
        // The freeze: no winner — the unit is neither completed nor failed,
        // and the milestone parks for the human judgement act.
        assert!(
            !events.iter().any(|e| matches!(
                &e.kind,
                EventKind::FeatureCompleted { feature_id, .. } | EventKind::FeatureFailed { feature_id, .. }
                if feature_id == "f-1-1"
            )),
            "no code path completes or fails the unit from a candidate"
        );
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                EventKind::MilestoneBlocked { milestone_id, reason }
                if milestone_id == "ms-1" && reason.contains("candidate for judgement")
            )),
            "the milestone must park for judgement: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
    }

    /// Acceptance hint 3: one stream failing (a crashed backend session) does
    /// not abort its sibling — both terminal states are recorded.
    #[tokio::test]
    async fn dispatch_pool_one_stream_failure_keeps_sibling_terminal_state() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let claude_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("claude candidate", "claude.txt", "claude was here"),
        ]));
        let codex_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            crate::backend_mock::MockScript::single_shot_json(&dispatch_pool_report(
                "codex claimed pass before dying",
            ))
            .with_exit(SessionExit::Failed("codex exploded".to_string())),
        ]));
        let backend: Arc<dyn AgentBackend> = claude_mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", dispatch_pool_cfg()).unwrap();
        engine.seed_worker_auth_verdict_for_test(AuthVerdict::Authenticated);
        engine.seed_kind_backend_for_test(BackendKind::Codex, codex_mock.clone());
        engine
            .state
            .mission
            .milestones
            .push(dispatch_pool_milestone(&engine));

        // The sibling's failure must not error the dispatch itself.
        engine.run_feature(0, 0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        // Both streams got run records (replayed in candidate order) with
        // their own terminal states: pass for the survivor, fail for the
        // crashed sibling.
        let spawns: Vec<Option<CandidateLink>> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::WorkerSpawned { candidate, .. } => Some(candidate.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(spawns.len(), 2, "both streams spawned: {spawns:?}");
        assert_eq!(spawns[0].as_ref().map(|c| c.index), Some(0));
        assert_eq!(spawns[1].as_ref().map(|c| c.index), Some(1));
        let completed: Vec<RunResult> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::WorkerCompleted { result, .. } => Some(*result),
                _ => None,
            })
            .collect();
        assert_eq!(
            completed,
            vec![RunResult::Pass, RunResult::Fail],
            "both terminal states recorded, in candidate order"
        );
        // The failure is named in the dispatch record, and the mission still
        // parks for judgement (never auto-completes from the survivor).
        let detail = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::OrchestratorDecision { summary, detail }
                    if summary.starts_with("dispatch pool:") =>
                {
                    detail.clone()
                }
                _ => None,
            })
            .expect("dispatch decision recorded");
        assert!(detail.contains("run Fail"), "failed stream named: {detail}");
        assert!(
            detail.contains("run Pass"),
            "surviving stream named: {detail}"
        );
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            EventKind::MilestoneBlocked { milestone_id, .. } if milestone_id == "ms-1"
        )));
        assert!(!events.iter().any(|e| matches!(
            &e.kind,
            EventKind::FeatureCompleted { feature_id, .. } if feature_id == "f-1-1"
        )));
    }

    /// 12th-pass review (P1): the pool checkpoint reopens each candidate's
    /// HOSTILE worktree and runs status/commit there with the engine's
    /// ambient privileges. A worker that planted `core.fsmonitor` in the
    /// (shared) git config or a hook in the (shared) hooks dir must never
    /// get its payload EXECUTED by those engine git invocations — and the
    /// checkpoint must still commit the deliverable. Fixture idiom mirrors
    /// validator_integrity's planted-fsmonitor test.
    #[cfg(unix)]
    #[tokio::test]
    async fn pool_checkpoint_hooks_disabled_against_planted_fsmonitor_and_hook() {
        use std::os::unix::fs::PermissionsExt as _;
        // Premise-gate (ticket gate-sandbox-supervision-dogfood): the planted
        // fsmonitor payload identifies its parent via `ps -p $PPID`, but
        // `/bin/ps` is setuid root on this host's macOS and setuid exec is
        // kernel-denied inside ANY Seatbelt sandbox (probed 2026-08-05 —
        // EPERM even under `(allow default)`, not SBPL-expressible). Under
        // a wrapped `cargo test` the payload can never log, so the
        // anti-vacuity assertion below would fail on the sandbox's presence
        // rather than the engine's behavior — skip with a detectable
        // marker, the same posture as the nested-sandbox skips.
        if std::process::Command::new("ps")
            .args(["-p", &std::process::id().to_string(), "-o", "command="])
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!(
                "SKIP-UNDER-WRAP (gate-sandbox-supervision-dogfood): \
                 pool_checkpoint_hooks_disabled_against_planted_fsmonitor_and_hook — \
                 /bin/ps cannot execute inside the gate sandbox wrap, so the fsmonitor \
                 payload's identity logging is unobservable here; skipping"
            );
            return;
        }
        let Some((dir, root)) = lessons_test_repo() else {
            return;
        };
        let claude_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("claude candidate", "claude.txt", "claude was here"),
        ]));
        let codex_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("codex candidate", "codex.txt", "codex was here"),
        ]));
        let backend: Arc<dyn AgentBackend> = claude_mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", dispatch_pool_cfg()).unwrap();
        engine.seed_worker_auth_verdict_for_test(AuthVerdict::Authenticated);
        engine.seed_kind_backend_for_test(BackendKind::Codex, codex_mock.clone());
        engine
            .state
            .mission
            .milestones
            .push(dispatch_pool_milestone(&engine));

        // Arm the hostile metadata the way a worker would from inside its
        // session (a linked worktree shares the main repo's git dir): a
        // `core.fsmonitor` command (fired by `git status`) and a pre-commit
        // hook (fired by `git commit`). The fsmonitor payload logs its PARENT
        // command line so the assertion below can tell the checkpoint's own
        // status/commit apart from Phase A machinery; both payloads log
        // OUTSIDE the repo so they can never become deliverable content.
        let fsmonitor_log = dir.path().join("fsmonitor-invocations");
        let hook_log = dir.path().join("hook-invocations");
        let fsmonitor = dir.path().join("evil-fsmonitor");
        std::fs::write(
            &fsmonitor,
            format!(
                "#!/bin/sh\nps -p $PPID -o command= >> '{}'\nexit 1\n",
                fsmonitor_log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fsmonitor, std::fs::Permissions::from_mode(0o755)).unwrap();
        let hook = root.join(".git/hooks/pre-commit");
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\necho \"pre-commit:$PWD\" >> '{}'\n",
                hook_log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        git(&["config", "core.fsmonitor", fsmonitor.to_str().unwrap()]);

        // Fixture proof (validator-integrity idiom): ORDINARY git invocations
        // execute both payloads — then reset the logs so any later invocation
        // can only have come from the engine's dispatch.
        git(&["status", "--porcelain"]);
        git(&["commit", "--allow-empty", "-m", "fixture probe"]);
        assert!(
            std::fs::read_to_string(&fsmonitor_log)
                .map(|hits| !hits.is_empty())
                .unwrap_or(false),
            "fixture: ordinary git status runs the planted fsmonitor"
        );
        assert!(
            std::fs::read_to_string(&hook_log)
                .map(|hits| !hits.is_empty())
                .unwrap_or(false),
            "fixture: ordinary git commit runs the planted pre-commit hook"
        );
        std::fs::remove_file(&fsmonitor_log).unwrap();
        std::fs::remove_file(&hook_log).unwrap();

        let pre_run_sha = engine.repo.head_sha().unwrap();
        engine.run_feature(0, 0).await.unwrap();

        // The checkpoint's own git never executed either payload. The
        // fsmonitor log may hold `git worktree add`'s INTERNAL `reset --hard`
        // (Phase A fork, which populates each new worktree via a child reset
        // that refreshes its index) — that runs BEFORE the worker session
        // could have planted anything, so it is not the checkpoint surface
        // this finding covers; what must never appear is a checkpoint-shaped
        // invocation (status/add/commit) executing the planted payload. The
        // pre-commit hook has no such pre-worker noise: it must not fire at
        // all.
        let fsmonitor_hits = std::fs::read_to_string(&fsmonitor_log).unwrap_or_default();
        for line in fsmonitor_hits.lines() {
            assert!(
                line.contains("reset --hard"),
                "only worktree-add's internal reset may consult the planted fsmonitor — \
                 the checkpoint's own status/add/commit must never execute it: {fsmonitor_hits}"
            );
        }
        assert!(
            !hook_log.exists(),
            "the checkpoint must never execute the planted hook: {}",
            std::fs::read_to_string(&hook_log).unwrap_or_default()
        );

        // And the happy path still commits both deliverables — the checkpoint
        // commit lands with hooks disabled.
        let mission_id = engine.mission_id().to_string();
        for (index, file) in [(0usize, "claude.txt"), (1usize, "codex.txt")] {
            let branch = format!("kranz/pool/{mission_id}/f-1-1-c{index}");
            let commits = engine.repo.commits_between(&pre_run_sha, &branch).unwrap();
            assert_eq!(
                commits.len(),
                1,
                "candidate {index} carries exactly its checkpoint commit"
            );
            let shown = engine
                .repo
                .show_file(&branch, file)
                .expect("git show works")
                .expect("candidate branch carries the deliverable");
            assert!(String::from_utf8(shown).unwrap().contains("was here"));
        }
    }

    /// 12th-pass review (P2): a candidate whose worktree cannot be INSPECTED
    /// at the checkpoint (here: the worker removed its `.git`) was once read
    /// as "clean, 0 commits" and REAPED with its deliverable inside. Now the
    /// candidate is recorded FAILED exactly where stream failures are
    /// recorded, its worktree dir + branch survive — and the sibling's happy
    /// path is byte-identical.
    #[tokio::test]
    async fn candidate_inspection_failure_fails_pool_candidate_and_preserves_bytes() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let claude_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("claude candidate", "claude.txt", "claude was here")
                .removes_path(".git"),
        ]));
        let codex_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("codex candidate", "codex.txt", "codex was here"),
        ]));
        let backend: Arc<dyn AgentBackend> = claude_mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", dispatch_pool_cfg()).unwrap();
        engine.seed_worker_auth_verdict_for_test(AuthVerdict::Authenticated);
        engine.seed_kind_backend_for_test(BackendKind::Codex, codex_mock.clone());
        let pre_run_sha = engine.repo.head_sha().unwrap();
        engine
            .state
            .mission
            .milestones
            .push(dispatch_pool_milestone(&engine));

        // The inspection failure must not error the dispatch itself.
        engine.run_feature(0, 0).await.unwrap();

        let mission_id = engine.mission_id().to_string();
        // Candidate 0's worktree dir SURVIVES with the deliverable bytes
        // inside (nothing was verified, so nothing is destroyed)…
        let c0_path = pool_worktree_path(&root, &mission_id, "f-1-1", 0);
        assert!(
            c0_path.exists(),
            "an uninspectable candidate's worktree dir must be preserved, not reaped"
        );
        assert_eq!(
            std::fs::read_to_string(c0_path.join("claude.txt")).unwrap(),
            "claude was here",
            "the unverified deliverable bytes survive for human inspection"
        );
        // … and so does its branch.
        let c0_branch = format!("kranz/pool/{mission_id}/f-1-1-c0");
        assert!(
            engine.repo.branch_exists(&c0_branch).unwrap(),
            "an uninspectable candidate's branch must be preserved"
        );
        // The healthy sibling is reaped exactly as before — only the failed
        // candidate is preserved.
        let c1_path = pool_worktree_path(&root, &mission_id, "f-1-1", 1);
        assert!(
            !c1_path.exists(),
            "the healthy sibling's worktree dir is reaped as before"
        );
        let c1_branch = format!("kranz/pool/{mission_id}/f-1-1-c1");
        let sibling_commits = engine
            .repo
            .commits_between(&pre_run_sha, &c1_branch)
            .unwrap();
        assert_eq!(
            sibling_commits.len(),
            1,
            "the sibling's checkpoint commit still lands"
        );

        // The failure is recorded where stream failures are recorded: the
        // dispatch decision detail. The sibling's line keeps its exact
        // happy-path shape.
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        let detail = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::OrchestratorDecision { summary, detail }
                    if summary.starts_with("dispatch pool:") =>
                {
                    detail.clone()
                }
                _ => None,
            })
            .expect("dispatch decision recorded");
        assert!(
            detail.contains("- candidate 0/1: `claude` / `sonnet` → branch `kranz/pool/")
                && detail.contains("worktree inspection failed")
                && detail.contains("preserved for inspection"),
            "the inspection failure is the candidate's recorded terminal state: {detail}"
        );
        assert!(
            detail.contains("- candidate 1/1: `codex` / `gpt-5-codex` → branch `kranz/pool/")
                && detail.contains("— run Pass, 1 commit(s)"),
            "the sibling's decision line keeps its byte-identical happy-path shape: {detail}"
        );
        // Both streams still completed Pass (the failure is at the
        // checkpoint, after the runs), and the freeze holds: the unit is
        // neither completed nor failed from a candidate.
        let completed: Vec<RunResult> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::WorkerCompleted { result, .. } => Some(*result),
                _ => None,
            })
            .collect();
        assert_eq!(completed, vec![RunResult::Pass, RunResult::Pass]);
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            EventKind::MilestoneBlocked { milestone_id, .. } if milestone_id == "ms-1"
        )));
        assert!(!events.iter().any(|e| matches!(
            &e.kind,
            EventKind::FeatureCompleted { feature_id, .. } | EventKind::FeatureFailed { feature_id, .. }
            if feature_id == "f-1-1"
        )));

        // The preserved dir lives in the shared temp dir (outside the
        // repo tempdir) — sweep it so the test leaves nothing behind.
        let _ = std::fs::remove_dir_all(&c0_path);
    }

    /// 13th-pass review (P2): a candidate whose inspection FAILED still has
    /// a run record (its stream completed Pass), so run-record presence
    /// alone once let it into the divergence comparison — letting
    /// rejected/untouched bytes produce an apparent agreement or
    /// divergence. Now only successfully inspected candidates participate:
    /// with one of two streams uninspectable there is ONE eligible
    /// candidate, below the two-candidate floor, so NO record is emitted at
    /// all — and the surviving posture still parks for judgement.
    #[tokio::test]
    async fn divergence_eligibility_excludes_failed_inspection_candidates() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let claude_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("claude candidate", "claude.txt", "claude was here")
                .removes_path(".git"),
        ]));
        let codex_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("codex candidate", "codex.txt", "codex was here"),
        ]));
        let backend: Arc<dyn AgentBackend> = claude_mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", dispatch_pool_cfg()).unwrap();
        engine.seed_worker_auth_verdict_for_test(AuthVerdict::Authenticated);
        engine.seed_kind_backend_for_test(BackendKind::Codex, codex_mock.clone());
        engine
            .state
            .mission
            .milestones
            .push(dispatch_pool_milestone(&engine));

        engine.run_feature(0, 0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        // Crux: BOTH streams still carry run records with terminal Pass —
        // eligibility must NOT be inferred from that alone…
        let completed: Vec<RunResult> = events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::WorkerCompleted { result, .. } => Some(*result),
                _ => None,
            })
            .collect();
        assert_eq!(
            completed,
            vec![RunResult::Pass, RunResult::Pass],
            "both streams completed; only the INSPECTION failed"
        );
        // …and with just one inspected candidate there is NO comparison:
        // no divergence record, and no vacuous one-stream "agreement".
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::DivergenceNoted { .. })),
            "a failed-inspection candidate must not join the comparison — \
             fewer than two eligible candidates means NO record: {:?}",
            events
                .iter()
                .filter(|e| matches!(&e.kind, EventKind::DivergenceNoted { .. }))
                .map(|e| &e.kind)
                .collect::<Vec<_>>()
        );
        // The surviving posture still parks for judgement, with the
        // inspection failure named in the dispatch record.
        let detail = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::OrchestratorDecision { summary, detail }
                    if summary.starts_with("dispatch pool:") =>
                {
                    detail.clone()
                }
                _ => None,
            })
            .expect("dispatch decision recorded");
        assert!(
            detail.contains("worktree inspection failed"),
            "the failed candidate is named in the decision detail: {detail}"
        );
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            EventKind::MilestoneBlocked { milestone_id, .. } if milestone_id == "ms-1"
        )));
        assert!(!events.iter().any(|e| matches!(
            &e.kind,
            EventKind::FeatureCompleted { feature_id, .. } | EventKind::FeatureFailed { feature_id, .. }
            if feature_id == "f-1-1"
        )));

        // Sweep the preserved worktree dir (shared temp dir, outside the
        // repo tempdir) so the test leaves nothing behind.
        let mission_id = engine.mission_id().to_string();
        let c0_path = pool_worktree_path(&root, &mission_id, "f-1-1", 0);
        let _ = std::fs::remove_dir_all(&c0_path);
    }

    /// 12th-pass review (P2, parallel-batch half): the M3 checkpoint treats
    /// an uninspectable worktree the same way — the feature is failed via
    /// the checkpoint decision record, and the cleanup guard spares BOTH its
    /// worktree dir and its branch. Both workers sabotage their own `.git`
    /// so the (racy) script→feature assignment cannot make the outcome
    /// nondeterministic; the happy-path half of the guard is covered by the
    /// existing parallel integration tests and the pool sibling above.
    #[tokio::test]
    async fn candidate_inspection_failure_fails_parallel_feature_and_preserves_bytes() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("one", "deliverable.txt", "worker output")
                .removes_path(".git"),
            dispatch_pool_pass_script("two", "deliverable.txt", "worker output")
                .removes_path(".git"),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let mut engine = MissionEngine::create(
            backend,
            &root,
            "goal",
            MissionConfig {
                worker_isolation: WorkerIsolation::Checkout,
                ..MissionConfig::default()
            },
        )
        .unwrap();
        engine.seed_worker_auth_verdict_for_test(AuthVerdict::Authenticated);
        // Two Pending plan features on one Active milestone.
        let mut milestone = dispatch_pool_milestone(&engine);
        milestone.features.push(Feature {
            id: "f-1-2".to_string(),
            title: "f".to_string(),
            spec: "s".to_string(),
            validation_criteria: vec![],
            origin: FeatureOrigin::Plan,
            status: FeatureStatus::Pending,
            worker_runs: vec![],
            commits: vec![],
            respawns: 0,
        });
        engine.state.mission.milestones.push(milestone);

        engine
            .run_parallel_batch(0, &[("f-1-1".to_string(), 0), ("f-1-2".to_string(), 1)])
            .await
            .unwrap();

        let mission_id = engine.mission_id().to_string();
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        for feature_id in ["f-1-1", "f-1-2"] {
            // The checkpoint decision records the inspection failure…
            assert!(
                events.iter().any(|e| matches!(
                    &e.kind,
                    EventKind::OrchestratorDecision { summary, .. }
                        if summary == &format!(
                            "parallel checkpoint for {feature_id}: worktree inspection failed"
                        )
                )),
                "inspection failure decision recorded for {feature_id}"
            );
            // … the feature is failed with the preservation named…
            assert!(
                events.iter().any(|e| matches!(
                    &e.kind,
                    EventKind::FeatureFailed { feature_id: fid, reason }
                        if fid == feature_id
                            && reason.contains("worktree inspection failed")
                            && reason.contains("preserved")
                )),
                "feature.failed names the preservation for {feature_id}"
            );
            // … and BOTH the worktree dir (deliverable bytes inside) and its
            // branch survive the cleanup guard.
            let wt = parallel_worktree_path(&root, &mission_id, feature_id);
            assert!(
                wt.exists(),
                "{feature_id}'s uninspectable worktree dir must be preserved"
            );
            assert_eq!(
                std::fs::read_to_string(wt.join("deliverable.txt")).unwrap(),
                "worker output",
                "{feature_id}'s unverified deliverable bytes survive"
            );
            assert!(
                engine
                    .repo
                    .branch_exists(&format!("kranz/wt/{mission_id}/{feature_id}"))
                    .unwrap(),
                "{feature_id}'s branch must be preserved"
            );
        }
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::FeatureCompleted { .. })),
            "nothing merges from an unverified worktree"
        );

        // The preserved dirs live in the shared temp dir — sweep them.
        for feature_id in ["f-1-1", "f-1-2"] {
            let _ = std::fs::remove_dir_all(parallel_worktree_path(&root, &mission_id, feature_id));
        }
    }

    /// Failure isolation at the spawn boundary: a candidate whose backend
    /// cannot start gets NO fabricated run record — its terminal state is
    /// recorded in the dispatch decision, and its sibling runs unaffected.
    #[tokio::test]
    async fn dispatch_pool_spawn_failure_records_stream_terminal_state() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let claude_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("claude candidate", "claude.txt", "claude was here"),
        ]));
        // No scripts queued: start() errors, exactly a backend-unavailable
        // spawn failure.
        let codex_mock = Arc::new(crate::backend_mock::MockBackend::new());
        let backend: Arc<dyn AgentBackend> = claude_mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", dispatch_pool_cfg()).unwrap();
        engine.seed_worker_auth_verdict_for_test(AuthVerdict::Authenticated);
        engine.seed_kind_backend_for_test(BackendKind::Codex, codex_mock.clone());
        engine
            .state
            .mission
            .milestones
            .push(dispatch_pool_milestone(&engine));

        engine.run_feature(0, 0).await.unwrap();

        assert_eq!(claude_mock.started_specs().len(), 1);
        assert_eq!(
            codex_mock.started_specs().len(),
            0,
            "the failed stream never started a session"
        );
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        let spawns: Vec<&EventKind> = events
            .iter()
            .filter_map(|e| match &e.kind {
                kind @ EventKind::WorkerSpawned { .. } => Some(kind),
                _ => None,
            })
            .collect();
        assert_eq!(
            spawns.len(),
            1,
            "only the surviving stream has a run record — never a fabricated one: {spawns:?}"
        );
        let detail = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::OrchestratorDecision { summary, detail }
                    if summary.starts_with("dispatch pool:") =>
                {
                    detail.clone()
                }
                _ => None,
            })
            .expect("dispatch decision recorded");
        assert!(
            detail.contains("stream failed, no run record") && detail.contains("no script queued"),
            "the spawn failure is the stream's recorded terminal state: {detail}"
        );
        // The survivor's sibling linkage still names the full sibling set.
        match spawns[0] {
            EventKind::WorkerSpawned { candidate, .. } => {
                let link = candidate.as_ref().expect("survivor is candidate-linked");
                assert_eq!(link.count, 2);
                assert_eq!(link.unit, "f-1-1");
            }
            _ => unreachable!("filtered to spawned"),
        }
        assert!(events.iter().any(|e| matches!(
            &e.kind,
            EventKind::MilestoneBlocked { milestone_id, reason }
            if milestone_id == "ms-1" && reason.contains("1/2 candidate stream(s)")
        )));
    }

    /// The re-dispatch guard: a unit with a recorded candidate set is never
    /// fanned out again silently (each dispatch is N paid sessions) — it
    /// re-parks with the same judgement-pending reason.
    #[tokio::test]
    async fn dispatch_pool_redispatch_guard_never_refans_silently() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let claude_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("claude candidate", "claude.txt", "claude was here"),
        ]));
        let codex_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("codex candidate", "codex.txt", "codex was here"),
        ]));
        let backend: Arc<dyn AgentBackend> = claude_mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", dispatch_pool_cfg()).unwrap();
        engine.seed_worker_auth_verdict_for_test(AuthVerdict::Authenticated);
        engine.seed_kind_backend_for_test(BackendKind::Codex, codex_mock.clone());
        engine
            .state
            .mission
            .milestones
            .push(dispatch_pool_milestone(&engine));

        engine.run_feature(0, 0).await.unwrap();
        engine.run_feature(0, 0).await.unwrap();

        assert_eq!(claude_mock.started_specs().len(), 1, "no silent re-fan-out");
        assert_eq!(codex_mock.started_specs().len(), 1, "no silent re-fan-out");
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        let blocked = events
            .iter()
            .filter(|e| matches!(&e.kind, EventKind::MilestoneBlocked { .. }))
            .count();
        assert_eq!(
            blocked, 1,
            "the milestone is already parked; the guard must not spam duplicate blocks"
        );
        let spawns = events
            .iter()
            .filter(|e| matches!(&e.kind, EventKind::WorkerSpawned { .. }))
            .count();
        assert_eq!(spawns, 2, "exactly the first dispatch's two streams ran");
    }

    /// Acceptance hint 2 (consent): plan approval names N and the multiplied
    /// estimate — the pool's cost multiplier is explicit in the surface the
    /// operator approves, and the persisted estimate prices the SUM.
    #[tokio::test]
    async fn dispatch_pool_plan_approval_consent_names_n_and_multiplied_estimate() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let cfg = dispatch_pool_cfg();
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg.clone()).unwrap();
        let plan = Plan {
            goal: "g".into(),
            validation_contract: vec![],
            milestones: vec![PlanMilestone {
                title: "m".into(),
                features: vec![PlanFeature {
                    title: "f".into(),
                    spec: "s".into(),
                    validation_criteria: vec![],
                }],
            }],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
        };

        engine.approve_plan(plan.clone()).unwrap();

        // What the operator consents to: the fresh-repo calibration is the
        // built-in default band, so the approval estimate is the raw
        // pool-multiplied estimate.
        let expected = cost::estimate(&plan, &cfg, &cost::EstimateParams::default());
        let single = cost::estimate(
            &plan,
            &MissionConfig {
                worker_candidates: vec![],
                ..cfg.clone()
            },
            &cost::EstimateParams::default(),
        );
        assert_eq!(expected.worker_runs, single.worker_runs * 2.0);

        let plan_md = std::fs::read_to_string(engine.paths().plan_md_file()).unwrap();
        assert!(
            plan_md.contains("## Dispatch pool — 2 candidates per unit of work"),
            "plan.md names N:\n{plan_md}"
        );
        assert!(
            plan_md.contains("`claude` / `sonnet`") && plan_md.contains("`codex` / `gpt-5-codex`"),
            "plan.md names the candidates:\n{plan_md}"
        );
        assert!(
            plan_md.contains("Cost multiplies by 2")
                && plan_md.contains("budget applies to that SUM"),
            "plan.md states the multiplier and the sum-budget:\n{plan_md}"
        );
        assert!(
            plan_md.contains("candidate for judgement") && plan_md.contains("not throughput"),
            "plan.md states the freeze properties:\n{plan_md}"
        );
        assert!(
            plan_md.contains(&format!("expected ~${:.2}", expected.expected_usd)),
            "plan.md renders the MULTIPLIED estimate (${:.2}), not the single-backend one (${:.2}):\n{plan_md}",
            expected.expected_usd,
            single.expected_usd
        );
        // The persisted approval estimate (what the completion report will
        // compare actuals against) is the multiplied one.
        let persisted: cost::CostEstimate =
            serde_json::from_str(&std::fs::read_to_string(engine.paths().estimate_file()).unwrap())
                .unwrap();
        assert_eq!(persisted.expected_usd, expected.expected_usd);
        assert_eq!(persisted.worker_runs, expected.worker_runs);
    }

    /// Acceptance hint 2 (regression): an empty pool is today's exact
    /// single-backend behavior — the sequential run/judge path, no candidate
    /// linkage anywhere.
    #[tokio::test]
    async fn dispatch_pool_absent_pool_is_single_backend_regression() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let report = dispatch_pool_report("did the thing");
        let mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            crate::backend_mock::MockScript::single_shot("auth ok"),
            crate::backend_mock::MockScript::single_shot_json(&report)
                .with_exit(SessionExit::Aborted),
        ]));
        let backend: Arc<dyn AgentBackend> = mock.clone();
        let cfg = MissionConfig {
            max_respawns: 0,
            worker_isolation: WorkerIsolation::Checkout,
            ..MissionConfig::default()
        };
        assert!(
            cfg.worker_candidates.is_empty(),
            "default config has no pool"
        );
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg).unwrap();
        engine
            .state
            .mission
            .milestones
            .push(dispatch_pool_milestone(&engine));

        engine.run_feature(0, 0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        // The sequential path: one run, no candidate linkage, the existing
        // fail/respawn judgement — no pool parking.
        let spawns: Vec<&EventKind> = events
            .iter()
            .filter_map(|e| match &e.kind {
                kind @ EventKind::WorkerSpawned { .. } => Some(kind),
                _ => None,
            })
            .collect();
        assert_eq!(spawns.len(), 1);
        match spawns[0] {
            EventKind::WorkerSpawned { candidate, .. } => assert_eq!(*candidate, None),
            _ => unreachable!(),
        }
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                EventKind::FeatureFailed { feature_id, .. } if feature_id == "f-1-1"
            )),
            "the sequential judgement still fails the feature"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneBlocked { .. })),
            "no pool parking on the single-backend path"
        );
        assert!(
            !events.iter().any(|e| matches!(
                &e.kind,
                EventKind::OrchestratorDecision { summary, .. } if summary.starts_with("dispatch pool:")
            )),
            "no pool decision on the single-backend path"
        );
    }

    // -----------------------------------------------------------------------
    // Divergence as a first-class event (ticket divergence-first-class-event,
    // KRZ-304)
    // -----------------------------------------------------------------------

    /// The streaming orchestrator session for the resolution tests: one
    /// init/ready pair, then one scripted reply per unblock decision turn.
    fn divergence_orch_script(replies: Vec<String>) -> crate::backend_mock::MockScript {
        use crate::backend_mock::{mock_init, mock_result_text, mock_text};
        crate::backend_mock::MockScript::streaming(vec![
            mock_init("orch-session"),
            mock_result_text("ready"),
        ])
        .responding(
            replies
                .iter()
                .map(|reply| vec![mock_text(reply), mock_result_text(reply)])
                .collect(),
        )
    }

    /// Acceptance hint 1 (noted): two divergent candidate diffs produce ONE
    /// divergence record referencing BOTH candidates — run ids, branch refs,
    /// backends, and the exact tree hashes the verdict was computed from —
    /// emitted before the milestone parks for judgement.
    #[tokio::test]
    async fn divergence_event_divergent_candidates_record_references_both_streams() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let claude_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("claude candidate", "claude.txt", "claude was here"),
        ]));
        let codex_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("codex candidate", "codex.txt", "codex was here"),
        ]));
        let backend: Arc<dyn AgentBackend> = claude_mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", dispatch_pool_cfg()).unwrap();
        engine.seed_worker_auth_verdict_for_test(AuthVerdict::Authenticated);
        engine.seed_kind_backend_for_test(BackendKind::Codex, codex_mock.clone());
        engine
            .state
            .mission
            .milestones
            .push(dispatch_pool_milestone(&engine));

        engine.run_feature(0, 0).await.unwrap();

        let mission_id = engine.mission_id().to_string();
        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        let noted: Vec<&Event> = events
            .iter()
            .filter(|e| matches!(&e.kind, EventKind::DivergenceNoted { .. }))
            .collect();
        assert_eq!(noted.len(), 1, "exactly one comparison record per unit");
        let EventKind::DivergenceNoted {
            unit,
            candidates,
            diverged,
        } = &noted[0].kind
        else {
            unreachable!()
        };
        assert_eq!(unit, "f-1-1");
        assert!(diverged, "different contents must record a divergence");
        assert_eq!(candidates.len(), 2, "the record references BOTH streams");
        // Candidate order is stream order; every ref (run id, branch,
        // backend, tree) names the candidate diff it was computed from.
        let expected_runs: Vec<String> = {
            let mut linked: Vec<&WorkerRun> = engine
                .state()
                .runs
                .values()
                .filter(|r| r.candidate.is_some())
                .collect();
            linked.sort_by_key(|r| r.candidate.as_ref().unwrap().index);
            linked.iter().map(|r| r.id.clone()).collect()
        };
        for (index, candidate) in candidates.iter().enumerate() {
            let branch = format!("kranz/pool/{mission_id}/f-1-1-c{index}");
            assert_eq!(candidate.run_id, expected_runs[index]);
            assert_eq!(candidate.branch, branch);
            assert_eq!(
                candidate.tree,
                engine
                    .repo
                    .rev_parse(&format!("{branch}^{{tree}}"))
                    .unwrap(),
                "the tree hash pins the exact candidate bytes"
            );
        }
        assert_eq!(candidates[0].backend, "claude");
        assert_eq!(candidates[1].backend, "codex");
        assert_ne!(
            candidates[0].tree, candidates[1].tree,
            "divergent streams carry distinct tree hashes"
        );
        // The record lands BEFORE the park it explains.
        let noted_seq = noted[0].seq;
        let blocked_seq = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::MilestoneBlocked { milestone_id, .. } if milestone_id == "ms-1" => {
                    Some(e.seq)
                }
                _ => None,
            })
            .expect("the milestone parks for judgement");
        assert!(
            noted_seq < blocked_seq,
            "the record precedes the park: noted seq {noted_seq}, blocked seq {blocked_seq}"
        );
    }

    /// Acceptance hint 3 (agreement, the load-bearing rule): identical
    /// candidate trees produce the agreement record (`diverged: false`) —
    /// **logged, never trusted**: the park posture is byte-identical to the
    /// divergent case, no gate is consulted or skipped because the streams
    /// agreed, and no code path completes the unit.
    #[tokio::test]
    async fn divergence_event_identical_candidates_log_agreement_and_no_gate_is_skipped() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        // Both streams write the SAME path with the SAME bytes: the two
        // checkpoint commits differ (per-index messages) but the branch
        // TREES are identical — agreement is a tree comparison, never a
        // commit-message one.
        let claude_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("claude candidate", "same.txt", "identical bytes"),
        ]));
        let codex_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("codex candidate", "same.txt", "identical bytes"),
        ]));
        let backend: Arc<dyn AgentBackend> = claude_mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", dispatch_pool_cfg()).unwrap();
        engine.seed_worker_auth_verdict_for_test(AuthVerdict::Authenticated);
        engine.seed_kind_backend_for_test(BackendKind::Codex, codex_mock.clone());
        engine
            .state
            .mission
            .milestones
            .push(dispatch_pool_milestone(&engine));

        engine.run_feature(0, 0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        let noted = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::DivergenceNoted {
                    unit,
                    candidates,
                    diverged,
                } => Some((unit, candidates, diverged)),
                _ => None,
            })
            .expect("identical streams still produce the record");
        assert_eq!(noted.0, "f-1-1");
        assert!(!noted.2, "identical trees record agreement, not divergence");
        assert_eq!(noted.1.len(), 2);
        assert_eq!(
            noted.1[0].tree, noted.1[1].tree,
            "same bytes on both branches → one tree hash"
        );

        // Agreement changes NOTHING about the mission's course:
        // - the milestone parks with the SAME judgement-pending reason as
        //   the divergent case (no "streams agreed" shortcut);
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                EventKind::MilestoneBlocked { milestone_id, reason }
                if milestone_id == "ms-1" && reason.contains("candidate for judgement")
            )),
            "agreement never un-parks the judgement: {:?}",
            events.iter().map(|e| &e.kind).collect::<Vec<_>>()
        );
        // - no gate was consulted, so none could have been skipped on the
        //   agreement (the ladder runs only in the normal validation flow,
        //   after judgement — never on the stream verdict);
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::GateResult { .. })),
            "no gate.result anywhere: agreement skips no gate"
        );
        // - the unit is neither completed nor failed from the agreement;
        // - and no validation round ran (a unit is done when gates are
        //   green and no escalation is open — not when streams agree).
        assert!(
            !events.iter().any(|e| matches!(
                &e.kind,
                EventKind::FeatureCompleted { feature_id, .. } | EventKind::FeatureFailed { feature_id, .. }
                if feature_id == "f-1-1"
            )),
            "agreement never completes or fails the unit"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(&e.kind, EventKind::MilestoneValidating { .. })),
            "agreement never starts a validation round"
        );
    }

    /// Acceptance hint 1 (resolution): the operator's steer on the parked
    /// milestone appends ONE resolution naming the chosen candidate, the
    /// why, and the decider — before the unblock it rides on. First
    /// judgement wins: a later steer re-acting on the same unit records no
    /// second resolution (the folded set is the durable memory).
    #[tokio::test]
    async fn divergence_event_resolution_records_the_decider_once() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let first = serde_json::json!({
            "action": "unblock-skip-findings",
            "note": "candidate 1 kept the parser total",
            "candidate": 1,
        })
        .to_string();
        let second = serde_json::json!({
            "action": "skip-milestone",
            "note": "skip it now",
            "candidate": 0,
        })
        .to_string();
        let claude_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("claude candidate", "claude.txt", "claude was here"),
            divergence_orch_script(vec![first, second]),
        ]));
        let codex_mock = Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
            dispatch_pool_pass_script("codex candidate", "codex.txt", "codex was here"),
        ]));
        let backend: Arc<dyn AgentBackend> = claude_mock.clone();
        let mut engine =
            MissionEngine::create(backend, &root, "goal", dispatch_pool_cfg()).unwrap();
        engine.seed_worker_auth_verdict_for_test(AuthVerdict::Authenticated);
        engine.seed_kind_backend_for_test(BackendKind::Codex, codex_mock.clone());
        engine
            .state
            .mission
            .milestones
            .push(dispatch_pool_milestone(&engine));
        engine.run_feature(0, 0).await.unwrap();

        // The operator judges: "candidate 1, and carry on".
        engine
            .emit(EventKind::UserMessage {
                text: "take candidate 1".into(),
                interrupt: false,
            })
            .unwrap();
        let status = engine.handle_blocked(0).await.unwrap();
        assert_eq!(status, None, "an unblock action moves the milestone");

        // A second steer re-acts on the same unit (here: dispose of it) —
        // the FIRST resolution already stands, so nothing new is recorded.
        engine
            .emit(EventKind::UserMessage {
                text: "actually, just skip the milestone".into(),
                interrupt: false,
            })
            .unwrap();
        engine.handle_blocked(0).await.unwrap();

        let events = EventLog::read_events(&engine.paths.events_file()).expect("read events");
        let resolutions: Vec<&Event> = events
            .iter()
            .filter(|e| matches!(&e.kind, EventKind::DivergenceResolved { .. }))
            .collect();
        assert_eq!(
            resolutions.len(),
            1,
            "first judgement wins — no second resolution for the unit"
        );
        let EventKind::DivergenceResolved {
            unit,
            selected,
            reason,
            decided_by,
        } = &resolutions[0].kind
        else {
            unreachable!()
        };
        assert_eq!(unit, "f-1-1");
        assert_eq!(*selected, Some(1), "the operator's candidate, verbatim");
        assert_eq!(reason, "candidate 1 kept the parser total");
        assert_eq!(decided_by, "operator", "the unblock path names the decider");
        // The resolution precedes the unblock it rode in on.
        let unblock_seq = events
            .iter()
            .find_map(|e| match &e.kind {
                EventKind::MilestoneUnblocked { milestone_id, .. } if milestone_id == "ms-1" => {
                    Some(e.seq)
                }
                _ => None,
            })
            .expect("the unblock landed");
        assert!(
            resolutions[0].seq < unblock_seq,
            "record-then-move: resolution seq {} < unblock seq {unblock_seq}",
            resolutions[0].seq
        );
        // The folded set is the restart-safe memory of "already judged".
        assert!(
            engine.state().resolved_divergence_units.contains("f-1-1"),
            "the unit joins the folded resolution set"
        );
        // The second steer still disposed of the milestone — the dedupe
        // suppresses only the duplicate RECORD, never the operator's act.
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                EventKind::MilestoneCompleted { milestone_id, .. } if milestone_id == "ms-1"
            )),
            "the skip still completes the milestone"
        );
    }
}
