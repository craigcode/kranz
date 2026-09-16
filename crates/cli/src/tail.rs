//! Live event printer for `kranz run`: a read-only tail of `events.jsonl`
//! (never the lock — the engine is the single writer, §4.3).
//!
//! [`tail_events`] polls [`EventLog::read_events_after`] every
//! [`POLL_INTERVAL`] from the pre-run head seq and prints one human line per
//! event to stderr. [`EventRenderer`] does the event → line mapping and is
//! separate so tests can assert on exact lines.

use crate::output::{ansi, one_line, sanitize_untrusted};
use kranz_engine::event_log::EventLog;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::types::{GrantKind, MissionState, Role, RunResult};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Tail poll interval.
pub const POLL_INTERVAL: Duration = Duration::from_millis(300);

/// Hard cap on a rendered line (visible characters, ANSI codes excluded).
const LINE_MAX: usize = 160;

/// Role/scope info remembered per run id so `worker.message` lines can be
/// tagged `[worker f-1-2]`, `[orch]`, `[validator-scrutiny ms-1]`, ...
struct RunTag {
    role: Role,
    feature_id: Option<String>,
    milestone_id: Option<String>,
}

/// Renders one [`Event`] as one role-tagged, optionally colored, single line.
pub struct EventRenderer {
    color: bool,
    runs: HashMap<String, RunTag>,
    /// Planning REPL mode: orchestrator text replies are printed in full by
    /// the REPL itself, so the tail suppresses `text` deltas and shows only
    /// activity (tool use, results, denials, system notes).
    suppress_text: bool,
}

impl EventRenderer {
    pub fn new(color: bool) -> Self {
        EventRenderer {
            color,
            runs: HashMap::new(),
            suppress_text: false,
        }
    }

    /// Renderer for the planning REPL (see `suppress_text`).
    pub fn planning(state: &MissionState, color: bool) -> Self {
        let mut renderer = Self::seeded(state, color);
        renderer.suppress_text = true;
        renderer
    }

    /// Renderer pre-seeded with the runs already in `state`, so messages of
    /// sessions spawned before this tail started still get proper tags.
    pub fn seeded(state: &MissionState, color: bool) -> Self {
        let mut renderer = Self::new(color);
        for run in state.runs.values() {
            renderer.runs.insert(
                run.id.clone(),
                RunTag {
                    role: run.role,
                    feature_id: run.feature_id.clone(),
                    milestone_id: run.milestone_id.clone(),
                },
            );
        }
        renderer
    }

    /// `[tag]` + color for a run id; unknown runs degrade to a bare worker tag.
    fn run_tag(&self, run_id: &str) -> (String, &'static str) {
        match self.runs.get(run_id) {
            Some(tag) => match tag.role {
                Role::Orchestrator => ("orch".to_string(), ansi::CYAN),
                Role::Worker => (
                    match &tag.feature_id {
                        Some(feature) => format!("worker {feature}"),
                        None => "worker".to_string(),
                    },
                    ansi::GREEN,
                ),
                Role::ValidatorScrutiny => (
                    scoped("validator-scrutiny", &tag.milestone_id),
                    ansi::MAGENTA,
                ),
                Role::ValidatorFunctional => (
                    scoped("validator-functional", &tag.milestone_id),
                    ansi::MAGENTA,
                ),
            },
            None => ("worker".to_string(), ansi::GREEN),
        }
    }

