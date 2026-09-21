//! Host-selected stage evidence. This API accepts observed receipts and approved
//! objects, never paths to worker manifests, transcripts or provider state.
use super::evidence::FrozenEvidence;
use super::lifecycle::{Outcome, Policy, Record};
use super::protocol::*;
use super::snapshot::SourceSnapshot;
use crate::pack::evaluator::{Kind, PinnedRegistration};
use crate::types::Plan;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Engine-observed execution metadata. Do not construct these from a worker's
/// report or infer an assertion count from the subprocess exit code.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedCheck {
    pub check_id: Id,
    pub run_id: Id,
    pub sequence: u64,
    pub checked_content: Digest,
    pub environment: Digest,
    pub command: String,
    pub exit_code: Option<i32>,
    pub assertions_executed: Option<u64>,
    /// Bounded, scrubbed process output; never an assertion-count claim.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_summary: Option<String>,
}

/// Selected by stage policy, separately from whichever receipts happen to exist.
pub struct RequiredCheck {
    pub id: Id,
    pub command: String,
    pub require_assertions: bool,
}

#[derive(Clone)]
pub struct Checks<'a> {
    pub environment: Digest,
    pub required: &'a [RequiredCheck],
    pub observed: &'a [ObservedCheck],
}

/// Identifies an independently accepted feature without importing its worker
/// report. The stage driver resolves these from existing engine events.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureReceipt {
    pub feature_id: Id,
    pub worker_run_id: Id,
    pub candidate_commit: GitObject,
    pub independent_validation_attempt: Id,
}

#[derive(Clone)]
pub enum StageInput<'a> {
    Plan {
        revision: u64,
        base_commit: GitObject,
    },
    Invocation(&'a crate::live_permission::Request),
    Milestone {
        milestone_id: Id,
        plan_index: usize,
        snapshot: &'a SourceSnapshot,
    },
    Deliverable {
        snapshot: &'a SourceSnapshot,
        features: &'a [FeatureReceipt],
    },
    Integration {
        snapshot: &'a SourceSnapshot,
        live_base_commit: GitObject,
        candidate_commit: GitObject,
        integration_tree: GitObject,
    },
}

impl StageInput<'_> {
    pub(crate) fn stage(&self) -> Stage {
        match self {
            Self::Plan { .. } => Stage::PlanApproval,
            Self::Invocation(_) => Stage::CommandPermission,
            Self::Milestone { .. } => Stage::MilestoneValidation,
            Self::Deliverable { .. } => Stage::FinalGate,
            Self::Integration { .. } => Stage::Merge,
        }
    }
}

pub struct BuildInput<'a> {
    pub mission_id: Id,
    pub evaluation_id: Id,
    pub attempt_id: Id,
    pub workspace_id: Id,
    /// The exact engine-approved serialization, not a freshly rendered plan.
    pub plan_bytes: &'a [u8],
    pub policy: Policy,
    pub registration: &'a PinnedRegistration,
    pub stage: StageInput<'a>,
    pub checks: Checks<'a>,
    /// Existing stage diagnostics retain their own verdict and posture. They
    /// are not command exits and never satisfy required command receipts.
    pub diagnostics: &'a [crate::gate::GateReport],
    /// Previously validated independent results selected from the engine log.
    pub prior_findings: &'a [&'a Record],
    pub source_log_range: Option<LogRange>,
    pub deadline: DateTime<Utc>,
    pub limits: Limits,
}

pub struct BuiltInput {
    pub evidence: FrozenEvidence,
    pub policy: Policy,
    pub permission_request_id: Option<String>,
}

