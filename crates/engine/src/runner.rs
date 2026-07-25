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

use crate::auth_verify::AuthVerdict;
use crate::backend::{AgentBackend, AgentEvent, PromptMode, SessionExit, SessionSpec};
use crate::error::{EngineError, Result};
use crate::event_log::EventLog;
use crate::events::EventKind;
use crate::paths::MissionPaths;
use crate::permissions;
use crate::prompts;
use crate::scrub;
use crate::types::{
    Assertion, AssertionCheck, Feature, Milestone, MissionConfig, Role, RoleConfig, RunResult,
    SandboxEnforce, TokenUsage, ValidatorReport, WorkerReport,
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
    /// Distinct denied SHELL commands (`Bash`), each correlated from the tool
    /// call that was blocked — the candidates a grant could unblock
    /// (grant-request-decision-flow). Captured for validator runs, whose
    /// denials are allow-set misses that extending `command_grants` clears;
    /// scrubbed + bounded + de-duplicated. `ToolResult` carries no tool-use id
    /// on the Claude backend, so this is the command from the immediately
    /// preceding `ToolUse` (a denied result follows its call in the stream).
    pub denied_commands: Vec<String>,
}

/// Whether `tool` names a shell whose `ToolUse` summary is a runnable command a
/// `command_grants` entry could unblock. Claude's `Bash` and Codex's
/// `command_execution` both carry the literal command in the summary
/// (`backend_claude.rs`, `backend_codex.rs`); Droid emits no tool events, so its
/// command denials never reach this path.
fn is_grantable_shell_tool(tool: &str) -> bool {
    tool.eq_ignore_ascii_case("bash") || tool.eq_ignore_ascii_case("command_execution")
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
        quant: "n/a".to_string(),
        weight_hash: None,
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
    // Positional ToolUse↔ToolResult correlation for grant-request: remember
    // the last tool call so a denied result can name the command it blocked.
    let mut last_tool_use: Option<(String, String)> = None;
    let mut denied_commands: Vec<String> = Vec::new();
    const DENIED_COMMANDS_CAP: usize = 16;
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
                    if let AgentEvent::ToolUse { tool, summary, .. } = &event {
                        last_tool_use = Some((tool.clone(), summary.clone()));
                    }
                    if sink.handle(&run_meta.run_id, &event)? {
                        denied_count += 1;
                        // Attribute the denial to the immediately-preceding tool
                        // call: a ToolResult carries no tool-use id (Claude sets
                        // tool=None, Codex/Droid don't correlate), so the command
                        // lives only on the preceding ToolUse. Only a shell tool's
                        // summary is a grantable command — Claude's "Bash" and
                        // Codex's "command_execution" both put the literal command
                        // there. `take()` consumes it: a later unrelated denial
                        // can't re-attribute a stale command. (A parallel-tool-call
                        // batch can still mis-pick within one turn; the grant is
                        // operator-confirmed, so the worst case is a visible wrong
                        // prefix, never a fabricated denial. Trim-guard matches the
                        // reducer's non-empty check so a whitespace-only capture
                        // can't be emitted and then rejected on fold.)
                        if let Some((tool, summary)) = last_tool_use.take() {
                            if is_grantable_shell_tool(&tool)
                                && denied_commands.len() < DENIED_COMMANDS_CAP
                            {
                                let cmd = scrub::scrub_and_truncate(&summary, MESSAGE_CONTENT_MAX);
                                if !cmd.trim().is_empty() && !denied_commands.contains(&cmd) {
                                    denied_commands.push(cmd);
                                }
                            }
                        }
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
        denied_commands,
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
///
/// `auth_verdict` is the worker-HOME auth-preflight decision input (mission
/// m-165b6f, f-1-2): [`AuthVerdict::Authenticated`] relocates HOME/
/// CLAUDE_CONFIG_DIR to a verified scratch env, anything else is a loud
/// fail-safe that inherits the real HOME. Real per-spawn preflight + caching
/// (computing this via [`crate::auth_verify::verify_worker_auth`] against
/// `backend`) is not yet wired here — that is the next milestone; today
/// callers pass the decision they already have.
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
    deny_exceptions: &[String],
    auth_verdict: AuthVerdict,
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
        deny_exceptions,
        auth_verdict,
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
    deny_exceptions: &[String],
    auth_verdict: AuthVerdict,
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
        deny_exceptions,
        paths.mission_dir(),
        auth_verdict,
    )?;
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
    deny_exceptions: &[String],
    auth_verdict: AuthVerdict,
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
        deny_exceptions,
        paths.mission_dir(),
        auth_verdict,
    )?;
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
///
/// Relocation is GATED on `auth_verdict` (mission m-165b6f, f-1-2): a worker
/// only launches into the scratch HOME when it has been proven — via
/// [`crate::auth_verify::verify_worker_auth`] driving a real trivial session
/// under the candidate scratch env — able to authenticate there
/// ([`AuthVerdict::Authenticated`]). Any other verdict
/// ([`AuthVerdict::Unauthenticated`] or [`AuthVerdict::Inconclusive`]) is a
/// loud fail-safe: the worker inherits the real `HOME`/`CLAUDE_CONFIG_DIR`
/// rather than risk launching unauthenticated and silently producing no
/// output (observed 2026-07-06, m-66aff8: "no report, no commits, empty
/// diff" — see fix-worker-env-hygiene-starves-auth). Git identity injection
/// below is independent of this gate and always applies.
fn seed_worker_env(
    spec: &mut SessionSpec,
    auth_verdict: AuthVerdict,
    real_home: Option<&std::path::Path>,
    real_config_dir: Option<&std::path::Path>,
) {
    let mut relocated = false;
    if auth_verdict == AuthVerdict::Authenticated {
        let scratch_root = crate::backend_claude::scratch_home_root(&spec.session_id);
        if let Ok((home, config_dir)) = crate::backend_claude::seed_worker_scratch_home(
            &scratch_root,
            real_home,
            real_config_dir,
        ) {
            spec.env
                .insert("HOME".to_string(), home.display().to_string());
            spec.env.insert(
                "CLAUDE_CONFIG_DIR".to_string(),
                config_dir.display().to_string(),
            );
            relocated = true;
        }
    }

    // Loud decision record (mission m-165b6f, f-1-3): every worker spec build
    // logs which HOME branch was taken and the non-sensitive reason, so a
    // fallback to the real HOME is never silent. Never logs secret/credential
    // values — only the verdict and the decision.
    let (decision, reason) = if relocated {
        (
            "relocated",
            "auth preflight confirmed and scratch HOME seeded",
        )
    } else {
        let reason = if auth_verdict == AuthVerdict::Authenticated {
            "scratch HOME seeding failed after a successful auth preflight"
        } else {
            "auth preflight did not confirm authentication in the scratch env"
        };
        ("inherited", reason)
    };
    // One callsite for both decisions keeps this operational event consistent
    // and makes subscriber behavior independent of which branch registered
    // its callsite first.
    tracing::info!(
        session_id = %spec.session_id,
        decision,
        auth_verdict = ?auth_verdict,
        reason,
        "worker HOME isolation decision"
    );

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
    deny_exceptions: &[String],
    mission_dir: std::path::PathBuf,
    auth_verdict: AuthVerdict,
) -> Result<(SessionSpec, RunMeta)> {
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
        writable: true,
        settings_json: None,
        json_schema: Some(worker_report_schema()),
        max_budget_usd: role_cfg.max_budget_usd,
        max_turns: role_cfg.max_turns,
        env: HashMap::new(),
        sandbox: None,
    };
    spec.env = contract_env(base_sha);
    let real_home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let real_config_dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from);
    seed_worker_env(
        &mut spec,
        auth_verdict,
        real_home.as_deref(),
        real_config_dir.as_deref(),
    );
    spec.sandbox = resolve_sandbox_or_refuse(role_cfg, session_cwd, &mission_dir)?;
    permissions::apply(
        permissions::for_role(role, cfg, &[], grants, deny_exceptions),
        &mut spec,
    );

    let run_meta = RunMeta {
        run_id: uuid::Uuid::new_v4().to_string(),
        role,
        feature_id: Some(feature.id.clone()),
        milestone_id: None,
        model: role_cfg.model.clone(),
        prompt_hash: prompts::hash(role),
    };
    Ok((spec, run_meta))
}

