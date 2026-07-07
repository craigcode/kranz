//! Spawn-one-session-and-stream-it plumbing (plan §4.6) shared by workers and
//! validators, and reused for orchestrator turns in Phase C.
//!
//! [`run_session`] is the single choke point: it opens the run transcript,
//! emits `worker.spawned`, pumps every [`AgentEvent`] through a [`RunSink`]
//! (raw line → transcript, selected events → `worker.message`), aggregates
//! `Result` events, parses the role's report from the final text, computes
//! the [`RunResult`], and emits `worker.completed`. [`run_worker`] and
//! [`run_validator`] are thin wrappers that render the role prompt, build the
//! [`SessionSpec`] (permissions via [`permissions::for_role`], report schema
//! via `--json-schema`), and delegate.
//!
//! Cancellation: callers may pass an `Arc<tokio::sync::Notify>`; when it
//! fires, the session is aborted and the run finishes as `Partial`/`Aborted`.
//!
//! ## Buffered runs (roadmap M3 — wall-clock overlap)
//!
//! The default path writes each event to the shared single-writer
//! [`EventLog`] as the stream arrives ([`LogTarget::Live`]). That is
//! incompatible with running N worker sessions concurrently: two live sessions
//! would race the one `&mut EventLog`. So a run may instead target an
//! in-memory buffer ([`LogTarget::Buffer`]): every [`EventKind`] the run would
//! have appended (`worker.spawned`, throttled `worker.message` deltas,
//! `worker.completed`) is collected in order into a `Vec` and returned
//! alongside the [`RunOutcome`], and NOTHING touches the EventLog. The engine
//! then replays those buffered kinds through its own single-writer `emit`
//! serially, in a deterministic order, AFTER the concurrent sessions finish —
//! so the single-writer / monotonic-seq invariant is preserved while the
//! claude sessions themselves overlapped in wall-clock (see
//! [`run_worker_in_buffered`]). Per-run transcripts (`runs/<id>.jsonl`) are
//! separate files, not the single-writer log, so they are written live in both
//! modes.

use crate::backend::{AgentBackend, AgentEvent, PromptMode, SessionExit, SessionSpec};
use crate::error::{EngineError, Result};
use crate::event_log::EventLog;
use crate::events::EventKind;
use crate::paths::MissionPaths;
use crate::permissions;
use crate::prompts;
use crate::scrub;
use crate::types::{
    Assertion, AssertionCheck, Feature, Milestone, MissionConfig, Role, RunResult, TokenUsage,
    ValidatorReport, WorkerReport,
};
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use tokio::sync::Notify;

/// Max characters of `worker.message` content (after scrubbing).
const MESSAGE_CONTENT_MAX: usize = 2000;

// ---------------------------------------------------------------------------
// Log target: live single-writer append vs. in-memory buffer
// ---------------------------------------------------------------------------

/// Where the event KINDS a run produces are sent.
///
/// [`Live`](LogTarget::Live) appends each kind to the shared single-writer
/// [`EventLog`] immediately — the sequential path, byte-for-byte as before.
/// [`Buffer`](LogTarget::Buffer) collects them in order into a `Vec` and
/// touches no log, so a run can execute concurrently with others; the engine
/// later replays the buffer through its own single-writer `emit`
/// (roadmap M3 wall-clock overlap). Transcripts are files, not the log, and
/// are written live regardless of the target.
pub enum LogTarget<'a> {
    /// Append straight to the single-writer log (default sequential path).
    Live(&'a mut EventLog),
    /// Collect kinds in append order; the engine emits them later, serially.
    Buffer(Vec<EventKind>),
}

