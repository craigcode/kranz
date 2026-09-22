//! Versioned display mapping over recorded causes. Never an execution policy.
use crate::events::{Event, EventKind};
use crate::gate_evaluation::{lifecycle, protocol};
use crate::live_permission::Actor;
use crate::types::{BlockCause, BlockContext, BlockOwner, MissionStatus, RunResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAPPING_VERSION: u32 = 1;

pub fn render_text(report: &Report) -> String {
    use std::fmt::Write as _;
    let safe = |text: &str| {
        text.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
    };
    let mut out = format!(
        "Recorded outcome reasons (mapping v{})\n{}\n",
        report.mapping_version, report.selection
    );
    let _ = writeln!(
        out,
        "Window: {} → {}",
        report
            .from
            .map(|d| d.to_rfc3339())
            .unwrap_or_else(|| "all history".into()),
        report
            .through
            .map(|d| d.to_rfc3339())
            .unwrap_or_else(|| "latest recorded event".into())
    );
    if report.missions.is_empty() {
        out.push_str("No missions in this activity window.\n");
    }
    for class in &report.task_classes {
        let _ = writeln!(
            out,
            "  {}: {} missions; {} mixed; {} with unresolved requests or blocks",
            safe(&class.task_class),
            class.missions,
            class.mixed_missions,
            class.unresolved_missions
        );
        for count in &class.counts {
            let _ = writeln!(
                out,
                "    {}: {}/{} missions, {} observations",
                count.category.label(),
                count.missions,
                class.missions,
                count.observations
            );
        }
    }
    for mission in &report.missions {
        let _ = writeln!(
            out,
            "  {}: current state {}",
            safe(&mission.mission_id),
            mission
                .current_status
                .map(|s| serde_json::to_string(&s).unwrap_or_default())
                .unwrap_or_else(|| "unknown".into())
        );
        for row in &mission.observations {
            let _ = writeln!(
                out,
                "    event #{} [{}; {:?}] {}: {}",
                row.seq,
                row.category.label(),
                row.state,
                row.event_type,
                safe(&row.detail)
            );
            let _ = writeln!(out, "      milestone={:?} feature={:?} run={:?} attempt={:?} stage={:?} permission={:?} actor={:?} resolution-event={:?}", row.milestone_id, row.feature_id, row.run_id, row.attempt_id, row.stage, row.permission_request_id, row.actor, row.resolution_seq);
        }
    }
    if !report.unavailable_logs.is_empty() {
        let _ = writeln!(
            out,
            "Unavailable logs (excluded): {}",
            safe(&report.unavailable_logs.join(", "))
        );
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    ReportedDefect,
    EnvironmentPrerequisite,
    HumanPolicyBoundary,
    Interrupted,
    Cancelled,
    Unknown,
}
impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Self::ReportedDefect => "reported defect",
            Self::EnvironmentPrerequisite => "environment prerequisite",
            Self::HumanPolicyBoundary => "human/policy boundary",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResolutionState {
    Recorded,
    Unresolved,
    Resolved,
    Closed,
    Expired,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub event_type: String,
    pub category: Category,
    pub detail: String,
    pub state: ResolutionState,
    pub resolution_seq: Option<u64>,
    pub milestone_id: Option<String>,
    pub feature_id: Option<String>,
    pub run_id: Option<String>,
    pub attempt_id: Option<String>,
    pub permission_request_id: Option<String>,
    pub stage: Option<protocol::Stage>,
    pub actor: Option<Actor>,
    pub block_context: Option<BlockContext>,
    pub deadline: Option<DateTime<Utc>>,
}
impl Observation {
    fn new(event: &Event, category: Category, detail: &str) -> Self {
        Self {
            seq: event.seq,
            ts: event.ts,
            event_type: event.kind.type_name().into(),
            category,
            detail: crate::scrub::scrub_and_truncate(detail, 4096),
            state: ResolutionState::Recorded,
            resolution_seq: None,
            milestone_id: None,
            feature_id: None,
            run_id: None,
            attempt_id: None,
            permission_request_id: None,
            stage: None,
            actor: None,
            block_context: None,
            deadline: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissionReasons {
    pub mapping_version: u32,
    pub mission_id: String,
    pub task_class: String,
    /// The existing reducer's status, never a replacement outcome taxonomy.
    pub current_status: Option<MissionStatus>,
    pub latest_event_at: Option<DateTime<Utc>>,
    pub latest_event_seq: Option<u64>,
    pub observations: Vec<Observation>,
}

fn block_category(context: Option<BlockContext>) -> Category {
    match context {
        Some(BlockContext {
            owner: BlockOwner::Engine,
            cause: BlockCause::Authentication,
        }) => Category::EnvironmentPrerequisite,
        Some(BlockContext {
            owner: BlockOwner::Operator,
            cause: BlockCause::Operator,
        }) => Category::HumanPolicyBoundary,
        Some(BlockContext {
            owner: BlockOwner::Engine,
            cause:
                BlockCause::Grant
                | BlockCause::SecretScan
                | BlockCause::UntrustedValidator
                | BlockCause::ValidatorTamper
                | BlockCause::ReviewerIndependence,
        }) => Category::HumanPolicyBoundary,
        // A workspace check or generic validation failure can be infrastructure
        // or defective work. Neither the owner nor its prose establishes which.
        _ => Category::Unknown,
    }
}

fn resolve(
    rows: &mut [Observation],
    pending: &mut BTreeMap<String, usize>,
    key: &str,
    seq: u64,
    state: ResolutionState,
) {
    if let Some(index) = pending.remove(key) {
        rows[index].state = state;
        rows[index].resolution_seq = Some(seq);
    }
}

/// Inputs already belong to one mission and are in log order. Unknown reducer
/// state makes attribution untrusted; the projection still exposes provenance.
pub(super) fn mission(
    mission_id: &str,
    events: &[&Event],
    status: Option<MissionStatus>,
    task_class: Option<&str>,
) -> MissionReasons {
    let mut rows = Vec::new();
    let mut pending = BTreeMap::new();
    let mut attempts = BTreeMap::new();
    let mut runs = BTreeMap::new();
    let mut permissions = BTreeMap::new();
    for event in events {
        let mut key = None;
        let mut row = match &event.kind {
            EventKind::WorkerSpawned { run_id, feature_id, milestone_id, role, .. } => {
                runs.insert(run_id.as_str(), (feature_id.clone(), milestone_id.clone(), *role));
                continue;
            }
            EventKind::MilestoneBlocked { milestone_id, reason, block_context } => {
                key = Some(format!("block:{milestone_id}"));
                let mut row = Observation::new(event, block_category(*block_context), reason);
                row.milestone_id = Some(milestone_id.clone());
                row.block_context = *block_context;
                row
            }
            EventKind::MilestoneUnblocked { milestone_id, .. } | EventKind::MilestoneCompleted { milestone_id, .. } => {
                resolve(&mut rows, &mut pending, &format!("block:{milestone_id}"), event.seq, ResolutionState::Resolved);
                continue;
            }
            EventKind::ValidationFinding { milestone_id, run_id, finding } => {
                let category = if run_id == crate::reducer::ENGINE_RUN_ID && finding.class == "out-of-contract-write" {
                    Category::HumanPolicyBoundary
                } else if runs.get(run_id.as_str()).is_some_and(|(_, _, role)| matches!(role, crate::types::Role::ValidatorScrutiny | crate::types::Role::ValidatorFunctional)) {
                    Category::ReportedDefect
                } else {
                    // In particular, a native command-assertion failure does
                    // not distinguish a broken implementation from setup.
                    Category::Unknown
                };
                let mut row = Observation::new(event, category, &format!("{}: {}", finding.subject, finding.evidence));
                row.milestone_id = Some(milestone_id.clone());
                row.run_id = Some(run_id.clone());
                row.stage = Some(protocol::Stage::MilestoneValidation);
                row
            }
            EventKind::ValidatorTamper { milestone_id, run_id, .. } => {
                let mut row = Observation::new(event, Category::HumanPolicyBoundary, "Validator isolation tripwire recorded checkout or metadata drift.");
                row.milestone_id = Some(milestone_id.clone());
                row.run_id = Some(run_id.clone());
                row
            }
            EventKind::WorkerCompleted { run_id, result, .. } if *result != RunResult::Pass => {
                let mut row = Observation::new(event, Category::Unknown, "A non-passing worker result does not identify its cause.");
                row.run_id = Some(run_id.clone());
                row
            }
            EventKind::FeatureFailed { feature_id, reason, .. } => {
                let mut row = Observation::new(event, Category::Unknown, reason);
                row.feature_id = Some(feature_id.clone());
                row
            }
            EventKind::GateResult { verdict: crate::gate::GateVerdict::Fail, surface, gate, .. } => {
                let mut row = Observation::new(event, Category::Unknown, &format!("Non-passing gate {gate}; this event does not distinguish a defect from unavailable evidence or execution failure."));
                row.stage = Some(match surface {
                    crate::gate::GateSurface::Approval => protocol::Stage::PlanApproval,
                    crate::gate::GateSurface::FinalGate => protocol::Stage::FinalGate,
                });
                row
            }
            EventKind::GrantRequested { milestone_id, command, .. } => {
                key = Some("grant".into());
                let mut row = Observation::new(event, Category::HumanPolicyBoundary, command);
                row.milestone_id = Some(milestone_id.clone());
                row
            }
            EventKind::GrantApproved { .. } => {
                resolve(&mut rows, &mut pending, "grant", event.seq, ResolutionState::Resolved);
                continue;
            }
            EventKind::GrantDenied { reason, .. } => {
                resolve(&mut rows, &mut pending, "grant", event.seq, ResolutionState::Resolved);
                Observation::new(event, Category::HumanPolicyBoundary, reason)
            }
            EventKind::PermissionRequested { request } => {
                permissions.insert(request.proposal.id.as_str(), request.binding.run_id.as_str());
                key = Some(format!("permission:{}", request.proposal.id));
                let mut row = Observation::new(event, Category::HumanPolicyBoundary, request.proposal.prohibition.as_deref().unwrap_or("One-call permission requested; no decision recorded yet."));
                row.run_id = Some(request.binding.run_id.clone());
                row.permission_request_id = Some(request.proposal.id.clone());
                row.stage = Some(protocol::Stage::CommandPermission);
                row.deadline = Some(request.proposal.deadline);
                row
            }
            EventKind::PermissionResolved { resolution } => {
                resolve(&mut rows, &mut pending, &format!("permission:{}", resolution.request_id), event.seq, ResolutionState::Resolved);
                let mut row = Observation::new(event, Category::HumanPolicyBoundary, &resolution.reason);
                row.actor = Some(resolution.actor.clone());
                row.permission_request_id = Some(resolution.request_id.clone());
                row.run_id = permissions.get(resolution.request_id.as_str()).map(|s| (*s).into());
                row.stage = Some(protocol::Stage::CommandPermission);
                row
            }
            EventKind::PermissionClosed { request_id, .. } => {
                resolve(&mut rows, &mut pending, &format!("permission:{request_id}"), event.seq, ResolutionState::Closed);
                continue;
            }
            EventKind::PlanRevisionProposed { instructions, .. } => {
                key = Some("revision".into());
                Observation::new(event, Category::HumanPolicyBoundary, instructions)
            }
            EventKind::PlanRevised { .. } | EventKind::PlanRevisionRejected { .. } => {
                resolve(&mut rows, &mut pending, "revision", event.seq, ResolutionState::Resolved);
                continue;
            }
            EventKind::QuestionOpened { question_id, text, run_id, feature_id, milestone_id, .. } => {
                key = Some(format!("question:{question_id}"));
                let mut row = Observation::new(event, Category::HumanPolicyBoundary, text);
                row.run_id = run_id.clone(); row.feature_id = feature_id.clone(); row.milestone_id = milestone_id.clone();
                row
            }
            EventKind::QuestionAnswered { question_id, .. } | EventKind::QuestionCleared { question_id, .. } => {
                resolve(&mut rows, &mut pending, &format!("question:{question_id}"), event.seq,
                    if matches!(event.kind, EventKind::QuestionAnswered { .. }) { ResolutionState::Resolved } else { ResolutionState::Closed });
                continue;
            }
            EventKind::GateEvaluationRequested { evaluation } => {
                attempts.insert(evaluation.request.params.attempt_id.as_str(), evaluation.as_ref());
                continue;
            }
            EventKind::GateEvaluationFinished { evaluation } => {
                let request = attempts.get(evaluation.attempt_id.as_str());
                let (category, detail) = match &evaluation.outcome {
                    lifecycle::Outcome::Error { message } => (Category::Unknown, message.as_str()),
                    lifecycle::Outcome::Evaluated { result, .. } if result.status == protocol::Status::Escalate => (Category::HumanPolicyBoundary, result.rationale.as_str()),
                    lifecycle::Outcome::Evaluated { result, .. } if result.verdict == Some(protocol::Verdict::Fail) => {
                        let defect = request.is_some_and(|r| r.policy.kind == crate::pack::evaluator::Kind::Judgment)
                            && result.findings.as_ref().is_some_and(|f| !f.is_empty());
                        (if defect { Category::ReportedDefect } else { Category::Unknown }, result.rationale.as_str())
                    }
                    _ => continue,
                };
                let mut row = Observation::new(event, category, detail);
                row.attempt_id = Some(evaluation.attempt_id.as_str().into());
                row.stage = request.map(|r| r.request.params.stage);
                row
            }
            EventKind::GateResolutionRecorded { resolution } if resolution.disposition != lifecycle::Disposition::Proceed => {
                if resolution.disposition == lifecycle::Disposition::RequireHuman {
                    key = Some(format!("gate:{}", resolution.attempt_id.as_str()));
                }
                let mut row = Observation::new(event, Category::HumanPolicyBoundary, &resolution.rationale);
                row.attempt_id = Some(resolution.attempt_id.as_str().into());
                row.stage = attempts.get(resolution.attempt_id.as_str()).map(|r| r.request.params.stage);
                row.actor = resolution.consent.as_ref().map(|c| c.actor.clone());
                row
            }
            EventKind::GateResolutionConsumed { consumption } => {
                resolve(&mut rows, &mut pending, &format!("gate:{}", consumption.attempt_id.as_str()), event.seq, ResolutionState::Resolved);
                continue;
            }
            EventKind::GateEvaluationClosed { attempt_id, .. } => {
                resolve(&mut rows, &mut pending, &format!("gate:{}", attempt_id.as_str()), event.seq, ResolutionState::Closed);
                continue;
            }
            EventKind::MissionPaused {} | EventKind::UserMessage { interrupt: true, .. } => Observation::new(event, Category::Interrupted, "An explicit pause or interrupt was recorded; its underlying cause is not inferred."),
            EventKind::MissionAbandoned { reason } => Observation::new(event, Category::Cancelled, reason),
            EventKind::MissionFailed { reason } => Observation::new(event, Category::Unknown, reason),
            EventKind::MissionCompleted {} => {
                for index in std::mem::take(&mut pending).into_values() {
                    rows[index].state = ResolutionState::Closed;
                    rows[index].resolution_seq = Some(event.seq);
                }
                continue;
            }
            _ => continue,
        };
        if let Some((feature, milestone, _)) = row.run_id.as_deref().and_then(|id| runs.get(id)) {
            row.feature_id = row.feature_id.or_else(|| feature.clone());
            row.milestone_id = row.milestone_id.or_else(|| milestone.clone());
        }
        if let Some(key) = key {
            row.state = ResolutionState::Unresolved;
            if let Some(previous) = pending.insert(key, rows.len()) {
                rows[previous].state = ResolutionState::Superseded;
                rows[previous].resolution_seq = Some(event.seq);
            }
        }
        if matches!(
            event.kind,
            EventKind::MissionFailed { .. } | EventKind::MissionAbandoned { .. }
        ) {
            for index in std::mem::take(&mut pending).into_values() {
                rows[index].state = ResolutionState::Closed;
                rows[index].resolution_seq = Some(event.seq);
            }
        }
        rows.push(row);
    }
    if let Some(latest) = events.last() {
        for row in &mut rows {
            if row.state == ResolutionState::Unresolved
                && row.deadline.is_some_and(|d| d <= latest.ts)
            {
                row.state = ResolutionState::Expired;
            }
        }
    }
    if status.is_none() {
        for row in &mut rows {
            row.category = Category::Unknown;
        }
        if let Some(event) = events.last() {
            rows.push(Observation::new(event, Category::Unknown, "The existing reducer cannot establish mission state from this log; cause attribution is untrusted."));
        }
    }
    MissionReasons {
        mapping_version: MAPPING_VERSION,
        mission_id: mission_id.into(),
        task_class: task_class.unwrap_or(super::UNCLASSIFIED_TASK_CLASS).into(),
        current_status: status,
        latest_event_at: events.last().map(|e| e.ts),
        latest_event_seq: events.last().map(|e| e.seq),
        observations: rows,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Count {
    pub category: Category,
    pub missions: u64,
    pub observations: u64,
    pub share: Option<f64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassCounts {
    pub task_class: String,
    pub missions: u64,
    pub mixed_missions: u64,
    pub unresolved_missions: u64,
    pub counts: Vec<Count>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub mapping_version: u32,
    pub from: Option<DateTime<Utc>>,
    pub through: Option<DateTime<Utc>>,
    pub selection: String,
    pub missions: Vec<MissionReasons>,
    pub task_classes: Vec<ClassCounts>,
    pub unavailable_logs: Vec<String>,
}

/// Cohort by latest recorded activity; retain the full history of each selected
/// mission so a repair never erases an earlier cause or inflates the denominator.
pub fn report(
    mut missions: Vec<MissionReasons>,
    unavailable_logs: Vec<String>,
    window: Option<(u64, DateTime<Utc>)>,
) -> anyhow::Result<Report> {
    let (from, through) = match window {
        Some((days, now)) => {
            anyhow::ensure!(
                days <= super::MAX_MERGED_CHANGE_WINDOW_DAYS,
                "outcome reason window exceeds {} days",
                super::MAX_MERGED_CHANGE_WINDOW_DAYS
            );
            (
                Some(
                    now.checked_sub_signed(chrono::Duration::days(days as i64))
                        .ok_or_else(|| anyhow::anyhow!("outcome reason window underflow"))?,
                ),
                Some(now),
            )
        }
        None => (None, None),
    };
    missions.retain(|m| {
        window.is_none()
            || m.latest_event_at
                .is_some_and(|at| Some(at) >= from && Some(at) <= through)
    });
    missions.sort_by(|a, b| a.mission_id.cmp(&b.mission_id));
    let mut classes: BTreeMap<String, ClassCounts> = BTreeMap::new();
    for mission in &mut missions {
        if let Some(now) = through {
            for row in &mut mission.observations {
                if row.state == ResolutionState::Unresolved
                    && row.deadline.is_some_and(|d| d <= now)
                {
                    row.state = ResolutionState::Expired;
                }
            }
        }
        let class = classes
            .entry(mission.task_class.clone())
            .or_insert_with(|| ClassCounts {
                task_class: mission.task_class.clone(),
                missions: 0,
                mixed_missions: 0,
                unresolved_missions: 0,
                counts: vec![],
            });
        class.missions += 1;
        let categories: BTreeSet<_> = mission.observations.iter().map(|r| r.category).collect();
        class.mixed_missions += u64::from(categories.len() > 1);
        class.unresolved_missions += u64::from(
            mission
                .observations
                .iter()
                .any(|r| r.state == ResolutionState::Unresolved),
        );
        for category in categories {
            let index = class
                .counts
                .iter()
                .position(|c| c.category == category)
                .unwrap_or_else(|| {
                    class.counts.push(Count {
                        category,
                        missions: 0,
                        observations: 0,
                        share: None,
                    });
                    class.counts.len() - 1
                });
            class.counts[index].missions += 1;
            class.counts[index].observations += mission
                .observations
                .iter()
                .filter(|r| r.category == category)
                .count() as u64;
        }
    }
    let mut task_classes: Vec<_> = classes.into_values().collect();
    task_classes.sort_by_key(|c| {
        (
            c.task_class == super::UNCLASSIFIED_TASK_CLASS,
            c.task_class.clone(),
        )
    });
    for class in &mut task_classes {
        class.counts.sort_by_key(|c| c.category);
        for count in &mut class.counts {
            count.share = Some(count.missions as f64 / class.missions as f64);
        }
    }
    Ok(Report { mapping_version: MAPPING_VERSION, from, through,
        selection: "Missions whose latest recorded event is within the inclusive window; counts cover their full recorded history. Categories overlap. Unavailable logs are excluded; completion is not release or deployment.".into(),
        missions, task_classes, unavailable_logs })
}