    /// One event → one human line (role-tagged, truncated to [`LINE_MAX`]).
    pub fn render(&mut self, event: &Event) -> String {
        let (tag, color, body) = match &event.kind {
            EventKind::GateEvaluationRequested { evaluation } => (
                "gate".into(),
                ansi::YELLOW,
                format!(
                    "{} {:?}: evaluation requested (attempt {})",
                    evaluation.request.params.gate_id.as_str(),
                    evaluation.request.params.stage,
                    evaluation.request.params.attempt_id.as_str()
                ),
            ),
            EventKind::GateEvaluationFinished { evaluation } => (
                "gate".into(),
                ansi::YELLOW,
                format!(
                    "{}: evaluator finished; cleanup confirmed={}",
                    evaluation.attempt_id.as_str(),
                    evaluation.cleanup_confirmed
                ),
            ),
            EventKind::GateResolutionRecorded { resolution } => (
                "gate".into(),
                ansi::YELLOW,
                format!(
                    "{}: {:?} — {}",
                    resolution.attempt_id.as_str(),
                    resolution.disposition,
                    resolution.rationale
                ),
            ),
            EventKind::GateResolutionConsumed { consumption } => (
                "gate".into(),
                ansi::YELLOW,
                format!(
                    "{}: {:?} attempted; effect completion is separate",
                    consumption.attempt_id.as_str(),
                    consumption.action
                ),
            ),
            EventKind::PermissionRequested { request } => (
                "permission".into(),
                ansi::YELLOW,
                format!(
                    "{} awaits one-call consent (run {}, expires {})",
                    request.proposal.id, request.binding.run_id, request.proposal.deadline
                ),
            ),
            EventKind::PermissionResolved { resolution } => (
                "permission".into(),
                ansi::YELLOW,
                format!(
                    "{}: {} once; response pending",
                    resolution.request_id,
                    if resolution.allow { "allow" } else { "deny" }
                ),
            ),
            EventKind::PermissionResponseRecorded {
                request_id,
                delivery,
            } => (
                "permission".into(),
                ansi::YELLOW,
                format!("{request_id}: response {delivery:?}; tool outcome is separate"),
            ),
            EventKind::PermissionClosed { request_id, reason } => (
                "permission".into(),
                ansi::YELLOW,
                format!("{request_id}: closed ({reason})"),
            ),
            EventKind::MissionCreated { goal, .. } => (
                "mission".to_string(),
                ansi::YELLOW,
                format!("created: {goal}"),
            ),
            EventKind::PlanApproved { plan, .. } => {
                let features: usize = plan.milestones.iter().map(|m| m.features.len()).sum();
                (
                    "mission".to_string(),
                    ansi::YELLOW,
                    format!(
                        "plan approved ({} milestone(s), {features} feature(s))",
                        plan.milestones.len()
                    ),
                )
            }
            EventKind::PlanRevisionProposed { revision, .. } => (
                "mission".to_string(),
                ansi::YELLOW,
                format!("revision {revision} proposed; awaiting approval"),
            ),
            EventKind::PlanRevised { revision, .. } => (
                "mission".to_string(),
                ansi::YELLOW,
                format!("revision {revision} approved"),
            ),
            EventKind::PlanRevisionRejected { revision, .. } => (
                "mission".to_string(),
                ansi::YELLOW,
                format!("revision {revision} rejected"),
            ),
            EventKind::GrantRequested {
                milestone_id,
                kind,
                command,
            } => (
                format!("milestone {milestone_id}"),
                ansi::YELLOW,
                format!(
                    "{} grant requested: `{command}`; awaiting approval",
                    grant_kind_label(kind)
                ),
            ),
            EventKind::GrantApproved { kind, command } => (
                "mission".to_string(),
                ansi::YELLOW,
                format!("{} grant approved: `{command}`", grant_kind_label(kind)),
            ),
            EventKind::GrantDenied {
                kind,
                command,
                reason,
            } => (
                "mission".to_string(),
                ansi::YELLOW,
                format!(
                    "{} grant denied: `{command}` ({reason})",
                    grant_kind_label(kind)
                ),
            ),
            EventKind::QuestionOpened {
                question_id,
                text,
                options,
                ..
            } => (
                "mission".to_string(),
                ansi::YELLOW,
                if options.is_empty() {
                    format!("question {question_id} opened: {text} (free-text answer)")
                } else {
                    format!(
                        "question {question_id} opened: {text} ({} option(s)); awaiting an answer",
                        options.len()
                    )
                },
            ),
            EventKind::QuestionAnswered {
                question_id,
                answer,
                ..
            } => (
                "mission".to_string(),
                ansi::YELLOW,
                format!("question {question_id} answered: {answer}"),
            ),
            EventKind::QuestionCleared { question_id, why } => (
                "mission".to_string(),
                ansi::DIM,
                format!("question {question_id} cleared ({why})"),
            ),
            EventKind::MilestoneStarted { milestone_id, .. } => (
                format!("milestone {milestone_id}"),
                ansi::YELLOW,
                "started".to_string(),
            ),
            EventKind::FeatureStarted { feature_id } => (
                format!("feature {feature_id}"),
                ansi::BLUE,
                "started".to_string(),
            ),
            EventKind::FeatureProgress {
                feature_id,
                commits,
                ..
            } => (
                format!("feature {feature_id}"),
                ansi::DIM,
                format!("recorded {} cumulative commit(s)", commits.len()),
            ),
            EventKind::WorkerSpawned {
                run_id,
                role,
                feature_id,
                milestone_id,
                model,
                ..
            } => {
                self.runs.insert(
                    run_id.clone(),
                    RunTag {
                        role: *role,
                        feature_id: feature_id.clone(),
                        milestone_id: milestone_id.clone(),
                    },
                );
                let (tag, color) = self.run_tag(run_id);
                (tag, color, format!("spawned ({model})"))
            }
            EventKind::WorkerMessage {
                run_id,
                tag: kind,
                content,
            } => {
                if self.suppress_text && kind == "text" {
                    return String::new();
                }
                let (tag, mut color) = self.run_tag(run_id);
                let body = match kind.as_str() {
                    "text" => content.clone(),
                    "denied" => {
                        color = ansi::RED;
                        format!("DENIED: {content}")
                    }
                    other => format!("{other}: {content}"),
                };
                (tag, color, body)
            }
            EventKind::WorkerEgressDenied {
                run_id,
                denials,
                omitted_count,
            } => {
                let (tag, _) = self.run_tag(run_id);
                let first = denials
                    .first()
                    .map(|denial| format!("{}:{}", denial.host, denial.port))
                    .unwrap_or_else(|| "destination unavailable".to_string());
                let more = (denials.len().saturating_sub(1) as u64).saturating_add(*omitted_count);
                let suffix = if more > 0 {
                    format!(" (+{more} additional record(s))")
                } else {
                    String::new()
                };
                (tag, ansi::RED, format!("EGRESS DENIED: {first}{suffix}"))
            }
            EventKind::WorkerCompleted {
                run_id,
                result,
                cost_usd,
                ..
            } => {
                let (tag, color) = self.run_tag(run_id);
                let cost = cost_usd.map(|c| format!(" (${c:.2})")).unwrap_or_default();
                (
                    tag,
                    color,
                    format!("completed: {}{cost}", result_label(*result)),
                )
            }
            EventKind::FeatureCompleted {
                feature_id,
                commits,
            } => (
                format!("feature {feature_id}"),
                ansi::BLUE,
                format!("complete ({} commit(s))", commits.len()),
            ),
            EventKind::FeatureFailed {
                feature_id, reason, ..
            } => (
                format!("feature {feature_id}"),
                ansi::RED,
                format!("FAILED: {reason}"),
            ),
            EventKind::FeatureSkipped { feature_id, reason } => (
                format!("feature {feature_id}"),
                ansi::BLUE,
                format!("skipped: {reason}"),
            ),
            EventKind::MilestoneValidating { milestone_id } => (
                format!("milestone {milestone_id}"),
                ansi::YELLOW,
                "validating".to_string(),
            ),
            EventKind::ValidationFinding {
                milestone_id,
                finding,
                ..
            } => (
                format!("milestone {milestone_id}"),
                ansi::YELLOW,
                format!(
                    "finding [{}] {}: {}",
                    finding.severity, finding.subject, finding.evidence
                ),
            ),
            EventKind::ValidatorTamper {
                milestone_id,
                head_before,
                head_after,
                appeared,
                resolved,
                git_metadata_changed,
                ..
            } => {
                let mut what: Vec<String> = appeared.iter().take(3).cloned().collect();
                if head_before != head_after {
                    what.push("HEAD moved".to_string());
                }
                if *git_metadata_changed {
                    what.push(".git metadata".to_string());
                }
                if !resolved.is_empty() {
                    what.push(format!("{} entr(ies) hidden", resolved.len()));
                }
                (
                    format!("milestone {milestone_id}"),
                    ansi::RED,
                    format!("validator TAMPER: {}", what.join("; ")),
                )
            }
            EventKind::ValidationSnapshot {
                milestone_id,
                target_tier,
                detail,
                ..
            } => (
                format!("milestone {milestone_id}"),
                ansi::DIM,
                match detail {
                    Some(detail) => {
                        format!("validator snapshot (target: {target_tier}) — {detail}")
                    }
                    None => format!("validator snapshot (target: {target_tier})"),
                },
            ),
            EventKind::ValidationConfirm {
                milestone_id,
                confirmed,
                disagreements,
                ..
            } => (
                format!("milestone {milestone_id}"),
                if disagreements.is_empty() {
                    ansi::DIM
                } else {
                    ansi::YELLOW
                },
                if disagreements.is_empty() {
                    format!(
                        "local validator PASS frontier-confirmed ({} check(s))",
                        confirmed.len()
                    )
                } else {
                    format!(
                        "local validator MISS: frontier overturned {} PASS(es): {}",
                        disagreements.len(),
                        disagreements
                            .iter()
                            .map(|f| f.subject.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                },
            ),
            EventKind::ValidationPtyTranscript {
                milestone_id,
                assertion_id,
                verdict,
                ..
            } => (
                format!("milestone {milestone_id}"),
                match verdict {
                    kranz_engine::gate::GateVerdict::Pass => ansi::DIM,
                    kranz_engine::gate::GateVerdict::Fail => ansi::YELLOW,
                },
                format!(
                    "pty validation [{assertion_id}] {} (transcript artifact)",
                    match verdict {
                        kranz_engine::gate::GateVerdict::Pass => "PASS",
                        kranz_engine::gate::GateVerdict::Fail => "FAIL",
                    }
                ),
            ),
            EventKind::GateResult {
                gate,
                surface,
                kind,
                index,
                verdict,
                ..
            } => (
                "gate".to_string(),
                match verdict {
                    kranz_engine::gate::GateVerdict::Pass => ansi::GREEN,
                    kranz_engine::gate::GateVerdict::Fail => ansi::YELLOW,
                },
                format!(
                    "{gate} ({}, {} #{index}) {}",
                    match surface {
                        kranz_engine::gate::GateSurface::Approval => "approval",
                        kranz_engine::gate::GateSurface::FinalGate => "final gate",
                    },
                    match kind {
                        kranz_engine::gate::GateKind::Deterministic => "det",
                        kranz_engine::gate::GateKind::ModelJudged => "model",
                    },
                    match verdict {
                        kranz_engine::gate::GateVerdict::Pass => "pass",
                        kranz_engine::gate::GateVerdict::Fail => "FAIL",
                    }
                ),
            ),
            EventKind::HookGateFired {
                gate,
                tool,
                subject,
                verdict,
                ..
            } => (
                "hook gate".to_string(),
                if verdict == "blocked" {
                    ansi::YELLOW
                } else {
                    ansi::DIM
                },
                format!("{gate}: {tool} {subject} {verdict} (in-process)"),
            ),
            EventKind::DivergenceNoted {
                unit,
                candidates,
                diverged,
            } => (
                format!("feature {unit}"),
                ansi::BLUE,
                // The agreement wording carries the rule: logged, never
                // trusted — the tail must not read as a green light.
                if *diverged {
                    format!("pool streams DIVERGED ({} candidates)", candidates.len())
                } else {
                    format!(
                        "pool streams agreed ({} candidates) — logged, never trusted",
                        candidates.len()
                    )
                },
            ),
            EventKind::DivergenceResolved {
                unit,
                selected,
                decided_by,
                ..
            } => (
                format!("feature {unit}"),
                ansi::BLUE,
                match selected {
                    Some(index) => format!("pool resolved → candidate c{index} (by {decided_by})"),
                    None => format!("pool resolved → no candidate (by {decided_by})"),
                },
            ),
            EventKind::FixFeatureCreated {
                milestone_id,
                feature,
            } => (
                format!("milestone {milestone_id}"),
                ansi::YELLOW,
                format!("fix feature {}: {}", feature.id, feature.title),
            ),
            EventKind::TierEscalated {
                milestone_id,
                from,
                to,
                reason,
            } => (
                format!("milestone {milestone_id}"),
                ansi::YELLOW,
                format!("escalated {from:?} -> {to:?}: {reason}"),
            ),
            EventKind::WorkerEscalated {
                feature_id,
                from,
                to,
                reason,
                ..
            } => (
                format!("feature {feature_id}"),
                ansi::YELLOW,
                format!("worker asked the frontier advisor ({from:?} -> {to:?}): {reason}"),
            ),
            EventKind::MilestoneBlocked {
                milestone_id,
                reason,
                ..
            } => (
                format!("milestone {milestone_id}"),
                ansi::RED,
                format!("BLOCKED: {reason}"),
            ),
            EventKind::MilestoneUnblocked {
                milestone_id,
                reason,
                ..
            } => (
                format!("milestone {milestone_id}"),
                ansi::YELLOW,
                format!("unblocked: {reason}"),
            ),
            EventKind::MilestoneCompleted { milestone_id, tag } => {
                let tag_note = tag
                    .as_deref()
                    .map(|t| format!(" (tag {t})"))
                    .unwrap_or_default();
                (
                    format!("milestone {milestone_id}"),
                    ansi::YELLOW,
                    format!("complete{tag_note}"),
                )
            }
            EventKind::MissionValidating {} => (
                "mission".to_string(),
                ansi::YELLOW,
                "final contract gate".to_string(),
            ),
            EventKind::MissionPaused {} => {
                ("mission".to_string(), ansi::YELLOW, "paused".to_string())
            }
            EventKind::MissionResumed {} => {
                ("mission".to_string(), ansi::YELLOW, "resumed".to_string())
            }
            EventKind::UserMessage { text, interrupt } => (
                "user".to_string(),
                ansi::MAGENTA,
                if *interrupt {
                    format!("(interrupt) {text}")
                } else {
                    text.clone()
                },
            ),
            EventKind::OrchestratorDecision { summary, .. } => (
                "orch".to_string(),
                ansi::CYAN,
                format!("decision: {summary}"),
            ),
            EventKind::SecretRedacted {
                rule_id,
                fingerprint,
                location,
            } => (
                "secret".to_string(),
                ansi::YELLOW,
                format!("redacted {rule_id} {fingerprint} at {location}"),
            ),
            EventKind::ConfigChanged { .. } => (
                "mission".to_string(),
                ansi::YELLOW,
                "config changed".to_string(),
            ),
            EventKind::MissionCompleted {} => {
                ("mission".to_string(), ansi::GREEN, "COMPLETE".to_string())
            }
            EventKind::MissionFailed { reason } => (
                "mission".to_string(),
                ansi::RED,
                format!("FAILED: {reason}"),
            ),
            EventKind::MissionAbandoned { reason } => (
                "mission".to_string(),
                ansi::DIM,
                format!("ABANDONED: {reason}"),
            ),
            EventKind::WorkspaceProvisioned {
                provider,
                cwd,
                detail,
                ..
            } => (
                "workspace".to_string(),
                ansi::BLUE,
                match detail {
                    Some(detail) => format!("provisioned ({provider}): {cwd} — {detail}"),
                    None => format!("provisioned ({provider}): {cwd}"),
                },
            ),
            EventKind::WorkspaceReadinessReport { outcome, detail } => (
                "workspace".to_string(),
                if outcome == "ready" {
                    ansi::GREEN
                } else {
                    ansi::RED
                },
                match detail {
                    Some(detail) => format!("readiness {outcome}: {detail}"),
                    None => format!("readiness {outcome}"),
                },
            ),
            EventKind::WorkspaceTeardown { mode, state } => (
                "workspace".to_string(),
                ansi::DIM,
                // The outcome rides along when present (ticket
                // workspace-idle-hibernate); old stateless lines render as
                // before.
                match state {
                    Some(state) => format!("teardown ({mode}): {state}"),
                    None => format!("teardown ({mode})"),
                },
            ),
            EventKind::WorkspaceProviderPinned {
                provider,
                template,
                version,
            } => (
                "workspace".to_string(),
                ansi::BLUE,
                format!(
                    "provider pinned: {provider} · {template} · {}",
                    if provider == "remote" {
                        // The remote kind pins the ADAPTER version, not a
                        // contract schema (workspace-remote-coder-provider).
                        format!("adapter {version}")
                    } else if version == "none" {
                        "no contract".to_string()
                    } else {
                        format!("contract v{version}")
                    }
                ),
            ),
            EventKind::StandardsResolved {
                pack_name,
                digest,
                rules,
                ..
            } => (
                "standards".to_string(),
                ansi::BLUE,
                format!(
                    "resolved: pack {pack_name} · {} rule(s) pinned · sha256:{digest}",
                    rules.len()
                ),
            ),
            EventKind::StandardsDrifted { changed_rules, .. } => (
                "standards".to_string(),
                ansi::RED,
                format!(
                    "policy drift: merge refused ({} change(s) to the applicable enforced set)",
                    changed_rules.len()
                ),
            ),
            EventKind::StandardsWaiverApproved {
                rule_id,
                rule_revision,
                approver,
                ..
            } => (
                "standards".to_string(),
                ansi::BLUE,
                format!("waiver approved: {rule_id} r{rule_revision} by {approver}"),
            ),
            EventKind::StandardsAttestationApproved {
                rule_id,
                rule_revision,
                approver,
                ..
            } => (
                "standards".to_string(),
                ansi::BLUE,
                format!("attestation approved: {rule_id} r{rule_revision} by {approver}"),
            ),
        };

        // Budget: "[tag] body" must fit LINE_MAX visible chars. Both halves
        // interpolate model-authored ids and prose, so both are sanitized
        // before they reach the operator's terminal (H9); `one_line` already
        // sanitizes, the tag needs it explicitly.
        let tag = sanitize_untrusted(&tag);
        let budget = LINE_MAX.saturating_sub(tag.chars().count() + 3);
        let body = one_line(&body, budget);
        if self.color {
            format!("{color}[{tag}]{reset} {body}", reset = ansi::RESET)
        } else {
            format!("[{tag}] {body}")
        }
    }
}

/// Short label for a grant's kind, prefixed onto the tail line.
fn grant_kind_label(kind: &GrantKind) -> &'static str {
    match kind {
        GrantKind::Command => "command",
        GrantKind::TouchPath => "touch-set",
        GrantKind::WorkerDeny => "deny-lift",
        GrantKind::Egress => "egress",
    }
}

fn scoped(base: &str, milestone_id: &Option<String>) -> String {
    match milestone_id {
        Some(id) => format!("{base} {id}"),
        None => base.to_string(),
    }
}

fn result_label(result: RunResult) -> &'static str {
    match result {
        RunResult::Pass => "pass",
        RunResult::Fail => "fail",
        RunResult::Partial => "partial",
    }
}

/// Tail `events.jsonl` from `from_seq`, printing each new event as one line
/// to stderr, until `stop` is set (a final catch-up read runs after that).
///
/// Read errors are treated as transient (the engine may be mid-write on
/// another thread; a torn *final* line is already tolerated by the reader).
pub async fn tail_events(
    events_path: PathBuf,
    from_seq: u64,
    mut renderer: EventRenderer,
    stop: Arc<AtomicBool>,
) {
    let mut last_seq = from_seq;
    loop {
        let stopping = stop.load(Ordering::Relaxed);
        match EventLog::read_events_after(&events_path, last_seq) {
            Ok(events) => {
                for event in &events {
                    eprintln!("{}", renderer.render(event));
                    last_seq = event.seq;
                }
            }
            Err(error) => {
                tracing::debug!(%error, "event tail read failed (transient)");
            }
        }
        if stopping {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
