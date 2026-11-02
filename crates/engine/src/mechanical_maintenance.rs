//! Governed mechanical-maintenance workflow: draft-only PR publishing,
//! two-phase 24-hour dead-code observation before deletion proposals, and
//! replay-gated routine corrections from rejected draft PRs.
//!
//! Implements `.kranz/tickets/mechanical-maintenance-draft-pr-feedback-loop.md`
//! under `docs/knowledge/decisions/positioning-governance-evidence-layer.md`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{EngineError, Result};

/// Default minimum observation window before suspected dead code can be
/// proposed for deletion in a draft PR (24 hours = 86,400 seconds).
pub const MIN_OBSERVATION_WINDOW_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MaintenanceTransformKind {
    Formatting,
    GeneratedFileDrift,
    ObservedDeadCodeDeletion,
    NewBehavior,
    ProductDecision,
    ArchitecturalChange,
}

impl MaintenanceTransformKind {
    pub fn is_feature_scope(self) -> bool {
        matches!(
            self,
            Self::NewBehavior | Self::ProductDecision | Self::ArchitecturalChange
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaintenanceRoutine {
    pub routine_id: String,
    pub revision: u32,
    pub allowed_transforms: Vec<MaintenanceTransformKind>,
    pub allowed_path_prefixes: Vec<String>,
    #[serde(default)]
    pub excluded_symbols: Vec<String>,
    pub cadence_seconds: u64,
    pub max_cost_cents: u64,
    pub validation_commands: Vec<String>,
    pub approved_by: String,
}

impl MaintenanceRoutine {
    pub fn validate(&self) -> Result<()> {
        if self.routine_id.trim().is_empty()
            || self.revision == 0
            || self.allowed_transforms.is_empty()
            || self.allowed_path_prefixes.is_empty()
            || self.cadence_seconds == 0
            || self.validation_commands.is_empty()
            || self.approved_by.trim().is_empty()
        {
            return Err(EngineError::Config(
                "maintenance routine is missing required scope, cadence, validation, or approval fields"
                    .into(),
            ));
        }
        if self.allowed_transforms.iter().any(|t| t.is_feature_scope()) {
            return Err(EngineError::Config(
                "mechanical maintenance routine cannot authorize feature-scope transforms".into(),
            ));
        }
        Ok(())
    }

    pub fn digest(&self) -> String {
        canonical_sha256(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaintenanceProposal {
    pub proposal_id: String,
    pub routine_id: String,
    pub routine_revision: u32,
    pub target_key: String,
    pub transform: MaintenanceTransformKind,
    pub touched_paths: Vec<String>,
    pub base_sha: String,
    pub head_branch: String,
    pub estimated_cost_cents: u64,
    pub validation_passed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "reason", deny_unknown_fields)]
pub enum ProposalRefusalReason {
    IncludesFeatureBehavior {
        transform: MaintenanceTransformKind,
    },
    TransformNotAllowed {
        transform: MaintenanceTransformKind,
    },
    PathOutsideGrant {
        path: String,
    },
    ExceedsCostBudget {
        estimated_cost_cents: u64,
        max_cost_cents: u64,
    },
    OverlapsActiveFeatureMission {
        path: String,
    },
    DuplicateOpenDraftPr {
        target_key: String,
    },
    StaleBaseOrUnvalidated {
        proposal_base_sha: String,
        current_base_sha: String,
        validation_passed: bool,
    },
}

/// Validates a maintenance proposal against its routine contract, active
/// feature-mission paths, open draft PRs, and current base SHA.
pub fn validate_maintenance_proposal(
    routine: &MaintenanceRoutine,
    proposal: &MaintenanceProposal,
    current_base_sha: &str,
    active_feature_paths: &[String],
    open_draft_pr_targets: &[String],
) -> std::result::Result<(), ProposalRefusalReason> {
    if proposal.transform.is_feature_scope() {
        return Err(ProposalRefusalReason::IncludesFeatureBehavior {
            transform: proposal.transform,
        });
    }
    if !routine.allowed_transforms.contains(&proposal.transform) {
        return Err(ProposalRefusalReason::TransformNotAllowed {
            transform: proposal.transform,
        });
    }
    if proposal.estimated_cost_cents > routine.max_cost_cents {
        return Err(ProposalRefusalReason::ExceedsCostBudget {
            estimated_cost_cents: proposal.estimated_cost_cents,
            max_cost_cents: routine.max_cost_cents,
        });
    }
    for path in &proposal.touched_paths {
        if !routine
            .allowed_path_prefixes
            .iter()
            .any(|prefix| path.starts_with(prefix))
        {
            return Err(ProposalRefusalReason::PathOutsideGrant { path: path.clone() });
        }
        if active_feature_paths
            .iter()
            .any(|active| path.starts_with(active) || active.starts_with(path))
        {
            return Err(ProposalRefusalReason::OverlapsActiveFeatureMission { path: path.clone() });
        }
    }
    if open_draft_pr_targets.contains(&proposal.target_key) {
        return Err(ProposalRefusalReason::DuplicateOpenDraftPr {
            target_key: proposal.target_key.clone(),
        });
    }
    if proposal.base_sha != current_base_sha || !proposal.validation_passed {
        return Err(ProposalRefusalReason::StaleBaseOrUnvalidated {
            proposal_base_sha: proposal.base_sha.clone(),
            current_base_sha: current_base_sha.to_string(),
            validation_passed: proposal.validation_passed,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftPrReceipt {
    pub pr_id: String,
    pub proposal_id: String,
    pub routine_id: String,
    pub routine_revision: u32,
    pub base_sha: String,
    pub head_branch: String,
    pub draft: bool,
    pub auto_merge_enabled: bool,
    pub ready_for_review: bool,
    pub merged: bool,
}

impl DraftPrReceipt {
    /// Verifies that the external publisher created a strictly draft PR with no
    /// auto-merge, ready-for-review promotion, or unattended merge.
    pub fn verify_draft_only(&self) -> Result<()> {
        if !self.draft || self.auto_merge_enabled || self.ready_for_review || self.merged {
            return Err(EngineError::Config(
                "mechanical maintenance publisher must produce draft-only PRs without auto-merge or ready-for-review promotion"
                    .into(),
            ));
        }
        Ok(())
    }
}

pub trait ExternalDraftPrPublisher {
    fn publish_draft_pr(
        &mut self,
        routine: &MaintenanceRoutine,
        proposal: &MaintenanceProposal,
    ) -> Result<DraftPrReceipt>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeadCodeObservation {
    pub candidate_symbol: String,
    pub file_path: String,
    pub code_fingerprint: String,
    pub reference_count: u32,
    pub observed_uses: u32,
    pub observed_at_epoch_secs: u64,
    pub coverage_complete: bool,
    #[serde(default)]
    pub dynamic_hazard_notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeadCodeObservationBatchReceipt {
    pub base_sha: String,
    pub recorded_at_epoch_secs: u64,
    pub observations: Vec<DeadCodeObservation>,
    pub deletions_proposed: usize,
}

/// Phase 1 of dead-code maintenance: records candidate observations and
/// proposes zero deletions.
pub fn record_dead_code_observations(
    base_sha: &str,
    now_epoch_secs: u64,
    observations: Vec<DeadCodeObservation>,
) -> DeadCodeObservationBatchReceipt {
    DeadCodeObservationBatchReceipt {
        base_sha: base_sha.to_string(),
        recorded_at_epoch_secs: now_epoch_secs,
        observations,
        deletions_proposed: 0,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CurrentSymbolState {
    pub candidate_symbol: String,
    pub code_fingerprint: String,
    pub reference_count: u32,
    pub observed_uses: u32,
    pub coverage_complete: bool,
    pub has_unresolved_dynamic_use: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "reason", deny_unknown_fields)]
pub enum WithheldReason {
    ObservationWindowNotElapsed {
        elapsed_secs: u64,
        required_secs: u64,
    },
    MissingCurrentEvidence,
    FingerprintChanged {
        observed: String,
        current: String,
    },
    ReferencesPresent {
        reference_count: u32,
    },
    ObservedRuntimeUse {
        observed_uses: u32,
    },
    InsufficientTelemetryOrCoverage,
    UnresolvedDynamicDispatch {
        notes: Vec<String>,
    },
    ExcludedByRoutineRevision {
        routine_revision: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "decision", deny_unknown_fields)]
pub enum DeadCodeDecision {
    ProposeDraftDeletion {
        candidate_symbol: String,
        file_path: String,
        code_fingerprint: String,
        elapsed_secs: u64,
    },
    Withheld {
        candidate_symbol: String,
        file_path: String,
        reason: WithheldReason,
    },
}

/// Phase 2 of dead-code maintenance: evaluates previously recorded observations
/// at `now_epoch_secs` (`>= 24h` required) against `current_states` and the
/// active `routine`.
pub fn evaluate_dead_code_deletion_candidates(
    routine: &MaintenanceRoutine,
    observations: &[DeadCodeObservation],
    current_states: &[CurrentSymbolState],
    now_epoch_secs: u64,
) -> Vec<DeadCodeDecision> {
    let mut decisions = Vec::with_capacity(observations.len());
    for obs in observations {
        if routine.excluded_symbols.contains(&obs.candidate_symbol) {
            decisions.push(DeadCodeDecision::Withheld {
                candidate_symbol: obs.candidate_symbol.clone(),
                file_path: obs.file_path.clone(),
                reason: WithheldReason::ExcludedByRoutineRevision {
                    routine_revision: routine.revision,
                },
            });
            continue;
        }
        let elapsed_secs = now_epoch_secs.saturating_sub(obs.observed_at_epoch_secs);
        if elapsed_secs < MIN_OBSERVATION_WINDOW_SECS {
            decisions.push(DeadCodeDecision::Withheld {
                candidate_symbol: obs.candidate_symbol.clone(),
                file_path: obs.file_path.clone(),
                reason: WithheldReason::ObservationWindowNotElapsed {
                    elapsed_secs,
                    required_secs: MIN_OBSERVATION_WINDOW_SECS,
                },
            });
            continue;
        }
        let Some(current) = current_states
            .iter()
            .find(|s| s.candidate_symbol == obs.candidate_symbol)
        else {
            decisions.push(DeadCodeDecision::Withheld {
                candidate_symbol: obs.candidate_symbol.clone(),
                file_path: obs.file_path.clone(),
                reason: WithheldReason::MissingCurrentEvidence,
            });
            continue;
        };
        if current.code_fingerprint != obs.code_fingerprint {
            decisions.push(DeadCodeDecision::Withheld {
                candidate_symbol: obs.candidate_symbol.clone(),
                file_path: obs.file_path.clone(),
                reason: WithheldReason::FingerprintChanged {
                    observed: obs.code_fingerprint.clone(),
                    current: current.code_fingerprint.clone(),
                },
            });
            continue;
        }
        if obs.reference_count > 0 || current.reference_count > 0 {
            decisions.push(DeadCodeDecision::Withheld {
                candidate_symbol: obs.candidate_symbol.clone(),
                file_path: obs.file_path.clone(),
                reason: WithheldReason::ReferencesPresent {
                    reference_count: current.reference_count.max(obs.reference_count),
                },
            });
            continue;
        }
        if obs.observed_uses > 0 || current.observed_uses > 0 {
            decisions.push(DeadCodeDecision::Withheld {
                candidate_symbol: obs.candidate_symbol.clone(),
                file_path: obs.file_path.clone(),
                reason: WithheldReason::ObservedRuntimeUse {
                    observed_uses: current.observed_uses.max(obs.observed_uses),
                },
            });
            continue;
        }
        if !obs.coverage_complete || !current.coverage_complete {
            decisions.push(DeadCodeDecision::Withheld {
                candidate_symbol: obs.candidate_symbol.clone(),
                file_path: obs.file_path.clone(),
                reason: WithheldReason::InsufficientTelemetryOrCoverage,
            });
            continue;
        }
        if current.has_unresolved_dynamic_use || !obs.dynamic_hazard_notes.is_empty() {
            let mut notes = obs.dynamic_hazard_notes.clone();
            if current.has_unresolved_dynamic_use && notes.is_empty() {
                notes.push("unresolved dynamic dispatch or reflection".into());
            }
            decisions.push(DeadCodeDecision::Withheld {
                candidate_symbol: obs.candidate_symbol.clone(),
                file_path: obs.file_path.clone(),
                reason: WithheldReason::UnresolvedDynamicDispatch { notes },
            });
            continue;
        }
        decisions.push(DeadCodeDecision::ProposeDraftDeletion {
            candidate_symbol: obs.candidate_symbol.clone(),
            file_path: obs.file_path.clone(),
            code_fingerprint: obs.code_fingerprint.clone(),
            elapsed_secs,
        });
    }
    decisions
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RejectedMaintenancePr {
    pub pr_id: String,
    pub routine_id: String,
    pub routine_revision: u32,
    pub false_positive_symbol: String,
    pub file_path: String,
    pub human_disposition: String,
    pub concrete_reproducer: String,
    pub actor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutineReplayCase {
    pub case_id: String,
    pub observation: DeadCodeObservation,
    pub current_state: CurrentSymbolState,
    pub eval_epoch_secs: u64,
    pub expect_deletion_proposed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutineCorrectionProposal {
    pub correction_id: String,
    pub source_rejected_pr: RejectedMaintenancePr,
    pub candidate_routine: MaintenanceRoutine,
    pub regression_case: RoutineReplayCase,
    pub positive_case: RoutineReplayCase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutineReplayReceipt {
    pub correction_id: String,
    pub candidate_routine_digest: String,
    pub regression_withheld: bool,
    pub positive_proposed: bool,
    pub passed: bool,
}

impl RoutineReplayReceipt {
    pub fn digest(&self) -> String {
        canonical_sha256(self)
    }
}

/// Builds a reviewable routine correction proposal (`revision + 1`) from a
/// rejected draft PR, pairing a regression case for the false positive with a
/// positive maintenance case so suppressing all output cannot pass.
pub fn propose_routine_correction(
    active_routine: &MaintenanceRoutine,
    rejected_pr: RejectedMaintenancePr,
    correction_id: &str,
    regression_case: RoutineReplayCase,
    positive_case: RoutineReplayCase,
) -> Result<RoutineCorrectionProposal> {
    if rejected_pr.routine_id != active_routine.routine_id
        || rejected_pr.routine_revision != active_routine.revision
    {
        return Err(EngineError::Config(
            "rejected PR does not match the active routine identity and revision".into(),
        ));
    }
    if regression_case.expect_deletion_proposed || !positive_case.expect_deletion_proposed {
        return Err(EngineError::Config(
            "routine correction requires both a negative regression case and a positive maintenance case"
                .into(),
        ));
    }
    let mut candidate_routine = active_routine.clone();
    candidate_routine.revision = active_routine.revision + 1;
    if !candidate_routine
        .excluded_symbols
        .contains(&rejected_pr.false_positive_symbol)
    {
        candidate_routine
            .excluded_symbols
            .push(rejected_pr.false_positive_symbol.clone());
    }
    candidate_routine.validate()?;
    Ok(RoutineCorrectionProposal {
        correction_id: correction_id.to_string(),
        source_rejected_pr: rejected_pr,
        candidate_routine,
        regression_case,
        positive_case,
    })
}

/// Evaluates a `RoutineCorrectionProposal` against its negative regression case
/// and positive maintenance case, plus any additional fixed replay cases.
pub fn evaluate_routine_correction(
    proposal: &RoutineCorrectionProposal,
    extra_cases: &[RoutineReplayCase],
) -> RoutineReplayReceipt {
    let run_case = |case: &RoutineReplayCase| -> bool {
        let decisions = evaluate_dead_code_deletion_candidates(
            &proposal.candidate_routine,
            std::slice::from_ref(&case.observation),
            std::slice::from_ref(&case.current_state),
            case.eval_epoch_secs,
        );
        matches!(
            decisions.first(),
            Some(DeadCodeDecision::ProposeDraftDeletion { .. })
        )
    };
    let regression_withheld = !run_case(&proposal.regression_case);
    let positive_proposed = run_case(&proposal.positive_case);
    let extras_ok = extra_cases
        .iter()
        .all(|c| run_case(c) == c.expect_deletion_proposed);
    let passed = regression_withheld && positive_proposed && extras_ok;
    RoutineReplayReceipt {
        correction_id: proposal.correction_id.clone(),
        candidate_routine_digest: proposal.candidate_routine.digest(),
        regression_withheld,
        positive_proposed,
        passed,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HumanRoutineApproval {
    pub approver: String,
    pub correction_id: String,
    pub candidate_routine_digest: String,
    pub replay_receipt_digest: String,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "event", deny_unknown_fields)]
pub enum MaintenanceAuditEvent {
    ObservationsRecorded {
        base_sha: String,
        count: usize,
    },
    DraftPrPublished {
        pr_id: String,
        proposal_id: String,
        routine_revision: u32,
    },
    ProposalRefused {
        proposal_id: String,
        reason: ProposalRefusalReason,
    },
    PrRejected {
        pr_id: String,
        false_positive_symbol: String,
    },
    RoutineCorrectionActivated {
        correction_id: String,
        new_revision: u32,
        approver: String,
    },
    RoutineCorrectionRefused {
        correction_id: String,
        active_revision: u32,
        reason: String,
    },
    RoutineRolledBack {
        from_revision: u32,
        restored_revision: u32,
        actor: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MaintenanceGovernanceLedger {
    pub active_routine: MaintenanceRoutine,
    pub prior_revisions: Vec<MaintenanceRoutine>,
    pub draft_prs: Vec<DraftPrReceipt>,
    pub rejected_prs: Vec<RejectedMaintenancePr>,
    pub audit_log: Vec<MaintenanceAuditEvent>,
}

impl MaintenanceGovernanceLedger {
    pub fn new(initial_routine: MaintenanceRoutine) -> Result<Self> {
        initial_routine.validate()?;
        Ok(Self {
            active_routine: initial_routine,
            prior_revisions: Vec::new(),
            draft_prs: Vec::new(),
            rejected_prs: Vec::new(),
            audit_log: Vec::new(),
        })
    }

    pub fn submit_proposal<P: ExternalDraftPrPublisher>(
        &mut self,
        publisher: &mut P,
        proposal: &MaintenanceProposal,
        current_base_sha: &str,
        active_feature_paths: &[String],
    ) -> std::result::Result<DraftPrReceipt, ProposalRefusalReason> {
        let open_targets: Vec<String> = self
            .draft_prs
            .iter()
            .filter(|pr| !pr.merged)
            .map(|pr| pr.proposal_id.clone())
            .collect();
        if let Err(reason) = validate_maintenance_proposal(
            &self.active_routine,
            proposal,
            current_base_sha,
            active_feature_paths,
            &open_targets,
        ) {
            self.audit_log.push(MaintenanceAuditEvent::ProposalRefused {
                proposal_id: proposal.proposal_id.clone(),
                reason: reason.clone(),
            });
            return Err(reason);
        }
        let receipt = publisher
            .publish_draft_pr(&self.active_routine, proposal)
            .expect("publisher execution");
        receipt
            .verify_draft_only()
            .expect("draft-only PR invariant");
        self.audit_log
            .push(MaintenanceAuditEvent::DraftPrPublished {
                pr_id: receipt.pr_id.clone(),
                proposal_id: receipt.proposal_id.clone(),
                routine_revision: self.active_routine.revision,
            });
        self.draft_prs.push(receipt.clone());
        Ok(receipt)
    }

    pub fn activate_routine_correction(
        &mut self,
        proposal: &RoutineCorrectionProposal,
        receipt: &RoutineReplayReceipt,
        approval: Option<&HumanRoutineApproval>,
    ) -> Result<u32> {
        let refuse = |ledger: &mut Self, reason: &str| -> Result<u32> {
            ledger
                .audit_log
                .push(MaintenanceAuditEvent::RoutineCorrectionRefused {
                    correction_id: proposal.correction_id.clone(),
                    active_revision: ledger.active_routine.revision,
                    reason: reason.to_string(),
                });
            Err(EngineError::Config(reason.to_string()))
        };

        if !receipt.passed || !receipt.regression_withheld || !receipt.positive_proposed {
            return refuse(
                self,
                "routine correction replay did not pass both negative regression and positive maintenance cases",
            );
        }
        let candidate_digest = proposal.candidate_routine.digest();
        if proposal.candidate_routine.routine_id != self.active_routine.routine_id
            || proposal.candidate_routine.revision != self.active_routine.revision + 1
            || receipt.correction_id != proposal.correction_id
            || receipt.candidate_routine_digest != candidate_digest
        {
            return refuse(
                self,
                "routine correction receipt is stale relative to active routine revision or candidate digest",
            );
        }
        let Some(approval) = approval else {
            return refuse(
                self,
                "routine correction activation requires explicit human approval",
            );
        };
        if approval.approver.trim().is_empty()
            || approval.rationale.trim().is_empty()
            || approval.correction_id != proposal.correction_id
            || approval.candidate_routine_digest != candidate_digest
            || approval.replay_receipt_digest != receipt.digest()
        {
            return refuse(
                self,
                "human routine approval does not match candidate routine or replay receipt digest",
            );
        }

        self.rejected_prs.push(proposal.source_rejected_pr.clone());
        self.prior_revisions.push(self.active_routine.clone());
        self.active_routine = proposal.candidate_routine.clone();
        self.audit_log
            .push(MaintenanceAuditEvent::RoutineCorrectionActivated {
                correction_id: proposal.correction_id.clone(),
                new_revision: self.active_routine.revision,
                approver: approval.approver.clone(),
            });
        Ok(self.active_routine.revision)
    }

    pub fn rollback_routine_revision(&mut self, actor: &str, reason: &str) -> Result<u32> {
        if actor.trim().is_empty() || reason.trim().is_empty() {
            return Err(EngineError::Config(
                "routine rollback requires non-empty actor and reason".into(),
            ));
        }
        let Some(prior) = self.prior_revisions.pop() else {
            return Err(EngineError::Config(
                "no prior routine revision available for rollback".into(),
            ));
        };
        let from_revision = self.active_routine.revision;
        let restored_revision = prior.revision;
        self.active_routine = prior;
        self.audit_log
            .push(MaintenanceAuditEvent::RoutineRolledBack {
                from_revision,
                restored_revision,
                actor: actor.to_string(),
                reason: reason.to_string(),
            });
        Ok(restored_revision)
    }
}

fn canonical_sha256<T: Serialize>(value: &T) -> String {
    use std::fmt::Write;
    let bytes = serde_json::to_vec(value).expect("serializable maintenance value");
    let digest = Sha256::digest(&bytes);
    let mut out = String::from("sha256:");
    for b in digest {
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DeterministicDraftPublisher {
        next_pr: u32,
    }

    impl ExternalDraftPrPublisher for DeterministicDraftPublisher {
        fn publish_draft_pr(
            &mut self,
            routine: &MaintenanceRoutine,
            proposal: &MaintenanceProposal,
        ) -> Result<DraftPrReceipt> {
            self.next_pr += 1;
            Ok(DraftPrReceipt {
                pr_id: format!("pr-{}", self.next_pr),
                proposal_id: proposal.target_key.clone(),
                routine_id: routine.routine_id.clone(),
                routine_revision: routine.revision,
                base_sha: proposal.base_sha.clone(),
                head_branch: proposal.head_branch.clone(),
                draft: true,
                auto_merge_enabled: false,
                ready_for_review: false,
                merged: false,
            })
        }
    }

    fn sample_routine() -> MaintenanceRoutine {
        MaintenanceRoutine {
            routine_id: "nightly-dead-code-and-fmt".into(),
            revision: 1,
            allowed_transforms: vec![
                MaintenanceTransformKind::Formatting,
                MaintenanceTransformKind::ObservedDeadCodeDeletion,
            ],
            allowed_path_prefixes: vec!["crates/engine/src/".into()],
            excluded_symbols: vec![],
            cadence_seconds: 86_400,
            max_cost_cents: 500,
            validation_commands: vec!["cargo test --workspace".into()],
            approved_by: "operator-craig".into(),
        }
    }

    #[test]
    fn mechanical_maintenance_draft_pr_only_and_scope_overlap_duplicate_guards() {
        let routine = sample_routine();
        let mut ledger = MaintenanceGovernanceLedger::new(routine).unwrap();
        let mut publisher = DeterministicDraftPublisher { next_pr: 100 };

        let valid_proposal = MaintenanceProposal {
            proposal_id: "prop-1".into(),
            routine_id: "nightly-dead-code-and-fmt".into(),
            routine_revision: 1,
            target_key: "dead-code:legacy_helper".into(),
            transform: MaintenanceTransformKind::ObservedDeadCodeDeletion,
            touched_paths: vec!["crates/engine/src/legacy.rs".into()],
            base_sha: "sha-main-1".into(),
            head_branch: "kranz/maint-legacy".into(),
            estimated_cost_cents: 120,
            validation_passed: true,
        };
        let receipt = ledger
            .submit_proposal(&mut publisher, &valid_proposal, "sha-main-1", &[])
            .unwrap();
        assert!(receipt.draft);
        assert!(!receipt.auto_merge_enabled);
        assert!(!receipt.ready_for_review);
        assert!(!receipt.merged);
        receipt.verify_draft_only().unwrap();

        // Duplicate open draft PR for the same target_key is refused.
        assert_eq!(
            ledger.submit_proposal(&mut publisher, &valid_proposal, "sha-main-1", &[]),
            Err(ProposalRefusalReason::DuplicateOpenDraftPr {
                target_key: "dead-code:legacy_helper".into()
            })
        );

        // Feature behavior, path outside grant, budget overrun, active feature overlap, and stale base are refused.
        let mut feature_attempt = valid_proposal.clone();
        feature_attempt.target_key = "other".into();
        feature_attempt.transform = MaintenanceTransformKind::NewBehavior;
        assert_eq!(
            ledger.submit_proposal(&mut publisher, &feature_attempt, "sha-main-1", &[]),
            Err(ProposalRefusalReason::IncludesFeatureBehavior {
                transform: MaintenanceTransformKind::NewBehavior
            })
        );

        let mut overlap = valid_proposal.clone();
        overlap.target_key = "overlap-target".into();
        assert_eq!(
            ledger.submit_proposal(
                &mut publisher,
                &overlap,
                "sha-main-1",
                &["crates/engine/src/legacy.rs".into()]
            ),
            Err(ProposalRefusalReason::OverlapsActiveFeatureMission {
                path: "crates/engine/src/legacy.rs".into()
            })
        );
    }

    #[test]
    fn mechanical_maintenance_dead_code_two_phase_24h_observation_and_withholding_reasons() {
        let routine = sample_routine();
        let t0 = 1_700_000_000_u64;
        let observations = vec![
            DeadCodeObservation {
                candidate_symbol: "truly_dead_fn".into(),
                file_path: "crates/engine/src/legacy.rs".into(),
                code_fingerprint: "fp-dead-1".into(),
                reference_count: 0,
                observed_uses: 0,
                observed_at_epoch_secs: t0,
                coverage_complete: true,
                dynamic_hazard_notes: vec![],
            },
            DeadCodeObservation {
                candidate_symbol: "changed_fn".into(),
                file_path: "crates/engine/src/legacy.rs".into(),
                code_fingerprint: "fp-old".into(),
                reference_count: 0,
                observed_uses: 0,
                observed_at_epoch_secs: t0,
                coverage_complete: true,
                dynamic_hazard_notes: vec![],
            },
            DeadCodeObservation {
                candidate_symbol: "dynamic_plugin_hook".into(),
                file_path: "crates/engine/src/plugins.rs".into(),
                code_fingerprint: "fp-dyn".into(),
                reference_count: 0,
                observed_uses: 0,
                observed_at_epoch_secs: t0,
                coverage_complete: true,
                dynamic_hazard_notes: vec!["registered via dlsym".into()],
            },
        ];

        // Phase 1 records observations and proposes zero deletions.
        let batch = record_dead_code_observations("sha-main-1", t0, observations.clone());
        assert_eq!(batch.deletions_proposed, 0);

        let current_states = vec![
            CurrentSymbolState {
                candidate_symbol: "truly_dead_fn".into(),
                code_fingerprint: "fp-dead-1".into(),
                reference_count: 0,
                observed_uses: 0,
                coverage_complete: true,
                has_unresolved_dynamic_use: false,
            },
            CurrentSymbolState {
                candidate_symbol: "changed_fn".into(),
                code_fingerprint: "fp-new".into(),
                reference_count: 0,
                observed_uses: 0,
                coverage_complete: true,
                has_unresolved_dynamic_use: false,
            },
            CurrentSymbolState {
                candidate_symbol: "dynamic_plugin_hook".into(),
                code_fingerprint: "fp-dyn".into(),
                reference_count: 0,
                observed_uses: 0,
                coverage_complete: true,
                has_unresolved_dynamic_use: true,
            },
        ];

        // Before 24 hours: every candidate is withheld with ObservationWindowNotElapsed.
        let early = evaluate_dead_code_deletion_candidates(
            &routine,
            &batch.observations,
            &current_states,
            t0 + MIN_OBSERVATION_WINDOW_SECS - 1,
        );
        assert!(early.iter().all(|d| matches!(
            d,
            DeadCodeDecision::Withheld {
                reason: WithheldReason::ObservationWindowNotElapsed { .. },
                ..
            }
        )));

        // After 24 hours: truly_dead_fn is proposed for draft deletion, while
        // changed_fn and dynamic_plugin_hook are withheld with explicit reasons.
        let after_24h = evaluate_dead_code_deletion_candidates(
            &routine,
            &batch.observations,
            &current_states,
            t0 + MIN_OBSERVATION_WINDOW_SECS,
        );
        assert_eq!(
            after_24h[0],
            DeadCodeDecision::ProposeDraftDeletion {
                candidate_symbol: "truly_dead_fn".into(),
                file_path: "crates/engine/src/legacy.rs".into(),
                code_fingerprint: "fp-dead-1".into(),
                elapsed_secs: MIN_OBSERVATION_WINDOW_SECS,
            }
        );
        assert!(matches!(
            after_24h[1],
            DeadCodeDecision::Withheld {
                reason: WithheldReason::FingerprintChanged { .. },
                ..
            }
        ));
        assert!(matches!(
            after_24h[2],
            DeadCodeDecision::Withheld {
                reason: WithheldReason::UnresolvedDynamicDispatch { .. },
                ..
            }
        ));
    }

    #[test]
    fn mechanical_maintenance_rejected_pr_routine_correction_requires_replay_and_human_approval() {
        let routine = sample_routine();
        let mut ledger = MaintenanceGovernanceLedger::new(routine.clone()).unwrap();
        let t0 = 1_700_000_000_u64;
        let t1 = t0 + MIN_OBSERVATION_WINDOW_SECS + 60;

        let rejected = RejectedMaintenancePr {
            pr_id: "pr-101".into(),
            routine_id: routine.routine_id.clone(),
            routine_revision: 1,
            false_positive_symbol: "serde_default_hook".into(),
            file_path: "crates/engine/src/types.rs".into(),
            human_disposition: "Invoked via #[serde(default = \"serde_default_hook\")]".into(),
            concrete_reproducer: "crates/engine/src/types.rs:serde_default_hook".into(),
            actor: "operator-craig".into(),
        };

        let regression_case = RoutineReplayCase {
            case_id: "serde-default-hook-false-positive".into(),
            observation: DeadCodeObservation {
                candidate_symbol: "serde_default_hook".into(),
                file_path: "crates/engine/src/types.rs".into(),
                code_fingerprint: "fp-serde".into(),
                reference_count: 0,
                observed_uses: 0,
                observed_at_epoch_secs: t0,
                coverage_complete: true,
                dynamic_hazard_notes: vec![],
            },
            current_state: CurrentSymbolState {
                candidate_symbol: "serde_default_hook".into(),
                code_fingerprint: "fp-serde".into(),
                reference_count: 0,
                observed_uses: 0,
                coverage_complete: true,
                has_unresolved_dynamic_use: false,
            },
            eval_epoch_secs: t1,
            expect_deletion_proposed: false,
        };
        let positive_case = RoutineReplayCase {
            case_id: "genuinely-dead-helper".into(),
            observation: DeadCodeObservation {
                candidate_symbol: "obsolete_helper".into(),
                file_path: "crates/engine/src/legacy.rs".into(),
                code_fingerprint: "fp-obsolete".into(),
                reference_count: 0,
                observed_uses: 0,
                observed_at_epoch_secs: t0,
                coverage_complete: true,
                dynamic_hazard_notes: vec![],
            },
            current_state: CurrentSymbolState {
                candidate_symbol: "obsolete_helper".into(),
                code_fingerprint: "fp-obsolete".into(),
                reference_count: 0,
                observed_uses: 0,
                coverage_complete: true,
                has_unresolved_dynamic_use: false,
            },
            eval_epoch_secs: t1,
            expect_deletion_proposed: true,
        };

        let proposal = propose_routine_correction(
            &ledger.active_routine,
            rejected,
            "corr-1",
            regression_case,
            positive_case,
        )
        .unwrap();
        assert_eq!(proposal.candidate_routine.revision, 2);

        let receipt = evaluate_routine_correction(&proposal, &[]);
        assert!(receipt.passed);
        assert!(receipt.regression_withheld);
        assert!(receipt.positive_proposed);

        // Passing replay alone does not activate revision 2 without human approval.
        assert!(ledger
            .activate_routine_correction(&proposal, &receipt, None)
            .is_err());
        assert_eq!(ledger.active_routine.revision, 1);

        let approval = HumanRoutineApproval {
            approver: "operator-craig".into(),
            correction_id: "corr-1".into(),
            candidate_routine_digest: proposal.candidate_routine.digest(),
            replay_receipt_digest: receipt.digest(),
            rationale: "Exclude serde_default_hook while keeping dead-code routine active".into(),
        };
        assert_eq!(
            ledger
                .activate_routine_correction(&proposal, &receipt, Some(&approval))
                .unwrap(),
            2
        );
        assert_eq!(ledger.active_routine.revision, 2);

        // Rollback restores revision 1 and records an audit event.
        assert_eq!(
            ledger
                .rollback_routine_revision("operator-craig", "Test rollback")
                .unwrap(),
            1
        );
        assert_eq!(ledger.active_routine.revision, 1);
    }
}
