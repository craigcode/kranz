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
// Sink: transcript + event-log fan-out for one run's stream
// ---------------------------------------------------------------------------

/// Where a run's event stream lands: every event's raw JSON goes to the
/// transcript (one line each, scrubbed); selected events are appended to the
/// event log as `worker.message` deltas (scrubbed + truncated).
pub struct RunSink<'a> {
    pub log: &'a mut EventLog,
    pub transcript: &'a mut (dyn std::io::Write + Send),
}

impl RunSink<'_> {
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
            AgentEvent::ToolResult { tool, denied, summary, .. } => {
                let content = match tool {
                    Some(tool) => format!("{tool}: {summary}"),
                    None => summary.clone(),
                };
                (if *denied { "denied" } else { "tool-result" }, content, *denied)
            }
            _ => return Ok(false),
        };

        self.log.append(EventKind::WorkerMessage {
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
pub async fn run_session(
    backend: &dyn AgentBackend,
    spec: SessionSpec,
    log: &mut EventLog,
    paths: &MissionPaths,
    run_meta: RunMeta,
    cancel: Option<Arc<Notify>>,
) -> Result<RunOutcome> {
    std::fs::create_dir_all(paths.runs_dir())?;
    let transcript_path = paths.transcript_file(&run_meta.run_id);
    let mut transcript = std::io::BufWriter::new(std::fs::File::create(&transcript_path)?);

    // The sdk session id recorded for --resume bookkeeping: the resumed id
    // when resuming, else the engine-chosen fresh id.
    let sdk_session_id = spec.resume.clone().unwrap_or_else(|| spec.session_id.clone());
    log.append(EventKind::WorkerSpawned {
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
        let mut sink = RunSink { log, transcript: &mut transcript };
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
            Role::Worker => report.as_ref().map(|r| r.result).unwrap_or(RunResult::Partial),
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

    log.append(EventKind::WorkerCompleted {
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
            "commits": { "type": "array", "items": { "type": "string" } }
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
                        "suggestedFix": { "type": "string" }
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
) -> Result<RunOutcome> {
    let role = Role::Worker;
    let role_cfg = cfg.role(role);

    let criteria = bullet_list(&feature.validation_criteria);
    let turn_budget =
        role_cfg.max_turns.map(|n| n.to_string()).unwrap_or_else(|| "unlimited".to_string());
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
        settings_json: None,
        json_schema: Some(worker_report_schema()),
        max_budget_usd: role_cfg.max_budget_usd,
        max_turns: role_cfg.max_turns,
        env: HashMap::new(),
    };
    permissions::apply(permissions::for_role(role, cfg, &[]), &mut spec);

    let run_meta = RunMeta {
        run_id: uuid::Uuid::new_v4().to_string(),
        role,
        feature_id: Some(feature.id.clone()),
        milestone_id: None,
        model: role_cfg.model.clone(),
        prompt_hash: prompts::hash(role),
    };
    run_session(backend, spec, log, paths, run_meta, cancel).await
}

/// Run one validator session for a milestone (plan §4.4/§4.6).
///
/// `kind` must be [`Role::ValidatorScrutiny`] or [`Role::ValidatorFunctional`].
/// Contract `command` strings (plus config `allow_validator_commands`) become
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
        .flat_map(|f| f.validation_criteria.iter().map(|c| format!("[{}] {}", f.id, c)))
        .collect();
    let criteria = bullet_list(&criteria_items);

    let contract_commands: Vec<String> =
        contract.iter().filter_map(|a| a.command.clone()).collect();
    let mut allowed_commands = contract_commands.clone();
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
        cwd: paths.repo_root.clone(),
        prompt: PromptMode::SingleShot(task),
        append_system_prompt: Some(role_prompt),
        model: role_cfg.model.clone(),
        effort: role_cfg.reasoning_effort.clone(),
        session_id: uuid::Uuid::new_v4().to_string(),
        resume: None,
        permission_mode: None,
        allowed_tools: Vec::new(),
        disallowed_tools: Vec::new(),
        settings_json: None,
        json_schema: Some(validator_report_schema()),
        max_budget_usd: role_cfg.max_budget_usd,
        max_turns: role_cfg.max_turns,
        env: HashMap::new(),
    };
    permissions::apply(permissions::for_role(kind, cfg, &contract_commands), &mut spec);

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
    items.iter().map(|item| format!("- {item}")).collect::<Vec<_>>().join("\n")
}