impl LogTarget<'_> {
    /// Record one event kind: append it live, or push it onto the buffer.
    /// Order is preserved either way (the buffer is drained in push order).
    fn record(&mut self, kind: EventKind) -> Result<()> {
        match self {
            LogTarget::Live(log) => {
                log.append(kind)?;
            }
            LogTarget::Buffer(buf) => buf.push(kind),
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Sink: transcript + event-log fan-out for one run's stream
// ---------------------------------------------------------------------------

/// Where a run's event stream lands: every event's raw JSON goes to the
/// transcript (one line each, scrubbed); selected events are recorded to the
/// [`LogTarget`] as `worker.message` deltas (scrubbed + truncated).
pub struct RunSink<'a, 'l> {
    pub log: &'a mut LogTarget<'l>,
    pub transcript: &'a mut (dyn std::io::Write + Send),
}

impl RunSink<'_, '_> {
    /// Process one event. Returns `true` when the event was a denied tool
    /// result (a guardrail hit, §4.7).
    ///
    /// Log mapping: `Text` → tag `"text"`, `ToolUse` → `"tool-use"`
    /// (`<tool>: <summary>`), `ToolResult` → `"denied"` or `"tool-result"`;
    /// everything else is transcript-only.
    pub fn handle(&mut self, run_id: &str, event: &AgentEvent) -> Result<bool> {
        let raw = match event {
            AgentEvent::Init { raw, .. }
            | AgentEvent::Text { raw, .. }
            | AgentEvent::ToolUse { raw, .. }
            | AgentEvent::ToolResult { raw, .. }
            | AgentEvent::Result { raw, .. }
            | AgentEvent::Other { raw } => raw,
        };
        let line = scrub::scrub(&serde_json::to_string(raw)?);
        writeln!(self.transcript, "{line}")?;

        let (tag, content, denied) = match event {
            AgentEvent::Text { text, .. } => ("text", text.clone(), false),
            AgentEvent::ToolUse { tool, summary, .. } => {
                ("tool-use", format!("{tool}: {summary}"), false)
            }
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
                (
                    if *denied { "denied" } else { "tool-result" },
                    content,
                    *denied,
                )
            }
            _ => return Ok(false),
        };

        self.log.record(EventKind::WorkerMessage {
            run_id: run_id.to_string(),
            tag: tag.to_string(),
            content: scrub::scrub_and_truncate(&content, MESSAGE_CONTENT_MAX),
        })?;
        Ok(denied)
    }
}

// ---------------------------------------------------------------------------
// Run metadata / outcome
// ---------------------------------------------------------------------------

/// Identity of one run, decided by the caller before the session starts.
#[derive(Debug, Clone)]
pub struct RunMeta {
    pub run_id: String,
    pub role: Role,
    pub feature_id: Option<String>,
    pub milestone_id: Option<String>,
    pub model: String,
    pub prompt_hash: String,
}

/// Everything the engine learns from one completed session.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub run_id: String,
    /// Session id actually in use (== spec session id unless resumed).
    pub session_id: String,
    pub result: RunResult,
    pub usage: TokenUsage,
    pub cost_usd: Option<f64>,
    /// Text of the last `Result` event (report JSON lives here), credential-
    /// scrubbed like every other model-authored string the engine persists.
    pub final_text: String,
    /// Parsed from `final_text` when the role is [`Role::Worker`].
    pub report: Option<WorkerReport>,
    /// Parsed from `final_text` when the role is a validator.
    pub validator_report: Option<ValidatorReport>,
    pub exit: SessionExit,
    /// Guardrail hits: denied tool results seen in the stream (§4.7).
    pub denied_count: u32,
}

// ---------------------------------------------------------------------------
// run_session — the shared choke point
// ---------------------------------------------------------------------------

/// One `tokio::select!` step of the pump loop. Separated into an enum so the
/// cancel branch never borrows the session while `next_event` does.
enum Step {
    Cancelled,
    Event(Option<AgentEvent>),
}

/// Spawn one session and stream it to completion.
///
/// Emits `worker.spawned` before the session starts and `worker.completed`
/// after it ends. `Result` events are aggregated: usage is summed across all
/// of them (streaming sessions emit one per turn), text/is_error come from
/// the last, and cost is the last one reported.
///
/// Result mapping: `Fail` if the last result `is_error` or the session exit
/// is `Failed`; `Partial` if the exit is `Aborted` (budget/interrupt — forced
/// even when a report parses); otherwise the role's report decides (`Pass`
/// only downgradeable by the report; a missing/unparseable report for a
/// worker or validator is `Partial`, plan §4.6).
///
/// `cancel`: when the notify fires the session is aborted (`exit: Aborted`).
///
/// This is the [`LogTarget::Live`] convenience form: the caller passes the
/// shared single-writer log and every event kind is appended to it as it
/// arrives. [`run_session_to`] is the same logic over an arbitrary
/// [`LogTarget`], used by the buffered concurrent path (roadmap M3).
pub async fn run_session(
    backend: &dyn AgentBackend,
    spec: SessionSpec,
    log: &mut EventLog,
    paths: &MissionPaths,
    run_meta: RunMeta,
    cancel: Option<Arc<Notify>>,
) -> Result<RunOutcome> {
    let mut target = LogTarget::Live(log);
    run_session_to(backend, spec, &mut target, paths, run_meta, cancel).await
}