/// Freeze all hashes from the supplied bytes in one operation. Retrying a
/// checker reuses the returned evidence rather than rebuilding this input.
pub fn build(input: BuildInput<'_>) -> Result<BuiltInput, String> {
    if input.policy.kind != input.registration.declaration().kind
        || input.policy.enforcement != input.registration.declaration().enforcement
    {
        return Err("gate policy differs from the approved registration".into());
    }
    let plan: Plan = super::protocol::decode(input.plan_bytes)?;
    let mut parts = Parts::default();
    parts.add("plan", ArtifactRole::Plan, input.plan_bytes.to_vec(), None)?;
    parts.json(
        "scope",
        ArtifactRole::Scope,
        &serde_json::json!({"goal":plan.goal,"touchSet":plan.touch_set}),
    )?;
    parts.add(
        "registration",
        ArtifactRole::Registration,
        input.registration.bytes().to_vec(),
        None,
    )?;
    let plan_digest = Digest::of(input.plan_bytes);
    let mut permission_request_id = None;
    let (subject, checked_content) = match input.stage {
        StageInput::Plan {
            revision,
            base_commit,
        } => {
            parts.json(
                "criteria",
                ArtifactRole::Criteria,
                &plan.validation_contract,
            )?;
            (
                Subject::Plan {
                    revision,
                    plan_digest: plan_digest.clone(),
                    base_commit,
                },
                plan_digest.clone(),
            )
        }
        StageInput::Invocation(permission) => {
            permission.validate().map_err(|e| e.to_string())?;
            let workspace = super::lifecycle::workspace_id(&permission.binding.workspace);
            if permission.binding.mission_id != input.mission_id.as_str()
                || workspace != input.workspace_id
                || plan_digest.as_str() != format!("sha256:{}", permission.binding.plan_digest)
                || input.policy.mission_policy_digest.as_str()
                    != format!("sha256:{}", permission.binding.policy_digest)
                || input.deadline > permission.proposal.deadline
                || permission.proposal.prohibition.is_some()
            {
                return Err("invocation evidence does not match live permission authority".into());
            }
            let action_digest =
                parts.json("action", ArtifactRole::Action, &permission.proposal.action)?;
            let options_digest = parts.json(
                "permission-options",
                ArtifactRole::PermissionOptions,
                &permission.proposal.options,
            )?;
            permission_request_id = Some(permission.proposal.id.clone());
            (
                Subject::Invocation {
                    run_id: Id::try_from(permission.binding.run_id.clone())?,
                    peer_session_id: permission.proposal.peer_session_id.clone(),
                    tool_call_id: permission.proposal.tool_call_id.clone(),
                    peer_request_id: serde_json::from_value(
                        permission.proposal.peer_request_id.clone(),
                    )
                    .map_err(|e| e.to_string())?,
                    action_digest: action_digest.clone(),
                    options_digest,
                    cwd_id: workspace,
                },
                action_digest,
            )
        }
        StageInput::Milestone {
            milestone_id,
            plan_index,
            snapshot,
        } => {
            let milestone = plan
                .milestones
                .get(plan_index)
                .ok_or("milestone is absent from approved plan")?;
            let criteria = milestone
                .features
                .iter()
                .map(|f| &f.validation_criteria)
                .collect::<Vec<_>>();
            let criteria_digest = parts.json("criteria", ArtifactRole::Criteria, &criteria)?;
            let content = parts.snapshot(snapshot)?;
            (
                Subject::Milestone {
                    milestone_id,
                    start_commit: git_object(&snapshot.identity.base)?,
                    snapshot_digest: snapshot.identity.inventory_digest.clone(),
                    criteria_digest,
                },
                content,
            )
        }
        StageInput::Deliverable { snapshot, features } => {
            let mut ids = BTreeSet::new();
            for feature in features {
                if !ids.insert(&feature.feature_id) {
                    return Err("duplicate feature receipt".into());
                }
                if git_object(&feature.candidate_commit.value)? != feature.candidate_commit {
                    return Err("feature receipt Git algorithm does not match its object".into());
                }
            }
            if features.is_empty() {
                return Err("deliverable evidence requires accepted feature receipts".into());
            }
            let feature_receipt_digest =
                parts.json("feature-receipts", ArtifactRole::FeatureReceipts, &features)?;
            parts.json(
                "criteria",
                ArtifactRole::Criteria,
                &plan.validation_contract,
            )?;
            let content = parts.snapshot(snapshot)?;
            (
                Subject::Deliverable {
                    base_commit: git_object(&snapshot.identity.base)?,
                    snapshot_digest: snapshot.identity.inventory_digest.clone(),
                    feature_receipt_digest,
                },
                content,
            )
        }
        StageInput::Integration {
            snapshot,
            live_base_commit,
            candidate_commit,
            integration_tree,
        } => {
            if live_base_commit != git_object(&snapshot.identity.base)? {
                return Err("integration snapshot does not use the live base".into());
            }
            parts.json(
                "criteria",
                ArtifactRole::Criteria,
                &plan.validation_contract,
            )?;
            let content = parts.snapshot(snapshot)?;
            (
                Subject::Integration {
                    live_base_commit,
                    candidate_commit,
                    integration_tree,
                    snapshot_digest: snapshot.identity.inventory_digest.clone(),
                },
                content,
            )
        }
    };
    let mut policy = input.policy;
    policy.mechanical_prerequisites_passed &= parts.checks(&input.checks, &checked_content)?;
    if policy.kind == Kind::Judgment && !policy.mechanical_prerequisites_passed {
        return Err("judgment requires current, nonvacuous mechanical evidence".into());
    }
    parts.prior_findings(input.prior_findings, &input.mission_id)?;
    for (index, report) in input.diagnostics.iter().enumerate() {
        parts.json(
            &format!("diagnostic-{index}"),
            ArtifactRole::CheckReceipt,
            &serde_json::json!({"name":report.name,"kind":report.kind,
                "verdict":report.outcome.verdict,"reference":report.outcome.artefact.reference,
                "detail":report.outcome.artefact.detail,"ruleIds":report.outcome.rule_ids,
                "authority":"existing-stage-policy; diagnostic is not an execution receipt"}),
        )?;
    }
    let policy_digest = parts.json("policy", ArtifactRole::Policy, &policy)?;
    let subject_digest = parts.json("subject", ArtifactRole::Subject, &subject)?;
    let binding = Binding {
        subject_digest,
        plan_digest,
        policy_digest,
        registration_digest: input.registration.digest(),
        workspace_id: input.workspace_id,
    };
    let manifest_bytes = serde_json::to_vec(&Manifest {
        schema_version: 1,
        mission_id: input.mission_id.clone(),
        binding: binding.clone(),
        artifacts: parts.artifacts.into_values().collect(),
        source_log_range: input.source_log_range,
    })
    .map_err(|e| e.to_string())?;
    let request = Request {
        jsonrpc: "2.0".into(),
        id: input.attempt_id.clone(),
        method: "gate/evaluate".into(),
        params: Parameters {
            schema_version: 1,
            evaluation_id: input.evaluation_id,
            attempt_id: input.attempt_id,
            gate_id: input.registration.declaration().name.clone(),
            mission_id: input.mission_id,
            binding,
            stage: subject.stage(),
            subject,
            evidence: ArtifactRef {
                path: WirePath::try_from("inputs/manifest.json".to_string())?,
                digest: Digest::of(&manifest_bytes),
                bytes: manifest_bytes.len() as u64,
            },
            deadline: input.deadline.to_rfc3339_opts(SecondsFormat::Secs, true),
            limits: input.limits,
        },
    };
    let evidence = FrozenEvidence::new(
        serde_json::to_vec(&request).map_err(|e| e.to_string())?,
        manifest_bytes,
        parts.bytes,
        input.registration,
    )?;
    Ok(BuiltInput {
        evidence,
        policy,
        permission_request_id,
    })
}

