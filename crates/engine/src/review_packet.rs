//! Read-only human review, derived from the sealed log and retained evidence.
//! This is deliberately not part of MissionState or any validator input. A
//! packet describes an observation, never grants consent or authorizes reuse.
use crate::error::{EngineError, Result};
use crate::events::{Event, EventKind};
use crate::gate_evaluation::{
    authority::Authority,
    input_builder::ObservedCheck,
    lifecycle::{Outcome, RetainedArtifact},
    protocol::{Digest, Status, Subject, Verdict},
    snapshot::{Identity, SourceSnapshot},
};
use crate::git_ops::GitRepo;
use crate::paths::MissionPaths;
use crate::types::{MissionState, MissionStatus, Plan, WorkerIsolation};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPacket {
    pub mission_id: String,
    pub observed_at: DateTime<Utc>,
    pub through_seq: u64,
    pub approved_plan: Option<Plan>,
    pub approval_seq: Option<u64>,
    pub candidate: Option<Candidate>,
    pub committed_change: Option<CommittedChange>,
    pub decisions: Vec<Entry>,
    pub evaluations: Vec<Evaluation>,
    pub history: Vec<Entry>,
    pub unknowns: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommittedChange {
    pub base: String,
    pub head: String,
    pub diff_stat: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub identity: Identity,
    /// Committed changes; working-tree changes are identified separately.
    pub diff_stat: String,
    pub working_tree: String,
    pub excluded_paths: Vec<String>,
    pub scope_deviations: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub seq: u64,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceStatus {
    CurrentPass,
    Failed,
    Escalated,
    Historical,
    Unavailable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Evaluation {
    pub attempt_id: String,
    pub gate_id: String,
    pub stage: crate::gate_evaluation::protocol::Stage,
    pub kind: crate::pack::evaluator::Kind,
    pub enforcement: crate::pack::evaluator::Enforcement,
    pub requested_seq: u64,
    pub result_seq: Option<u64>,
    pub status: EvidenceStatus,
    pub detail: String,
    pub subject: Subject,
    pub binding: crate::gate_evaluation::protocol::Binding,
    pub checks: Vec<Check>,
    pub findings: Vec<crate::gate_evaluation::protocol::Finding>,
    pub artifacts: Vec<Artifact>,
    pub preceding_attempt: Option<String>,
    pub changes_since_review: String,
    pub paths_changed_after_review: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    pub id: String,
    pub command: String,
    pub status: EvidenceStatus,
    pub receipt: Option<ObservedCheck>,
    pub artifact: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub path: String,
    pub digest: Digest,
    pub available: bool,
}

/// A live operator read. Never writes a packet, creates a worktree, reads a
/// transcript, or invokes a provider. Missing worktrees remain unavailable.
pub fn compute_review_packet(repo_root: &Path, mission_id: &str) -> Result<ReviewPacket> {
    if !MissionPaths::is_safe_id(mission_id) {
        return Err(EngineError::InvalidState("invalid mission id".into()));
    }
    let paths = MissionPaths::new(repo_root, mission_id);
    paths.require_no_follow()?;
    let events = crate::event_log::EventLog::read_events(&paths.events_file())?;
    let state = crate::reducer::fold(&events)?;
    let root = match state.config.isolation() {
        WorkerIsolation::Worktree => {
            crate::orchestrator::mission_worktree_path(repo_root, mission_id)
        }
        WorkerIsolation::Checkout => repo_root.to_path_buf(),
    };
    let repo = GitRepo::open(&root).ok().filter(|repo| {
        repo.current_branch().ok().as_deref() == Some(state.mission.mission_branch.as_str())
    });
    let mut packet = project(
        &paths.mission_dir(),
        &state,
        &events,
        repo.as_ref(),
        Utc::now(),
    )?;
    // Delivery can remove the execution worktree. Git still proves the
    // committed diff, but cannot establish the missing working-tree bytes.
    if let Ok(repository) = GitRepo::open(repo_root) {
        if packet.candidate.is_none() {
            if let Some((base, head)) = state
                .mission
                .base_sha
                .as_ref()
                .zip(repository.rev_parse(&state.mission.mission_branch).ok())
            {
                if let Ok(diff_stat) = repository.diff_stat(base, &head) {
                    packet.committed_change = Some(CommittedChange {
                        base: base.clone(),
                        head,
                        diff_stat,
                    });
                }
            }
        }
        if state.mission.status == MissionStatus::Complete
            && crate::merged::merged_bit(&repository, &state.mission) == Some(true)
        {
            packet.decisions.retain(|d| d.title != "Mission decision");
        }
    }
    Ok(packet)
}

/// Shared by live CLI/API reads and the point-in-time mission report. The
/// caller supplies only the mission's execution tree, never another checkout.
pub fn project(
    mission_dir: &Path,
    state: &MissionState,
    events: &[Event],
    repo: Option<&GitRepo>,
    now: DateTime<Utc>,
) -> Result<ReviewPacket> {
    let approved = events.iter().rev().find_map(|e| match &e.kind {
        EventKind::PlanApproved { plan, .. } | EventKind::PlanRevised { plan, .. } => {
            Some((e.seq, plan))
        }
        _ => None,
    });
    let mut packet = ReviewPacket {
        mission_id: state.mission.id.clone(),
        observed_at: now,
        through_seq: state.last_seq,
        approved_plan: approved.map(|(_, p)| p.clone()),
        approval_seq: approved.map(|(s, _)| s),
        candidate: None,
        committed_change: None,
        decisions: vec![],
        evaluations: vec![],
        history: vec![],
        unknowns: vec![],
    };
    if approved.is_none() {
        packet
            .unknowns
            .push("Approved scope and criteria unavailable: no approved plan in the log.".into());
    }
    let authority = repo.and_then(|r| Authority::from_events(r, state, events).ok());
    let mut snapshots: BTreeMap<String, (Identity, Vec<u8>)> = BTreeMap::new();
    if let Some((repo, base)) = repo.zip(state.mission.base_sha.as_deref()) {
        match SourceSnapshot::capture(repo, base) {
            Ok(snapshot) => {
                snapshots.insert(
                    snapshot.identity.base.clone(),
                    (snapshot.identity.clone(), snapshot.inventory.clone()),
                );
                packet.candidate = Some(Candidate {
                    diff_stat: repo.diff_stat(base, &snapshot.identity.head)?,
                    working_tree: repo.porcelain_status()?,
                    identity: snapshot.identity,
                    excluded_paths: snapshot.excluded_paths,
                    scope_deviations: scope_deviations(repo, base, &state.mission.touch_set)?,
                });
            }
            Err(_) => packet.unknowns.push(
                "Candidate source snapshot unavailable; freshness cannot be established.".into(),
            ),
        }
    } else {
        packet.unknowns.push("Candidate unavailable: no pinned base or active mission checkout. Retained evidence is historical.".into());
    }
    let mut records: Vec<_> = state.gate_evaluations.values().collect();
    records.sort_by_key(|r| r.requested_seq);
    let mut budget = 128 * 1024 * 1024usize;
    // Spend the bounded read budget on the newest evidence first.
    for (index, record) in records.iter().enumerate().rev() {
        let p = &record.requested.request.params;
        let mut bytes = BTreeMap::new();
        let artifacts: Vec<_> = record
            .requested
            .retained_inputs
            .iter()
            .chain(record.finished.iter().flat_map(|f| &f.artifacts))
            .map(|artifact| {
                let read = retained_bytes(mission_dir, artifact, &mut budget);
                let available = read.is_some();
                if let Some(read) = read {
                    // Only these small, structured inputs inform the human projection.
                    let leaf = artifact.path.as_str().rsplit('/').next().unwrap_or("");
                    if matches!(
                        leaf,
                        "source-identity" | "snapshot-inventory" | "check-requirements"
                    ) || leaf.starts_with("check-")
                    {
                        // A scrubbed transformation may not be used to reconstruct raw bindings.
                        if Digest::of(&read) == artifact.raw_digest {
                            bytes.insert(
                                leaf.to_string(),
                                (artifact.path.as_str().to_string(), read),
                            );
                        }
                    }
                }
                Artifact {
                    path: artifact.path.as_str().to_string(),
                    digest: artifact.retained_digest.clone(),
                    available,
                }
            })
            .collect();
        let retained_identity = bytes
            .get("source-identity")
            .and_then(|(_, b)| serde_json::from_slice::<Identity>(b).ok());
        if let Some(identity) = &retained_identity {
            if !snapshots.contains_key(&identity.base) && snapshots.len() < 32 {
                if let Some(snapshot) =
                    repo.and_then(|r| SourceSnapshot::capture(r, &identity.base).ok())
                {
                    snapshots.insert(
                        identity.base.clone(),
                        (snapshot.identity, snapshot.inventory),
                    );
                }
            }
        }
        let current_source = match &p.subject {
            Subject::Plan {
                revision,
                base_commit,
                ..
            } => {
                *revision == u64::from(state.latest_plan_revision)
                    && state.mission.base_sha.as_deref() == Some(base_commit.value.as_str())
            }
            // A permission can expire or be answered independently of candidate bytes.
            Subject::Invocation { .. } => record
                .requested
                .permission_request_id
                .as_ref()
                .and_then(|id| state.permissions.get(id))
                .is_some_and(|r| r.pending(now)),
            _ => retained_identity.as_ref().is_some_and(|id| {
                snapshots.get(&id.base).map(|(identity, _)| identity) == Some(id)
                    && subject_snapshot(&p.subject) == Some(id.inventory_digest.clone())
            }),
        };
        let current_integration = match &p.subject {
            Subject::Integration {
                live_base_commit,
                candidate_commit,
                ..
            } => repo.is_some_and(|r| {
                r.rev_parse(&state.mission.base_branch).ok().as_deref()
                    == Some(live_base_commit.value.as_str())
                    && r.rev_parse(&state.mission.mission_branch).ok().as_deref()
                        == Some(candidate_commit.value.as_str())
            }),
            _ => true,
        };
        let paths_changed_after_review = retained_identity.as_ref().and_then(|identity| {
            let (_, before) = bytes.get("snapshot-inventory")?;
            let (_, after) = snapshots.get(&identity.base)?;
            changed_snapshot_paths(before, after)
        });
        let workspace = match &p.subject {
            Subject::Invocation { .. } => record
                .requested
                .permission_request_id
                .as_ref()
                .and_then(|id| state.permissions.get(id))
                .map(|r| {
                    crate::gate_evaluation::lifecycle::workspace_id(&r.request.binding.workspace)
                }),
            _ => mission_dir.ancestors().nth(3).map(|root| {
                crate::gate_evaluation::lifecycle::workspace_id(&root.to_string_lossy())
            }),
        };
        let superseded = records[index + 1..].iter().any(|r| {
            let newer = &r.requested.request.params;
            newer.stage == p.stage
                && newer.gate_id == p.gate_id
                && same_review_unit(&newer.subject, &p.subject)
        });
        let current = current_source
            && current_integration
            && workspace.as_ref() == Some(&p.binding.workspace_id)
            && !superseded
            && record.closed.is_none()
            && record.consumed.is_none()
            && DateTime::parse_from_rfc3339(&p.deadline).is_ok_and(|d| d > now)
            && authority.as_ref().is_some_and(|a| {
                Digest::of(&a.plan_bytes) == p.binding.plan_digest
                    && a.policy == record.requested.policy.mission_policy_digest
                    && a.registrations
                        .iter()
                        .any(|r| r.digest() == p.binding.registration_digest)
            });
        let checks_projection = project_checks(&bytes, current);
        let all_available = !artifacts.is_empty() && artifacts.iter().all(|a| a.available);
        let requirements_available = checks_projection.is_some();
        let checks = checks_projection.unwrap_or_default();
        let checks_pass = requirements_available
            && checks
                .iter()
                .all(|c| matches!(c.status, EvidenceStatus::CurrentPass));
        let (status, mut detail, findings) = match record.finished.as_ref().map(|f| &f.outcome) {
            Some(Outcome::Error { message }) => (EvidenceStatus::Failed, message.clone(), vec![]),
            Some(Outcome::Evaluated { result, .. }) => {
                let status = if result.status == Status::Escalate {
                    EvidenceStatus::Escalated
                } else if result.verdict == Some(Verdict::Fail) {
                    EvidenceStatus::Failed
                } else if !all_available || !requirements_available {
                    EvidenceStatus::Unavailable
                } else if !current {
                    EvidenceStatus::Historical
                } else if !checks_pass
                    || !record.requested.policy.mechanical_prerequisites_passed
                    || !record
                        .finished
                        .as_ref()
                        .is_some_and(|f| f.cleanup_confirmed && f.exit_code == Some(0))
                {
                    EvidenceStatus::Failed
                } else {
                    EvidenceStatus::CurrentPass
                };
                (
                    status,
                    result.rationale.clone(),
                    result.findings.clone().unwrap_or_default(),
                )
            }
            None => (
                EvidenceStatus::Unavailable,
                "Evaluation has no finished result.".into(),
                vec![],
            ),
        };
        if !current_source {
            detail.push_str("\nThe live subject differs or cannot be observed.");
        }
        if !current && current_source {
            detail.push_str("\nThe attempt is closed, consumed, expired, superseded, or its current approval/workspace/policy binding cannot be established.");
        }
        if !all_available {
            detail.push_str(
                "\nRetained evidence is missing, changed, unreadable or exceeds the read budget.",
            );
        }
        let previous = records[..index].iter().rev().find(|r| {
            let prior = &r.requested.request.params;
            prior.gate_id == p.gate_id
                && prior.stage == p.stage
                && same_review_unit(&prior.subject, &p.subject)
        });
        let changes = previous.map(|r| {
            let prior = &r.requested.request.params;
            format!("Since event #{}: subject {}; plan {}; policy {}; registration {}. Review events #{}–#{} for intervening decisions and repairs.",
                r.requested_seq, changed(prior.binding.subject_digest != p.binding.subject_digest),
                changed(prior.binding.plan_digest != p.binding.plan_digest), changed(prior.binding.policy_digest != p.binding.policy_digest),
                changed(prior.binding.registration_digest != p.binding.registration_digest), r.requested_seq + 1, record.requested_seq)
        }).unwrap_or_else(|| "No preceding evaluation for this gate and stage is recorded.".into());
        packet.evaluations.push(Evaluation {
            attempt_id: p.attempt_id.as_str().into(),
            gate_id: p.gate_id.as_str().into(),
            stage: p.stage,
            kind: record.requested.policy.kind,
            enforcement: record.requested.policy.enforcement,
            requested_seq: record.requested_seq,
            result_seq: events.iter().find_map(|e| match &e.kind {
                EventKind::GateEvaluationFinished { evaluation }
                    if evaluation.attempt_id == p.attempt_id =>
                {
                    Some(e.seq)
                }
                _ => None,
            }),
            status,
            detail,
            subject: p.subject.clone(),
            binding: p.binding.clone(),
            checks,
            findings,
            artifacts,
            preceding_attempt: previous
                .map(|r| r.requested.request.params.attempt_id.as_str().into()),
            changes_since_review: changes,
            paths_changed_after_review,
        });
    }
    packet.evaluations.reverse();
    if records.is_empty() {
        packet.unknowns.push("No source-bound external check receipts recorded. Native gate outcomes below are recorded results, not proof of a current pass.".into());
    }
    pending_decisions(&mut packet, state, events, now);
    for event in events {
        let entry = match &event.kind {
            EventKind::GateResult { gate, surface, verdict, artefact_ref, artefact_detail, .. } => {
                let availability = match crate::gate_results::resolve_artefact(mission_dir, artefact_ref) {
                    crate::gate_results::ArtefactResolution::Resolved { .. } => "artifact present (legacy reference, no digest verification)",
                    crate::gate_results::ArtefactResolution::Inline => "inline evidence in this event",
                    crate::gate_results::ArtefactResolution::Unresolved { .. } => "artifact unavailable",
                };
                Some((format!("Native check {gate} ({surface:?}) — recorded {verdict:?}"),
                    format!("{artefact_ref}\n{availability}\n{}\nCurrent candidate binding unavailable for this record.", artefact_detail.as_deref().unwrap_or(""))))
            },
            EventKind::ValidationFinding { run_id, finding, .. } => Some((
                format!("Finding: {} — {}", finding.subject, finding.severity),
                format!("{}\nClass: {}. Run: {}. Resolution is not inferred; inspect later checks and explicit waivers.", finding.evidence, finding.class, run_id))),
            EventKind::StandardsWaiverApproved { .. } => Some(("Explicit standards waiver (recorded; applicability must be rechecked)".into(), serde_json::to_string_pretty(&event.kind)?)),
            EventKind::OrchestratorDecision { summary, detail } if summary.starts_with("waived") => Some(("Recorded orchestrator waiver (not human consent)".into(), detail.as_ref().unwrap_or(summary).clone())),
            EventKind::GrantApproved { kind, command } => Some((format!("Approved capability extension: {kind:?}"), command.clone())),
            _ => None,
        };
        if let Some((title, detail)) = entry {
            packet.history.push(Entry {
                seq: event.seq,
                title,
                detail,
            });
        }
    }
    packet.unknowns.push("This observation does not authorize work. Check environments are the recorded environments; approvals, deadlines and bindings are rechecked by the existing decision path.".into());
    Ok(packet)
}

fn changed(value: bool) -> &'static str {
    if value {
        "changed"
    } else {
        "unchanged"
    }
}

fn same_review_unit(a: &Subject, b: &Subject) -> bool {
    match (a, b) {
        (
            Subject::Milestone {
                milestone_id: a, ..
            },
            Subject::Milestone {
                milestone_id: b, ..
            },
        ) => a == b,
        // Permission attempts for another tool call/session cannot supersede
        // one another, even when they share a gate and stage.
        (Subject::Invocation { .. }, Subject::Invocation { .. }) => a == b,
        (Subject::Plan { .. }, Subject::Plan { .. })
        | (Subject::Deliverable { .. }, Subject::Deliverable { .. })
        | (Subject::Integration { .. }, Subject::Integration { .. }) => true,
        _ => false,
    }
}

fn changed_snapshot_paths(before: &[u8], after: &[u8]) -> Option<Vec<String>> {
    use crate::gate_evaluation::evidence::{SnapshotEntry, SnapshotInventory};
    fn entries(bytes: &[u8]) -> Option<BTreeMap<String, (Option<Digest>, bool)>> {
        let inventory: SnapshotInventory = serde_json::from_slice(bytes).ok()?;
        Some(
            inventory
                .entries
                .into_iter()
                .map(|entry| match entry {
                    SnapshotEntry::File {
                        path,
                        digest,
                        executable,
                        ..
                    } => (path.as_str().to_string(), (Some(digest), executable)),
                    SnapshotEntry::Deleted { path } => (path.as_str().to_string(), (None, false)),
                })
                .collect(),
        )
    }
    let before = entries(before)?;
    let after = entries(after)?;
    let paths: std::collections::BTreeSet<_> = before.keys().chain(after.keys()).collect();
    Some(
        paths
            .into_iter()
            .filter(|p| before.get(*p) != after.get(*p))
            .cloned()
            .collect(),
    )
}
fn scope_deviations(repo: &GitRepo, base: &str, touch_set: &[String]) -> Result<Vec<String>> {
    if touch_set.is_empty() {
        return Ok(vec![]);
    }
    repo.review_changed_paths(base)?
        .into_iter()
        .filter(|p| !crate::gate_evaluation::snapshot::excluded(p))
        .filter_map(
            |path| match crate::contract_sweep::touch_set_includes(touch_set, &path) {
                Ok(true) => None,
                Ok(false) => Some(Ok(path)),
                Err(e) => Some(Err(EngineError::InvalidState(format!(
                    "invalid approved touch set: {e}"
                )))),
            },
        )
        .collect()
}
fn subject_snapshot(subject: &Subject) -> Option<Digest> {
    match subject {
        Subject::Milestone {
            snapshot_digest, ..
        }
        | Subject::Deliverable {
            snapshot_digest, ..
        }
        | Subject::Integration {
            snapshot_digest, ..
        } => Some(snapshot_digest.clone()),
        _ => None,
    }
}

/// Bounded, no-follow and digest-checked reads; never follow an artifact's
/// contents as another path. The aggregate budget applies across the packet.
fn retained_bytes(root: &Path, artifact: &RetainedArtifact, budget: &mut usize) -> Option<Vec<u8>> {
    let len = usize::try_from(artifact.retained_bytes).ok()?;
    if len > *budget || len > 64 * 1024 * 1024 {
        return None;
    }
    *budget -= len;
    let file = crate::paths::open_read_nofollow(&root.join(artifact.path.as_str())).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() != artifact.retained_bytes {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return None;
        }
    }
    let mut bytes = Vec::new();
    file.take(artifact.retained_bytes + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() == len && Digest::of(&bytes) == artifact.retained_digest).then_some(bytes)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Requirements {
    checked_content: Digest,
    environment: Digest,
    required: Vec<Required>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Required {
    id: String,
    command: String,
    require_assertions: bool,
}
#[derive(Deserialize)]
struct Receipt {
    receipt: ObservedCheck,
}

fn project_checks(
    bytes: &BTreeMap<String, (String, Vec<u8>)>,
    current: bool,
) -> Option<Vec<Check>> {
    let requirements = bytes
        .get("check-requirements")
        .and_then(|(_, b)| serde_json::from_slice::<Requirements>(b).ok());
    let requirements = requirements?;
    let receipts: Vec<_> = bytes
        .values()
        .filter_map(|(path, b)| {
            serde_json::from_slice::<Receipt>(b)
                .ok()
                .map(|r| (path, r.receipt))
        })
        .collect();
    Some(
        requirements
            .required
            .into_iter()
            .map(|required| {
                let latest = receipts
                    .iter()
                    .filter(|(_, r)| r.check_id.as_str() == required.id)
                    .max_by_key(|(_, r)| r.sequence);
                let status = match latest {
                    None => EvidenceStatus::Unavailable,
                    Some((_, r))
                        if r.command != required.command
                            || r.checked_content != requirements.checked_content
                            || r.environment != requirements.environment
                            || !current =>
                    {
                        EvidenceStatus::Historical
                    }
                    Some((_, r))
                        if r.exit_code != Some(0)
                            || r.assertions_executed == Some(0)
                            || (required.require_assertions && r.assertions_executed.is_none()) =>
                    {
                        EvidenceStatus::Failed
                    }
                    _ => EvidenceStatus::CurrentPass,
                };
                Check {
                    id: required.id,
                    command: required.command,
                    status,
                    receipt: latest.map(|(_, r)| r.clone()),
                    artifact: latest.map(|(p, _)| (*p).clone()),
                }
            })
            .collect(),
    )
}

fn pending_decisions(
    packet: &mut ReviewPacket,
    state: &MissionState,
    events: &[Event],
    now: DateTime<Utc>,
) {
    let mut seen = std::collections::BTreeSet::new();
    for event in events.iter().rev() {
        let decision = match &event.kind {
            EventKind::PlanRevisionProposed { revision, .. } if state.pending_revision.as_ref().is_some_and(|p| p.revision == *revision) => Some((format!("Plan revision {revision}"), "Approve or reject the proposed revision through the existing revision control.".into())),
            EventKind::GrantRequested { kind, command, milestone_id } if state.pending_grant_request.as_ref().is_some_and(|p| p.kind == *kind && p.command == *command && p.milestone_id == *milestone_id) => Some((format!("Capability grant: {kind:?}"), format!("{milestone_id}: {command}. Approve or deny this exact request in the grant control."))),
            EventKind::PermissionRequested { request } if state.permissions.get(&request.proposal.id).is_some_and(|r| r.pending(now)) => Some((format!("Permission {}", request.proposal.id), format!("Binding {}. Deadline {}. Answer this request in the permission control.", request.binding_digest, request.proposal.deadline))),
            EventKind::QuestionOpened { question_id, text, .. } if state.pending_questions.iter().any(|q| q.question_id == *question_id) => Some((format!("Question {question_id}"), text.clone())),
            EventKind::MilestoneBlocked { milestone_id, reason, block_context }
                if state.mission.milestones.iter().any(|m| m.id == *milestone_id && m.status == crate::types::MilestoneStatus::Blocked) =>
                Some((format!("Blocked milestone {milestone_id}"), format!("{reason}\nRecorded cause: {}. Inspect this block before using the existing unblock or abandon control.", serde_json::to_string(block_context).unwrap_or_else(|_| "unavailable".into())))),
            EventKind::GateEvaluationRequested { evaluation } => {
                let id = evaluation.request.params.attempt_id.as_str();
                state.gate_evaluations.get(id).filter(|r| r.closed.is_none() && r.consumed.is_none())
                    .and_then(|r| r.resolution.as_ref()).filter(|r| r.disposition != crate::gate_evaluation::lifecycle::Disposition::Proceed)
                    .map(|r| (format!("Gate attempt {id}"), format!("{:?}: {}. Correct the cause and retry the existing stage; this packet has no consent action.", r.disposition, r.rationale)))
            }
            _ => None,
        };
        if let Some((title, detail)) = decision {
            if !seen.insert(title.clone()) {
                continue;
            }
            packet.decisions.push(Entry {
                seq: event.seq,
                title,
                detail,
            });
        }
    }
    packet.decisions.reverse();
    if matches!(
        state.mission.status,
        MissionStatus::Planning | MissionStatus::Complete | MissionStatus::Blocked
    ) {
        let detail = match state.mission.status {
            MissionStatus::Planning => "Review and approve the proposed plan using the existing plan control. No approved scope exists yet.",
            MissionStatus::Complete => "Review the deliverable and current merge readiness. Merge remains a separate human action; completion alone is not merge approval.",
            _ => "Inspect the recorded block, then use the existing retry, unblock or abandon controls. A packet cannot waive a block.",
        };
        packet.decisions.push(Entry {
            seq: state.last_seq,
            title: "Mission decision".into(),
            detail: detail.into(),
        });
    }
}

/// Markdown for the CLI and mission report. Untrusted prose is literal text:
/// it cannot introduce links to the human API or active markup in a renderer.
pub fn render_markdown(packet: &ReviewPacket) -> String {
    fn literal(text: &str) -> String {
        crate::scrub::scrub(text)
            .chars()
            .flat_map(|c| {
                if "\\`*_{}[]<>()#+-.!|".contains(c) {
                    vec!['\\', c]
                } else {
                    vec![c]
                }
            })
            .collect()
    }
    fn name(value: &impl Serialize) -> String {
        serde_json::to_value(value)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    }
    fn entries(out: &mut String, entries: &[Entry]) {
        for entry in entries {
            let _ = writeln!(
                out,
                "\n- **{}** — event #{}\n\n  {}\n",
                literal(&entry.title),
                entry.seq,
                literal(&entry.detail).replace('\n', "\n  ")
            );
        }
    }
    let mut out = format!("\n## Human review packet\n\nMission {} · observed {} · through event #{}. Refresh with `kranz review-packet` before acting.\n", literal(&packet.mission_id), packet.observed_at, packet.through_seq);
    if let Some(plan) = &packet.approved_plan {
        let _ = writeln!(
            out,
            "\n### Approved scope — event #{}\n\n{}\n\nTouch set: {}\n",
            packet.approval_seq.unwrap_or(0),
            literal(&plan.goal),
            literal(&plan.touch_set.join(", "))
        );
        for a in &plan.validation_contract {
            let _ = writeln!(
                out,
                "- {}: {} ({})",
                literal(&a.id),
                literal(&a.statement),
                literal(a.command.as_deref().unwrap_or("judgment"))
            );
        }
        for m in &plan.milestones {
            for f in &m.features {
                for c in &f.validation_criteria {
                    let _ = writeln!(
                        out,
                        "- {} / {}: {}",
                        literal(&m.title),
                        literal(&f.title),
                        literal(c)
                    );
                }
            }
        }
    }
    out.push_str("\n### Actual change\n");
    if let Some(c) = &packet.candidate {
        let _ = writeln!(out, "\nBase: {}\n\nHEAD: {}\n\nSource: {}\n\nCommitted diff:\n\n{}\n\nWorking tree (separate from the committed diff):\n\n{}\n\nExcluded from source evidence: {}\n", c.identity.base, c.identity.head, c.identity.inventory_digest.as_str(), literal(&c.diff_stat), literal(&c.working_tree), literal(&c.excluded_paths.join(", ")));
        for path in &c.scope_deviations {
            let _ = writeln!(
                out,
                "- **Outside the approved touch set:** {}",
                literal(path)
            );
        }
    } else if let Some(c) = &packet.committed_change {
        let _ = writeln!(out, "\nCommitted base: {}\n\nCommitted candidate: {}\n\n{}\n\nWorking-tree source identity unavailable. This diff does not establish check freshness.\n", c.base, c.head, literal(&c.diff_stat));
    } else {
        out.push_str("\nCandidate unavailable.\n");
    }
    out.push_str("\n### Remaining human decisions\n");
    if packet.decisions.is_empty() {
        out.push_str("\nNo pending human decision is recorded at this observation.\n");
    }
    entries(&mut out, &packet.decisions);
    out.push_str("\n### Checks and independent judgment\n\nCurrent pass means the recorded result still binds to the observed source and policy. It is not consent.\n");
    for e in &packet.evaluations {
        let _ = writeln!(out, "\n#### {} / {} — {}\n\n{} · {} · attempt {} · request event #{} · result event {}\n\n{}\n\n{}\n", literal(&e.gate_id), name(&e.stage), name(&e.status), name(&e.kind), name(&e.enforcement), literal(&e.attempt_id), e.requested_seq, e.result_seq.map(|s| format!("#{s}")).unwrap_or_else(|| "unavailable".into()), literal(&e.detail), literal(&e.changes_since_review));
        for check in &e.checks {
            let _ = writeln!(
                out,
                "- {}: {} — {} · receipt {}",
                literal(&check.id),
                literal(&check.command),
                name(&check.status),
                literal(check.artifact.as_deref().unwrap_or("unavailable"))
            );
        }
        match &e.paths_changed_after_review {
            Some(paths) if paths.is_empty() => {
                out.push_str("\nNo selected source paths changed after this review.\n")
            }
            Some(paths) => {
                let _ = writeln!(
                    out,
                    "\nPaths changed after this review: {}\n",
                    literal(&paths.join(", "))
                );
            }
            None => out.push_str(
                "\nPath changes after this review: unavailable (no comparable source inventory).\n",
            ),
        }
        for f in &e.findings {
            let _ = writeln!(
                out,
                "- Finding {} ({}): {}. Resolution is not inferred. Anchors: {}",
                literal(f.id.as_str()),
                name(&f.severity),
                literal(&f.summary),
                literal(&serde_json::to_string(&f.evidence).unwrap_or_default())
            );
        }
        let _ = writeln!(out, "\nRetained artifacts: {} verified, {} unavailable. Full paths and digests are in the JSON packet and evidence bundle.", e.artifacts.iter().filter(|a| a.available).count(), e.artifacts.iter().filter(|a| !a.available).count());
        for a in e.artifacts.iter().filter(|a| !a.available) {
            let _ = writeln!(
                out,
                "- {} — unavailable · {}",
                literal(&a.path),
                a.digest.as_str()
            );
        }
    }
    out.push_str("\n### Recorded checks, findings and explicit exceptions\n");
    entries(&mut out, &packet.history);
    out.push_str("\n### Unknowns and limits\n");
    for unknown in &packet.unknowns {
        let _ = writeln!(out, "\n- {}", literal(unknown));
    }
    out
}