/// [`run_session`] over an explicit [`LogTarget`].
///
/// With [`LogTarget::Live`] this is byte-for-byte the sequential behaviour
/// (every kind appended to the log immediately). With [`LogTarget::Buffer`]
/// the exact same kinds — `worker.spawned`, throttled `worker.message` deltas,
/// `worker.completed` — are collected in append order into the buffer instead,
/// and NO log is touched, so the session can run concurrently with others; the
/// engine replays the buffer through its own single-writer `emit` afterwards
/// (preserving monotonic seq). The transcript file is written live in both
/// modes (it is not the single-writer log).
pub async fn run_session_to(
    backend: &dyn AgentBackend,
    spec: SessionSpec,
    log: &mut LogTarget<'_>,
    paths: &MissionPaths,
    run_meta: RunMeta,
    cancel: Option<Arc<Notify>>,
) -> Result<RunOutcome> {
    std::fs::create_dir_all(paths.runs_dir())?;
    let transcript_path = paths.transcript_file(&run_meta.run_id);
    let mut transcript = std::io::BufWriter::new(std::fs::File::create(&transcript_path)?);

    // The sdk session id recorded for --resume bookkeeping: the resumed id
    // when resuming, else the engine-chosen fresh id.
    let sdk_session_id = spec
        .resume
        .clone()
        .unwrap_or_else(|| spec.session_id.clone());
    log.record(EventKind::WorkerSpawned {
        run_id: run_meta.run_id.clone(),
        role: run_meta.role,
        feature_id: run_meta.feature_id.clone(),
        milestone_id: run_meta.milestone_id.clone(),
        sdk_session_id,
        model: run_meta.model.clone(),
        prompt_hash: run_meta.prompt_hash.clone(),
        transcript_path: MissionPaths::transcript_rel(&run_meta.run_id),
    })?;

    let mut session = backend.start(spec).await?;
    let session_id = session.session_id();

    let mut usage = TokenUsage::default();
    let mut cost_usd: Option<f64> = None;
    let mut final_text = String::new();
    let mut last_is_error = false;
    let mut denied_count: u32 = 0;
    let mut cancelled = false;

    {
        let mut sink = RunSink {
            log,
            transcript: &mut transcript,
        };
        loop {
            let step = match &cancel {
                Some(notify) if !cancelled => tokio::select! {
                    _ = notify.notified() => Step::Cancelled,
                    event = session.next_event() => Step::Event(event?),
                },
                _ => Step::Event(session.next_event().await?),
            };
            match step {
                Step::Cancelled => {
                    cancelled = true;
                    session.abort().await?;
                }
                Step::Event(None) => break,
                Step::Event(Some(event)) => {
                    if sink.handle(&run_meta.run_id, &event)? {
                        denied_count += 1;
                    }
                    if let AgentEvent::Result {
                        text,
                        is_error,
                        usage: turn_usage,
                        cost_usd: turn_cost,
                        ..
                    } = &event
                    {
                        usage.add(turn_usage);
                        final_text = text.clone();
                        last_is_error = *is_error;
                        if turn_cost.is_some() {
                            cost_usd = *turn_cost;
                        }
                    }
                }
            }
        }
    }
    transcript.flush()?;

    let exit = session.exit_status().unwrap_or_else(|| {
        if cancelled {
            SessionExit::Aborted
        } else {
            SessionExit::Failed("session stream closed without an exit status".to_string())
        }
    });

    // Scrub the final text BEFORE parsing reports: report string fields
    // (summary, testEvidence, finding evidence, …) are stored verbatim in
    // `worker.completed` events and consumed by the orchestrator, so a secret
    // inside the raw result text would otherwise bypass the transcript/
    // message scrubbing and land in events.jsonl unredacted. Scrubbing
    // replaces token-shaped substrings only, so valid report JSON stays
    // parseable. The outcome's `final_text` is the scrubbed form too.
    let final_text = scrub::scrub(&final_text);

    let mut report: Option<WorkerReport> = None;
    let mut validator_report: Option<ValidatorReport> = None;
    match run_meta.role {
        Role::Worker => report = parse_worker_report(&final_text),
        Role::ValidatorScrutiny | Role::ValidatorFunctional => {
            validator_report = parse_validator_report(&final_text);
        }
        Role::Orchestrator => {}
    }

    let result = if last_is_error || matches!(exit, SessionExit::Failed(_)) {
        RunResult::Fail
    } else if exit == SessionExit::Aborted {
        // Budget/interrupt: forced Partial even when a report parsed.
        RunResult::Partial
    } else {
        match run_meta.role {
            Role::Worker => report
                .as_ref()
                .map(|r| r.result)
                .unwrap_or(RunResult::Partial),
            Role::ValidatorScrutiny | Role::ValidatorFunctional => {
                if validator_report.is_some() {
                    RunResult::Pass
                } else {
                    RunResult::Partial
                }
            }
            Role::Orchestrator => RunResult::Pass,
        }
    };

    log.record(EventKind::WorkerCompleted {
        run_id: run_meta.run_id.clone(),
        result,
        tokens: usage.clone(),
        cost_usd,
        report: report.clone(),
    })?;

    Ok(RunOutcome {
        run_id: run_meta.run_id,
        session_id,
        result,
        usage,
        cost_usd,
        final_text,
        report,
        validator_report,
        exit,
        denied_count,
    })
}

// ---------------------------------------------------------------------------
// Report parsing (plan §4.6): strict, then lenient
// ---------------------------------------------------------------------------

/// Parse a report from a session's final text: strict whole-text parse, then
/// the first-`{`-to-last-`}` substring, then a fenced ```json block.
pub fn parse_report<T: DeserializeOwned>(text: &str) -> Option<T> {
    let trimmed = text.trim();
    if let Ok(parsed) = serde_json::from_str::<T>(trimmed) {
        return Some(parsed);
    }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if start < end {
            if let Ok(parsed) = serde_json::from_str::<T>(&trimmed[start..=end]) {
                return Some(parsed);
            }
        }
    }
    fenced_block(trimmed).and_then(|block| serde_json::from_str::<T>(block).ok())
}

