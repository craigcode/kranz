//! Live event printer for `kranz run`: a read-only tail of `events.jsonl`
//! (never the lock — the engine is the single writer, §4.3).
//!
//! [`tail_events`] polls [`EventLog::read_events_after`] every
//! [`POLL_INTERVAL`] from the pre-run head seq and prints one human line per
//! event to stderr. [`EventRenderer`] does the event → line mapping and is
//! separate so tests can assert on exact lines.

use crate::output::{ansi, one_line};
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
            EventKind::FeatureFailed { feature_id, reason } => (
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
            EventKind::MilestoneBlocked {
                milestone_id,
                reason,
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
            EventKind::WorkspaceProvisioned { provider, cwd } => (
                "workspace".to_string(),
                ansi::BLUE,
                format!("provisioned ({provider}): {cwd}"),
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
            EventKind::WorkspaceTeardown { mode } => (
                "workspace".to_string(),
                ansi::DIM,
                format!("teardown ({mode})"),
            ),
        };

        // Budget: "[tag] body" must fit LINE_MAX visible chars.
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
