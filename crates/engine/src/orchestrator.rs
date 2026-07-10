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
use crate::config;
use crate::contract_sweep;
use crate::control;
use crate::cost;
use crate::digest;
use crate::error::{EngineError, Result};
use crate::event_log::{EventLog, LockForce};
use crate::events::{Event, EventKind};
use crate::git_ops::{with_kranz_trailers, CommitInfo, GitRepo, KranzCommitMetadata};
use crate::lessons;
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
use std::path::{Path, PathBuf};
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
}

struct SelectedBackend {
    backend: Arc<dyn AgentBackend>,
    kind: BackendKind,
    cfg: MissionConfig,
    fallback_reason: Option<String>,
}

/// What the findings-conversion turn decided (see [`MissionEngine::convert_findings`]).
enum FindingsConversion {
    /// Convert into fix features. Unparseable answers and answers that
    /// neither fix nor waive land here with specs synthesized 1:1 from the
    /// findings — the conservative default.
    Fix {
        specs: Vec<FixFeatureSpec>,
        summary: String,
        text: String,
    },
    /// Every finding waived, each with a one-line justification.
    Waive { waived: Vec<WaivedFinding> },
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
// Environment preflight (roadmap M2)
// ---------------------------------------------------------------------------

/// One environment-preflight issue surfaced at run start (roadmap M2).
///
/// Preflight is advisory only: it never blocks a mission (the final contract
/// gate stays authoritative). `severity` is `"warn"` for a probably-missing
/// prerequisite and `"error"` for a hard environment defect (not a git repo,
/// `.kranz` not writable) that will almost certainly break the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightIssue {
    /// `"warn"` | `"error"`.
    pub severity: &'static str,
    pub message: String,
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
    pending_research: Option<Research>,
    /// Lazily-built [`crate::backend_codex::CodexBackend`] cache for roles
    /// whose `backend = "codex"`. `None` until the first successful probe; a
    /// failed probe is never cached (so a codex install that appears
    /// mid-mission is picked up on the next role spawn).
    codex_backend: Option<Arc<dyn AgentBackend>>,
    /// Lazily-built [`crate::backend_droid::DroidBackend`] cache for roles
    /// whose `backend = "droid"`. Mirrors `codex_backend`: `None` until the
    /// first successful probe; a failed probe is never cached.
    droid_backend: Option<Arc<dyn AgentBackend>>,
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
            pending_seed_reply: None,
            pending_research: None,
            codex_backend: None,
            droid_backend: None,
            active_tree: None,
            primary_branch_at_start: None,
            worker_auth_verdict: None,
        })
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
                let path = parallel_worktree_path(mission_id, &feature.id);
                if path.exists() {
                    let _ = repo.remove_worktree(&path);
                }
            }
        }
        // A leaked mission integration worktree (M7 tier 1) is the same story:
        // it exists only while a lock-holding engine has one set up, so with
        // the lock now held it is a crash leak. Reap it the same way.
        let integration_path = mission_worktree_path(mission_id);
        if integration_path.exists() {
            let _ = repo.remove_worktree(&integration_path);
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
            active_tree: None,
            primary_branch_at_start: None,
            worker_auth_verdict: None,
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
    fn active_root(&self) -> &Path {
        match &self.active_tree {
            Some((root, _)) => root.as_path(),
            None => self.paths.repo_root.as_path(),
        }
    }

    /// The [`GitRepo`] paired with [`Self::active_root`].
    fn active_repo(&self) -> &GitRepo {
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
    fn active_paths(&self) -> MissionPaths {
        MissionPaths::new(self.active_root(), self.state.mission.id.clone())
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
    // Environment preflight (roadmap M2)
    // -----------------------------------------------------------------------

    /// Best-effort check of obvious prerequisites of the validation contract's
    /// `command` assertions, run once at the start of [`Self::run`] before the
    /// first worker spawns (roadmap M2). Advisory only: the returned issues are
    /// surfaced as a single `orchestrator.decision`, never as a block — the
    /// contract gate at mission completion is still the authoritative check.
    ///
    /// For each `command` assertion the leading program token is extracted (the
    /// interpreter for `sh -c` / `python3 -c` shapes, else the first word) and
    /// probed on PATH; a clearly-missing program is a `warn`. Two hard
    /// environment defects are `error`s: the repo not being a git repo, and
    /// `.kranz` not being writable. The probe is intentionally lenient — only
    /// programs that plainly do not resolve are flagged, so a shell builtin or
    /// an odd-but-valid command never produces a false warning.
    pub fn preflight(&self) -> Vec<PreflightIssue> {
        let mut issues = Vec::new();

        // Hard defects first (an "error" severity): a run against a non-repo or
        // a read-only .kranz is almost certainly doomed.
        if !is_git_repo(self.paths.repo_root.as_path()) {
            issues.push(PreflightIssue {
                severity: "error",
                message: format!("{} is not a git repository", self.paths.repo_root.display()),
            });
        }
        if !kranz_dir_is_writable(&self.paths) {
            issues.push(PreflightIssue {
                severity: "error",
                message: ".kranz directory is not writable".to_string(),
            });
        }

        for role in [
            Role::Orchestrator,
            Role::Worker,
            Role::ValidatorScrutiny,
            Role::ValidatorFunctional,
        ] {
            let role_key = role_config_key(role);
            match self.state.config.backend_kind(role) {
                BackendKind::Codex => {
                    if let Err(err) = crate::backend_codex::discover_codex_binary(None) {
                        issues.push(PreflightIssue {
                            severity: "warn",
                            message: format!(
                                "{role_key}.backend is \"codex\" but no codex binary was found \
                                 ({err}); that role will fall back to the claude backend"
                            ),
                        });
                    }
                }
                BackendKind::Droid => {
                    if let Err(err) = crate::backend_droid::discover_droid_binary(None) {
                        issues.push(PreflightIssue {
                            severity: "warn",
                            message: format!(
                                "{role_key}.backend is \"droid\" but no droid binary was found \
                                 ({err}); that role will fall back to the claude backend"
                            ),
                        });
                    }
                }
                BackendKind::Claude => {}
            }
        }

        // Contract command programs: probe the leading token of each distinct
        // command, flagging only ones that clearly do not resolve on PATH.
        let mut probed: std::collections::HashSet<String> = std::collections::HashSet::new();
        for assertion in &self.state.mission.validation_contract {
            if assertion.check != AssertionCheck::Command {
                continue;
            }
            let Some(command) = assertion.command.as_deref() else {
                continue;
            };
            let Some(program) = leading_program(command) else {
                continue;
            };
            if !probed.insert(program.clone()) {
                continue; // already reported/checked this program
            }
            if !program_resolves(&program) {
                issues.push(PreflightIssue {
                    severity: "warn",
                    message: format!(
                        "command assertion [{}] uses '{program}', which was not found on PATH",
                        assertion.id
                    ),
                });
            }
            if !contract_sweep::cargo_test_has_anti_vacuity(command) {
                issues.push(PreflightIssue {
                    severity: "warn",
                    message: format!(
                        "command assertion [{}] runs `cargo test` without anti-vacuity \
                         (`ok. [1-9]`); a zero-test filter would pass vacuously",
                        assertion.id
                    ),
                });
            }
        }

        // Sandbox preflight (f-2-3/f-2-4): surface unsupported/missing
        // sandbox tooling as a warning, and on macOS run each distinct
        // contract `command` assertion under the generated worker Seatbelt
        // profile. Best-effort and advisory only: never an `error`, never a
        // block. `session_cwd` uses `self.paths.repo_root` (the primary
        // checkout) as a cheap stand-in for the actual per-session worktree
        // root, which does not exist yet at preflight time.
        if self.state.config.worker.sandbox.enforce != crate::types::SandboxEnforce::Off {
            let mission_dir = self.paths.mission_dir();
            let (_resolved, warn) = crate::sandbox::resolve_for_session(
                &self.state.config.worker.sandbox,
                self.paths.repo_root.as_path(),
                &mission_dir,
            );
            if let Some(warn) = warn {
                issues.push(PreflightIssue {
                    severity: "warn",
                    message: warn,
                });
            }
        }
        if matches!(
            self.state.config.worker.sandbox.enforce,
            crate::types::SandboxEnforce::Fs | crate::types::SandboxEnforce::FsNet
        ) && cfg!(target_os = "macos")
        {
            issues.extend(self.sandbox_command_preflight());
        }

        issues
    }

    /// Run each distinct contract `command` assertion under the worker's
    /// generated Seatbelt profile; a non-zero exit under the sandbox becomes a
    /// `warn` `PreflightIssue` naming the assertion. Best-effort: any failure
    /// to resolve the sandbox or write the profile file is silently skipped
    /// (never escalated) rather than reported, since this probe must never
    /// block or mislabel an environment problem as a sandbox problem.
    fn sandbox_command_preflight(&self) -> Vec<PreflightIssue> {
        let mission_dir = self.paths.mission_dir();
        let (resolved, _warn) = crate::sandbox::resolve_for_session(
            &self.state.config.worker.sandbox,
            self.paths.repo_root.as_path(),
            &mission_dir,
        );
        let Some(resolved) = resolved else {
            return Vec::new();
        };
        if resolved.backend != crate::sandbox::SandboxBackend::Seatbelt {
            return Vec::new();
        }
        let profile = crate::sandbox::generate_profile(&resolved.inputs);
        let profile_path = match crate::sandbox::write_profile_file(&mission_dir, &profile)
            .or_else(|_| crate::sandbox::write_profile_file(&resolved.inputs.tmpdir, &profile))
        {
            Ok(path) => path,
            Err(_) => return Vec::new(),
        };

        let mut issues = Vec::new();
        let mut probed: std::collections::HashSet<&str> = std::collections::HashSet::new();
        const MAX_PROBES: usize = 20;
        const TIMEOUT: Duration = Duration::from_secs(5);
        for assertion in &self.state.mission.validation_contract {
            if issues.len() >= MAX_PROBES || probed.len() >= MAX_PROBES {
                break;
            }
            if assertion.check != AssertionCheck::Command {
                continue;
            }
            let Some(command) = assertion.command.as_deref() else {
                continue;
            };
            if !probed.insert(command) {
                continue; // already probed this exact command
            }
            let (program, args) = crate::backend_claude::sandbox_command(
                &profile_path,
                std::path::Path::new("/bin/sh"),
                &["-c".to_string(), command.to_string()],
            );
            match run_with_timeout(&program, &args, TIMEOUT) {
                Some(output) if !output.status.success() => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let tail = last_chars_local(&stderr, 200);
                    issues.push(PreflightIssue {
                        severity: "warn",
                        message: format!(
                            "command assertion [{}] fails under the fs sandbox profile: {tail}",
                            assertion.id
                        ),
                    });
                }
                _ => {}
            }
        }
        issues
    }

    // -----------------------------------------------------------------------
    // emit / catch_up — the log/state/snapshot lockstep
    // -----------------------------------------------------------------------

    /// Append one event, fold it into state, and refresh the snapshot.
    ///
    /// The snapshot write is mandatory for lifecycle events and best-effort
    /// for `worker.message` stream deltas (recoverable by refolding the log).
    fn emit(&mut self, kind: EventKind) -> Result<Event> {
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
    fn emit_decision(&mut self, summary: &str, detail: Option<String>) -> Result<()> {
        self.emit(EventKind::OrchestratorDecision {
            summary: scrub::scrub_and_truncate(summary, DECISION_SUMMARY_MAX),
            detail: detail.map(|d| scrub::scrub(&d)),
        })?;
        Ok(())
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
            BackendKind::Codex => {
                if let Some(cached) = &self.codex_backend {
                    set_effective_model(&mut cfg, BackendKind::Codex);
                    return SelectedBackend {
                        backend: Arc::clone(cached),
                        kind: BackendKind::Codex,
                        cfg,
                        fallback_reason: None,
                    };
                }
                match crate::backend_codex::discover_codex_binary(None) {
                    Ok(binary) => {
                        let backend: Arc<dyn AgentBackend> =
                            Arc::new(crate::backend_codex::CodexBackend::new(binary));
                        self.codex_backend = Some(Arc::clone(&backend));
                        set_effective_model(&mut cfg, BackendKind::Codex);
                        SelectedBackend {
                            backend,
                            kind: BackendKind::Codex,
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
                                "codex backend requested for the {role_name} but not available \
                                 ({err}); falling back to the claude {role_name}"
                            )),
                        }
                    }
                }
            }
            BackendKind::Droid => {
                if let Some(cached) = &self.droid_backend {
                    set_effective_model(&mut cfg, BackendKind::Droid);
                    return SelectedBackend {
                        backend: Arc::clone(cached),
                        kind: BackendKind::Droid,
                        cfg,
                        fallback_reason: None,
                    };
                }
                match crate::backend_droid::discover_droid_binary(None) {
                    Ok(binary) => {
                        let backend: Arc<dyn AgentBackend> =
                            Arc::new(crate::backend_droid::DroidBackend::new(binary));
                        self.droid_backend = Some(Arc::clone(&backend));
                        set_effective_model(&mut cfg, BackendKind::Droid);
                        SelectedBackend {
                            backend,
                            kind: BackendKind::Droid,
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
                                "droid backend requested for the {role_name} but not available \
                                 ({err}); falling back to the claude {role_name}"
                            )),
                        }
                    }
                }
            }
            BackendKind::Claude => {
                set_effective_model(&mut cfg, BackendKind::Claude);
                SelectedBackend {
                    backend: Arc::clone(&self.backend),
                    kind: BackendKind::Claude,
                    cfg,
                    fallback_reason: None,
                }
            }
        }
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

    /// Demand the plan JSON (types::Plan, camelCase). Lenient parse with one
    /// retry demanding bare JSON; a plan parses to [`PlanRequest::Ready`]
    /// (unapproved). When the retry ALSO answers with prose, the orchestrator
    /// is simply not ready to emit (it wants answers first) — that text comes
    /// back as [`PlanRequest::NotReady`], never as an error.
    pub async fn request_plan(&mut self) -> Result<PlanRequest> {
        let message = format!(
            "Emit the plan now. Output ONLY a JSON object conforming exactly to this JSON \
             Schema — no prose before or after:\n{}\n\n{}\n\n{}",
            plan_schema(),
            considered_alternatives_prompt_policy(&self.state.config),
            research_prompt_policy(&self.state.config)
        );
        let text = self.orch_turn(&message).await?;
        if let Some(plan) = runner::parse_report::<Plan>(&text) {
            self.pending_research = extract_research(&text);
            return Ok(PlanRequest::Ready(plan));
        }
        let retry = self.orch_turn(JSON_RETRY_MSG).await?;
        match runner::parse_report::<Plan>(&retry) {
            Some(plan) => {
                self.pending_research = extract_research(&retry);
                Ok(PlanRequest::Ready(plan))
            }
            // Prefer the retry's text (the model's latest word); fall back to
            // the first turn's when the retry came back empty. Both are
            // already scrubbed by pump_turn.
            None => Ok(PlanRequest::NotReady(if retry.trim().is_empty() {
                text
            } else {
                retry
            })),
        }
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
        assign_assertion_ids(&mut plan.validation_contract);

        let calibration = cost::calibrate(&self.paths.repo_root);
        let estimate = cost::estimate(&plan, &self.state.config, &calibration.params);
        let estimate = cost::apply_shape(estimate, &plan, &calibration);
        validate_considered_alternatives(&plan, &estimate, &self.state.config)?;
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

        // Human-readable twin, committed alongside: reviewable in any git UI
        // and diffable across re-plans (plan.json stays the durable source).
        // The calibrated cost estimate is baked in here so the Reviewable
        // human queue gate (and any future surface reading plan.md) sees it
        // without recomputing it — `calibrate` never fails.
        let plan_md_body = render_plan_markdown(
            &plan,
            &self.state.mission,
            &estimate,
            calibration.missions_used,
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

        self.emit(EventKind::PlanApproved {
            plan,
            base_sha: Some(base_sha),
        })?;
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

    /// Propose a REVISED plan for the not-yet-complete work of a running or
    /// blocked mission (roadmap M2). An orchestrator turn — digest + the
    /// current milestone/feature status + a revise-the-remainder instruction —
    /// that returns a full [`Plan`] (completed milestones unchanged and first,
    /// then the revised remainder). Reuses the streaming orchestrator, the
    /// lenient JSON parse, and the [`PlanRequest`] `Ready`/`NotReady` enum
    /// exactly like [`Self::request_plan`]; prose (the orchestrator wants to
    /// discuss first) comes back as `NotReady`, never an error.
    ///
    /// This only PROPOSES; [`Self::approve_revised_plan`] validates and applies
    /// the subset the event vocabulary can express (see the contract note
    /// above).
    pub async fn request_revised_plan(&mut self) -> Result<PlanRequest> {
        self.request_revised_plan_with_instructions("").await
    }

    async fn request_revised_plan_with_instructions(
        &mut self,
        instructions: &str,
    ) -> Result<PlanRequest> {
        let instructions = instructions.trim();
        let instructions_block = if instructions.is_empty() {
            "No extra operator instructions were supplied.".to_string()
        } else {
            format!("Operator revision request:\n{instructions}")
        };
        let message = format!(
            "The mission is already underway. Propose a REVISED plan for the work that is \
             NOT yet complete. Rules: keep every already-COMPLETE milestone exactly as it is \
             and list those completed milestones FIRST and unchanged (same title, same \
             features, same order); then revise the remaining milestones' features as the \
             current situation warrants (drop features no longer needed, add features now \
             required). Output ONLY a JSON object conforming exactly to this JSON Schema — \
             no prose before or after:\n{}\n\n{}\n\n{}\n\n{}",
            plan_schema(),
            considered_alternatives_prompt_policy(&self.state.config),
            research_prompt_policy(&self.state.config),
            instructions_block
        );
        let text = self.orch_turn(&message).await?;
        if let Some(plan) = runner::parse_report::<Plan>(&text) {
            self.pending_research = extract_research(&text);
            return Ok(PlanRequest::Ready(plan));
        }
        let retry = self.orch_turn(JSON_RETRY_MSG).await?;
        match runner::parse_report::<Plan>(&retry) {
            Some(plan) => {
                self.pending_research = extract_research(&retry);
                Ok(PlanRequest::Ready(plan))
            }
            None => Ok(PlanRequest::NotReady(if retry.trim().is_empty() {
                text
            } else {
                retry
            })),
        }
    }

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
        let plan_md_body = render_plan_markdown(
            plan,
            &self.state.mission,
            &estimate,
            calibration.missions_used,
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

        let result = self.run_loop().await;

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
    async fn run_loop(&mut self) -> Result<MissionStatus> {
        // Environment preflight (roadmap M2): surface obvious missing
        // prerequisites of the contract commands as ONE advisory decision
        // before the first worker spawns. Never blocks — the contract gate at
        // completion stays authoritative. Emitted only once per run() call, and
        // only when there is something to report.
        let issues = self.preflight();
        if !issues.is_empty() {
            let summary = format!(
                "preflight: {} issue(s): {}",
                issues.len(),
                issues
                    .iter()
                    .map(|i| format!("[{}] {}", i.severity, i.message))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            self.emit_decision(&summary, None)?;
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
            None => (
                "stay-blocked".to_string(),
                "unparseable unblock decision".to_string(),
            ),
        };
        self.emit_decision(
            &format!("unblock decision for {milestone_id}: {action}"),
            Some(text),
        )?;

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
                    .filter(|f| matches!(f.status, FeatureStatus::Pending | FeatureStatus::Active))
                    .map(|f| f.id.clone())
                    .collect();
                for feature_id in to_skip {
                    self.emit(EventKind::FeatureSkipped {
                        feature_id,
                        reason: "milestone skipped".to_string(),
                    })?;
                }
                self.emit(EventKind::MilestoneCompleted {
                    milestone_id,
                    tag: None,
                })?;
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
            let base_sha = self.state.mission.base_sha.clone();
            let grants = self.state.mission.command_grants.clone();
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
                    auth_verdict,
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
                    auth_verdict,
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
            if !self.active_repo().is_clean()? && !self.resolve_dirty_tree(&feature.id).await? {
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
    /// feature was failed instead — by the orchestrator's own decision, or
    /// because the checkpoint's secret scan refused the commit.
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
                // the mission continues (or blocks) with an audit trail, and
                // the leftover tree plus the refusal's allowlist guidance is
                // the operator's cleanup cue. Real git failures still `?` out
                // above.
                self.emit_decision(
                    &format!("dirty tree after {feature_id}: checkpoint refused by secret scan"),
                    Some(detail.clone()),
                )?;
                self.emit(EventKind::FeatureFailed {
                    feature_id: feature_id.to_string(),
                    reason: format!("dirty-tree checkpoint refused by secret scan: {detail}"),
                })?;
                Ok(false)
            }
        }
    }

    /// Post-run judgement turn (§4.5 f): report + commits + diff stat →
    /// JSON `{decision, guidance, summary}`. Unparseable after retry →
    /// conservative default: respawn-if-budget-else-fail (mapped to Respawn
    /// here; the caller enforces the budget).
    async fn judge_worker_run(
        &mut self,
        feature_id: &str,
        outcome: &runner::RunOutcome,
        commits: &[String],
        diff_stat: &str,
    ) -> Result<JudgementOutcome> {
        let runner_summary = run_outcome_summary(outcome);
        if outcome.result != RunResult::Pass {
            let summary = format!("worker run not trusted: {runner_summary}");
            self.emit_decision(&format!("judgement for {feature_id}: {summary}"), None)?;
            return Ok(JudgementOutcome::Respawn(format!(
                "{summary}. Re-run the feature; do not rely on the previous worker report."
            )));
        }

        let report_text = match outcome.report.as_ref() {
            Some(r) => serde_json::to_string_pretty(r)?,
            None => "NO REPORT — treat sceptically".to_string(),
        };
        let commits_text = if commits.is_empty() {
            "(none)".to_string()
        } else {
            commits
                .iter()
                .map(|c| format!("- {c}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let message = format!(
            "A worker run for feature {feature_id} just finished. Judge it.\n\n\
             RUNNER VERDICT:\n{runner_summary}\n\n\
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
                let summary = if d.summary.is_empty() {
                    verdict.clone()
                } else {
                    d.summary
                };
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
        self.emit_decision(
            &format!("judgement for {feature_id}: {summary}"),
            Some(text),
        )?;

        Ok(match verdict.as_str() {
            "complete" => JudgementOutcome::Complete,
            "failed" | "fail" => JudgementOutcome::Failed(summary),
            // "respawn" and anything unrecognized take the conservative path.
            _ => JudgementOutcome::Respawn(if guidance.is_empty() {
                summary
            } else {
                guidance
            }),
        })
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
                path: parallel_worktree_path(&self.state.mission.id, feature_id),
            })
            .collect();

        // The whole batch is wrapped so we can ALWAYS clean up worktrees, even
        // on an error return. `batch_result` carries the fallible body's error
        // to re-raise after cleanup.
        let batch_result = self
            .run_parallel_batch_inner(mi, &start_sha, &mission_branch, &workspaces)
            .await;

        // Cleanup guard: remove every worktree + branch we created. Best-effort
        // and idempotent (remove_worktree/delete_branch_force tolerate absence);
        // a cleanup failure is logged, never allowed to mask the batch outcome.
        for ws in &workspaces {
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

        let path = mission_worktree_path(&self.state.mission.id);
        // Idempotent: a stale integration worktree from a prior crash must be
        // gone before checking the branch out again (git refuses to check the
        // same branch out twice).
        let _ = self.repo.remove_worktree(&path);
        let _ = self.repo.prune_worktrees();

        self.repo.add_worktree_checkout(&path, &mission_branch)?;
        let wt_repo = GitRepo::open(&path)?;
        Ok((path, wt_repo))
    }

    /// Tear down the mission integration worktree created by
    /// [`Self::setup_mission_worktree`]. Best-effort and idempotent, mirroring
    /// the parallel-batch cleanup guard: failures are logged, never fatal.
    fn teardown_mission_worktree(&self) {
        let path = mission_worktree_path(&self.state.mission.id);
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
    async fn run_parallel_batch_inner(
        &mut self,
        mi: usize,
        start_sha: &str,
        mission_branch: &str,
        workspaces: &[ParallelWorkspace],
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
                    auth_verdict,
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
        let mut worker_ok: Vec<bool> = Vec::with_capacity(workspaces.len());
        for (idx, ws) in workspaces.iter().enumerate() {
            let (events, outcome) = buffered[idx]
                .take()
                .expect("every non-errored workspace has a buffered result");
            let ok = self
                .append_and_judge_worktree(ws, start_sha, events, &outcome)
                .await?;
            worker_ok.push(ok);
        }

        // (2) Merge the per-feature branches into the mission branch in the
        // declared order. Clean → keep the feature's commits + feature.completed;
        // conflict (aborted, clean tree) → feature.failed PLUS a resolution
        // fix-feature (below); a worker that failed in its worktree →
        // feature.failed without attempting a merge.
        for (ws, ok) in workspaces.iter().zip(&worker_ok) {
            let feature_id = ws.feature_id.clone();
            if !ok {
                self.emit(EventKind::FeatureFailed {
                    feature_id,
                    reason: "worker run did not complete in its parallel worktree".to_string(),
                })?;
                continue;
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
    /// worktree, and judge the run. Returns `true` when the work is ready to
    /// merge, `false` when it should be failed.
    ///
    /// `buffered` is exactly the `worker.spawned` / `worker.message` /
    /// `worker.completed` kinds `run_worker_in_buffered` collected while the
    /// session ran concurrently in Phase B — replaying them here, serially,
    /// through `emit` is what keeps events.jsonl single-writer with contiguous
    /// seq even though the sessions overlapped. `feature.started` was already
    /// emitted in Phase A.
    ///
    /// Deliberately does NOT respawn: the parallel batch is best-effort per the
    /// honest subset. A non-complete judgement fails the feature (its branch is
    /// discarded by the cleanup guard); the sequential path — with its full
    /// respawn/dirty-tree machinery — remains the way a feature gets retried.
    async fn append_and_judge_worktree(
        &mut self,
        ws: &ParallelWorkspace,
        start_sha: &str,
        buffered: Vec<EventKind>,
        outcome: &runner::RunOutcome,
    ) -> Result<bool> {
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
        let wt_repo = GitRepo::open(&ws.path)?;
        wt_repo.ensure_identity()?;

        // Commit any worker output on the per-feature branch (in the worktree)
        // so the merge carries it. The worker session's own commits (if any)
        // already landed on the branch; a dirty tree is checkpoint-committed
        // here rather than run through the sequential dirty-tree turn — the
        // parallel subset keeps its worktree self-contained. A real git
        // failure `?`-aborts the batch (the caller's cleanup guard still
        // reaps every worktree); a secret-scan refusal is recorded below, so
        // dirty deliverables are never silently dropped before judgement.
        if !wt_repo.is_clean().unwrap_or(true) {
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
                    return Ok(false);
                }
            }
        }

        // Judge the run against the worktree's own commit range (start_sha..HEAD
        // in the worktree — the branch was forked at start_sha).
        let commits: Vec<String> = wt_repo
            .commits_between(start_sha, "HEAD")
            .unwrap_or_default()
            .iter()
            .map(|c| format!("{} {}", c.sha, c.subject))
            .collect();
        let diff_stat = wt_repo.diff_stat(start_sha, "HEAD").unwrap_or_default();
        match self
            .judge_worker_run(&ws.feature_id, outcome, &commits, &diff_stat)
            .await?
        {
            JudgementOutcome::Complete => Ok(true),
            // Respawn/Failed both mean "not ready to merge" in the parallel
            // subset (no respawn here); the feature is failed by the caller.
            JudgementOutcome::Failed(_) | JudgementOutcome::Respawn(_) => Ok(false),
        }
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

    /// Milestone validation: scrutiny then functional validators (v1:
    /// sequential; each skippable by config). Findings go to the conversion
    /// turn, where the orchestrator turns each into a fix feature or waives
    /// it; no findings — or all findings waived — means a tag + completion.
    async fn validation_round(&mut self, mi: usize) -> Result<()> {
        let milestone_id = self.state.mission.milestones[mi].id.clone();
        self.emit(EventKind::MilestoneValidating {
            milestone_id: milestone_id.clone(),
        })?;

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
            let base_sha = self.state.mission.base_sha.clone();
            let grants = self.state.mission.command_grants.clone();
            let worker_commands = worker_commands_for_milestone(&self.state, &milestone);

            let selected = self.select_backend(role);
            if let Some(reason) = selected.fallback_reason.as_deref() {
                self.emit_decision(reason, None)?;
            }
            let selected_kind = selected.kind;
            let backend = Arc::clone(&selected.backend);
            let cfg = selected.cfg;

            let session_cwd = self.active_root().to_path_buf();
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
                &worker_commands,
            )
            .await;
            let caught = self.catch_up();
            let mut outcome = outcome?;
            caught?;

            // Bounded (exactly one retry) runtime fallback: a validator run
            // that did not produce a trusted pass is retried once with the
            // injected Claude backend. A crashed/aborted validator must never
            // collapse into "no findings" and green-light validation.
            if !validator_outcome_trusted(&outcome) {
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
                let retry_session_cwd = self.active_root().to_path_buf();
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
                    &worker_commands,
                )
                .await;
                let caught = self.catch_up();
                outcome = retry_outcome?;
                caught?;
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
            FindingsConversion::Fix {
                specs,
                summary,
                text,
            } => {
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
        // per-commit diff against its predecessor in the range. Engine/meta
        // commits are skipped entirely so their paths never enter the
        // candidate set, even when outside the touch-set — but only when the
        // commit's own paths PROVE it is one: a subject template alone is
        // spoofable by a worker's `git commit` ("[kranz] mission report
        // cleanup"), so a template-subject commit touching anything beyond
        // mission-record metadata is swept like any other worker commit
        // (contract_sweep::is_meta_commit_with_paths).
        let mut changes: Vec<(String, CommitInfo)> = Vec::new();
        let mut prev_sha = milestone_start_sha.to_string();
        let mut worker_commit_count = 0usize;
        for commit in &commits {
            let paths = repo.changed_paths(&prev_sha, &commit.sha)?;
            prev_sha = commit.sha.clone();
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
        let waived_subjects: std::collections::HashSet<&str> =
            waived.iter().map(|w| w.subject.as_str()).collect();
        let unwaived_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| !waived_subjects.contains(f.subject.as_str()))
            .collect();
        if specs.is_empty() && !waived.is_empty() && unwaived_findings.is_empty() {
            return Ok(FindingsConversion::Waive { waived });
        }
        // Partial fixFeatures must not drop uncovered findings. When the model
        // returns fewer specs than unwaived findings, union with synthesized
        // fixes for subjects its titles do not reference. When it returns at
        // least one spec per unwaived finding, trust the model — titles are
        // often generic ("fix issue 1") and must not force an extra worker.
        let specs = if specs.is_empty() {
            synthesize_fix_specs(unwaived_findings)
        } else if specs.len() >= unwaived_findings.len() {
            specs
        } else {
            let covered: std::collections::HashSet<String> =
                specs.iter().map(|s| s.title.clone()).collect();
            let mut merged = specs;
            let uncovered: Vec<&Finding> = unwaived_findings
                .iter()
                .copied()
                .filter(|f| {
                    !covered.iter().any(|title| {
                        title == &f.subject
                            || title.contains(&f.subject)
                            || title == &format!("fix {}", f.subject)
                    })
                })
                .collect();
            merged.extend(synthesize_fix_specs(uncovered));
            merged
        };
        Ok(FindingsConversion::Fix {
            specs,
            summary,
            text,
        })
    }

    /// Emit the all-waived `orchestrator.decision`: summary names the waived
    /// subjects, detail carries the justifications. Both fields are
    /// credential-scrubbed by [`Self::emit_decision`] — waiver reasons are
    /// model-authored text.
    fn emit_waive_decision(&mut self, waived: &[WaivedFinding]) -> Result<()> {
        let subjects = waived
            .iter()
            .map(|w| w.subject.as_str())
            .collect::<Vec<_>>()
            .join(", ");
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
            &format!(
                "{} fix feature(s) for {milestone_id}: {summary}",
                specs.len()
            ),
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
        // contract_sweep::is_meta_commit_with_paths).
        let commits = self.active_repo().commits_between(&base, "HEAD")?;
        let gate_mission_id = self.state.mission.id.clone();
        let mut prev_sha = base.clone();
        let mut non_meta_commit_count = 0usize;
        for commit in &commits {
            let paths = self.active_repo().changed_paths(&prev_sha, &commit.sha)?;
            prev_sha = commit.sha.clone();
            if !contract_sweep::is_meta_commit_with_paths(&commit.subject, &gate_mission_id, &paths)
            {
                non_meta_commit_count += 1;
            }
        }
        if non_meta_commit_count == 0 {
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
        let env = runner::contract_env(self.state.mission.base_sha.as_deref());

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
            let (ok, output) = run_shell_command(self.active_root(), command, &env).await;
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
                if self.fix_cycle_exhausted(li) {
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
        // Diff against the base pinned at approval, never the live base branch
        // (a base that advanced mid-mission would silently change the final
        // judgement's diff). See judge_diff_base / judge_gate_diff_uses_pinned_base_sha.
        let base = judge_diff_base(
            self.state.mission.base_sha.as_deref(),
            &self.state.mission.base_branch,
        );
        let diff_stat = self
            .active_repo()
            .diff_stat(&base, "HEAD")
            .unwrap_or_default();
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
                            class: String::new(),
                        }),
                        None => findings.push(Finding {
                            subject: assertion.id.clone(),
                            severity: "critical".to_string(),
                            evidence: "no verdict returned for this assertion".to_string(),
                            suggested_fix: String::new(),
                            class: String::new(),
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
                        class: String::new(),
                    });
                }
                "unparseable verdicts; all judgement assertions failed conservatively".to_string()
            }
        };
        self.emit_decision(&format!("final gate verdicts: {summary}"), Some(text))?;
        Ok(findings)
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
        let report = render_mission_report(&self.state, &events, &plan, &estimate);

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
    // Cross-mission lesson capture
    // -----------------------------------------------------------------------

    /// One final orchestrator turn at mission completion: distill at most one
    /// reusable lesson for a future mission in this repo, write it to
    /// `.kranz/lessons/<mission-id>.md`, and append it to the lesson index.
    ///
    /// Best-effort BY DESIGN, same contract as [`Self::write_mission_report`]:
    /// the orchestrator only PRODUCES the lesson text — this engine method is
    /// the one that writes files — and any turn/parse/write failure is
    /// downgraded to a warning rather than stranding a mission that already
    /// passed its final gate. Returns the paths written (lesson file, index),
    /// or `None` if there was nothing worth carrying forward or capture
    /// failed.
    pub async fn capture_lesson(&mut self) -> Option<Vec<PathBuf>> {
        match self.try_capture_lesson().await {
            Ok(written) => written,
            Err(e) => {
                tracing::warn!(error = %e, "lesson capture failed; completing without a lesson");
                None
            }
        }
    }

    /// Fallible body of [`Self::capture_lesson`].
    async fn try_capture_lesson(&mut self) -> Result<Option<Vec<PathBuf>>> {
        let message = format!(
            "MISSION GOAL:\n{}\n\nThe mission has just completed. Distill at most ONE \
             reusable lesson that a FUTURE mission in THIS repository would need — as a \
             short imperative note — or reply with the single word NONE if there is \
             nothing worth carrying forward.",
            self.state.mission.goal
        );
        let text = self.orch_turn(&message).await?;
        let trimmed = text.trim();
        if trimmed.is_empty() || is_none_reply(trimmed) {
            return Ok(None);
        }

        // Written under active_paths (the integration worktree in worktree
        // mode) because these files are folded into write_mission_report's
        // commit, which commits via active_repo — see that method's doc note.
        let active_paths = self.active_paths();
        let lessons_dir = active_paths.lessons_dir();
        std::fs::create_dir_all(&lessons_dir)?;
        let mission_id = self.state.mission.id.clone();
        let lesson_file = lessons_dir.join(format!("{mission_id}.md"));
        let body = normalize_lesson_body(trimmed);
        std::fs::write(&lesson_file, &body)?;

        let summary = first_nonempty_line(&body);
        let index = active_paths.lessons_index();
        let line = format!("- {mission_id}.md · {summary}\n");
        {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&index)?;
            file.write_all(line.as_bytes())?;
        }

        Ok(Some(vec![lesson_file, index]))
    }

    // -----------------------------------------------------------------------
    // Orchestrator session management (i)
    // -----------------------------------------------------------------------

    /// One orchestrator turn with re-seed resilience: ensure the session,
    /// send the digest-prefixed message, pump to the turn's `Result`. If the
    /// session dies mid-turn, re-seed once and retry; two consecutive
    /// failures → [`EngineError::Backend`].
    async fn orch_turn(&mut self, message: &str) -> Result<String> {
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
            if let Some(index) = lessons::render_lessons_index(&self.paths.repo_root) {
                seed.push_str("\n\n");
                seed.push_str(&index);
            }
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
            permissions::for_role(Role::Orchestrator, &cfg, &[], &[]),
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
            if let Some(index) = lessons::render_lessons_index(&self.paths.repo_root) {
                seed.push_str("\n\n");
                seed.push_str(&index);
            }
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
                    if let Some(index) = lessons::render_lessons_index(&self.paths.repo_root) {
                        seed.push_str("\n\n");
                        seed.push_str(&index);
                    }
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
            permissions::for_role(Role::Orchestrator, &cfg, &[], &[]),
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

fn run_outcome_summary(outcome: &runner::RunOutcome) -> String {
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

fn synthesize_fix_specs(findings: Vec<&Finding>) -> Vec<FixFeatureSpec> {
    findings
        .into_iter()
        .map(|f| FixFeatureSpec {
            title: format!("Fix finding: {}", f.subject),
            spec: format!(
                "Address this validation finding.\nEvidence: {}\nSuggested fix: {}",
                f.evidence, f.suggested_fix
            ),
            validation_criteria: vec![format!("finding '{}' no longer reproduces", f.subject)],
        })
        .collect()
}

/// What the judgement turn decided for a worker run.
enum JudgementOutcome {
    Complete,
    Failed(String),
    /// Respawn with this guidance (budget enforced by the caller).
    Respawn(String),
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

/// Result of one buffered parallel worker session (roadmap M3): the event
/// kinds it collected (to be replayed by the engine's single writer) plus its
/// [`runner::RunOutcome`], or the error that aborted the session.
type BufferedRunResult = Result<(Vec<EventKind>, runner::RunOutcome)>;

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
fn parallel_worktree_path(mission_id: &str, feature_id: &str) -> PathBuf {
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
    std::env::temp_dir().join(format!("kranz-wt-{mission_id}-{safe}"))
}

/// Absolute directory for one mission's INTEGRATION worktree (M7 tier 1):
/// the single worktree, checked out to the mission branch, that all
/// mission-branch mutations run in when `workerIsolation = worktree`. Lives
/// under the same temp-dir base as [`parallel_worktree_path`], namespaced
/// with a `_integration` suffix that no real feature id can produce (feature
/// ids never start with `_`), so it never collides with a per-feature path.
fn mission_worktree_path(mission_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("kranz-wt-{mission_id}-_integration"))
}

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

fn role_config_key(role: Role) -> &'static str {
    match role {
        Role::Orchestrator => "orchestrator",
        Role::Worker => "worker",
        Role::ValidatorScrutiny => "validatorScrutiny",
        Role::ValidatorFunctional => "validatorFunctional",
    }
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::Orchestrator => "orchestrator",
        Role::Worker => "worker",
        Role::ValidatorScrutiny => "scrutiny validator",
        Role::ValidatorFunctional => "functional validator",
    }
}

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

/// Upsert one mission's line in the missions catalog (`missions/index.md`):
/// `- <date> · [<id>](<id>/plan.md) — <goal>`. Newest last; a re-approval
/// replaces the mission's existing line instead of appending a duplicate.
pub fn upsert_mission_index(
    existing: &str,
    mission_id: &str,
    goal: &str,
    date: chrono::NaiveDate,
) -> String {
    const HEADER: &str = "# Kranz missions\n\nApproved plans, newest last.\n";
    let goal = crate::scrub::truncate_chars(goal.trim(), 120).replace('\n', " ");
    let line = format!("- {date} · [{mission_id}]({mission_id}/plan.md) — {goal}");
    let marker = format!("[{mission_id}](");

    let mut out = String::new();
    let mut replaced = false;
    let body = if existing.trim().is_empty() {
        HEADER
    } else {
        existing
    };
    for l in body.lines() {
        if l.contains(&marker) {
            out.push_str(&line);
            replaced = true;
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    if !replaced {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Render the approved plan as human-readable markdown — committed to the
/// mission branch beside plan.json for later review and reference. Pure and
/// deterministic given its inputs (no wall-clock reads; the estimate is
/// computed by the caller and passed in).
pub fn render_plan_markdown(
    plan: &Plan,
    mission: &Mission,
    estimate: &cost::CostEstimate,
    missions_used: usize,
) -> String {
    use std::fmt::Write as _;
    let mut md = String::new();
    let _ = writeln!(md, "# Mission plan — {}", mission.id);
    let _ = writeln!(md, "\n**Goal:** {}\n", plan.goal);
    let _ = writeln!(
        md,
        "Branch `{}` (from `{}`). Approved plan of record; the machine-readable \
         twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.\n",
        mission.mission_branch, mission.base_branch
    );

    let _ = writeln!(md, "## Cost estimate\n");
    let provenance = if missions_used == 0 {
        "built-in defaults — no completed missions yet".to_string()
    } else {
        format!("based on {missions_used} completed mission(s)")
    };
    match estimate.confidence {
        cost::Confidence::High => {
            let _ = writeln!(
                md,
                "Estimated **${:.2} – ${:.2}** (expected ~${:.2}). Rough estimate — live usage is \
                 authoritative; {provenance}.\n",
                estimate.low_usd, estimate.high_usd, estimate.expected_usd
            );
        }
        cost::Confidence::Low => {
            let _ = writeln!(
                md,
                "Estimated **${:.2} – ${:.2}** (expected ~${:.2}). Doc-heavy / judgement-heavy \
                 shape — the calibration corpus lacks a comparable mission, so this is **LOW \
                 CONFIDENCE** and ${:.2} is a soft ceiling, not a tight bound; {provenance}.\n",
                estimate.low_usd, estimate.high_usd, estimate.expected_usd, estimate.high_usd
            );
        }
    }

    if let Some(alternatives) = &plan.considered_alternatives {
        let _ = writeln!(md, "## Considered alternatives\n");
        let _ = writeln!(md, "**Chosen approach:** {}\n", alternatives.chosen.trim());
        if !alternatives.rejected.is_empty() {
            let _ = writeln!(md, "Rejected shapes:");
            for rejected in &alternatives.rejected {
                let _ = writeln!(
                    md,
                    "- **{}** — {}",
                    rejected.approach.trim(),
                    rejected.trade_off.trim()
                );
            }
            let _ = writeln!(md);
        }
    }

    let _ = writeln!(md, "## Validation contract\n");
    let _ = writeln!(
        md,
        "Defined before any feature; gates mission completion.\n"
    );
    for a in &plan.validation_contract {
        match (&a.check, &a.command) {
            (AssertionCheck::Command, Some(cmd)) => {
                let _ = writeln!(md, "- **[{}]** {} \n  `{}`", a.id, a.statement, cmd);
            }
            _ => {
                let _ = writeln!(md, "- **[{}]** {} *(agent judgement)*", a.id, a.statement);
            }
        }
    }

    for (mi, m) in plan.milestones.iter().enumerate() {
        let _ = writeln!(md, "\n## Milestone {} — {}\n", mi + 1, m.title);
        for (fi, f) in m.features.iter().enumerate() {
            let _ = writeln!(md, "### {}.{} {}\n", mi + 1, fi + 1, f.title);
            let _ = writeln!(md, "{}\n", f.spec.trim());
            if !f.validation_criteria.is_empty() {
                let _ = writeln!(md, "Done when:");
                for c in &f.validation_criteria {
                    let _ = writeln!(md, "- {c}");
                }
                let _ = writeln!(md);
            }
        }
    }
    md
}

/// The `research` evidence object the orchestrator may emit alongside the plan
/// JSON (repo-knowledge-store slice 1). The [`Plan`] struct ignores it (no
/// `deny_unknown_fields`); [`extract_research`] pulls it from the raw JSON so it
/// can be rendered to `research.md` without adding a field to every `Plan`
/// literal. All fields optional so a partial object still parses.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Research {
    files_read: Vec<String>,
    sources: Vec<String>,
    facts: Vec<ResearchFact>,
    ambiguities: Vec<String>,
    candidate_knowledge_updates: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ResearchFact {
    fact: String,
    evidence: String,
}

impl Research {
    fn is_empty(&self) -> bool {
        self.files_read.is_empty()
            && self.sources.is_empty()
            && self.facts.is_empty()
            && self.ambiguities.is_empty()
            && self.candidate_knowledge_updates.is_empty()
    }
}

/// Pull the optional `research` object out of the orchestrator's plan JSON.
/// Returns `None` when absent, unparseable, or empty.
fn extract_research(plan_text: &str) -> Option<Research> {
    let value: serde_json::Value = runner::parse_report(plan_text)?;
    let research: Research = serde_json::from_value(value.get("research")?.clone()).ok()?;
    (!research.is_empty()).then_some(research)
}

/// Render a `research.md` audit artifact from the extracted evidence.
fn render_research_markdown(research: &Research, mission_id: &str) -> String {
    use std::fmt::Write as _;
    let mut md = format!(
        "# Research — {mission_id}\n\nEvidence behind the approved plan \
         (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates \
         feed `docs/knowledge/`.\n"
    );
    let list = |md: &mut String, title: &str, items: &[String]| {
        let items: Vec<&str> = items
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if items.is_empty() {
            return;
        }
        let _ = write!(md, "\n## {title}\n\n");
        for it in items {
            let _ = writeln!(md, "- {it}");
        }
    };
    list(&mut md, "Files & docs read", &research.files_read);
    list(&mut md, "External sources", &research.sources);
    let facts: Vec<&ResearchFact> = research
        .facts
        .iter()
        .filter(|f| !f.fact.trim().is_empty())
        .collect();
    if !facts.is_empty() {
        let _ = write!(md, "\n## Facts\n\n");
        for f in facts {
            let fact = f.fact.trim();
            let ev = f.evidence.trim();
            if ev.is_empty() {
                let _ = writeln!(md, "- {fact}");
            } else {
                let _ = writeln!(md, "- {fact} — `{ev}`");
            }
        }
    }
    list(&mut md, "Ambiguities & stale docs", &research.ambiguities);
    list(
        &mut md,
        "Candidate knowledge updates",
        &research.candidate_knowledge_updates,
    );
    md
}

/// Which base ref the final gate's agent-judgement turn diffs against: the base
/// pinned at approval (`base_sha`), never the moving base branch — a base that
/// advanced mid-mission would silently change the judge's diff (same
/// never-re-resolve rule as the command env's `KRANZ_BASE_SHA`). Falls back to
/// `base_branch` only for legacy missions with no pinned sha.
fn judge_diff_base(base_sha: Option<&str>, base_branch: &str) -> String {
    match base_sha {
        Some(sha) if !sha.is_empty() => sha.to_string(),
        _ => base_branch.to_string(),
    }
}

/// Prompt policy asking the orchestrator to document its research for
/// broad/expensive plans (mirrors [`considered_alternatives_prompt_policy`]'s
/// triggers). The evidence lands in `research.md` beside `plan.md`.
fn research_prompt_policy(cfg: &MissionConfig) -> String {
    let mut triggers = Vec::new();
    if cfg.considered_alternatives_feature_threshold > 0 {
        triggers.push(format!(
            "{}+ features",
            cfg.considered_alternatives_feature_threshold
        ));
    }
    if cfg.considered_alternatives_touch_set_threshold > 0 {
        triggers.push(format!(
            "{}+ touchSet patterns",
            cfg.considered_alternatives_touch_set_threshold
        ));
    }
    if cfg.considered_alternatives_high_usd_threshold > 0.0 {
        triggers.push(format!(
            "likely high estimate at or above ${:.2}",
            cfg.considered_alternatives_high_usd_threshold
        ));
    }
    if triggers.is_empty() {
        return "The optional research object may be omitted.".to_string();
    }
    format!(
        "Policy: if this plan crosses any large-scope trigger ({}) include a research object \
         documenting filesRead, sources, facts (each with concise evidence: a path, command, or \
         URL), ambiguities/stale docs found, and candidateKnowledgeUpdates (proposed docs/knowledge/ \
         notes). Small plans may omit it.",
        triggers.join(", ")
    )
}

fn considered_alternatives_prompt_policy(cfg: &MissionConfig) -> String {
    let feature_threshold = cfg.considered_alternatives_feature_threshold;
    let touch_threshold = cfg.considered_alternatives_touch_set_threshold;
    let cost_threshold = cfg.considered_alternatives_high_usd_threshold;
    let mut triggers = Vec::new();
    if feature_threshold > 0 {
        triggers.push(format!("{feature_threshold}+ features"));
    }
    if touch_threshold > 0 {
        triggers.push(format!("{touch_threshold}+ touchSet patterns"));
    }
    if cost_threshold > 0.0 {
        triggers.push(format!(
            "likely high estimate at or above ${cost_threshold:.2}"
        ));
    }
    if triggers.is_empty() {
        return "The optional consideredAlternatives field may be omitted.".to_string();
    }
    format!(
        "Policy: if this plan crosses any large-scope trigger ({}) include \
         consideredAlternatives with a non-empty chosen approach and at least two rejected \
         approaches, each with a one-line tradeOff. Small plans may omit it.",
        triggers.join(", ")
    )
}

fn validate_considered_alternatives(
    plan: &Plan,
    estimate: &cost::CostEstimate,
    cfg: &MissionConfig,
) -> Result<()> {
    let required = considered_alternatives_requirement(plan, estimate, cfg);
    match (&plan.considered_alternatives, required) {
        (None, Some(reason)) => Err(EngineError::InvalidState(format!(
            "considered alternatives required: {reason}. Add consideredAlternatives with a \
             chosen approach and at least two rejected approaches with tradeOff."
        ))),
        (Some(alternatives), _) => validate_considered_alternatives_body(alternatives),
        (None, None) => Ok(()),
    }
}

fn considered_alternatives_requirement(
    plan: &Plan,
    estimate: &cost::CostEstimate,
    cfg: &MissionConfig,
) -> Option<String> {
    let features = plan_feature_count(plan);
    if cfg.considered_alternatives_feature_threshold > 0
        && features >= cfg.considered_alternatives_feature_threshold
    {
        return Some(format!(
            "{features} feature(s) >= feature threshold {}",
            cfg.considered_alternatives_feature_threshold
        ));
    }
    let touch_set = plan.touch_set.len();
    if cfg.considered_alternatives_touch_set_threshold > 0
        && touch_set >= cfg.considered_alternatives_touch_set_threshold
    {
        return Some(format!(
            "{touch_set} touchSet pattern(s) >= touchSet threshold {}",
            cfg.considered_alternatives_touch_set_threshold
        ));
    }
    if cfg.considered_alternatives_high_usd_threshold > 0.0
        && estimate.high_usd >= cfg.considered_alternatives_high_usd_threshold
    {
        return Some(format!(
            "estimated high cost ${:.2} >= cost threshold ${:.2}",
            estimate.high_usd, cfg.considered_alternatives_high_usd_threshold
        ));
    }
    None
}

fn validate_considered_alternatives_body(alternatives: &ConsideredAlternatives) -> Result<()> {
    if alternatives.chosen.trim().is_empty() {
        return Err(EngineError::InvalidState(
            "consideredAlternatives.chosen must not be empty".to_string(),
        ));
    }
    let valid_rejected = alternatives
        .rejected
        .iter()
        .filter(|r| !r.approach.trim().is_empty() && !r.trade_off.trim().is_empty())
        .count();
    if valid_rejected < 2 {
        return Err(EngineError::InvalidState(
            "consideredAlternatives.rejected must include at least two entries with approach \
             and tradeOff"
                .to_string(),
        ));
    }
    Ok(())
}

fn plan_feature_count(plan: &Plan) -> usize {
    plan.milestones.iter().map(|m| m.features.len()).sum()
}

fn validate_revised_plan_for_gate(mission: &Mission, plan: &Plan) -> Result<()> {
    if plan.milestones.is_empty() {
        return Err(EngineError::InvalidState(
            "revised plan has no milestones".to_string(),
        ));
    }
    if let Some(empty) = plan.milestones.iter().find(|m| m.features.is_empty()) {
        return Err(EngineError::InvalidState(format!(
            "revised plan milestone '{}' has no features",
            empty.title
        )));
    }
    validate_contract_extends(&mission.validation_contract, &plan.validation_contract)?;
    validate_vec_extends(
        "commandGrants",
        &mission.command_grants,
        &plan.command_grants,
    )?;
    validate_vec_extends("touchSet", &mission.touch_set, &plan.touch_set)?;

    let completed: Vec<&Milestone> = mission
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
    Ok(())
}

fn validate_contract_extends(existing: &[Assertion], revised: &[Assertion]) -> Result<()> {
    for old in existing {
        let Some(new) = revised.iter().find(|a| a.id == old.id) else {
            return Err(EngineError::InvalidState(format!(
                "revised plan removes validation assertion '{}'",
                old.id
            )));
        };
        if old.statement != new.statement || old.check != new.check || old.command != new.command {
            return Err(EngineError::InvalidState(format!(
                "revised plan changes validation assertion '{}'",
                old.id
            )));
        }
    }
    Ok(())
}

fn validate_vec_extends(label: &str, existing: &[String], revised: &[String]) -> Result<()> {
    for old in existing {
        if !revised.iter().any(|new| new == old) {
            return Err(EngineError::InvalidState(format!(
                "revised plan removes {label} entry '{old}'"
            )));
        }
    }
    Ok(())
}

/// Whether a completed milestone's feature set is reproduced UNCHANGED in the
/// revised plan milestone (roadmap M2 re-planning guard). Delegates to the
/// reducer's canonical [`reducer::completed_features_match`] so this pre-emit
/// gate and the reducer's fold can never disagree: if they did, the gate could
/// accept a revision the reducer rejects, and `emit` (which appends before it
/// folds) would leave an unfoldable event in the append-only log.
fn completed_features_unchanged(done: &Milestone, revised: &PlanMilestone) -> bool {
    reducer::completed_features_match(&done.features, &revised.features)
}

/// Normalize a feature title for matching across a re-plan (trim + lowercase):
/// trivial editorial differences must not spuriously drop or re-add a feature.
fn norm_title(title: &str) -> String {
    title.trim().to_lowercase()
}

/// Render the revised plan as human-readable markdown for review (roadmap M2),
/// committed to the mission branch as `revised-plan.md`. Shows the full revised
/// plan plus a "Re-plan changes" section describing the reducer's merge rules.
/// Older single-milestone re-plan callers can still pass explicit dropped/added
/// feature lists; the event-driven M2 path records the exact structural change
/// in the `plan.revised` event and folded state.
pub fn render_revised_plan_markdown(
    plan: &Plan,
    mission: &Mission,
    dropped_feature_ids: &[String],
    added_features: &[PlanFeature],
) -> String {
    use std::fmt::Write as _;
    let mut md = String::new();
    let _ = writeln!(md, "# Revised mission plan — {}", mission.id);
    let _ = writeln!(md, "\n**Goal:** {}\n", plan.goal);
    let _ = writeln!(
        md,
        "Branch `{}` (from `{}`). Mid-mission revision of the plan of record \
         ([plan.md](plan.md)); completed milestones are preserved unchanged.\n",
        mission.mission_branch, mission.base_branch
    );

    let _ = writeln!(md, "## Re-plan changes applied\n");
    let _ = writeln!(
        md,
        "Completed milestones are frozen unchanged. Remaining milestones are merged by \
         position; existing pending features match by title, omitted pending features are \
         skipped, and new feature titles are appended with revision-scoped ids. Review the \
         resulting plan below and the `plan.revised` event for the exact machine state.\n"
    );
    if !dropped_feature_ids.is_empty() {
        let _ = writeln!(
            md,
            "- Dropped (skipped) features: {}",
            dropped_feature_ids.join(", ")
        );
    }
    if !added_features.is_empty() {
        let titles: Vec<String> = added_features
            .iter()
            .map(|f| f.title.trim().to_string())
            .collect();
        let _ = writeln!(md, "- Added features: {}", titles.join(", "));
    }
    if dropped_feature_ids.is_empty() && added_features.is_empty() {
        let _ = writeln!(
            md,
            "- Per-feature legacy diff: not supplied for this revision path"
        );
    }
    let _ = writeln!(md);

    let _ = writeln!(md, "## Full revised plan\n");
    for (mi, m) in plan.milestones.iter().enumerate() {
        let _ = writeln!(md, "### Milestone {} — {}\n", mi + 1, m.title);
        for (fi, f) in m.features.iter().enumerate() {
            let _ = writeln!(md, "#### {}.{} {}\n", mi + 1, fi + 1, f.title);
            let _ = writeln!(md, "{}\n", f.spec.trim());
            if !f.validation_criteria.is_empty() {
                let _ = writeln!(md, "Done when:");
                for c in &f.validation_criteria {
                    let _ = writeln!(md, "- {c}");
                }
                let _ = writeln!(md);
            }
        }
    }
    md
}

/// Remove a single mission's line from the missions catalog (deletion
/// counterpart to [`upsert_mission_index`]), matched by the same
/// `[<id>](` marker. Every other line and the header stay byte-for-byte;
/// pruning an id with no line is a no-op (modulo trailing-newline
/// normalization, same as [`mark_mission_index_report`]).
pub fn prune_mission_index(existing: &str, mission_id: &str) -> String {
    if existing.trim().is_empty() {
        return existing.to_string();
    }
    let marker = format!("[{mission_id}](");
    let mut out = String::new();
    for l in existing.lines() {
        if l.contains(&marker) {
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Every mission id appearing as `[<id>](` in the catalog body, in file
/// order, de-duplicated. Tolerant of the trailing ` · [report](<id>/report.md)`
/// link: the FIRST bracket on a line (the plan.md link) is taken as the id.
pub fn mission_index_ids(existing: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for l in existing.lines() {
        if !l.contains("](") {
            continue;
        }
        let Some(start) = l.find('[') else {
            continue;
        };
        let rest = &l[start + 1..];
        let Some(end) = rest.find("](") else {
            continue;
        };
        let id = &rest[..end];
        if !ids.iter().any(|existing_id: &String| existing_id == id) {
            ids.push(id.to_string());
        }
    }
    ids
}

/// Prune one mission's line from `<repo>/.kranz/missions/index.md` and
/// write the result back. A missing index file is a no-op — it is never
/// created here.
pub fn prune_mission_index_file(repo_root: &std::path::Path, mission_id: &str) {
    let index = MissionPaths::new(repo_root, "_")
        .missions_dir()
        .join("index.md");
    let Ok(existing) = std::fs::read_to_string(&index) else {
        return;
    };
    let updated = prune_mission_index(&existing, mission_id);
    let _ = std::fs::write(&index, updated);
}

/// Add a completion-report link to one mission's line in the missions
/// catalog, turning
/// `- <date> · [<id>](<id>/plan.md) — <goal>` into
/// `- <date> · [<id>](<id>/plan.md) — <goal> · [report](<id>/report.md)`.
///
/// Idempotent; every other line — and the line format itself — stays
/// untouched. When the mission has no line, the index comes back unchanged.
pub fn mark_mission_index_report(existing: &str, mission_id: &str) -> String {
    let marker = format!("[{mission_id}](");
    let link = format!("[report]({mission_id}/report.md)");
    let mut out = String::new();
    for l in existing.lines() {
        out.push_str(l);
        if l.contains(&marker) && !l.contains(&link) {
            out.push_str(" · ");
            out.push_str(&link);
        }
        out.push('\n');
    }
    out
}

/// Render the mission completion report (roadmap M1): elapsed time and cost
/// vs the pre-mission estimate, what shipped per feature, the validation
/// history including waived findings with their justifications, and the
/// contract outcomes. Committed beside plan.md at mission completion and
/// linked from missions/index.md.
///
/// Deterministic given its inputs: every timestamp derives from `events`
/// (no wall clock — the completion instant is the mission.completed ts when
/// present, else the last event's, since the engine renders the report just
/// before emitting mission.completed). No scrubbing happens here: all
/// model-authored text in `state`/`events` was scrubbed at emit time.
pub fn render_mission_report(
    state: &MissionState,
    events: &[Event],
    plan: &Plan,
    estimate: &cost::CostEstimate,
) -> String {
    use std::fmt::Write as _;
    let mission = &state.mission;
    let mut md = String::new();
    let _ = writeln!(md, "# Mission report — {}", mission.id);
    let _ = writeln!(md, "\n**Goal:** {}\n", mission.goal);
    let _ = writeln!(
        md,
        "Branch `{}` (from `{}`). Plan of record: [plan.md](plan.md).\n",
        mission.mission_branch, mission.base_branch
    );

    // Elapsed wall clock: created → completed, minus paused spans.
    let completed_ts = events
        .iter()
        .rev()
        .find_map(|e| matches!(e.kind, EventKind::MissionCompleted {}).then_some(e.ts))
        .or_else(|| events.last().map(|e| e.ts))
        .unwrap_or(mission.created_at);
    let paused = paused_time(events, completed_ts);
    let elapsed = std::cmp::max(
        completed_ts - mission.created_at - paused,
        chrono::Duration::zero(),
    );
    let _ = write!(md, "**Elapsed:** {}", format_duration(elapsed));
    if paused > chrono::Duration::zero() {
        let _ = write!(md, " ({} paused)", format_duration(paused));
    }
    let _ = writeln!(md);
    let t = &state.totals;
    let _ = writeln!(
        md,
        "**Tokens:** {} in / {} out / {} cache read / {} cache write",
        t.input, t.output, t.cache_read, t.cache_write
    );
    let _ = writeln!(
        md,
        "**Cost:** ${:.2} actual vs ${:.2}–${:.2} estimated (expected ${:.2})",
        state.total_cost_usd, estimate.low_usd, estimate.high_usd, estimate.expected_usd
    );

    // What shipped — per milestone, per feature (fix features included, in
    // the order the reducer materialized them).
    let _ = writeln!(md, "\n## What shipped");
    for (mi, m) in mission.milestones.iter().enumerate() {
        let _ = writeln!(
            md,
            "\n### Milestone {} — {} {}\n",
            mi + 1,
            m.title,
            milestone_icon(m.status)
        );
        for f in &m.features {
            let runs = f.worker_runs.len();
            let _ = write!(
                md,
                "- {} **{}**{} — {} run{}",
                feature_icon(f.status),
                f.title,
                if f.origin == FeatureOrigin::Fix {
                    " *(fix)*"
                } else {
                    ""
                },
                runs,
                if runs == 1 { "" } else { "s" },
            );
            if f.respawns > 0 {
                let _ = write!(
                    md,
                    ", {} respawn{}",
                    f.respawns,
                    if f.respawns == 1 { "" } else { "s" }
                );
            }
            let _ = writeln!(md);
            for commit in &f.commits {
                let _ = writeln!(md, "  - {}", short_commit(commit));
            }
        }
    }

    // Validation history — replayed from the event log.
    let _ = writeln!(md, "\n## Validation history");
    let rounds = collect_validation_rounds(events);
    let mut per_milestone_round: HashMap<&str, usize> = HashMap::new();
    let mut rendered_any = false;
    for round in &rounds {
        // Rounds with neither findings nor a clean completion are reopening
        // bookkeeping (the gate's fix path re-emits milestone.validating
        // before fixfeature.created), not validation rounds.
        if round.findings.is_empty() && !round.clean {
            continue;
        }
        rendered_any = true;
        match round.milestone_id {
            Some(id) => {
                let n = per_milestone_round.entry(id).or_insert(0);
                *n += 1;
                let title = mission
                    .milestones
                    .iter()
                    .find(|m| m.id == id)
                    .map(|m| m.title.as_str())
                    .unwrap_or("");
                let _ = writeln!(md, "\n### {id} round {n} — {title}\n");
            }
            None => {
                let _ = writeln!(md, "\n### Final gate\n");
            }
        }
        if round.findings.is_empty() {
            let _ = writeln!(md, "No findings.");
            continue;
        }
        for (run_id, finding) in &round.findings {
            let gate = if *run_id == crate::reducer::ENGINE_RUN_ID {
                " *(final gate)*"
            } else {
                ""
            };
            let evidence = scrub::scrub_and_truncate(
                &finding
                    .evidence
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                200,
            );
            let _ = writeln!(
                md,
                "- [{}] {}{gate} — {evidence}",
                finding.severity, finding.subject
            );
        }
        if round.fix_features > 0 {
            let _ = writeln!(
                md,
                "\nDisposition: {} fix feature(s) created.",
                round.fix_features
            );
        }
        if !round.waived.is_empty() {
            let _ = writeln!(md, "\nDisposition: waived.");
            for reasons in &round.waived {
                for line in reasons.lines() {
                    let _ = writeln!(md, "{line}");
                }
            }
        }
        if let Some(reason) = round.blocked {
            let _ = writeln!(md, "\nDisposition: milestone blocked — {reason}");
        }
    }
    if !rendered_any {
        let _ = writeln!(md, "\nNo validation rounds were recorded.");
    }

    // Contract outcomes — the mission completed, so every assertion passed
    // the final gate (or was explicitly waived; waivers are recorded above).
    let _ = writeln!(md, "\n## Contract outcomes");
    if plan.validation_contract.is_empty() {
        let _ = writeln!(md, "\nNo contract assertions were defined.");
    } else {
        let _ = writeln!(md);
        for a in &plan.validation_contract {
            let check = match (&a.check, &a.command) {
                (AssertionCheck::Command, Some(cmd)) => format!("command: `{cmd}`"),
                (AssertionCheck::Command, None) => "command".to_string(),
                _ => "agent judgement".to_string(),
            };
            let _ = writeln!(md, "- ✅ **[{}]** {} *({check})*", a.id, a.statement);
        }
        let _ = writeln!(
            md,
            "\nAll assertions passed at the final contract gate (waivers, if any, appear in \
             the validation history)."
        );
    }
    md
}

/// One validation round replayed from the event log: a `milestone.validating`
/// round, or the final contract gate (`mission.validating` / engine-attributed
/// findings).
struct ValidationRound<'a> {
    /// `None` marks the final gate.
    milestone_id: Option<&'a str>,
    /// `(run_id, finding)` in emit order.
    findings: Vec<(&'a str, &'a Finding)>,
    fix_features: usize,
    /// Waiver reason blocks: the detail of "waived …" orchestrator decisions
    /// (one `- subject: reason` line per finding), falling back to the summary.
    waived: Vec<&'a str>,
    blocked: Option<&'a str>,
    /// The round produced no findings and the milestone completed directly.
    clean: bool,
}

/// Group the log's validation traffic into [`ValidationRound`]s. Dispositions
/// (fix features, waivers, blocks) always follow the findings they answer, so
/// they attach to the last round that has findings — which also covers the
/// gate's fix path, where the reopening `milestone.validating` arrives between
/// the gate findings and their `fixfeature.created` events.
fn collect_validation_rounds(events: &[Event]) -> Vec<ValidationRound<'_>> {
    fn round(milestone_id: Option<&str>) -> ValidationRound<'_> {
        ValidationRound {
            milestone_id,
            findings: Vec::new(),
            fix_features: 0,
            waived: Vec::new(),
            blocked: None,
            clean: false,
        }
    }
    let mut rounds: Vec<ValidationRound<'_>> = Vec::new();
    for event in events {
        match &event.kind {
            EventKind::MilestoneValidating { milestone_id } => {
                rounds.push(round(Some(milestone_id)));
            }
            EventKind::MissionValidating {} => rounds.push(round(None)),
            EventKind::ValidationFinding {
                milestone_id,
                run_id,
                finding,
            } => {
                // Engine-attributed findings belong to a gate round; the
                // second gate pass runs without a fresh mission.validating
                // (the status is already Validating), so open one on demand.
                let gate = run_id == crate::reducer::ENGINE_RUN_ID;
                let fits = rounds
                    .last()
                    .is_some_and(|r| !gate || r.milestone_id.is_none());
                if !fits {
                    rounds.push(round(if gate { None } else { Some(milestone_id) }));
                }
                rounds
                    .last_mut()
                    .expect("pushed above")
                    .findings
                    .push((run_id, finding));
            }
            EventKind::FixFeatureCreated { .. } => {
                if let Some(r) = rounds.iter_mut().rev().find(|r| !r.findings.is_empty()) {
                    r.fix_features += 1;
                }
            }
            EventKind::OrchestratorDecision { summary, detail }
                if summary.starts_with("waived") =>
            {
                if let Some(r) = rounds.iter_mut().rev().find(|r| !r.findings.is_empty()) {
                    r.waived.push(detail.as_deref().unwrap_or(summary));
                }
            }
            EventKind::MilestoneBlocked { reason, .. } => {
                if let Some(r) = rounds.iter_mut().rev().find(|r| !r.findings.is_empty()) {
                    r.blocked = Some(reason);
                }
            }
            EventKind::MilestoneCompleted { milestone_id, .. } => {
                if let Some(r) = rounds.last_mut() {
                    if r.milestone_id == Some(milestone_id.as_str()) && r.findings.is_empty() {
                        r.clean = true;
                    }
                }
            }
            _ => {}
        }
    }
    rounds
}

/// Total time the mission spent paused: fold `mission.paused`/`mission.resumed`
/// spans; a pause still open at `end` counts up to `end` (defensive — a
/// completed mission always resumed).
fn paused_time(events: &[Event], end: chrono::DateTime<chrono::Utc>) -> chrono::Duration {
    let mut total = chrono::Duration::zero();
    let mut paused_at: Option<chrono::DateTime<chrono::Utc>> = None;
    for event in events {
        match &event.kind {
            EventKind::MissionPaused {} => {
                if paused_at.is_none() {
                    paused_at = Some(event.ts);
                }
            }
            EventKind::MissionResumed {} => {
                if let Some(start) = paused_at.take() {
                    total += event.ts - start;
                }
            }
            _ => {}
        }
    }
    if let Some(start) = paused_at {
        total += end - start;
    }
    total
}

/// `4h 02m 09s` / `4m 02s` / `42s` (whole seconds; sub-second missions say 0s).
fn format_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// One `feature.completed` commit entry (`<sha> <subject>`) as markdown:
/// short sha in backticks + the subject. Entries that don't look like a sha
/// pass through verbatim.
fn short_commit(entry: &str) -> String {
    let (sha, subject) = match entry.split_once(' ') {
        Some((sha, subject)) => (sha, subject.trim()),
        None => (entry, ""),
    };
    let looks_sha = sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit());
    match (looks_sha, subject.is_empty()) {
        (true, false) => format!("`{}` {}", &sha[..7], subject),
        (true, true) => format!("`{}`", &sha[..7]),
        _ => entry.to_string(),
    }
}

fn feature_icon(status: FeatureStatus) -> &'static str {
    match status {
        FeatureStatus::Complete => "✅",
        FeatureStatus::Failed => "❌",
        FeatureStatus::Skipped => "⏭",
        FeatureStatus::Active | FeatureStatus::Pending => "⏳",
    }
}

fn milestone_icon(status: MilestoneStatus) -> &'static str {
    match status {
        MilestoneStatus::Complete => "✅",
        MilestoneStatus::Blocked => "⛔",
        _ => "⏳",
    }
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
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
}

/// Whether a lesson-turn reply is the single word NONE (case-insensitive,
/// ignoring surrounding whitespace/punctuation).
fn is_none_reply(text: &str) -> bool {
    text.trim()
        .trim_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .eq_ignore_ascii_case("none")
}

/// Cap on the lesson index-entry line, so a long paragraph reply can't
/// masquerade as the one-line summary read by the injection step.
const LESSON_SUMMARY_CAP: usize = 140;

/// Normalize a lesson reply so its first line is a short, usable one-line
/// summary (the injection step reads the first line as the index entry).
/// If the reply's first line already fits under the cap it is left as-is;
/// otherwise a capped version of it is prepended as a new first line, ahead
/// of the reply's full prose.
fn normalize_lesson_body(text: &str) -> String {
    let trimmed = text.trim();
    let first_line = first_nonempty_line(trimmed);
    if first_line.chars().count() <= LESSON_SUMMARY_CAP {
        return trimmed.to_string();
    }
    let capped: String = first_line.chars().take(LESSON_SUMMARY_CAP - 1).collect();
    format!("{capped}…\n\n{trimmed}")
}

/// Canonicalize the repo root when possible (macOS tempdirs are symlinks
/// under /var → /private/var; git pathspec matching needs the real path).
fn canonical_root(root: PathBuf) -> PathBuf {
    std::fs::canonicalize(&root).unwrap_or(root)
}

// ---------------------------------------------------------------------------
// Preflight helpers (roadmap M2) — all pure/best-effort, no engine state
// ---------------------------------------------------------------------------

/// Extract the leading program token of a contract `command` line for a PATH
/// probe (roadmap M2 preflight). Best-effort by design:
///
/// - `sh -c '…'` / `bash -c '…'` / `python3 -c '…'`-style forms name the
///   INTERPRETER as the program (the thing that must exist), so the first
///   token is returned rather than trying to parse the embedded script.
/// - Otherwise the first whitespace-delimited token is returned, with common
///   leading `VAR=value` environment assignments skipped and a leading path
///   (`./scripts/check.sh`) reduced to its final component only for the
///   presence check semantics of [`program_resolves`].
///
/// Returns `None` when no plausible program token can be found (empty command,
/// or a line that is only environment assignments) — the caller then skips the
/// probe rather than emit a spurious warning.
fn leading_program(command: &str) -> Option<String> {
    // Skip leading `VAR=value` assignments ("FOO=bar cmd …" is common in
    // contract lines); the program is the first token that is not an
    // assignment.
    let mut token = None;
    for tok in command.split_whitespace() {
        if is_env_assignment(tok) {
            continue;
        }
        token = Some(tok);
        break;
    }
    let token = token?;
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

/// A `VAR=value` leading environment assignment (`FOO=bar`): an identifier,
/// then `=`. Used to skip past them when finding the program token.
fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) if !name.is_empty() => {
            let mut chars = name.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// Whether a program token plausibly resolves to something runnable
/// (roadmap M2 preflight). LENIENT: this only ever produces a warning, so it
/// errs heavily toward "resolves" to avoid false positives.
///
/// - A program containing a path separator is checked as a filesystem path
///   (it names its own location; PATH does not apply).
/// - A bare name is looked up across every `PATH` entry.
/// - Common POSIX shell builtins that have no on-disk binary (`cd`, `:`,
///   `true`, `false`, `echo`, `test`, `[`) always resolve — a contract line
///   like `cd . && …` must never warn.
///
/// On non-unix hosts the executable-bit check is skipped (mere existence in a
/// PATH dir counts), and `.exe`/`.bat`/`.cmd` variants are also accepted.
fn program_resolves(program: &str) -> bool {
    // Shell builtins with no backing binary — never a missing prerequisite.
    const BUILTINS: &[&str] = &[
        "cd", ":", "true", "false", "echo", "test", "[", "set", "export", "unset",
    ];
    if BUILTINS.contains(&program) {
        return true;
    }

    // A path-bearing program names its own location; PATH does not apply.
    if program.contains('/') || program.contains('\\') {
        return path_is_executable(std::path::Path::new(program));
    }

    let Some(path) = std::env::var_os("PATH") else {
        // No PATH to scan: cannot disprove existence, so do not warn.
        return true;
    };
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if path_is_executable(&dir.join(program)) {
            return true;
        }
        // Windows: accept the usual executable extensions.
        #[cfg(windows)]
        for ext in ["exe", "bat", "cmd", "com"] {
            if path_is_executable(&dir.join(format!("{program}.{ext}"))) {
                return true;
            }
        }
    }
    false
}

/// Whether `path` is a regular file that is executable (unix: any execute bit;
/// other platforms: mere existence as a file).
fn path_is_executable(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Run `program args` to completion, polling with a bounded wall-clock
/// (`timeout`) rather than blocking forever — the sandbox preflight probe
/// runs operator-authored contract commands and must never hang a mission
/// start. Returns `None` on spawn failure or on timeout (the child is killed).
fn run_with_timeout(
    program: &std::path::Path,
    args: &[String],
    timeout: Duration,
) -> Option<std::process::Output> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

/// Last `max` characters of `text` (for stderr tails in sandbox preflight
/// messages — never splits a code point).
fn last_chars_local(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(max);
    chars[start..].iter().collect()
}

/// Whether `root` looks like a git repository — a `.git` entry exists (a dir
/// for a normal repo, a file for a worktree/submodule gitlink). Best-effort:
/// only a plainly-absent `.git` produces the preflight error.
fn is_git_repo(root: &std::path::Path) -> bool {
    root.join(".git").exists()
}

/// Whether the mission's `.kranz` directory is writable: create it if needed,
/// then probe with a temp file. Conservative — any error other than a clean
/// write is reported as "not writable".
fn kranz_dir_is_writable(paths: &MissionPaths) -> bool {
    let dir = paths.kranz_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".preflight-{}", uuid::Uuid::new_v4().simple()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
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
             missions/*/estimate.json\n\
             missions/*/control/\n\
             missions/*/runs/\n\
             slack-threads.json\n\
             queue/\n\
             tickets/*.status\n\
             serve.token\n",
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Mission hygiene (roadmap M2): abandon + clean classification
//
// These are lifecycle helpers kept deliberately OUTSIDE the run loop — they
// never touch the orchestrator session and only ever append the terminal
// `mission.abandoned` event or classify a directory for removal.
// ---------------------------------------------------------------------------

/// Retire a mission as [`MissionStatus::Abandoned`] — a terminal, operator-
/// initiated end-of-life that is *not* a failure (§ roadmap M2 "mission
/// hygiene"; the contract already defines the event + reducer mapping).
///
/// Acquires the single-writer lock via [`EventLog::acquire`], so a live engine
/// holding it surfaces as [`EngineError::LockHeld`] (the CLI then tells the
/// operator to stop the running mission; `--force-lock` steals only a lock
/// whose holder is not provably alive, `--dangerously-steal-live-lock` steals
/// even a live one). A mission that
/// is already terminal (Complete/Failed/Abandoned) is rejected with
/// [`EngineError::InvalidState`] — abandoning is only meaningful for live work.
/// On success one `mission.abandoned` event is appended, the state snapshot is
/// refreshed, and the lock released on drop.
///
/// A ticket that points at this mission is left untouched: the reverse mapping
/// (`.kranz/tickets/<slug>.status` → mission id) is not cheaply invertible, and
/// abandoning the mission is the operator's intent regardless.
pub fn abandon_mission(
    repo_root: impl Into<PathBuf>,
    mission_id: &str,
    reason: &str,
    force: LockForce,
) -> Result<()> {
    let repo_root = canonical_root(repo_root.into());
    let paths = MissionPaths::new(&repo_root, mission_id);

    // Fold the existing log first so we can reject an already-terminal mission
    // before writing anything.
    let events = EventLog::read_events(&paths.events_file())?;
    let state = reducer::fold(&events)?;
    if is_terminal_status(state.mission.status) {
        return Err(EngineError::InvalidState(format!(
            "mission '{mission_id}' is already terminal ({:?}); nothing to abandon",
            state.mission.status
        )));
    }

    // Acquire the lock (LockHeld ⇒ a live engine owns this mission).
    let mut log = EventLog::acquire(
        &paths,
        mission_id,
        Duration::from_millis(state.config.event_stream_throttle_ms),
        force,
    )?;
    let (event, audits) = log.append_with_redaction_audits(EventKind::MissionAbandoned {
        reason: reason.to_string(),
    })?;
    // Fold the new event plus any redaction audits on top of the state we
    // already have and snapshot, so state.json matches the log without a full
    // re-fold.
    let mut state = state;
    reducer::apply(&mut state, &event)?;
    for audit in &audits {
        reducer::apply(&mut state, audit)?;
    }
    reducer::write_snapshot(&state, &paths.state_file())?;
    // `log` drops here: buffer flushed, lock released.
    Ok(())
}

/// Terminal mission statuses (no further work will ever run against them).
pub fn is_terminal_status(status: MissionStatus) -> bool {
    matches!(
        status,
        MissionStatus::Complete | MissionStatus::Failed | MissionStatus::Abandoned
    )
}

/// How a mission directory classifies for `kranz clean`. The decision is a
/// pure function of the folded status and whether a `plan.json` exists, so it
/// is trivially testable in isolation from the filesystem walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanClass {
    /// Retire by default (`kranz clean`): the mission failed, was abandoned, or
    /// is an abandoned-in-planning husk (still Planning with no plan.json).
    Stale,
    /// Only removed with `--all`: a Complete mission whose branch/report may
    /// still be under review.
    CompleteKeepByDefault,
    /// Never cleaned: the mission is live (Planning-with-plan, Running, Paused,
    /// Blocked, Validating).
    Keep,
}

impl CleanClass {
    /// Whether this class is removed given the `--all` opt-in.
    pub fn is_cleaned(self, all: bool) -> bool {
        match self {
            CleanClass::Stale => true,
            CleanClass::CompleteKeepByDefault => all,
            CleanClass::Keep => false,
        }
    }
}

/// Classify a mission for cleaning from its folded `status` and whether a
/// `plan.json` is present. Liveness (a held lock) is handled separately by the
/// caller — a running mission is *never* cleaned regardless of this class.
pub fn cleanable_class(status: MissionStatus, has_plan: bool) -> CleanClass {
    match status {
        MissionStatus::Failed | MissionStatus::Abandoned => CleanClass::Stale,
        // An abandoned-in-planning husk: never approved a plan, so nothing on a
        // branch to lose.
        MissionStatus::Planning if !has_plan => CleanClass::Stale,
        MissionStatus::Complete => CleanClass::CompleteKeepByDefault,
        // Planning-with-plan, Running, Paused, Blocked, Validating: live work.
        _ => CleanClass::Keep,
    }
}

/// True when a mission's lock file records a holder that is still alive (a
/// running engine). Delegates to the event-log module's canonical probe
/// ([`crate::event_log::lock_holder_is_alive`]) — ONE source of truth for
/// lock-file format and liveness semantics. A missing lock is not live; a
/// present-but-unreadable/unparseable one is treated as live (conservative —
/// a false "alive" only spares a directory from cleaning, while a false
/// "dead" could delete a mission out from under a running engine); a
/// provably-dead holder (ESRCH, or a token-proven pid reuse) is not live.
/// Non-unix platforms cannot probe, so an existing lock reads as live.
pub fn mission_lock_is_live(paths: &MissionPaths) -> bool {
    crate::event_log::lock_holder_is_alive(&paths.lock_file())
}

/// Pre-flight a `config.changed` patch: the merged result must deserialize
/// and validate, or the event must not be appended (the reducer would poison
/// every future fold of the log).
fn preview_config_patch(current: &MissionConfig, patch: &serde_json::Value) -> Result<()> {
    config::apply_validated_patch(current, patch).map(|_| ())
}

/// Run one user-authored contract command line at the final gate.
///
/// DELIBERATE shell usage (the one place in the engine): contract commands
/// are user-authored shell lines ("npm test -- --grep auth") that need real
/// shell semantics — argument splitting here would corrupt them. `cmd /C` on
/// Windows, `sh -c` elsewhere; cwd = repo root; 10-minute cap.
async fn run_shell_command(
    cwd: &std::path::Path,
    command: &str,
    env: &HashMap<String, String>,
) -> (bool, String) {
    run_shell_command_with_timeout(cwd, command, COMMAND_TIMEOUT, env).await
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
///
/// Windows has no process groups; the equivalent is a Job Object with
/// `KILL_ON_JOB_CLOSE` (see [`crate::backend_claude::win_job`]). The `cmd /C`
/// wrapper is assigned to such a job right after spawn, so on timeout
/// `TerminateJobObject` takes the whole `cmd` tree down — not just the
/// wrapper. That path compiles and is validated only on windows-latest CI,
/// never on the dev host.
async fn run_shell_command_with_timeout(
    cwd: &std::path::Path,
    command: &str,
    timeout: Duration,
    env: &HashMap<String, String>,
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
        .envs(env)
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

    // Windows: assign the `cmd /C` wrapper to a kill-on-close Job Object so the
    // timeout path can kill the whole command tree. Held across the await; on
    // timeout it is killed explicitly and, either way, dropped at scope end
    // (CloseHandle → KILL_ON_JOB_CLOSE). Job setup failure is non-fatal — the
    // command still runs, timeout just falls back to killing the wrapper only.
    // Compiled and validated only on windows-latest CI.
    #[cfg(windows)]
    let job = match child.raw_handle() {
        Some(handle) => crate::backend_claude::win_job::JobHandle::create_and_assign(handle)
            .map_err(|e| {
                tracing::warn!(error = %e, "failed to create Job Object for shell command; \
                    timeout will kill only the cmd wrapper");
            })
            .ok(),
        None => None,
    };

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
            // Windows: TerminateJobObject kills the whole `cmd` tree now
            // (dropping `job` at scope end would also do it via
            // KILL_ON_JOB_CLOSE, but the explicit kill is deterministic).
            #[cfg(windows)]
            if let Some(job) = &job {
                job.kill();
            }
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
            (
                output.status.success(),
                tail_chars(combined.trim_end(), COMMAND_OUTPUT_TAIL),
            )
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
            "consideredAlternatives": {
                "type": "object",
                "additionalProperties": false,
                "required": ["chosen", "rejected"],
                "properties": {
                    "chosen": { "type": "string" },
                    "rejected": {
                        "type": "array",
                        "minItems": 2,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["approach", "tradeOff"],
                            "properties": {
                                "approach": { "type": "string" },
                                "tradeOff": { "type": "string" }
                            }
                        }
                    }
                }
            },
            "research": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "filesRead": { "type": "array", "items": { "type": "string" } },
                    "sources": { "type": "array", "items": { "type": "string" } },
                    "facts": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "fact": { "type": "string" },
                                "evidence": { "type": "string" }
                            }
                        }
                    },
                    "ambiguities": { "type": "array", "items": { "type": "string" } },
                    "candidateKnowledgeUpdates": { "type": "array", "items": { "type": "string" } }
                }
            },
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
            },
            "commandGrants": {
                "type": "array",
                "items": { "type": "string" }
            },
            "touchSet": {
                "type": "array",
                "items": { "type": "string" }
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

    // -----------------------------------------------------------------------
    // Mission integration worktree primitive (M7 tier 1, feature f-1-2)
    // -----------------------------------------------------------------------

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
        assert_eq!(path, mission_worktree_path(&mission_id));
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
            listed
                .iter()
                .any(|p| std::path::Path::new(p) == canon_path.as_path()),
            "integration worktree not in list_worktrees: {listed:?}"
        );

        engine.teardown_mission_worktree();
        let after = engine.repo.list_worktrees().unwrap();
        assert!(
            !after
                .iter()
                .any(|p| std::path::Path::new(p) == canon_path.as_path()),
            "integration worktree still listed after teardown: {after:?}"
        );
        assert!(!path.exists(), "integration worktree dir must be gone");
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
        assert_eq!(path, mission_worktree_path(&mission_id));
        assert!(path.exists(), "integration worktree dir must exist");

        let listed = engine.repo.list_worktrees().unwrap();
        let canon_path = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        assert!(
            listed
                .iter()
                .any(|p| std::path::Path::new(p) == canon_path.as_path()),
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
            !after
                .iter()
                .any(|p| std::path::Path::new(p) == canon_path.as_path()),
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
        let integration = mission_worktree_path(mission_id);
        for feature_id in ["f-1-1", "f-1-2", "ms-collide-test-1"] {
            assert_ne!(
                integration,
                parallel_worktree_path(mission_id, feature_id),
                "collided with feature id {feature_id:?}"
            );
        }
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

    /// Serializes tests that mutate `KRANZ_DROID_BIN` so they don't race
    /// concurrently with each other (mirrors `CODEX_ENV_LOCK`; kept on its
    /// own dedicated mutex since it guards a different env var).
    static DROID_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard: points `KRANZ_DROID_BIN` at a path that cannot exist, so
    /// droid discovery misses deterministically. Restores the previous value
    /// on drop, including on panic.
    struct DroidEnvGuard {
        prev_bin: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl DroidEnvGuard {
        fn engage() -> Self {
            let lock = DROID_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let prev_bin = std::env::var_os("KRANZ_DROID_BIN");
            std::env::set_var(
                "KRANZ_DROID_BIN",
                "/nonexistent/kranz-test-droid-binary-absent",
            );
            DroidEnvGuard {
                prev_bin,
                _lock: lock,
            }
        }
    }

    impl Drop for DroidEnvGuard {
        fn drop(&mut self) {
            match self.prev_bin.take() {
                Some(v) => std::env::set_var("KRANZ_DROID_BIN", v),
                None => std::env::remove_var("KRANZ_DROID_BIN"),
            }
        }
    }

    /// `validatorScrutiny.backend = "droid"` with no droid binary reachable:
    /// preflight must warn and mention "droid".
    #[test]
    fn droid_preflight_warns_when_binary_absent() {
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

        let mut cfg = MissionConfig::default();
        cfg.validator_scrutiny.backend = Some("droid".to_string());

        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let engine = MissionEngine::create(backend, &root, "goal", cfg).expect("create engine");

        let env_guard = DroidEnvGuard::engage();
        let issues = engine.preflight();
        drop(env_guard);

        assert!(
            issues
                .iter()
                .any(|i| i.severity == "warn" && i.message.contains("droid")),
            "expected a droid preflight warning, got {issues:?}"
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
        let mut contract = vec![
            assertion(""),
            assertion("x"),
            assertion("x"),
            assertion("a-2"),
        ];
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
        };
        let run = WorkerRun {
            id: "run-1".to_string(),
            role: Role::Worker,
            feature_id: Some("f1".to_string()),
            milestone_id: None,
            sdk_session_id: "sdk-1".to_string(),
            model: "m".to_string(),
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
            },
            runs,
            totals: TokenUsage::default(),
            total_cost_usd: 0.0,
            pending_user_messages: vec![],
            recent_decisions: vec![],
            config: MissionConfig::default(),
            latest_plan_revision: 0,
            pending_revision: None,
            last_seq: 0,
        };

        assert_eq!(
            worker_commands_for_milestone(&state, &milestone),
            vec!["gc lint".to_string()]
        );
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

    /// Timeout kill discipline: the whole process GROUP dies, not just the
    /// `sh -c` wrapper — a backgrounded child must not survive the gate
    /// giving up. Unix-only test (`kill(-pgid)`); the Windows equivalent uses
    /// a kill-on-close Job Object (see `run_shell_command_with_timeout`) and
    /// is validated by windows-latest CI, not on this host.
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
            run_shell_command_with_timeout(
                dir.path(),
                &command,
                Duration::from_millis(500),
                &std::collections::HashMap::new(),
            ),
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

    /// The final gate's command executor must carry the same
    /// KRANZ_BASE_SHA env that worker/validator sessions get, via the one
    /// shared `runner::contract_env` constructor (mission m-d341a7's false
    /// CRITICAL came from this gate omitting it).
    #[cfg(unix)]
    #[tokio::test]
    async fn base_sha_reaches_final_gate_env() {
        let dir = tempfile::tempdir().unwrap();
        let env = runner::contract_env(Some("deadbeefcafe"));
        let (ok, output) = run_shell_command_with_timeout(
            dir.path(),
            "test \"$KRANZ_BASE_SHA\" = deadbeefcafe",
            Duration::from_secs(10),
            &env,
        )
        .await;
        assert!(ok, "expected command to succeed: {output}");
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
            considered_alternatives: Some(ConsideredAlternatives {
                chosen: "single safe slice".into(),
                rejected: vec![
                    RejectedAlternative {
                        approach: "big bang".into(),
                        trade_off: "too broad".into(),
                    },
                    RejectedAlternative {
                        approach: "docs only".into(),
                        trade_off: "does not deliver behavior".into(),
                    },
                ],
            }),
            command_grants: vec!["gc lint".into()],
            touch_set: vec!["src/**".into()],
        };
        let value = serde_json::to_value(&plan).unwrap();
        let schema = plan_schema();
        let props = schema["properties"].as_object().unwrap();
        for key in value.as_object().unwrap().keys() {
            assert!(
                props.contains_key(key),
                "schema missing top-level key {key}"
            );
        }
    }

    #[test]
    fn extract_research_pulls_the_optional_object() {
        let text = r#"{
            "goal": "g",
            "milestones": [{"title":"m","features":[{"title":"f","spec":"s","validationCriteria":["c"]}]}],
            "validationContract": [],
            "research": {
                "filesRead": ["crates/engine/src/orchestrator.rs"],
                "sources": ["https://example.com"],
                "facts": [{"fact":"the run loop folds events","evidence":"reducer.rs"}],
                "ambiguities": ["stale doc X"],
                "candidateKnowledgeUpdates": ["add architecture/run-loop.md"]
            }
        }"#;
        let r = extract_research(text).expect("research present");
        assert_eq!(r.files_read, vec!["crates/engine/src/orchestrator.rs"]);
        assert_eq!(r.facts.len(), 1);
        assert_eq!(r.facts[0].evidence, "reducer.rs");
        assert_eq!(r.candidate_knowledge_updates.len(), 1);

        // Absent research -> None.
        let none = r#"{"goal":"g","milestones":[{"title":"m","features":[{"title":"f","spec":"s","validationCriteria":["c"]}]}],"validationContract":[]}"#;
        assert!(extract_research(none).is_none());
        // Empty research object -> None (nothing to render).
        let empty = r#"{"goal":"g","milestones":[],"validationContract":[],"research":{}}"#;
        assert!(extract_research(empty).is_none());
    }

    #[test]
    fn render_research_markdown_lays_out_sections() {
        let r = Research {
            files_read: vec!["a.rs".into(), "  ".into()],
            sources: vec![],
            facts: vec![
                ResearchFact {
                    fact: "x holds".into(),
                    evidence: "a.rs:10".into(),
                },
                ResearchFact {
                    fact: "  ".into(),
                    evidence: "".into(),
                },
            ],
            ambiguities: vec!["doc drift".into()],
            candidate_knowledge_updates: vec!["note Y".into()],
        };
        let md = render_research_markdown(&r, "m-1");
        assert!(md.contains("# Research — m-1"), "{md}");
        assert!(md.contains("## Files & docs read"));
        assert!(md.contains("- a.rs"));
        assert!(md.contains("## Facts"));
        assert!(md.contains("- x holds — `a.rs:10`"), "{md}");
        assert!(md.contains("## Ambiguities & stale docs"));
        assert!(md.contains("- note Y"));
        // Empty sources section is omitted; the blank fact is skipped.
        assert!(!md.contains("## External sources"));
        assert!(!md.contains("-  \n"));
    }

    #[test]
    fn judge_gate_diff_uses_pinned_base_sha() {
        // The final gate's judge diffs against the base pinned at approval,
        // never the moving base branch. Selection: pinned sha wins; None/empty
        // falls back to base_branch (legacy missions).
        assert_eq!(judge_diff_base(Some("abc123"), "main"), "abc123");
        assert_eq!(judge_diff_base(None, "main"), "main");
        assert_eq!(judge_diff_base(Some(""), "main"), "main");

        // Non-vacuity, in a real repo: when the base branch MOVES after the
        // mission forks, the pinned-sha diff stays put while the moved-branch
        // diff changes — diffing the wrong base would silently alter the judge's
        // view of the mission's work. Skip when git is unavailable.
        let git_ok = std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !git_ok {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(dir.path())
                    .output()
                    .unwrap()
                    .status
                    .success(),
                "git {args:?} failed"
            );
        };
        if !std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            git(&["init"]);
            git(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        }
        git(&["config", "user.name", "t"]);
        git(&["config", "user.email", "t@e"]);
        std::fs::write(dir.path().join("base.txt"), "base\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-m", "base"]);

        let repo = GitRepo::open(dir.path()).unwrap();
        let pinned = repo.rev_parse("main").unwrap(); // base tip pinned at approval

        // Mission forks and adds its own feature commit.
        git(&["checkout", "-b", "kranz/mission-x"]);
        std::fs::write(dir.path().join("feature.txt"), "feature\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-m", "feature"]);

        // The base branch MOVES after the fork.
        git(&["checkout", "main"]);
        std::fs::write(dir.path().join("unrelated.txt"), "moved\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-m", "base moved"]);
        git(&["checkout", "kranz/mission-x"]);

        let diff_pinned = repo.diff_stat(&pinned, "HEAD").unwrap();
        let diff_moved = repo.diff_stat("main", "HEAD").unwrap();
        assert!(
            diff_pinned.contains("feature.txt"),
            "pinned diff should be the mission's own work: {diff_pinned}"
        );
        assert_ne!(
            diff_pinned, diff_moved,
            "a moved base must change the diff (else the guard is vacuous): \
             pinned={diff_pinned:?} moved={diff_moved:?}"
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
        tokio::time::sleep(Duration::from_millis(700)).await;
        let after = EventLog::read_events(&paths.events_file()).expect("read events.jsonl");
        handle.abort();

        assert!(
            after.iter().any(
                |e| matches!(&e.kind, EventKind::WorkerMessage { content, .. } if content == "buffered delta")
            ),
            "idle Paused loop must age-flush the buffered delta to disk without a lifecycle event"
        );
    }

    // -----------------------------------------------------------------------
    // Lesson capture (roadmap: cross-mission learning)
    // -----------------------------------------------------------------------

    /// A throwaway git repo (seeded, `main` branch), or `None` (with a skip
    /// note) when `git` is not on PATH.
    fn lessons_test_repo() -> Option<(tempfile::TempDir, PathBuf)> {
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

    /// One streaming orchestrator script: session init + one turn reply.
    fn lesson_orch_script(reply: &str) -> crate::backend_mock::MockScript {
        use crate::backend_mock::{mock_init, mock_result_text, mock_text};
        crate::backend_mock::MockScript::streaming(vec![
            mock_init("orch-session"),
            mock_result_text("ready"),
        ])
        .responding(vec![vec![mock_text(reply), mock_result_text(reply)]])
    }

    fn finding(subject: &str) -> Finding {
        Finding {
            subject: subject.to_string(),
            severity: "major".to_string(),
            evidence: format!("{subject} evidence"),
            suggested_fix: format!("fix {subject}"),
            class: String::new(),
        }
    }

    #[tokio::test]
    async fn partial_waiver_synthesizes_unwaived_findings() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let reply = serde_json::json!({
            "fixFeatures": [],
            "waived": [{ "subject": "covered", "reason": "not contract relevant" }],
            "summary": "waive one"
        })
        .to_string();
        let backend: Arc<dyn AgentBackend> =
            Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
                lesson_orch_script(&reply),
            ]));
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();

        let conversion = engine
            .convert_findings("ms-1", &[finding("covered"), finding("uncovered")])
            .await
            .unwrap();

        match conversion {
            FindingsConversion::Fix { specs, .. } => {
                assert_eq!(specs.len(), 1, "only the unwaived finding needs a fix");
                assert!(
                    specs[0].title.contains("uncovered"),
                    "expected synthesized fix for the uncovered finding: {}",
                    specs[0].title
                );
            }
            FindingsConversion::Waive { .. } => {
                panic!("a partial waiver must not waive the whole finding set")
            }
        }
    }

    #[tokio::test]
    async fn partial_fix_features_synthesizes_when_fewer_specs_than_findings() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        // Model returns one fixFeature for two unwaived findings — must
        // synthesize a fix for the omitted subject.
        let reply = serde_json::json!({
            "fixFeatures": [{
                "title": "fix covered",
                "spec": "address covered",
                "validationCriteria": ["covered fixed"]
            }],
            "waived": [],
            "summary": "fix one"
        })
        .to_string();
        let backend: Arc<dyn AgentBackend> =
            Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
                lesson_orch_script(&reply),
            ]));
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();

        let conversion = engine
            .convert_findings("ms-1", &[finding("covered"), finding("uncovered")])
            .await
            .unwrap();

        match conversion {
            FindingsConversion::Fix { specs, .. } => {
                assert_eq!(
                    specs.len(),
                    2,
                    "model fix + synthesized uncovered: {specs:?}"
                );
                assert!(
                    specs.iter().any(|s| s.title.contains("uncovered")),
                    "expected synthesized fix for uncovered: {specs:?}"
                );
            }
            FindingsConversion::Waive { .. } => {
                panic!("partial fixFeatures must not waive")
            }
        }
    }

    #[tokio::test]
    async fn fix_features_matching_finding_count_are_trusted() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        // Generic titles that do not contain finding subjects — still trusted
        // when the model returned one spec per unwaived finding.
        let reply = serde_json::json!({
            "fixFeatures": [{
                "title": "fix issue 1",
                "spec": "resolve validation finding 1",
                "validationCriteria": ["finding 1 resolved"]
            }],
            "waived": [],
            "summary": "1 fix feature(s)"
        })
        .to_string();
        let backend: Arc<dyn AgentBackend> =
            Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
                lesson_orch_script(&reply),
            ]));
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();

        let conversion = engine
            .convert_findings("ms-1", &[finding("part 1 works")])
            .await
            .unwrap();

        match conversion {
            FindingsConversion::Fix { specs, .. } => {
                assert_eq!(specs.len(), 1, "must not synthesize a duplicate: {specs:?}");
                assert_eq!(specs[0].title, "fix issue 1");
            }
            FindingsConversion::Waive { .. } => panic!("expected Fix"),
        }
    }

    #[tokio::test]
    async fn all_waived_findings_still_waive() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let reply = serde_json::json!({
            "fixFeatures": [],
            "waived": [
                { "subject": "one", "reason": "not contract relevant" },
                { "subject": "two", "reason": "duplicate of one" }
            ],
            "summary": "waive all"
        })
        .to_string();
        let backend: Arc<dyn AgentBackend> =
            Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
                lesson_orch_script(&reply),
            ]));
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();

        let conversion = engine
            .convert_findings("ms-1", &[finding("one"), finding("two")])
            .await
            .unwrap();

        match conversion {
            FindingsConversion::Waive { waived } => assert_eq!(waived.len(), 2),
            FindingsConversion::Fix { .. } => panic!("a full waiver should still waive"),
        }
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

    #[tokio::test]
    async fn lessons_capture_writes_file_and_index() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> =
            Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
                lesson_orch_script(
                    "Always check the plan for a base_branch override before assuming main.",
                ),
            ]));
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        let mission_id = engine.state.mission.id.clone();

        let written = engine.capture_lesson().await.expect("lesson written");
        assert_eq!(written.len(), 2, "expects lesson file + index path");

        let lesson_file = engine.paths.lessons_dir().join(format!("{mission_id}.md"));
        assert!(written.contains(&lesson_file));
        let body = std::fs::read_to_string(&lesson_file).expect("lesson file exists");
        assert_eq!(
            first_nonempty_line(&body),
            "Always check the plan for a base_branch override before assuming main."
        );

        let index = engine.paths.lessons_index();
        assert!(written.contains(&index));
        let index_text = std::fs::read_to_string(&index).expect("index exists");
        assert_eq!(
            index_text.lines().count(),
            1,
            "one manifest line per capture"
        );
        assert!(index_text.contains(&format!("{mission_id}.md")));
        assert!(index_text.contains("Always check the plan for a base_branch override"));
    }

    #[tokio::test]
    async fn lessons_capture_none_reply_writes_nothing() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> =
            Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
                lesson_orch_script("  none.  "),
            ]));
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();

        let written = engine.capture_lesson().await;
        assert!(written.is_none(), "NONE reply must write nothing");
        assert!(
            !engine.paths.lessons_dir().exists(),
            "lessons dir must not be created"
        );
    }

    #[tokio::test]
    async fn lessons_capture_turn_error_returns_none() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        // No scripts queued: the orchestrator turn fails immediately.
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();

        let written = engine.capture_lesson().await;
        assert!(
            written.is_none(),
            "a failed turn must downgrade to None, not panic"
        );
        assert!(!engine.paths.lessons_dir().exists());
    }

    #[test]
    fn lessons_is_none_reply_matches_case_and_punctuation() {
        assert!(is_none_reply("NONE"));
        assert!(is_none_reply("  none.  "));
        assert!(is_none_reply("None!"));
        assert!(!is_none_reply("none of this applies, still worth a lesson"));
    }

    #[test]
    fn lessons_normalize_body_prepends_capped_summary_when_first_line_too_long() {
        let long_first_line = "x".repeat(200);
        let text = format!("{long_first_line}\nmore detail");
        let normalized = normalize_lesson_body(&text);
        let first = first_nonempty_line(&normalized);
        assert!(first.chars().count() <= LESSON_SUMMARY_CAP);
        assert!(normalized.contains("more detail"));
    }

    #[test]
    fn lessons_normalize_body_keeps_short_first_line_as_is() {
        let text = "Short imperative note.\n\nMore context below.";
        assert_eq!(normalize_lesson_body(text), text);
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

    fn codex_scrutiny_milestone() -> Milestone {
        Milestone {
            id: "ms-1".to_string(),
            title: "m".to_string(),
            features: vec![],
            status: MilestoneStatus::Active,
            fix_cycles: 0,
            start_sha: Some("HEAD".to_string()),
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
    /// [`DROID_ENV_LOCK`] used by the other `KRANZ_DROID_BIN`-mutating tests
    /// so they never race each other.
    #[cfg(unix)]
    struct DroidStubEnvGuard {
        prev_bin: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    #[cfg(unix)]
    impl DroidStubEnvGuard {
        fn engage(stub: &std::path::Path) -> Self {
            let lock = DROID_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
}