/// Content of the first fenced code block (```json preferred, bare ```
/// otherwise), or `None` when there is no closed fence.
fn fenced_block(text: &str) -> Option<&str> {
    let start = match text.find("```json") {
        Some(i) => i + "```json".len(),
        None => text.find("```")? + "```".len(),
    };
    let rest = &text[start..];
    let end = rest.find("```")?;
    Some(rest[..end].trim())
}

/// [`parse_report`] for [`WorkerReport`].
pub fn parse_worker_report(text: &str) -> Option<WorkerReport> {
    parse_report(text)
}

/// [`parse_report`] for [`ValidatorReport`].
pub fn parse_validator_report(text: &str) -> Option<ValidatorReport> {
    parse_report(text)
}

// ---------------------------------------------------------------------------
// Report JSON schemas (enforced at the source via --json-schema)
// ---------------------------------------------------------------------------

/// JSON Schema matching [`WorkerReport`] (camelCase, closed object).
pub fn worker_report_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["result", "summary"],
        "properties": {
            "result": { "type": "string", "enum": ["pass", "fail", "partial"] },
            "summary": { "type": "string" },
            "filesTouched": { "type": "array", "items": { "type": "string" } },
            "testsAdded": { "type": "array", "items": { "type": "string" } },
            "testEvidence": { "type": "string" },
            "dependenciesAdded": { "type": "array", "items": { "type": "string" } },
            "knownGaps": { "type": "array", "items": { "type": "string" } },
            "commits": { "type": "array", "items": { "type": "string" } },
            "commandsRun": { "type": "array", "items": { "type": "string" } }
        }
    })
}

/// JSON Schema matching [`ValidatorReport`] (camelCase, closed objects).
pub fn validator_report_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["findings", "summary"],
        "properties": {
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["subject", "severity", "evidence"],
                    "properties": {
                        "subject": { "type": "string" },
                        "severity": { "type": "string", "enum": ["critical", "major", "minor"] },
                        "evidence": { "type": "string" },
                        "suggestedFix": { "type": "string" },
                        "class": { "type": "string" }
                    }
                }
            },
            "summary": { "type": "string" }
        }
    })
}

// ---------------------------------------------------------------------------
// Wrappers: worker / validator runs
// ---------------------------------------------------------------------------

/// The environment every contract-command execution context must carry, so
/// worker, validator, and the engine's final gate can never diverge. Adds
/// KRANZ_BASE_SHA only when a non-empty base SHA was pinned at approval.
pub fn contract_env(base_sha: Option<&str>) -> HashMap<String, String> {
    let mut env = HashMap::new();
    if let Some(sha) = base_sha.filter(|s| !s.is_empty()) {
        env.insert("KRANZ_BASE_SHA".to_string(), sha.to_string());
    }
    env
}

/// Run one worker session for a feature (plan §4.6).
///
/// The rendered role prompt goes to `append_system_prompt`; the single-shot
/// prompt is a short task statement (feature id/title/spec/criteria/guidance)
/// so the role text and the task stay separable in transcripts.
///
/// The worker session's `cwd` is the mission repo root (`paths.repo_root`).
/// For M3 parallel-within-milestone execution — where each worker runs in its
/// own git worktree — use [`run_worker_in`] to override just the session cwd
/// while the run's transcript and events stay under the real mission dir.
#[allow(clippy::too_many_arguments)]
pub async fn run_worker(
    backend: &dyn AgentBackend,
    log: &mut EventLog,
    paths: &MissionPaths,
    cfg: &MissionConfig,
    feature: &Feature,
    plan_goal: &str,
    milestone_title: &str,
    extra_guidance: Option<&str>,
    cancel: Option<Arc<Notify>>,
    base_sha: Option<&str>,
    grants: &[String],
) -> Result<RunOutcome> {
    let cwd = paths.repo_root.clone();
    run_worker_in(
        backend,
        log,
        paths,
        cfg,
        feature,
        plan_goal,
        milestone_title,
        extra_guidance,
        cancel,
        &cwd,
        base_sha,
        grants,
    )
    .await
}

/// [`run_worker`] with an explicit session working directory (roadmap M3).
///
/// Identical to [`run_worker`] except the spawned worker session's `cwd` is
/// `session_cwd` instead of `paths.repo_root`. The run's transcript and every
/// event it appends still live under `paths` (the real mission dir), so a
/// worker running in a per-feature git worktree writes its code there while its
/// bookkeeping stays with the mission. `run_worker` is the thin wrapper that
/// passes `paths.repo_root`, keeping the sequential path byte-for-byte.
#[allow(clippy::too_many_arguments)]
pub async fn run_worker_in(
    backend: &dyn AgentBackend,
    log: &mut EventLog,
    paths: &MissionPaths,
    cfg: &MissionConfig,
    feature: &Feature,
    plan_goal: &str,
    milestone_title: &str,
    extra_guidance: Option<&str>,
    cancel: Option<Arc<Notify>>,
    session_cwd: &std::path::Path,
    base_sha: Option<&str>,
    grants: &[String],
) -> Result<RunOutcome> {
    let (spec, run_meta) = build_worker_spec(
        cfg,
        feature,
        plan_goal,
        milestone_title,
        extra_guidance,
        session_cwd,
        base_sha,
        grants,
        paths.mission_dir(),
    );
    let mut target = LogTarget::Live(log);
    run_session_to(backend, spec, &mut target, paths, run_meta, cancel).await
}

