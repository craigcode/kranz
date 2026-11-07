//! Governed review-memory feedback capture, replay-suite evaluation, and
//! human-approved scoped rule promotion.
//!
//! Implements the governance and evidence contract for
//! `.kranz/tickets/review-memory-rule-promotion-replay-gate.md` under
//! `docs/knowledge/decisions/positioning-governance-evidence-layer.md`:
//! - Preserves structured accept/reject/edit feedback with original and edited
//!   wording, disposition, rationale, actor, source review/code revision, and
//!   explicit scope. Ambiguous feedback remains evidence without creating a
//!   candidate rule.
//! - Keeps candidate rules in `draft` state until they pass a versioned,
//!   human-reviewed replay suite (with both required and prohibited findings)
//!   and receive explicit human promotion approval bound to the exact candidate
//!   and replay receipt digests.
//! - Evaluates scoped rule retrieval on the actual review input path so
//!   out-of-scope rules cannot leak across domains or paths, and a reviewer
//!   emitting zero findings fails required-finding expectations.
//! - Retains prior active versions for audited rollback and scrubs exported
//!   bundles through [`crate::scrub::scrub`].

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{EngineError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleScope {
    pub domain: String,
    pub path_prefixes: Vec<String>,
    #[serde(default)]
    pub middleware_tags: Vec<String>,
}

impl RuleScope {
    pub fn validate(&self) -> Result<()> {
        if self.domain.trim().is_empty() {
            return Err(EngineError::Config(
                "review memory rule scope requires a non-empty domain".into(),
            ));
        }
        if self.path_prefixes.is_empty()
            || self
                .path_prefixes
                .iter()
                .any(|p| p.trim().is_empty() || p == "/" || p == "*")
        {
            return Err(EngineError::Config(
                "review memory rule scope requires explicit non-wildcard path prefixes".into(),
            ));
        }
        Ok(())
    }

    pub fn matches(&self, domain: &str, file_path: &str, active_middleware: &[String]) -> bool {
        if self.domain != domain {
            return false;
        }
        let path_ok = self
            .path_prefixes
            .iter()
            .any(|prefix| file_path.starts_with(prefix));
        if !path_ok {
            return false;
        }
        self.middleware_tags
            .iter()
            .all(|required| active_middleware.contains(required))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OriginalFinding {
    pub finding_id: String,
    pub behavior_key: String,
    pub evidence_target: String,
    pub file_path: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind", deny_unknown_fields)]
pub enum FeedbackDecision {
    Accept,
    Reject,
    Edit {
        #[serde(rename = "editedWording")]
        edited_wording: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FeedbackDisposition {
    IncorrectFinding,
    Duplicate,
    ExistingMitigation,
    OutOfScope,
    AcceptedRisk,
    ValidDefect,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewFindingFeedback {
    pub feedback_id: String,
    pub original_finding: OriginalFinding,
    pub decision: FeedbackDecision,
    pub disposition: FeedbackDisposition,
    pub rationale: String,
    pub actor: String,
    pub source_review_revision: String,
    pub source_code_revision: String,
    pub scope: RuleScope,
    pub recorded_at: String,
}

impl ReviewFindingFeedback {
    pub fn validate(&self) -> Result<()> {
        if [
            &self.feedback_id,
            &self.original_finding.finding_id,
            &self.original_finding.behavior_key,
            &self.original_finding.evidence_target,
            &self.original_finding.file_path,
            &self.original_finding.summary,
            &self.rationale,
            &self.actor,
            &self.source_review_revision,
            &self.source_code_revision,
            &self.recorded_at,
        ]
        .into_iter()
        .any(|s| s.trim().is_empty())
        {
            return Err(EngineError::Config(
                "review finding feedback contains empty required identity fields".into(),
            ));
        }
        if let FeedbackDecision::Edit { edited_wording } = &self.decision {
            if edited_wording.trim().is_empty()
                || edited_wording.trim() == self.original_finding.summary.trim()
            {
                return Err(EngineError::Config(
                    "edited finding feedback must retain distinct non-empty edited wording".into(),
                ));
            }
        }
        self.scope.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleLifecycleState {
    Draft,
    Active,
    Superseded,
    RolledBack,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateReviewRule {
    pub candidate_id: String,
    pub revision: u32,
    pub state: RuleLifecycleState,
    pub rationale: String,
    pub guidance_text: String,
    pub scope: RuleScope,
    pub source_feedback_ids: Vec<String>,
    /// Behavior keys this rule suppresses when all scope + mitigation conditions hold.
    #[serde(default)]
    pub suppress_behavior_keys: Vec<String>,
    /// Optional mitigation tag required before suppression applies; if `None`,
    /// the candidate suppresses unconditionally within `scope`.
    #[serde(default)]
    pub require_mitigation_tag: Option<String>,
    #[serde(default)]
    pub weakens_engine_floor: bool,
    #[serde(default)]
    pub broadens_waiver: bool,
    #[serde(default)]
    pub supersedes_rule_id: Option<String>,
}

impl CandidateReviewRule {
    pub fn validate(&self) -> Result<()> {
        if self.candidate_id.trim().is_empty()
            || self.revision == 0
            || self.rationale.trim().is_empty()
            || self.guidance_text.trim().is_empty()
            || self.source_feedback_ids.is_empty()
        {
            return Err(EngineError::Config(
                "candidate review rule is missing required provenance or guidance fields".into(),
            ));
        }
        if self.weakens_engine_floor || self.broadens_waiver {
            return Err(EngineError::Config(
                "candidate review rule cannot weaken an engine floor or broaden a waiver".into(),
            ));
        }
        self.scope.validate()
    }

    pub fn digest(&self) -> String {
        canonical_sha256(self)
    }
}

/// Derives a `Draft` candidate rule from structured feedback when appropriate.
///
/// - `Ambiguous`, `Duplicate`, `OutOfScope`, and `AcceptedRisk` dispositions
///   stay recorded evidence without automatically generating a candidate rule.
/// - Accepting or editing a single comment produces at most a `Draft` candidate
///   and never activates a standing rule without replay + human promotion.
pub fn derive_candidate_from_feedback(
    feedback: &ReviewFindingFeedback,
    candidate_id: &str,
    require_mitigation_tag: Option<String>,
) -> Result<Option<CandidateReviewRule>> {
    feedback.validate()?;
    match feedback.disposition {
        FeedbackDisposition::Ambiguous
        | FeedbackDisposition::Duplicate
        | FeedbackDisposition::OutOfScope
        | FeedbackDisposition::AcceptedRisk => return Ok(None),
        FeedbackDisposition::IncorrectFinding
        | FeedbackDisposition::ExistingMitigation
        | FeedbackDisposition::ValidDefect => {}
    }
    let guidance_text = match &feedback.decision {
        FeedbackDecision::Edit { edited_wording } => edited_wording.clone(),
        FeedbackDecision::Accept | FeedbackDecision::Reject => feedback.rationale.clone(),
    };
    let suppress_behavior_keys = match feedback.disposition {
        FeedbackDisposition::IncorrectFinding | FeedbackDisposition::ExistingMitigation => {
            vec![feedback.original_finding.behavior_key.clone()]
        }
        FeedbackDisposition::ValidDefect => Vec::new(),
        _ => Vec::new(),
    };
    let candidate = CandidateReviewRule {
        candidate_id: candidate_id.to_string(),
        revision: 1,
        state: RuleLifecycleState::Draft,
        rationale: feedback.rationale.clone(),
        guidance_text,
        scope: feedback.scope.clone(),
        source_feedback_ids: vec![feedback.feedback_id.clone()],
        suppress_behavior_keys,
        require_mitigation_tag,
        weakens_engine_floor: false,
        broadens_waiver: false,
        supersedes_rule_id: None,
    };
    candidate.validate()?;
    Ok(Some(candidate))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplayExpectation {
    pub behavior_key: String,
    pub evidence_target: String,
    pub description: String,
    #[serde(default)]
    pub accepted_wording_variants: Vec<String>,
}

impl ReplayExpectation {
    pub fn matches_finding(&self, finding: &SimulatedFinding) -> bool {
        if self.behavior_key != finding.behavior_key
            || self.evidence_target != finding.evidence_target
        {
            return false;
        }
        if self.accepted_wording_variants.is_empty() {
            return true;
        }
        let summary_lower = finding.summary.to_ascii_lowercase();
        self.accepted_wording_variants
            .iter()
            .any(|variant| summary_lower.contains(&variant.to_ascii_lowercase()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SimulatedFinding {
    pub behavior_key: String,
    pub evidence_target: String,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SimulatedCaseMode {
    Normal,
    ZeroFindings,
    Timeout,
    UnparseableOutput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplayCase {
    pub case_id: String,
    pub domain: String,
    pub file_path: String,
    #[serde(default)]
    pub middleware_tags: Vec<String>,
    pub diff_text: String,
    pub raw_findings: Vec<SimulatedFinding>,
    pub required_findings: Vec<ReplayExpectation>,
    pub prohibited_findings: Vec<ReplayExpectation>,
    #[serde(default = "default_case_mode")]
    pub mode: SimulatedCaseMode,
}

fn default_case_mode() -> SimulatedCaseMode {
    SimulatedCaseMode::Normal
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplaySuite {
    pub suite_id: String,
    pub version: u32,
    pub reviewed_by: String,
    pub cases: Vec<ReplayCase>,
}

impl ReplaySuite {
    pub fn validate(&self) -> Result<()> {
        if self.suite_id.trim().is_empty()
            || self.version == 0
            || self.reviewed_by.trim().is_empty()
            || self.cases.is_empty()
        {
            return Err(EngineError::Config(
                "replay suite must be non-empty and carry a reviewed version identity".into(),
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
pub struct EvaluationPin {
    pub suite_id: String,
    pub suite_version: u32,
    pub suite_digest: String,
    pub active_memory_version: u64,
    pub candidate_id: String,
    pub candidate_revision: u32,
    pub candidate_digest: String,
    pub backend_id: String,
    pub model_id: String,
    pub prompt_hash: String,
    pub retrieval_policy: String,
    pub repeat_runs: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EffectiveMemoryInput {
    pub active_memory_version: u64,
    pub retrieved_rule_ids: Vec<String>,
    pub candidate_included: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReplayCaseStatus {
    Passed,
    FailedExpectation,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplayCaseOutcome {
    pub case_id: String,
    pub run_index: u32,
    pub status: ReplayCaseStatus,
    pub effective_input: EffectiveMemoryInput,
    pub emitted_findings: Vec<SimulatedFinding>,
    pub missed_required: Vec<String>,
    pub emitted_prohibited: Vec<String>,
    pub unresolved_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplayEvaluationReceipt {
    pub pin: EvaluationPin,
    pub passed: bool,
    pub unresolved_cases: usize,
    pub failed_cases: usize,
    pub case_outcomes: Vec<ReplayCaseOutcome>,
}

impl ReplayEvaluationReceipt {
    pub fn digest(&self) -> String {
        canonical_sha256(self)
    }
}

/// Retrieves only the active (and optional candidate) rules whose scope matches
/// the target `(domain, file_path, middleware_tags)` on the review input path.
pub fn retrieve_scoped_rules<'a>(
    active_rules: &'a [CandidateReviewRule],
    candidate: Option<&'a CandidateReviewRule>,
    domain: &str,
    file_path: &str,
    active_middleware: &[String],
) -> Vec<&'a CandidateReviewRule> {
    let mut matched: Vec<&'a CandidateReviewRule> = active_rules
        .iter()
        .filter(|r| {
            r.state == RuleLifecycleState::Active
                && r.scope.matches(domain, file_path, active_middleware)
        })
        .collect();
    if let Some(cand) = candidate {
        if cand.scope.matches(domain, file_path, active_middleware) {
            matched.push(cand);
        }
    }
    matched
}

/// Evaluates `candidate` alongside `active_rules` against the pinned `suite`.
///
/// - Empty suites, `repeat_runs == 0`, timeouts, or unparseable outputs yield
///   `Unresolved` and fail closed (`passed: false`).
/// - Every declared `repeat_runs` iteration must pass; a single failing or
///   unresolved iteration fails the receipt.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_replay_suite(
    suite: &ReplaySuite,
    active_memory_version: u64,
    active_rules: &[CandidateReviewRule],
    candidate: &CandidateReviewRule,
    backend_id: &str,
    model_id: &str,
    prompt_hash: &str,
    retrieval_policy: &str,
    repeat_runs: u32,
) -> Result<ReplayEvaluationReceipt> {
    suite.validate()?;
    candidate.validate()?;
    if repeat_runs == 0 {
        return Err(EngineError::Config(
            "replay evaluation requires repeat_runs >= 1 declared in advance".into(),
        ));
    }
    let pin = EvaluationPin {
        suite_id: suite.suite_id.clone(),
        suite_version: suite.version,
        suite_digest: suite.digest(),
        active_memory_version,
        candidate_id: candidate.candidate_id.clone(),
        candidate_revision: candidate.revision,
        candidate_digest: candidate.digest(),
        backend_id: backend_id.to_string(),
        model_id: model_id.to_string(),
        prompt_hash: prompt_hash.to_string(),
        retrieval_policy: retrieval_policy.to_string(),
        repeat_runs,
    };

    let mut case_outcomes = Vec::new();
    let mut unresolved_cases = 0usize;
    let mut failed_cases = 0usize;

    for run_index in 0..repeat_runs {
        for case in &suite.cases {
            let scoped_rules = retrieve_scoped_rules(
                active_rules,
                Some(candidate),
                &case.domain,
                &case.file_path,
                &case.middleware_tags,
            );
            let effective_input = EffectiveMemoryInput {
                active_memory_version,
                retrieved_rule_ids: scoped_rules
                    .iter()
                    .map(|r| r.candidate_id.clone())
                    .collect(),
                candidate_included: scoped_rules
                    .iter()
                    .any(|r| r.candidate_id == candidate.candidate_id),
            };

            match case.mode {
                SimulatedCaseMode::Timeout => {
                    unresolved_cases += 1;
                    case_outcomes.push(ReplayCaseOutcome {
                        case_id: case.case_id.clone(),
                        run_index,
                        status: ReplayCaseStatus::Unresolved,
                        effective_input,
                        emitted_findings: Vec::new(),
                        missed_required: Vec::new(),
                        emitted_prohibited: Vec::new(),
                        unresolved_reason: Some("reviewer execution timed out".into()),
                    });
                    continue;
                }
                SimulatedCaseMode::UnparseableOutput => {
                    unresolved_cases += 1;
                    case_outcomes.push(ReplayCaseOutcome {
                        case_id: case.case_id.clone(),
                        run_index,
                        status: ReplayCaseStatus::Unresolved,
                        effective_input,
                        emitted_findings: Vec::new(),
                        missed_required: Vec::new(),
                        emitted_prohibited: Vec::new(),
                        unresolved_reason: Some("reviewer output was unparseable".into()),
                    });
                    continue;
                }
                SimulatedCaseMode::ZeroFindings | SimulatedCaseMode::Normal => {}
            }

            let emitted_findings: Vec<SimulatedFinding> =
                if case.mode == SimulatedCaseMode::ZeroFindings {
                    Vec::new()
                } else {
                    case.raw_findings
                        .iter()
                        .filter(|finding| {
                            !scoped_rules.iter().any(|rule| {
                                let key_matches =
                                    rule.suppress_behavior_keys.contains(&finding.behavior_key);
                                let mitigation_ok = match &rule.require_mitigation_tag {
                                    Some(tag) => case.middleware_tags.contains(tag),
                                    None => true,
                                };
                                key_matches && mitigation_ok
                            })
                        })
                        .cloned()
                        .collect()
                };

            let mut missed_required = Vec::new();
            for req in &case.required_findings {
                if !emitted_findings.iter().any(|f| req.matches_finding(f)) {
                    missed_required.push(format!("{}:{}", req.behavior_key, req.evidence_target));
                }
            }
            let mut emitted_prohibited = Vec::new();
            for proh in &case.prohibited_findings {
                if emitted_findings.iter().any(|f| proh.matches_finding(f)) {
                    emitted_prohibited
                        .push(format!("{}:{}", proh.behavior_key, proh.evidence_target));
                }
            }

            let status = if missed_required.is_empty() && emitted_prohibited.is_empty() {
                ReplayCaseStatus::Passed
            } else {
                failed_cases += 1;
                ReplayCaseStatus::FailedExpectation
            };

            case_outcomes.push(ReplayCaseOutcome {
                case_id: case.case_id.clone(),
                run_index,
                status,
                effective_input,
                emitted_findings,
                missed_required,
                emitted_prohibited,
                unresolved_reason: None,
            });
        }
    }

    let passed = unresolved_cases == 0 && failed_cases == 0 && !case_outcomes.is_empty();
    Ok(ReplayEvaluationReceipt {
        pin,
        passed,
        unresolved_cases,
        failed_cases,
        case_outcomes,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HumanPromotionApproval {
    pub approver: String,
    pub approved_candidate_id: String,
    pub approved_candidate_digest: String,
    pub approved_receipt_digest: String,
    pub rationale: String,
    pub approved_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "event", deny_unknown_fields)]
pub enum PromotionAuditEvent {
    Promoted {
        version: u64,
        candidate_id: String,
        candidate_digest: String,
        receipt_digest: String,
        approver: String,
    },
    Refused {
        active_version: u64,
        candidate_id: String,
        reason: String,
    },
    RolledBack {
        from_version: u64,
        restored_version: u64,
        actor: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewMemoryRegistry {
    pub active_version: u64,
    pub active_rules: Vec<CandidateReviewRule>,
    pub history_snapshots: Vec<(u64, Vec<CandidateReviewRule>)>,
    pub feedback_log: Vec<ReviewFindingFeedback>,
    pub candidates: Vec<CandidateReviewRule>,
    pub evaluation_receipts: Vec<ReplayEvaluationReceipt>,
    pub audit_log: Vec<PromotionAuditEvent>,
}

impl ReviewMemoryRegistry {
    pub fn record_feedback(&mut self, feedback: ReviewFindingFeedback) -> Result<()> {
        feedback.validate()?;
        self.feedback_log.push(feedback);
        Ok(())
    }

    pub fn register_candidate(&mut self, mut candidate: CandidateReviewRule) -> Result<()> {
        candidate.validate()?;
        candidate.state = RuleLifecycleState::Draft;
        self.candidates.push(candidate);
        Ok(())
    }

    /// Promotes `candidate` into `active_rules` only if `receipt` passed with
    /// zero unresolved cases, matches the current `suite`, `candidate`, and
    /// `active_version`, and carries a matching `HumanPromotionApproval`.
    pub fn promote_candidate_rule(
        &mut self,
        candidate: &CandidateReviewRule,
        suite: &ReplaySuite,
        receipt: &ReplayEvaluationReceipt,
        approval: Option<&HumanPromotionApproval>,
    ) -> Result<u64> {
        self.evaluation_receipts.push(receipt.clone());
        let refuse = |registry: &mut Self, reason: &str| -> Result<u64> {
            registry.audit_log.push(PromotionAuditEvent::Refused {
                active_version: registry.active_version,
                candidate_id: candidate.candidate_id.clone(),
                reason: reason.to_string(),
            });
            Err(EngineError::Config(reason.to_string()))
        };

        if let Err(err) = candidate.validate() {
            return refuse(self, &format!("invalid candidate rule: {err}"));
        }
        if let Err(err) = suite.validate() {
            return refuse(self, &format!("invalid replay suite: {err}"));
        }
        if !receipt.passed || receipt.unresolved_cases > 0 || receipt.failed_cases > 0 {
            return refuse(
                self,
                "replay evaluation did not pass all required and prohibited expectations",
            );
        }
        let candidate_digest = candidate.digest();
        let suite_digest = suite.digest();
        if receipt.pin.active_memory_version != self.active_version
            || receipt.pin.candidate_id != candidate.candidate_id
            || receipt.pin.candidate_revision != candidate.revision
            || receipt.pin.candidate_digest != candidate_digest
            || receipt.pin.suite_id != suite.suite_id
            || receipt.pin.suite_version != suite.version
            || receipt.pin.suite_digest != suite_digest
        {
            return refuse(
                self,
                "replay receipt is stale relative to current candidate, suite, or active memory version",
            );
        }
        let Some(approval) = approval else {
            return refuse(
                self,
                "candidate rule promotion requires explicit human approval",
            );
        };
        let receipt_digest = receipt.digest();
        if approval.approver.trim().is_empty()
            || approval.rationale.trim().is_empty()
            || approval.approved_candidate_id != candidate.candidate_id
            || approval.approved_candidate_digest != candidate_digest
            || approval.approved_receipt_digest != receipt_digest
        {
            return refuse(
                self,
                "human promotion approval does not match candidate or replay receipt digest",
            );
        }

        // Preserve prior snapshot for audited rollback before mutating active rules.
        self.history_snapshots
            .push((self.active_version, self.active_rules.clone()));
        if let Some(superseded_id) = &candidate.supersedes_rule_id {
            for rule in &mut self.active_rules {
                if &rule.candidate_id == superseded_id {
                    rule.state = RuleLifecycleState::Superseded;
                }
            }
        }
        let mut promoted = candidate.clone();
        promoted.state = RuleLifecycleState::Active;
        self.active_rules.push(promoted);
        self.active_version += 1;
        self.audit_log.push(PromotionAuditEvent::Promoted {
            version: self.active_version,
            candidate_id: candidate.candidate_id.clone(),
            candidate_digest,
            receipt_digest,
            approver: approval.approver.clone(),
        });
        Ok(self.active_version)
    }

    /// Rolls back to the most recent prior snapshot while preserving full audit
    /// and evaluation history.
    pub fn rollback_active_version(&mut self, actor: &str, reason: &str) -> Result<u64> {
        if actor.trim().is_empty() || reason.trim().is_empty() {
            return Err(EngineError::Config(
                "rollback requires non-empty actor and reason".into(),
            ));
        }
        let Some((prior_version, prior_rules)) = self.history_snapshots.pop() else {
            return Err(EngineError::Config(
                "no prior review memory version available to roll back".into(),
            ));
        };
        let from_version = self.active_version;
        self.active_rules = prior_rules;
        self.active_version = prior_version;
        self.audit_log.push(PromotionAuditEvent::RolledBack {
            from_version,
            restored_version: prior_version,
            actor: actor.to_string(),
            reason: reason.to_string(),
        });
        Ok(self.active_version)
    }

    /// Exports the registry as a secret-scrubbed JSON bundle.
    pub fn export_review_memory_bundle(&self) -> Result<serde_json::Value> {
        let raw = serde_json::to_string(self)?;
        let scrubbed = crate::scrub::scrub(&raw);
        let value = serde_json::from_str(&scrubbed)?;
        Ok(value)
    }
}

fn canonical_sha256<T: Serialize>(value: &T) -> String {
    use std::fmt::Write;
    let bytes = serde_json::to_vec(value).expect("serializable governance value");
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

    fn auth_replay_suite() -> ReplaySuite {
        ReplaySuite {
            suite_id: "auth-review-suite".into(),
            version: 1,
            reviewed_by: "security-reviewer".into(),
            cases: vec![
                ReplayCase {
                    case_id: "protected-route-with-middleware".into(),
                    domain: "http-auth".into(),
                    file_path: "crates/server/src/routes/protected.rs".into(),
                    middleware_tags: vec!["require_bearer_auth".into()],
                    diff_text: "+ async fn protected_handler() {}".into(),
                    raw_findings: vec![
                        SimulatedFinding {
                            behavior_key: "missing-route-authentication".into(),
                            evidence_target: "protected_handler".into(),
                            summary: "Route handler lacks explicit authentication check".into(),
                        },
                        SimulatedFinding {
                            behavior_key: "missing-resource-ownership-check".into(),
                            evidence_target: "protected_handler:account_id".into(),
                            summary: "Handler does not verify caller owns account_id".into(),
                        },
                    ],
                    required_findings: vec![ReplayExpectation {
                        behavior_key: "missing-resource-ownership-check".into(),
                        evidence_target: "protected_handler:account_id".into(),
                        description: "Must still detect missing resource ownership check".into(),
                        accepted_wording_variants: vec!["owns account_id".into()],
                    }],
                    prohibited_findings: vec![ReplayExpectation {
                        behavior_key: "missing-route-authentication".into(),
                        evidence_target: "protected_handler".into(),
                        description:
                            "Must not emit false-positive missing-auth on middleware-protected route"
                                .into(),
                        accepted_wording_variants: vec![],
                    }],
                    mode: SimulatedCaseMode::Normal,
                },
                ReplayCase {
                    case_id: "genuinely-unprotected-route".into(),
                    domain: "http-auth".into(),
                    file_path: "crates/server/src/routes/public_admin.rs".into(),
                    middleware_tags: vec![],
                    diff_text: "+ async fn admin_delete() {}".into(),
                    raw_findings: vec![SimulatedFinding {
                        behavior_key: "missing-route-authentication".into(),
                        evidence_target: "admin_delete".into(),
                        summary: "Unprotected admin route lacks authentication".into(),
                    }],
                    required_findings: vec![ReplayExpectation {
                        behavior_key: "missing-route-authentication".into(),
                        evidence_target: "admin_delete".into(),
                        description: "Must still report genuinely unprotected route".into(),
                        accepted_wording_variants: vec!["lacks authentication".into()],
                    }],
                    prohibited_findings: vec![],
                    mode: SimulatedCaseMode::Normal,
                },
            ],
        }
    }

    fn rejected_false_positive_feedback() -> ReviewFindingFeedback {
        ReviewFindingFeedback {
            feedback_id: "fb-auth-1".into(),
            original_finding: OriginalFinding {
                finding_id: "finding-101".into(),
                behavior_key: "missing-route-authentication".into(),
                evidence_target: "protected_handler".into(),
                file_path: "crates/server/src/routes/protected.rs".into(),
                summary: "Route handler lacks explicit authentication check".into(),
            },
            decision: FeedbackDecision::Reject,
            disposition: FeedbackDisposition::ExistingMitigation,
            rationale: "Router layer already wrapscrates/server/src/routes/protected.rs with require_bearer_auth middleware".into(),
            actor: "operator-alice".into(),
            source_review_revision: "rev-10".into(),
            source_code_revision: "sha-abc123".into(),
            scope: RuleScope {
                domain: "http-auth".into(),
                path_prefixes: vec!["crates/server/src/routes/".into()],
                middleware_tags: vec!["require_bearer_auth".into()],
            },
            recorded_at: "2026-11-01T12:00:00Z".into(),
        }
    }

    #[test]
    fn review_memory_promotion_narrow_candidate_passes_replay_and_activates_only_after_human_approval(
    ) {
        let mut registry = ReviewMemoryRegistry::default();
        let feedback = rejected_false_positive_feedback();
        registry.record_feedback(feedback.clone()).unwrap();

        let candidate = derive_candidate_from_feedback(
            &feedback,
            "cand-auth-middleware-1",
            Some("require_bearer_auth".into()),
        )
        .unwrap()
        .expect("narrow candidate derived");
        assert_eq!(candidate.state, RuleLifecycleState::Draft);
        registry.register_candidate(candidate.clone()).unwrap();

        let suite = auth_replay_suite();
        let receipt = evaluate_replay_suite(
            &suite,
            registry.active_version,
            &registry.active_rules,
            &candidate,
            "mock-fixture",
            "claude-sonnet-4-6",
            "prompt-v1",
            "scoped-exact-v1",
            2,
        )
        .unwrap();
        assert!(receipt.passed);
        assert_eq!(receipt.unresolved_cases, 0);
        assert_eq!(receipt.failed_cases, 0);
        // Evaluation alone never activates the rule.
        assert_eq!(registry.active_version, 0);
        assert!(registry.active_rules.is_empty());

        // Attempting promotion without human approval is refused.
        assert!(registry
            .promote_candidate_rule(&candidate, &suite, &receipt, None)
            .is_err());
        assert_eq!(registry.active_version, 0);

        let approval = HumanPromotionApproval {
            approver: "operator-alice".into(),
            approved_candidate_id: candidate.candidate_id.clone(),
            approved_candidate_digest: candidate.digest(),
            approved_receipt_digest: receipt.digest(),
            rationale: "Verified on auth-review-suite v1".into(),
            approved_at: "2026-11-01T12:05:00Z".into(),
        };
        let new_version = registry
            .promote_candidate_rule(&candidate, &suite, &receipt, Some(&approval))
            .unwrap();
        assert_eq!(new_version, 1);
        assert_eq!(registry.active_rules.len(), 1);
        assert_eq!(registry.active_rules[0].state, RuleLifecycleState::Active);
    }

    #[test]
    fn review_memory_promotion_overbroad_or_zero_finding_reviewer_fails_replay_and_cannot_promote()
    {
        let mut registry = ReviewMemoryRegistry::default();
        let suite = auth_replay_suite();

        // Overbroad candidate suppresses missing-route-authentication even when
        // `require_bearer_auth` middleware is absent!
        let mut overbroad_feedback = rejected_false_positive_feedback();
        overbroad_feedback.scope.middleware_tags.clear();
        let overbroad = derive_candidate_from_feedback(&overbroad_feedback, "cand-overbroad", None)
            .unwrap()
            .unwrap();

        let receipt = evaluate_replay_suite(
            &suite,
            registry.active_version,
            &registry.active_rules,
            &overbroad,
            "mock-fixture",
            "claude-sonnet-4-6",
            "prompt-v1",
            "scoped-exact-v1",
            1,
        )
        .unwrap();
        assert!(!receipt.passed);
        assert_eq!(receipt.failed_cases, 1);
        let approval = HumanPromotionApproval {
            approver: "operator-alice".into(),
            approved_candidate_id: overbroad.candidate_id.clone(),
            approved_candidate_digest: overbroad.digest(),
            approved_receipt_digest: receipt.digest(),
            rationale: "Attempted force approve".into(),
            approved_at: "2026-11-01T12:10:00Z".into(),
        };
        assert!(registry
            .promote_candidate_rule(&overbroad, &suite, &receipt, Some(&approval))
            .is_err());
        assert_eq!(registry.active_version, 0);

        // A reviewer returning zero findings also fails required-finding cases.
        let mut zero_suite = suite.clone();
        zero_suite.cases[0].mode = SimulatedCaseMode::ZeroFindings;
        let narrow = derive_candidate_from_feedback(
            &rejected_false_positive_feedback(),
            "cand-narrow",
            Some("require_bearer_auth".into()),
        )
        .unwrap()
        .unwrap();
        let zero_receipt = evaluate_replay_suite(
            &zero_suite,
            0,
            &[],
            &narrow,
            "mock-fixture",
            "claude-sonnet-4-6",
            "prompt-v1",
            "scoped-exact-v1",
            1,
        )
        .unwrap();
        assert!(!zero_receipt.passed);
        assert!(zero_receipt.failed_cases >= 1);
    }

    #[test]
    fn review_memory_promotion_edited_and_ambiguous_feedback_and_out_of_scope_isolation() {
        let mut edited = rejected_false_positive_feedback();
        edited.decision = FeedbackDecision::Edit {
            edited_wording:
                "Verify caller ownership of account_id even when bearer auth middleware is present"
                    .into(),
        };
        edited.validate().unwrap();
        let candidate = derive_candidate_from_feedback(
            &edited,
            "cand-edited",
            Some("require_bearer_auth".into()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(candidate.state, RuleLifecycleState::Draft);
        assert!(candidate.guidance_text.contains("Verify caller ownership"));

        // Ambiguous feedback stays evidence without generating a candidate rule.
        let mut ambiguous = rejected_false_positive_feedback();
        ambiguous.disposition = FeedbackDisposition::Ambiguous;
        assert!(
            derive_candidate_from_feedback(&ambiguous, "cand-ambiguous", None)
                .unwrap()
                .is_none()
        );

        // Out-of-scope rules cannot leak into unrelated paths or domains.
        let mut out_of_scope_rule = candidate.clone();
        out_of_scope_rule.candidate_id = "cand-billing-only".into();
        out_of_scope_rule.state = RuleLifecycleState::Active;
        out_of_scope_rule.scope = RuleScope {
            domain: "billing".into(),
            path_prefixes: vec!["crates/billing/".into()],
            middleware_tags: vec![],
        };
        let rules = [out_of_scope_rule];
        let retrieved = retrieve_scoped_rules(
            &rules,
            None,
            "http-auth",
            "crates/server/src/routes/protected.rs",
            &["require_bearer_auth".into()],
        );
        assert!(retrieved.is_empty());
    }

    #[test]
    fn review_memory_promotion_stale_receipt_unresolved_replay_rollback_and_redacted_export() {
        let mut registry = ReviewMemoryRegistry::default();
        let feedback = rejected_false_positive_feedback();
        let candidate = derive_candidate_from_feedback(
            &feedback,
            "cand-auth-1",
            Some("require_bearer_auth".into()),
        )
        .unwrap()
        .unwrap();
        let suite = auth_replay_suite();

        // Timeout / unparseable replay cases are Unresolved and prevent promotion.
        let mut timeout_suite = suite.clone();
        timeout_suite.cases[1].mode = SimulatedCaseMode::Timeout;
        let unresolved_receipt = evaluate_replay_suite(
            &timeout_suite,
            0,
            &[],
            &candidate,
            "mock-fixture",
            "claude-sonnet-4-6",
            "prompt-v1",
            "scoped-exact-v1",
            1,
        )
        .unwrap();
        assert!(!unresolved_receipt.passed);
        assert_eq!(unresolved_receipt.unresolved_cases, 1);

        // Valid promotion followed by stale receipt detection and rollback.
        let receipt = evaluate_replay_suite(
            &suite,
            0,
            &[],
            &candidate,
            "mock-fixture",
            "claude-sonnet-4-6",
            "prompt-v1",
            "scoped-exact-v1",
            1,
        )
        .unwrap();
        let approval = HumanPromotionApproval {
            approver: "operator-alice".into(),
            approved_candidate_id: candidate.candidate_id.clone(),
            approved_candidate_digest: candidate.digest(),
            approved_receipt_digest: receipt.digest(),
            rationale: "Initial promotion".into(),
            approved_at: "2026-11-01T12:20:00Z".into(),
        };
        assert_eq!(
            registry
                .promote_candidate_rule(&candidate, &suite, &receipt, Some(&approval))
                .unwrap(),
            1
        );

        // Mutating the candidate after replay makes the receipt stale.
        let mut mutated = candidate.clone();
        mutated.guidance_text.push_str(" (mutated)");
        assert!(registry
            .promote_candidate_rule(&mutated, &suite, &receipt, Some(&approval))
            .is_err());
        assert_eq!(registry.active_version, 1);

        // Rollback restores version 0 while keeping the audit trail.
        assert_eq!(
            registry
                .rollback_active_version("operator-alice", "Revert rule after post-merge review")
                .unwrap(),
            0
        );
        assert!(registry.active_rules.is_empty());
        assert_eq!(registry.audit_log.len(), 3);

        // Export scrubs credential tokens.
        let synthetic_key = format!("{}{}", "sk-ant-api03-", "secretsecretsecretsecret123456");
        let mut secret_fb = feedback;
        secret_fb.rationale = format!("Bearer {synthetic_key}");
        registry.record_feedback(secret_fb).unwrap();
        let exported = registry.export_review_memory_bundle().unwrap();
        let text = serde_json::to_string(&exported).unwrap();
        assert!(!text.contains(&synthetic_key));
    }
}
