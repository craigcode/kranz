//! Recover approval authority from the sealed log, never a worker's plan file
//! or a mutable pack directory. Later config changes cannot remove checkers.
use super::{driver::invalid, input_builder::FeatureReceipt, protocol::*};
use crate::{
    error::Result,
    events::{Event, EventKind},
    git_ops::GitRepo,
    pack::evaluator::PinnedRegistration,
    types::*,
};

pub(crate) struct Authority {
    pub plan_bytes: Vec<u8>,
    pub base: String,
    pub registrations: Vec<PinnedRegistration>,
    pub policy: Digest,
}

impl Authority {
    pub fn from_events(repo: &GitRepo, state: &MissionState, events: &[Event]) -> Result<Self> {
        let approval = events
            .iter()
            .position(|e| matches!(e.kind, EventKind::PlanApproved { .. }));
        let Some(approval) = approval else {
            if state.config.pack_dir.is_none() {
                return Ok(Self {
                    plan_bytes: vec![],
                    base: state.mission.base_sha.clone().unwrap_or_default(),
                    registrations: vec![],
                    policy: Digest::of(b"legacy-without-external-gates"),
                });
            }
            return Err(invalid("stage requires an approved plan"));
        };
        let approved = crate::reducer::fold(&events[..=approval])?;
        let base = approved
            .mission
            .base_sha
            .clone()
            .unwrap_or_else(|| approved.mission.base_branch.clone());
        let registrations = PinnedRegistration::configured_at_ref(repo, &approved.config, &base)
            .map_err(invalid)?;
        if !registrations.is_empty() && approved.mission.base_sha.is_none() {
            return Err(invalid(
                "external checks require an immutable approved base",
            ));
        }
        if state.config.pack_dir != approved.config.pack_dir {
            let current = PinnedRegistration::configured_at_ref(repo, &state.config, &base)
                .map_err(invalid)?;
            if !registrations.is_empty() || !current.is_empty() {
                return Err(invalid(
                    "evaluator pack changed after approval; start a newly approved mission",
                ));
            }
        }
        let plan = events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::PlanApproved { plan, .. } | EventKind::PlanRevised { plan, .. } => {
                    Some(plan)
                }
                _ => None,
            })
            .ok_or_else(|| invalid("approved plan is absent"))?;
        let mission = &state.mission;
        let policy = Digest::of(&serde_json::to_vec(&(
            &state.config,
            &mission.base_sha,
            &mission.command_grants,
            &mission.deny_exceptions,
            &mission.egress_grants,
            &mission.touch_set,
            &mission.standards_manifest,
        ))?);
        Ok(Self {
            plan_bytes: serde_json::to_vec(plan)?,
            base,
            registrations,
            policy,
        })
    }

    pub fn applies(&self, stage: Stage) -> bool {
        self.registrations
            .iter()
            .any(|r| r.declaration().stages.contains(&stage))
    }

    pub fn verify(&self, current: &Self) -> Result<()> {
        if self.base != current.base
            || self.plan_bytes != current.plan_bytes
            || self.policy != current.policy
            || self
                .registrations
                .iter()
                .map(|r| r.digest())
                .collect::<Vec<_>>()
                != current
                    .registrations
                    .iter()
                    .map(|r| r.digest())
                    .collect::<Vec<_>>()
        {
            return Err(invalid("approved authority changed while checks ran"));
        }
        Ok(())
    }
}

/// A completed feature plus the independent run that the engine accepted at
/// milestone completion. Worker prose alone cannot supply either receipt.
pub(crate) fn accepted_features(
    repo: &GitRepo,
    state: &MissionState,
    events: &[Event],
) -> Result<Vec<FeatureReceipt>> {
    let mut receipts = Vec::new();
    for milestone in &state.mission.milestones {
        if milestone.status != MilestoneStatus::Complete {
            return Err(invalid("deliverable contains an incomplete milestone"));
        }
        let completed_at = events
            .iter()
            .rev()
            .find_map(|e| match &e.kind {
                EventKind::MilestoneCompleted { milestone_id, .. }
                    if milestone_id == &milestone.id =>
                {
                    Some(e.ts)
                }
                _ => None,
            })
            .ok_or_else(|| invalid("milestone completion receipt is absent"))?;
        for feature in milestone
            .features
            .iter()
            .filter(|f| f.status == FeatureStatus::Complete)
        {
            let worker = feature
                .worker_runs
                .iter()
                .rev()
                .filter_map(|id| state.runs.get(id))
                .find(|r| r.ended_at.is_some() && r.result == Some(RunResult::Pass))
                .ok_or_else(|| invalid("completed feature lacks an accepted worker run"))?;
            let worker_end = worker.ended_at.expect("selected finished run");
            let reviewer = state
                .runs
                .values()
                .filter(|r| {
                    matches!(r.role, Role::ValidatorScrutiny | Role::ValidatorFunctional)
                        && r.milestone_id.as_deref() == Some(milestone.id.as_str())
                        && r.started_at >= worker_end
                        && r.ended_at.is_some_and(|end| end <= completed_at)
                        && r.result == Some(RunResult::Pass)
                        && r.sdk_session_id != worker.sdk_session_id
                })
                .max_by_key(|r| r.ended_at)
                .map(|r| r.id.clone());
            let external = state.gate_evaluations.values().filter(|r| {
                r.requested.policy.kind == crate::pack::evaluator::Kind::Judgment
                    && r.consumed.is_some() && r.requested_at >= worker_end
                    && r.finished_at.is_some_and(|end| end <= completed_at)
                    && matches!(&r.requested.request.params.subject, Subject::Milestone { milestone_id, .. } if milestone_id.as_str() == milestone.id)
                    && matches!(r.finished.as_ref().map(|f| &f.outcome), Some(super::lifecycle::Outcome::Evaluated { result, .. }) if result.verdict == Some(Verdict::Pass))
            }).max_by_key(|r| r.requested_seq).map(|r| r.requested.request.params.attempt_id.as_str().to_string());
            let reviewer = reviewer.or(external).ok_or_else(|| {
                invalid("completed feature lacks an accepted independent validation run")
            })?;
            let commit = feature
                .commits
                .last()
                .ok_or_else(|| invalid("completed feature lacks a commit"))?;
            let commit = commit
                .split_whitespace()
                .next()
                .ok_or_else(|| invalid("empty feature commit"))?;
            let commit_object = super::input_builder::git_object(commit).map_err(invalid)?;
            if !repo.is_ancestor(commit, "HEAD")? {
                return Err(invalid(
                    "accepted feature commit is absent from the deliverable",
                ));
            }
            receipts.push(FeatureReceipt {
                feature_id: Id::try_from(feature.id.clone()).map_err(invalid)?,
                worker_run_id: Id::try_from(worker.id.clone()).map_err(invalid)?,
                candidate_commit: commit_object,
                independent_validation_attempt: Id::try_from(reviewer).map_err(invalid)?,
            });
        }
    }
    Ok(receipts)
}