/// [`run_worker_in`] that BUFFERS its event kinds instead of appending them to
/// the shared log (roadmap M3 wall-clock overlap).
///
/// Returns the `worker.spawned` / `worker.message` / `worker.completed` kinds
/// this run produced, in append order, alongside the [`RunOutcome`]. It takes
/// NO `&mut EventLog`, so N of these can run concurrently (each in its own
/// worktree) via `tokio::join!`/`JoinSet` without racing the single writer.
/// The engine replays the returned kinds through its own single-writer `emit`
/// serially afterwards, in a deterministic order, preserving monotonic seq.
///
/// The per-run transcript is still written live under `paths` — transcripts
/// are per-run files, not the single-writer log, so concurrent writers to
/// distinct `runs/<id>.jsonl` files never conflict.
///
/// No `cancel`: the buffered concurrent path does not wire interrupts (matching
/// the parallel subset's live path). Interrupts remain a sequential-path
/// feature.
#[allow(clippy::too_many_arguments)]
pub async fn run_worker_in_buffered(
    backend: &dyn AgentBackend,
    paths: &MissionPaths,
    cfg: &MissionConfig,
    feature: &Feature,
    plan_goal: &str,
    milestone_title: &str,
    extra_guidance: Option<&str>,
    session_cwd: &std::path::Path,
    base_sha: Option<&str>,
    grants: &[String],
) -> Result<(Vec<EventKind>, RunOutcome)> {
    let (spec, run_meta) = build_worker_spec(
        cfg,
        feature,
        plan_goal,
        milestone_title,
        extra_guidance,
        session_cwd,
        base_sha,
        grants,
        paths.mission_dir(),
    );
    let mut target = LogTarget::Buffer(Vec::new());
    let outcome = run_session_to(backend, spec, &mut target, paths, run_meta, None).await?;
    let buffered = match target {
        LogTarget::Buffer(buf) => buf,
        LogTarget::Live(_) => unreachable!("buffered target constructed above"),
    };
    Ok((buffered, outcome))
}

/// Extend `spec.env` (already carrying [`contract_env`]) with a scratch
/// `HOME`/`CLAUDE_CONFIG_DIR` pair so the worker's `claude` CLI process
/// authenticates against an isolated, minimal copy of the operator's config
/// instead of the real `~/.claude` (worker env hygiene). Worker-role sessions
/// only — validator/orchestrator env is untouched by this function.
///
/// Also injects `GIT_AUTHOR_NAME`/`GIT_AUTHOR_EMAIL`/`GIT_COMMITTER_NAME`/
/// `GIT_COMMITTER_EMAIL` carrying the engine's resolved git identity (see
/// [`GitRepo::resolved_identity`]): relocating `HOME` hides the operator's
/// global `~/.gitconfig` from the worker, and `GitRepo::ensure_identity`'s
/// local-config write is conditional on no identity resolving anywhere — on
/// a host where a *global* identity resolves, that write is skipped, so
/// without this env injection a relocated-HOME worker's `git commit` would
/// fail with "Author identity unknown."
///
/// The scratch-config-dir COPY source honors an operator `CLAUDE_CONFIG_DIR`
/// override (falling back to `$HOME/.claude`) — the same resolution order
/// `claude` itself uses.
///
/// Best-effort: if seeding the scratch dir fails (e.g. an unwritable temp
/// dir), the worker falls back to inheriting the real `HOME`/`CLAUDE_CONFIG_DIR`
/// (i.e. the scratch-HOME half of this function is a no-op) rather than
/// failing spec construction.
fn seed_worker_env(spec: &mut SessionSpec) {
    let scratch_root = crate::backend_claude::scratch_home_root(&spec.session_id);
    let real_home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let real_config_dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from);
    if let Ok((home, config_dir)) = crate::backend_claude::seed_worker_scratch_home(
        &scratch_root,
        real_home.as_deref(),
        real_config_dir.as_deref(),
    ) {
        spec.env
            .insert("HOME".to_string(), home.display().to_string());
        spec.env.insert(
            "CLAUDE_CONFIG_DIR".to_string(),
            config_dir.display().to_string(),
        );
    }

    if let Ok(repo) = crate::git_ops::GitRepo::open(&spec.cwd) {
        if let Ok((name, email)) = repo.resolved_identity() {
            for key in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
                spec.env.insert(key.to_string(), name.clone());
            }
            for key in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
                spec.env.insert(key.to_string(), email.clone());
            }
        }
    }
}