/// Run one validator session for a milestone (plan §4.4/§4.6).
///
/// `kind` must be [`Role::ValidatorScrutiny`] or [`Role::ValidatorFunctional`].
/// Contract `command` strings (plus config `allow_validator_commands`,
/// `grants`, and the milestone's worker-executed `worker_commands`) become
/// `Bash(<command>*)` allows via [`permissions::for_role`].
/// Run one validator session for a milestone (plan §4.4/§4.6).
///
/// `kind` must be [`Role::ValidatorScrutiny`] or [`Role::ValidatorFunctional`].
/// Contract `command` strings (plus config `allow_validator_commands`,
/// `grants`, and the milestone's worker-executed `worker_commands`) become
/// `Bash(<command>*)` allows via [`permissions::for_role`]. Engine-run
/// contract results are a `validation_round` concern — this wrapper passes
/// none; callers with captured results use [`run_validator_in`] directly.
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
    guidance: Option<&str>,
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
        guidance,
        None,
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
    guidance: Option<&str>,
    contract_results: Option<&str>,
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

    let mut task = if kind == Role::ValidatorScrutiny {
        // Scrutiny/mechanical split: scrutiny reviews the range read-only
        // (Read/Grep/Glob + plain git) and is never advertised the contract
        // commands — running them is the functional validator's job.
        format!(
            "Validate milestone `{id}`: {title}\n\n\
             Commit range under review: {start_sha}..HEAD\n\n\
             Validation contract:\n{contract_rendered}\n\n\
             Feature validation criteria:\n{criteria}\n\n\
             You run no commands for this review — inspect the range with \
             Read/Grep/Glob and plain git (your cwd IS the worktree).\n",
            id = milestone.id,
            title = milestone.title,
        )
    } else {
        format!(
            "Validate milestone `{id}`: {title}\n\n\
             Commit range under review: {start_sha}..HEAD\n\n\
             Validation contract:\n{contract_rendered}\n\n\
             Feature validation criteria:\n{criteria}\n\n\
             Allowed commands:\n{commands}\n",
            id = milestone.id,
            title = milestone.title,
        )
    };

    // Operator unblock guidance is injected verbatim into whichever validator
    // runs (and its retry) — the only channel by which an operator's unblock
    // note reaches a fresh validator session. Carried in folded state, so it
    // survives a process restart; cleared when the milestone completes.
    if let Some(g) = guidance {
        task.push_str(&format!(
            "\nOperator guidance (applies to this validation):\n{g}\n"
        ));
    }

    // Engine-run contract results (validator repair 3/5): the functional
    // validator judges captured PASS/FAIL evidence instead of authoring
    // shell. Functional only — scrutiny's split task stays diff+criteria.
    if kind == Role::ValidatorFunctional {
        if let Some(results) = contract_results {
            task.push_str(&format!(
                "\nContract command results (executed engine-side with a bounded timeout; \
                 verbatim output tails — authoritative evidence, do NOT re-run these):\n\
                 {results}"
            ));
        }
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
        tools: cfg.role(kind).tools.clone(),
        writable: false,
        settings_json: None,
        json_schema: Some(validator_report_schema()),
        max_budget_usd: role_cfg.max_budget_usd,
        max_turns: role_cfg.max_turns,
        env: HashMap::new(),
        sandbox: None,
    };
    spec.env = contract_env(base_sha);
    spec.sandbox = resolve_sandbox_or_refuse(role_cfg, session_cwd, &paths.mission_dir())?;
    permissions::apply(
        permissions::for_role(kind, cfg, &combined_commands, grants, &[]),
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

fn resolve_sandbox_or_refuse(
    role_cfg: &RoleConfig,
    session_cwd: &std::path::Path,
    mission_dir: &std::path::Path,
) -> Result<Option<crate::sandbox::ResolvedSandbox>> {
    let (sandbox, warn) =
        crate::sandbox::resolve_for_session(&role_cfg.sandbox, session_cwd, mission_dir);
    if let Some(warn) = warn.as_deref() {
        tracing::warn!("{warn}");
    }
    if sandbox.is_none() && role_cfg.sandbox.enforce != SandboxEnforce::Off {
        return Err(EngineError::Backend(warn.unwrap_or_else(|| {
            format!(
                "sandbox enforce:{:?} requested but no sandbox could be resolved; refusing to run unsandboxed",
                role_cfg.sandbox.enforce
            )
        })));
    }
    Ok(sandbox)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_provider_refusal_propagates_through_resolve_sandbox_or_refuse() {
        // provider:container + fs+net + a per-host egress list is refused at
        // resolve time on every host (the refusal precedes runtime detection,
        // so this is independent of whether docker/podman is installed), and
        // the runner layer must turn that refusal into a hard error rather
        // than run unsandboxed.
        let role_cfg = RoleConfig {
            sandbox: crate::types::SandboxConfig {
                enforce: crate::types::SandboxEnforce::FsNet,
                provider: crate::types::SandboxProvider::Container,
                image: None,
                extra_write: vec![],
                egress: vec!["crates.io:443".to_string()],
            },
            ..MissionConfig::default().worker
        };
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let err = resolve_sandbox_or_refuse(&role_cfg, session.path(), mission.path())
            .expect_err("a refused container sandbox must error, never run unsandboxed");
        let message = err.to_string();
        assert!(message.contains("provider:container"), "{message}");
        assert!(message.contains("egress"), "{message}");
    }

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
            writable: true,
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

    /// Finding 1: a worker in a HOME with no `.gitconfig` must still be able
    /// to `git commit` — proving the injected `GIT_AUTHOR_*` / `GIT_COMMITTER_*`
    /// env vars actually carry the identity through, not merely that the keys
    /// are present. Uses an `Unauthenticated` preflight verdict (the fail-safe
    /// inherit branch — see [`worker_auth_preflight_failure_inherits_home`]),
    /// with HOME pointed at an empty dir, to prove the identity injection
    /// alone suffices when relocation does not happen.
    #[test]
    fn worker_env_hygiene_scratch_home_worker_can_commit() {
        let repo_dir = tempfile::tempdir().unwrap();
        assert!(git(repo_dir.path(), &["init", "-q"]).status.success());

        let mut spec = minimal_worker_spec(repo_dir.path().to_path_buf());
        seed_worker_env(&mut spec, AuthVerdict::Unauthenticated, None, None);

        // Existing contract env survives the env layering.
        assert_eq!(
            spec.env.get("KRANZ_BASE_SHA").map(String::as_str),
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
        );

        // An Unauthenticated preflight verdict is the fail-safe inherit
        // branch: seed_worker_env must NOT relocate HOME.
        assert!(
            !spec.env.contains_key("HOME"),
            "an Unauthenticated preflight verdict must not relocate HOME"
        );

        for key in [
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
        ] {
            assert!(spec.env.contains_key(key), "missing {key}");
        }

        // Point HOME at an empty dir (no ambient .gitconfig) to prove the
        // injected GIT_* identity alone carries a commit through.
        let empty_home = tempfile::tempdir().unwrap();
        std::fs::write(repo_dir.path().join("file.txt"), "content").unwrap();
        assert!(git(repo_dir.path(), &["add", "."]).status.success());

        let commit_status = std::process::Command::new("git")
            .args(["commit", "-m", "worker commit via injected identity"])
            .current_dir(repo_dir.path())
            .env("HOME", empty_home.path())
            .envs(&spec.env)
            .status()
            .expect("git commit spawns");
        assert!(
            commit_status.success(),
            "worker must be able to commit with the injected git identity env"
        );

        let log = git(repo_dir.path(), &["log", "-1", "--format=%an <%ae>"]);
        let logged = String::from_utf8_lossy(&log.stdout).trim().to_string();
        let expected = format!(
            "{} <{}>",
            spec.env["GIT_AUTHOR_NAME"], spec.env["GIT_AUTHOR_EMAIL"]
        );
        assert_eq!(logged, expected);
    }

    /// mission m-165b6f, f-1-2: an `Authenticated` preflight verdict gates
    /// HOME relocation ON. `spec.env` must carry a scratch HOME/
    /// CLAUDE_CONFIG_DIR pair, and the scratch config dir must contain only
    /// the [`crate::backend_claude::claude_min_config_entries`] allowlist —
    /// not arbitrary operator dotfiles that happened to sit alongside it.
    #[test]
    fn worker_auth_preflight_success_relocates() {
        let repo_dir = tempfile::tempdir().unwrap();
        assert!(git(repo_dir.path(), &["init", "-q"]).status.success());

        let real_home = tempfile::tempdir().unwrap();
        let real_config = real_home.path().join(".claude");
        std::fs::create_dir_all(&real_config).unwrap();
        std::fs::write(real_config.join(".credentials.json"), "{\"secret\":true}").unwrap();
        // Not on the allowlist — must never be copied into the scratch dir.
        std::fs::write(real_config.join("settings.json"), "{\"other\":true}").unwrap();

        let mut spec = minimal_worker_spec(repo_dir.path().to_path_buf());
        seed_worker_env(
            &mut spec,
            AuthVerdict::Authenticated,
            Some(real_home.path()),
            None,
        );

        let home = spec.env.get("HOME").expect("HOME must be relocated");
        let config_dir = spec
            .env
            .get("CLAUDE_CONFIG_DIR")
            .expect("CLAUDE_CONFIG_DIR must be relocated");
        let scratch_root = crate::backend_claude::scratch_home_root(&spec.session_id);
        assert!(std::path::Path::new(home).starts_with(&scratch_root));
        assert_eq!(
            std::path::Path::new(config_dir),
            std::path::Path::new(home).join(".claude")
        );

        let entries: Vec<_> = std::fs::read_dir(config_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            vec![".credentials.json".to_string()],
            "scratch config dir must contain only the allowlisted entries: {entries:?}"
        );

        for key in [
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
        ] {
            assert!(spec.env.contains_key(key), "missing {key}");
        }
    }

    /// mission m-165b6f, f-1-2: an unproven preflight verdict
    /// (`Unauthenticated` or `Inconclusive`) is the loud fail-safe — no HOME/
    /// CLAUDE_CONFIG_DIR key at all, so the worker inherits the real HOME,
    /// while git identity injection still applies.
    #[test]
    fn worker_auth_preflight_failure_inherits_home() {
        let repo_dir = tempfile::tempdir().unwrap();
        assert!(git(repo_dir.path(), &["init", "-q"]).status.success());
        let real_home = tempfile::tempdir().unwrap();

        for verdict in [AuthVerdict::Unauthenticated, AuthVerdict::Inconclusive] {
            let mut spec = minimal_worker_spec(repo_dir.path().to_path_buf());
            seed_worker_env(&mut spec, verdict, Some(real_home.path()), None);

            assert!(
                !spec.env.contains_key("HOME"),
                "{verdict:?} must not set HOME"
            );
            assert!(
                !spec.env.contains_key("CLAUDE_CONFIG_DIR"),
                "{verdict:?} must not set CLAUDE_CONFIG_DIR"
            );
            for key in [
                "GIT_AUTHOR_NAME",
                "GIT_AUTHOR_EMAIL",
                "GIT_COMMITTER_NAME",
                "GIT_COMMITTER_EMAIL",
            ] {
                assert!(spec.env.contains_key(key), "{verdict:?} missing {key}");
            }
        }
    }

    /// A no-dependency [`tracing::Subscriber`] that records every event's
    /// fields (debug-formatted) as one string per event, for tests that need
    /// to assert on emitted `tracing::info!` records without pulling in
    /// `tracing-subscriber`.
    struct CapturingSubscriber {
        events: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl tracing::Subscriber for CapturingSubscriber {
        fn register_callsite(
            &self,
            _metadata: &'static tracing::Metadata<'static>,
        ) -> tracing::subscriber::Interest {
            // Other parallel tests emit through these same static callsites
            // without a subscriber. Mark them always-interesting while this
            // dispatcher is installed so the global callsite cache cannot
            // make this capture test order-dependent.
            tracing::subscriber::Interest::always()
        }
        fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }
        fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
        fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
        fn event(&self, event: &tracing::Event<'_>) {
            struct Visitor(String);
            impl tracing::field::Visit for Visitor {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    use std::fmt::Write;
                    let _ = write!(self.0, " {}={:?}", field.name(), value);
                }
            }
            let mut visitor = Visitor(String::new());
            event.record(&mut visitor);
            self.events.lock().unwrap().push(visitor.0);
        }
        fn enter(&self, _span: &tracing::span::Id) {}
        fn exit(&self, _span: &tracing::span::Id) {}
    }

    /// mission m-165b6f, f-1-3: the relocate-vs-inherit decision must be
    /// recorded loudly for BOTH branches — never a silent fallback. This
    /// captures the `tracing::info!` records `seed_worker_env` emits and
    /// asserts the recorded decision matches the branch actually taken, that
    /// the `Unauthenticated`/`Inconclusive` branch carries a non-sensitive
    /// reason, and that no secret/credential value is ever logged.
    #[test]
    fn worker_auth_decision_is_recorded() {
        const CAPTURE_CHILD: &str = "KRANZ_WORKER_AUTH_CAPTURE_CHILD";
        if std::env::var_os(CAPTURE_CHILD).is_none() {
            // `tracing` callsite interest is process-global even when the
            // subscriber is thread-local. Parallel tests exercising the same
            // static info! callsite can therefore suppress this capture. Run
            // the actual assertion in this test binary with one test thread;
            // the env marker prevents recursion.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "runner::tests::worker_auth_decision_is_recorded",
                    "--exact",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CAPTURE_CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "isolated tracing capture failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let repo_dir = tempfile::tempdir().unwrap();
        assert!(git(repo_dir.path(), &["init", "-q"]).status.success());
        let real_home = tempfile::tempdir().unwrap();
        let real_config = real_home.path().join(".claude");
        std::fs::create_dir_all(&real_config).unwrap();
        let secret = "sk-super-secret-credential-value";
        std::fs::write(
            real_config.join(".credentials.json"),
            format!("{{\"token\":\"{secret}\"}}"),
        )
        .unwrap();

        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let subscriber = CapturingSubscriber {
            events: events.clone(),
        };
        let _guard = tracing::subscriber::set_default(subscriber);

        // Authenticated branch: must record "relocated".
        let mut spec = minimal_worker_spec(repo_dir.path().to_path_buf());
        seed_worker_env(
            &mut spec,
            AuthVerdict::Authenticated,
            Some(real_home.path()),
            None,
        );
        assert!(
            spec.env.contains_key("HOME"),
            "sanity: Authenticated verdict should have relocated HOME"
        );
        {
            let recorded = events.lock().unwrap();
            assert!(
                !recorded.is_empty(),
                "the Authenticated decision must be recorded"
            );
            let record = recorded.last().unwrap();
            assert!(
                record.contains("decision=\"relocated\""),
                "expected a relocated decision record, got: {record}"
            );
            assert!(
                record.contains("Authenticated"),
                "record must carry the verdict that drove it: {record}"
            );
        }

        // Unauthenticated/Inconclusive branch: must record "inherited" with a
        // non-sensitive reason.
        for verdict in [AuthVerdict::Unauthenticated, AuthVerdict::Inconclusive] {
            events.lock().unwrap().clear();
            let mut spec = minimal_worker_spec(repo_dir.path().to_path_buf());
            seed_worker_env(&mut spec, verdict, Some(real_home.path()), None);
            assert!(
                !spec.env.contains_key("HOME"),
                "sanity: {verdict:?} must not relocate HOME"
            );
            let recorded = events.lock().unwrap();
            assert!(
                !recorded.is_empty(),
                "{verdict:?} decision must be recorded"
            );
            let record = recorded.last().unwrap();
            assert!(
                record.contains("decision=\"inherited\""),
                "expected an inherited decision record for {verdict:?}, got: {record}"
            );
            assert!(
                record.contains("reason="),
                "record must carry a non-sensitive reason for {verdict:?}: {record}"
            );
            assert!(
                !record.contains(secret),
                "decision record must never contain a secret/credential value: {record}"
            );
        }
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