#[derive(Default)]
struct Parts {
    artifacts: BTreeMap<Id, InputArtifact>,
    bytes: BTreeMap<Id, Vec<u8>>,
    total: usize,
}
impl Parts {
    fn add(
        &mut self,
        name: &str,
        role: ArtifactRole,
        bytes: Vec<u8>,
        producer: Option<Producer>,
    ) -> Result<Digest, String> {
        if self.artifacts.len() >= 1000 || bytes.len() > 64 * 1024 * 1024 {
            return Err("stage input exceeds evidence limits".into());
        }
        self.total = self
            .total
            .checked_add(bytes.len())
            .ok_or("input byte overflow")?;
        if self.total > 128 * 1024 * 1024 {
            return Err("stage input exceeds aggregate byte limit".into());
        }
        let id = Id::try_from(name.to_string())?;
        if self.artifacts.contains_key(&id) {
            return Err("duplicate stage input ID".into());
        }
        let digest = Digest::of(&bytes);
        self.artifacts.insert(
            id.clone(),
            InputArtifact {
                id: id.clone(),
                role,
                content: ArtifactRef {
                    path: WirePath::try_from(format!("inputs/{name}"))?,
                    digest: digest.clone(),
                    bytes: bytes.len() as u64,
                },
                producer: producer.unwrap_or(Producer {
                    kind: ProducerKind::Engine,
                    run_id: None,
                }),
            },
        );
        self.bytes.insert(id, bytes);
        Ok(digest)
    }
    fn json(
        &mut self,
        name: &str,
        role: ArtifactRole,
        value: &impl Serialize,
    ) -> Result<Digest, String> {
        self.add(
            name,
            role,
            serde_json::to_vec(value).map_err(|e| e.to_string())?,
            None,
        )
    }
    fn snapshot(&mut self, snapshot: &SourceSnapshot) -> Result<Digest, String> {
        if Digest::of(&snapshot.inventory) != snapshot.identity.inventory_digest
            || Digest::of(&snapshot.selection) != snapshot.identity.exclusions_digest
        {
            return Err("source snapshot identity changed".into());
        }
        self.add(
            "snapshot-inventory",
            ArtifactRole::SnapshotInventory,
            snapshot.inventory.clone(),
            None,
        )?;
        self.add(
            "source-selection",
            ArtifactRole::Context,
            snapshot.selection.clone(),
            None,
        )?;
        for (id, bytes) in &snapshot.files {
            self.add(id.as_str(), ArtifactRole::Source, bytes.clone(), None)?;
        }
        self.json("source-identity", ArtifactRole::Context, &snapshot.identity)
    }
    fn checks(&mut self, checks: &Checks<'_>, content: &Digest) -> Result<bool, String> {
        if checks.observed.len() > 256 || checks.required.len() > 256 {
            return Err("too many stage checks".into());
        }
        let mut required_ids = BTreeSet::new();
        let mut seen = BTreeSet::new();
        for receipt in checks.observed {
            if receipt
                .output_summary
                .as_ref()
                .is_some_and(|s| s.len() > 32768)
                || receipt.command.trim().is_empty()
                || receipt.command.len() > 8192
                || !seen.insert((&receipt.check_id, receipt.sequence))
            {
                return Err("invalid or duplicate observed check".into());
            }
            self.add(&label("check", &(&receipt.check_id, receipt.sequence))?, ArtifactRole::CheckReceipt,
                serde_json::to_vec(&serde_json::json!({"receipt":receipt,"current":receipt.checked_content == *content && receipt.environment == checks.environment})).map_err(|e| e.to_string())?,
                Some(Producer { kind: ProducerKind::Engine, run_id: Some(receipt.run_id.clone()) }))?;
        }
        let mut passed = true;
        for required in checks.required {
            if !required_ids.insert(&required.id)
                || required.command.trim().is_empty()
                || required.command.len() > 8192
            {
                return Err("invalid required check set".into());
            }
            let latest = checks
                .observed
                .iter()
                .filter(|r| r.check_id == required.id)
                .max_by_key(|r| r.sequence);
            passed &= latest.is_some_and(|r| {
                r.command == required.command
                    && r.checked_content == *content
                    && r.environment == checks.environment
                    && r.exit_code == Some(0)
                    && (!required.require_assertions
                        || r.assertions_executed.is_some_and(|n| n > 0))
            });
        }
        self.json("check-requirements", ArtifactRole::Context, &serde_json::json!({
            "checkedContent":content,"environment":checks.environment,
            "required":checks.required.iter().map(|r| serde_json::json!({"id":r.id,"command":r.command,"requireAssertions":r.require_assertions})).collect::<Vec<_>>(),
            "passed":passed,
        }))?;
        Ok(passed)
    }
    fn prior_findings(&mut self, records: &[&Record], mission: &Id) -> Result<(), String> {
        if records.len() > 128 {
            return Err("too many prior finding records".into());
        }
        for record in records {
            let params = &record.requested.request.params;
            if params.mission_id != *mission || record.requested.policy.kind != Kind::Judgment {
                return Err(
                    "prior findings require an independent result from this mission".into(),
                );
            }
            let finished = record
                .finished
                .as_ref()
                .ok_or("prior findings have no finished evaluation")?;
            let Outcome::Evaluated { result, .. } = &finished.outcome else {
                return Err("failed process is not a prior finding".into());
            };
            let mut verified = Record::new(
                record.requested.clone(),
                record.requested_at,
                record.requested_seq,
            )?;
            verified.finish(
                finished.clone(),
                record
                    .finished_at
                    .ok_or("prior result timestamp is missing")?,
            )?;
            self.add(&label("prior", &params.attempt_id)?, ArtifactRole::PriorFinding,
                serde_json::to_vec(&serde_json::json!({"evaluationId":params.evaluation_id,"attemptId":params.attempt_id,
                    "binding":result.binding,"evidenceDigest":result.evidence_digest,"findings":result.findings})).map_err(|e| e.to_string())?,
                Some(Producer { kind: ProducerKind::IndependentChecker, run_id: None }))?;
        }
        Ok(())
    }
}

pub(crate) fn git_object(value: &str) -> Result<GitObject, String> {
    let algorithm = match value.len() {
        40 => "sha1",
        64 => "sha256",
        _ => return Err("invalid source Git identity".into()),
    };
    if !value
        .bytes()
        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err("invalid source Git identity".into());
    }
    Ok(GitObject {
        algorithm: algorithm.into(),
        value: value.into(),
    })
}

fn label(prefix: &str, value: &impl Serialize) -> Result<String, String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    Ok(format!("{prefix}-{}", &Digest::of(&bytes).as_str()[7..]))
}