/// Build the worker [`SessionSpec`] + [`RunMeta`] shared by the live and
/// buffered worker paths. Identical spec construction guarantees a buffered
/// run and a live run are byte-for-byte the same session, differing only in
/// where their event kinds land.
#[allow(clippy::too_many_arguments)]
fn build_worker_spec(
    cfg: &MissionConfig,
    feature: &Feature,
    plan_goal: &str,
    milestone_title: &str,
    extra_guidance: Option<&str>,
    session_cwd: &std::path::Path,
    base_sha: Option<&str>,
    grants: &[String],
    mission_dir: std::path::PathBuf,
) -> (SessionSpec, RunMeta) {
    let role = Role::Worker;
    let role_cfg = cfg.role(role);

    let criteria = bullet_list(&feature.validation_criteria);
    let turn_budget = role_cfg
        .max_turns
        .map(|n| n.to_string())
        .unwrap_or_else(|| "unlimited".to_string());
    let guidance = extra_guidance.unwrap_or("").trim().to_string();

    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("featureId", feature.id.clone());
    vars.insert("featureTitle", feature.title.clone());
    vars.insert("spec", feature.spec.clone());
    vars.insert("criteria", criteria.clone());
    vars.insert("missionGoal", plan_goal.to_string());
    vars.insert("milestoneTitle", milestone_title.to_string());
    vars.insert("turnBudget", turn_budget);
    vars.insert("guidance", guidance.clone());
    let role_prompt = prompts::render(prompts::text(role), &vars);

    let mut task = format!(
        "Implement feature `{id}`: {title}\n\n\
         Mission goal: {goal}\n\
         Milestone: {milestone}\n\n\
         Spec:\n{spec}\n\n\
         Validation criteria:\n{criteria}\n",
        id = feature.id,
        title = feature.title,
        goal = plan_goal,
        milestone = milestone_title,
        spec = feature.spec,
        criteria = criteria,
    );
    if !guidance.is_empty() {
        task.push_str(&format!("\nAdditional guidance:\n{guidance}\n"));
    }

    let mut spec = SessionSpec {
        cwd: session_cwd.to_path_buf(),
        prompt: PromptMode::SingleShot(task),
        append_system_prompt: Some(role_prompt),
        model: role_cfg.model.clone(),
        effort: role_cfg.reasoning_effort.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
        resume: None,
        permission_mode: None,
        allowed_tools: Vec::new(),
        disallowed_tools: Vec::new(),
        tools: cfg.role(role).tools.clone(),
        settings_json: None,
        json_schema: Some(worker_report_schema()),
        max_budget_usd: role_cfg.max_budget_usd,
        max_turns: role_cfg.max_turns,
        env: HashMap::new(),
        sandbox: None,
    };
    spec.env = contract_env(base_sha);
    seed_worker_env(&mut spec);
    let (sandbox, warn) =
        crate::sandbox::resolve_for_session(&role_cfg.sandbox, session_cwd, &mission_dir);
    if let Some(warn) = warn {
        tracing::warn!("{warn}");
    }
    spec.sandbox = sandbox;
    permissions::apply(permissions::for_role(role, cfg, &[], grants), &mut spec);

    let run_meta = RunMeta {
        run_id: uuid::Uuid::new_v4().to_string(),
        role,
        feature_id: Some(feature.id.clone()),
        milestone_id: None,
        model: role_cfg.model.clone(),
        prompt_hash: prompts::hash(role),
    };
    (spec, run_meta)
}

/// Run one validator session for a milestone (plan §4.4/§4.6).
///
/// `kind` must be [`Role::ValidatorScrutiny`] or [`Role::ValidatorFunctional`].
/// Contract `command` strings (plus config `allow_validator_commands`,
/// `grants`, and the milestone's worker-executed `worker_commands`) become
/// `Bash(<command>*)` allows via [`permissions::for_role`].
#[allow(clippy::too_many_arguments)]
pub async fn run_validator(
    backend: &dyn AgentBackend,
    log: &mut EventLog,
    paths: &MissionPaths,
    cfg: &MissionConfig,
    kind: Role,
    milestone: &Milestone,
    contract: &[Assertion],
    start_sha: &str,
    cancel: Option<Arc<Notify>>,
    base_sha: Option<&str>,
    grants: &[String],
    worker_commands: &[String],
) -> Result<RunOutcome> {
    let cwd = paths.repo_root.clone();
    run_validator_in(
        backend,
        log,
        paths,
        cfg,
        kind,
        milestone,
        contract,
        start_sha,
        cancel,
        &cwd,
        base_sha,
        grants,
        worker_commands,
    )
    .await
}

/// [`run_validator`] with an explicit session working directory (mirrors
/// [`run_worker_in`]).
///
/// Identical to [`run_validator`] except the spawned validator session's
/// `cwd` is `session_cwd` instead of `paths.repo_root`. `KRANZ_BASE_SHA` (via
/// [`contract_env`]) is preserved regardless of `session_cwd`. `run_validator`
/// is the thin wrapper that passes `paths.repo_root`, keeping the checkout-mode
/// path byte-for-byte.
#[allow(clippy::too_many_arguments)]
pub async fn run_validator_in(
    backend: &dyn AgentBackend,
    log: &mut EventLog,
    paths: &MissionPaths,
    cfg: &MissionConfig,
    kind: Role,
    milestone: &Milestone,
    contract: &[Assertion],
    start_sha: &str,
    cancel: Option<Arc<Notify>>,
    session_cwd: &std::path::Path,
    base_sha: Option<&str>,
    grants: &[String],
    worker_commands: &[String],
) -> Result<RunOutcome> {
    if !matches!(kind, Role::ValidatorScrutiny | Role::ValidatorFunctional) {
        return Err(EngineError::InvalidState(format!(
            "run_validator requires a validator role, got {kind:?}"
        )));
    }
    let role_cfg = cfg.role(kind);

    let contract_rendered = if contract.is_empty() {
        "- (none)".to_string()
    } else {
        contract
            .iter()
            .map(|a| match (a.check, &a.command) {
                (AssertionCheck::Command, Some(command)) => {
                    format!("- [{}] {} (command: `{}`)", a.id, a.statement, command)
                }
                (AssertionCheck::Command, None) => {
                    format!("- [{}] {} (command: MISSING)", a.id, a.statement)
                }
                (AssertionCheck::AgentJudgement, _) => {
                    format!("- [{}] {} (agent-judgement)", a.id, a.statement)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // All feature criteria of the milestone, tagged with their feature id.
    let criteria_items: Vec<String> = milestone
        .features
        .iter()
        .flat_map(|f| {
            f.validation_criteria
                .iter()
                .map(|c| format!("[{}] {}", f.id, c))
        })
        .collect();
    let criteria = bullet_list(&criteria_items);

    let contract_commands: Vec<String> =
        contract.iter().filter_map(|a| a.command.clone()).collect();
    let mut combined_commands = contract_commands.clone();
    for command in worker_commands {
        if !combined_commands.contains(command) {
            combined_commands.push(command.clone());
        }
    }
    let mut allowed_commands = combined_commands.clone();
    allowed_commands.extend(cfg.allow_validator_commands.iter().cloned());
    let commands = bullet_list(&allowed_commands);

    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("milestoneTitle", milestone.title.clone());
    vars.insert("startSha", start_sha.to_string());
    vars.insert("contract", contract_rendered.clone());
    vars.insert("criteria", criteria.clone());
    vars.insert("commands", commands.clone());
    let role_prompt = prompts::render(prompts::text(kind), &vars);

    let task = format!(
        "Validate milestone `{id}`: {title}\n\n\
         Commit range under review: {start_sha}..HEAD\n\n\
         Validation contract:\n{contract_rendered}\n\n\
         Feature validation criteria:\n{criteria}\n\n\
         Allowed commands:\n{commands}\n",
        id = milestone.id,
        title = milestone.title,
    );

    let mut spec = SessionSpec {
        cwd: session_cwd.to_path_buf(),
        prompt: PromptMode::SingleShot(task),
        append_system_prompt: Some(role_prompt),
        model: role_cfg.model.clone(),
        effort: role_cfg.reasoning_effort.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
        resume: None,
        permission_mode: None,
        allowed_tools: Vec::new(),
        disallowed_tools: Vec::new(),
        tools: cfg.role(kind).tools.clone(),
        settings_json: None,
        json_schema: Some(validator_report_schema()),
        max_budget_usd: role_cfg.max_budget_usd,
        max_turns: role_cfg.max_turns,
        env: HashMap::new(),
        sandbox: None,
    };
    spec.env = contract_env(base_sha);
    let (sandbox, warn) =
        crate::sandbox::resolve_for_session(&role_cfg.sandbox, session_cwd, &paths.mission_dir());
    if let Some(warn) = warn {
        tracing::warn!("{warn}");
    }
    spec.sandbox = sandbox;
    permissions::apply(
        permissions::for_role(kind, cfg, &combined_commands, grants),
        &mut spec,
    );

    let run_meta = RunMeta {
        run_id: uuid::Uuid::new_v4().to_string(),
        role: kind,
        feature_id: None,
        milestone_id: Some(milestone.id.clone()),
        model: role_cfg.model.clone(),
        prompt_hash: prompts::hash(kind),
    };
    run_session(backend, spec, log, paths, run_meta, cancel).await
}

/// `- item` per line; `- (none)` for an empty list.
fn bullet_list(items: &[String]) -> String {
    if items.is_empty() {
        return "- (none)".to_string();
    }
    items
        .iter()
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validator_report_schema_marks_finding_class_optional() {
        let schema = validator_report_schema();
        let finding_props = &schema["properties"]["findings"]["items"]["properties"];
        assert!(finding_props.get("class").is_some());
        let required = schema["properties"]["findings"]["items"]["required"]
            .as_array()
            .unwrap();
        assert!(!required.iter().any(|v| v == "class"));
    }

    #[test]
    fn validator_report_schema_finding_class_accepts_with_and_without() {
        let with_class = r#"{
            "findings": [{
                "subject": "a-1",
                "severity": "major",
                "evidence": "wrote outside touch-set",
                "class": "out-of-contract-write"
            }],
            "summary": "s"
        }"#;
        let report: ValidatorReport = serde_json::from_str(with_class).unwrap();
        assert_eq!(report.findings[0].class, "out-of-contract-write");

        let without_class = r#"{
            "findings": [{
                "subject": "a-1",
                "severity": "major",
                "evidence": "it broke"
            }],
            "summary": "s"
        }"#;
        let report: ValidatorReport = serde_json::from_str(without_class).unwrap();
        assert_eq!(report.findings[0].class, "");
    }

    // -- worker env hygiene (ms-2-fix-1-1) ----------------------------------

    fn minimal_worker_spec(cwd: std::path::PathBuf) -> SessionSpec {
        SessionSpec {
            cwd,
            prompt: PromptMode::SingleShot("task".to_string()),
            append_system_prompt: None,
            model: "claude-sonnet-5".to_string(),
            effort: "medium".to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            resume: None,
            permission_mode: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            tools: Vec::new(),
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: contract_env(Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")),
            sandbox: None,
        }
    }

    fn git(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git spawns")
    }

    /// Finding 1: a worker whose scratch HOME has no `.gitconfig` must still
    /// be able to `git commit` — proving the injected `GIT_AUTHOR_*` /
    /// `GIT_COMMITTER_*` env vars actually carry the identity through, not
    /// merely that the keys are present.
    #[test]
    fn worker_env_hygiene_scratch_home_worker_can_commit() {
        let repo_dir = tempfile::tempdir().unwrap();
        assert!(git(repo_dir.path(), &["init", "-q"]).status.success());

        let mut spec = minimal_worker_spec(repo_dir.path().to_path_buf());
        seed_worker_env(&mut spec);

        // Existing contract env survives the scratch-env layering.
        assert_eq!(
            spec.env.get("KRANZ_BASE_SHA").map(String::as_str),
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
        );

        let home = spec.env.get("HOME").expect("scratch HOME set").clone();
        assert!(
            !std::path::Path::new(&home).join(".gitconfig").exists(),
            "scratch HOME must carry no .gitconfig — that's the gap this test proves around"
        );

        for key in [
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
        ] {
            assert!(spec.env.contains_key(key), "missing {key}");
        }

        std::fs::write(repo_dir.path().join("file.txt"), "content").unwrap();
        assert!(git(repo_dir.path(), &["add", "."]).status.success());

        let commit_status = std::process::Command::new("git")
            .args(["commit", "-m", "worker commit under scratch HOME"])
            .current_dir(repo_dir.path())
            .envs(&spec.env)
            .status()
            .expect("git commit spawns");
        assert!(
            commit_status.success(),
            "worker must be able to commit with the scratch HOME + injected git identity env"
        );

        let log = git(repo_dir.path(), &["log", "-1", "--format=%an <%ae>"]);
        let logged = String::from_utf8_lossy(&log.stdout).trim().to_string();
        let expected = format!(
            "{} <{}>",
            spec.env["GIT_AUTHOR_NAME"], spec.env["GIT_AUTHOR_EMAIL"]
        );
        assert_eq!(logged, expected);
    }

    /// Finding 2: the credential-copy SOURCE dir honors an operator
    /// `CLAUDE_CONFIG_DIR` override rather than hardcoding `$HOME/.claude`.
    #[test]
    fn worker_env_hygiene_credential_source_honors_config_dir_override() {
        let scratch = tempfile::tempdir().unwrap();
        let real_home = tempfile::tempdir().unwrap();
        let relocated_config = tempfile::tempdir().unwrap();

        // Real $HOME/.claude has no credentials (operator relocated config).
        std::fs::create_dir_all(real_home.path().join(".claude")).unwrap();

        // The relocated CLAUDE_CONFIG_DIR does have credentials.
        std::fs::write(
            relocated_config.path().join(".credentials.json"),
            "{\"secret\":true}",
        )
        .unwrap();

        let (_, config_dir) = crate::backend_claude::seed_worker_scratch_home(
            scratch.path(),
            Some(real_home.path()),
            Some(relocated_config.path()),
        )
        .unwrap();

        let copied = config_dir.join(".credentials.json");
        assert!(
            copied.is_file(),
            "credentials must be copied from the CLAUDE_CONFIG_DIR override, not $HOME/.claude"
        );
        assert_eq!(
            std::fs::read_to_string(copied).unwrap(),
            "{\"secret\":true}"
        );
    }

    /// Validator sessions are untouched by worker env hygiene: no injected
    /// HOME/CLAUDE_CONFIG_DIR or git identity env vars.
    #[test]
    fn worker_env_hygiene_validator_env_unaffected() {
        let mut spec = minimal_worker_spec(std::env::temp_dir());
        spec.env = contract_env(None);
        // Validator spec construction never calls seed_worker_env at all;
        // this asserts the baseline it must remain at.
        assert!(!spec.env.contains_key("HOME"));
        assert!(!spec.env.contains_key("CLAUDE_CONFIG_DIR"));
        assert!(!spec.env.contains_key("GIT_AUTHOR_NAME"));
    }
}
