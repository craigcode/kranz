//! Pure audit transitions. A checker result is evidence; stage disposition,
//! consent and consumption are separately authored by the engine.
use super::protocol::{
    Binding, Digest, EvaluationResult, Id, Request, Response, Stage, Status, SuccessResponse,
    Verdict, WirePath,
};
use crate::pack::evaluator::{Enforcement, Kind};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const MAX_ATTEMPTS: usize = 2048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    pub kind: Kind,
    pub enforcement: Enforcement,
    pub mission_policy_digest: Digest,
    /// Only nonwaivable prerequisites; advisory diagnostics do not become floors.
    pub mechanical_prerequisites_passed: bool,
}
impl Policy {
    pub fn digest(&self) -> Digest {
        Digest::of(&serde_json::to_vec(self).expect("typed policy is serializable"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetainedArtifact {
    /// Mission-relative, engine-selected path. Missing bytes remain unresolved.
    pub path: WirePath,
    pub raw_digest: Digest,
    pub retained_digest: Digest,
    pub retained_bytes: u64,
    pub transformation: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Requested {
    pub request: Request,
    pub policy: Policy,
    pub retained_inputs: Vec<RetainedArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_request_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Outcome {
    Evaluated {
        result: Box<EvaluationResult>,
        #[serde(rename = "rawStdoutDigest")]
        raw_stdout_digest: Digest,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Finished {
    pub attempt_id: Id,
    pub outcome: Outcome,
    pub exit_code: Option<i32>,
    pub cleanup_confirmed: bool,
    pub artifacts: Vec<RetainedArtifact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Disposition {
    Proceed,
    Block,
    RequireHuman,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Consent {
    pub actor: crate::live_permission::Actor,
    pub allow: bool,
    /// Existing authenticated stage decision, separate from checker output.
    pub reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Resolution {
    pub id: Id,
    pub attempt_id: Id,
    pub binding: Binding,
    pub disposition: Disposition,
    pub rationale: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent: Option<Consent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    ApprovePlan,
    AnswerPermission,
    AcceptMilestone,
    AcceptDeliverable,
    AdvanceLocalBase,
}
impl Action {
    fn stage(&self) -> Stage {
        match self {
            Self::ApprovePlan => Stage::PlanApproval,
            Self::AnswerPermission => Stage::CommandPermission,
            Self::AcceptMilestone => Stage::MilestoneValidation,
            Self::AcceptDeliverable => Stage::FinalGate,
            Self::AdvanceLocalBase => Stage::Merge,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Consumed {
    pub attempt_id: Id,
    pub resolution_id: Id,
    pub rechecked_binding: Binding,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub requested: Requested,
    pub requested_at: DateTime<Utc>,
    pub requested_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished: Option<Finished>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed: Option<Consumed>,
}
impl Record {
    pub fn new(requested: Requested, at: DateTime<Utc>, seq: u64) -> Result<Self, String> {
        requested.request.validate()?;
        if requested.policy.digest() != requested.request.params.binding.policy_digest {
            return Err("gate policy bytes differ from the request binding".into());
        }
        if requested.policy.kind == Kind::Judgment
            && !requested.policy.mechanical_prerequisites_passed
        {
            return Err("judgment requires successful mechanical prerequisites".into());
        }
        if at >= deadline(&requested.request)? {
            return Err("gate request is already expired".into());
        }
        validate_artifacts(&requested.retained_inputs)?;
        Ok(Self {
            requested,
            requested_at: at,
            requested_seq: seq,
            finished: None,
            finished_at: None,
            resolution: None,
            resolved_at: None,
            consumed: None,
        })
    }

    pub fn finish(&mut self, finished: Finished, at: DateTime<Utc>) -> Result<(), String> {
        let request = &self.requested.request;
        if self.finished.is_some()
            || finished.attempt_id != request.params.attempt_id
            || at < self.requested_at
        {
            return Err("duplicate, foreign or out-of-order gate result".into());
        }
        validate_artifacts(&finished.artifacts)?;
        match &finished.outcome {
            Outcome::Evaluated { result, .. } => {
                if at >= deadline(request)?
                    || !finished.cleanup_confirmed
                    || finished.exit_code != Some(0)
                {
                    return Err("evaluation expired or process completion was not proved".into());
                }
                let wire = Response::Result(Box::new(SuccessResponse {
                    jsonrpc: "2.0".into(),
                    id: finished.attempt_id.clone(),
                    result: *result.clone(),
                }));
                Response::from_bytes(&serde_json::to_vec(&wire).map_err(|e| e.to_string())?)?
                    .correlate(request)?;
            }
            Outcome::Error { message } => bounded(message, 8192)?,
        }
        self.finished = Some(finished);
        self.finished_at = Some(at);
        Ok(())
    }

    pub fn disposition(&self, consent: Option<&Consent>) -> Result<Disposition, String> {
        let finished = self
            .finished
            .as_ref()
            .ok_or("gate has no terminal attempt result")?;
        if let Some(consent) = consent {
            bounded(&consent.reference, 1024)?;
            match &consent.actor {
                crate::live_permission::Actor::Policy => {
                    return Err("policy is not operator consent".into())
                }
                crate::live_permission::Actor::SlackUser(id) => bounded(id, 256)?,
                _ => {}
            }
            if !consent.allow {
                return Ok(Disposition::Block);
            }
        }
        if !self.requested.policy.mechanical_prerequisites_passed || !finished.cleanup_confirmed {
            return Ok(Disposition::Block);
        }
        match &finished.outcome {
            Outcome::Evaluated { result, .. } if result.status == Status::Escalate => {
                return Ok(Disposition::RequireHuman)
            }
            Outcome::Evaluated { result, .. }
                if result.verdict == Some(Verdict::Fail)
                    && self.requested.policy.enforcement == Enforcement::Blocking =>
            {
                return Ok(Disposition::Block)
            }
            Outcome::Error { .. } if self.requested.policy.enforcement == Enforcement::Blocking => {
                return Ok(Disposition::Block)
            }
            _ => {}
        }
        if matches!(
            self.requested.request.params.stage,
            Stage::PlanApproval | Stage::CommandPermission | Stage::Merge
        ) && consent.is_none()
        {
            return Ok(Disposition::RequireHuman);
        }
        Ok(Disposition::Proceed)
    }

    pub fn resolve(&mut self, resolution: Resolution, at: DateTime<Utc>) -> Result<(), String> {
        let request = &self.requested.request;
        if self.finished_at.is_none_or(|finished| at < finished)
            || self.resolution.is_some()
            || resolution.attempt_id != request.params.attempt_id
            || resolution.binding != request.params.binding
        {
            return Err("duplicate, foreign or changed gate resolution".into());
        }
        bounded(&resolution.rationale, 8192)?;
        if resolution.disposition != self.disposition(resolution.consent.as_ref())? {
            return Err("gate disposition violates recorded policy or required consent".into());
        }
        self.resolution = Some(resolution);
        self.resolved_at = Some(at);
        Ok(())
    }

    pub fn consume(&mut self, consumed: Consumed, at: DateTime<Utc>) -> Result<(), String> {
        let resolution = self.resolution.as_ref().ok_or("gate has no resolution")?;
        let request = &self.requested.request;
        if self.consumed.is_some()
            || resolution.disposition != Disposition::Proceed
            || at >= deadline(request)?
            || self.resolved_at.is_none_or(|resolved| at < resolved)
            || consumed.attempt_id != request.params.attempt_id
            || consumed.resolution_id != resolution.id
            || consumed.rechecked_binding != request.params.binding
            || consumed.action.stage() != request.params.stage
        {
            return Err("stale, duplicate or mismatched gate consumption".into());
        }
        self.consumed = Some(consumed);
        Ok(())
    }
}

fn deadline(request: &Request) -> Result<DateTime<Utc>, String> {
    DateTime::parse_from_rfc3339(&request.params.deadline)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| e.to_string())
}
fn bounded(value: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > max {
        Err("invalid bounded gate text".into())
    } else {
        Ok(())
    }
}
fn validate_artifacts(artifacts: &[RetainedArtifact]) -> Result<(), String> {
    if artifacts.len() > 10_000 {
        return Err("gate retention inventory exceeds limit".into());
    }
    super::protocol::validate_paths(artifacts.iter().map(|a| a.path.as_str()))?;
    let mut names = std::collections::BTreeSet::new();
    for artifact in artifacts {
        if artifact.retained_bytes > 64 * 1024 * 1024
            || !artifact.path.as_str().starts_with("runs/gates/")
            || !names.insert(artifact.path.as_str().to_ascii_lowercase())
        {
            return Err("invalid retained gate artifact".into());
        }
        bounded(&artifact.transformation, 512)?;
    }
    Ok(())
}

pub(crate) fn fold(
    state: &mut crate::types::MissionState,
    event: &crate::events::Event,
) -> crate::error::Result<()> {
    use crate::events::EventKind;
    let invalid = |message: String| crate::error::EngineError::InvalidState(message);
    if event.mission_id != state.mission.id {
        return Err(invalid("foreign gate mission".into()));
    }
    match &event.kind {
        EventKind::GateEvaluationRequested { evaluation } => {
            let request = &evaluation.request;
            validate_stage_status(state, request.params.stage).map_err(invalid)?;
            if state.gate_evaluations.len() >= MAX_ATTEMPTS
                || request.params.mission_id.as_str() != event.mission_id
                || state
                    .gate_evaluations
                    .contains_key(request.params.attempt_id.as_str())
            {
                return Err(invalid("duplicate, foreign or excess gate request".into()));
            }
            validate_permission_join(state, evaluation).map_err(invalid)?;
            let record = Record::new(*evaluation.clone(), event.ts, event.seq).map_err(invalid)?;
            state
                .gate_evaluations
                .insert(request.params.attempt_id.as_str().into(), record);
        }
        EventKind::GateEvaluationFinished { evaluation } => {
            state
                .gate_evaluations
                .get_mut(evaluation.attempt_id.as_str())
                .ok_or_else(|| invalid("gate result has no request".into()))?
                .finish(*evaluation.clone(), event.ts)
                .map_err(invalid)?;
        }
        EventKind::GateResolutionRecorded { resolution } => {
            let record = state
                .gate_evaluations
                .get(resolution.attempt_id.as_str())
                .ok_or_else(|| invalid("gate resolution has no request".into()))?;
            if let (Some(id), Some(consent)) =
                (&record.requested.permission_request_id, &resolution.consent)
            {
                let actual = state
                    .permissions
                    .get(id)
                    .and_then(|permission| permission.resolution.as_ref())
                    .ok_or_else(|| invalid("permission consent is absent".into()))?;
                if actual.actor != consent.actor
                    || actual.allow != consent.allow
                    || consent.reference != *id
                {
                    return Err(invalid(
                        "gate cannot replace or relabel permission consent".into(),
                    ));
                }
            }
            if state.gate_evaluations.values().any(|record| {
                record
                    .resolution
                    .as_ref()
                    .is_some_and(|r| r.id == resolution.id)
            }) {
                return Err(invalid("gate resolution ID was reused".into()));
            }
            state
                .gate_evaluations
                .get_mut(resolution.attempt_id.as_str())
                .ok_or_else(|| invalid("gate resolution has no request".into()))?
                .resolve(resolution.clone(), event.ts)
                .map_err(invalid)?;
        }
        EventKind::GateResolutionConsumed { consumption } => {
            let record = state
                .gate_evaluations
                .get(consumption.attempt_id.as_str())
                .ok_or_else(|| invalid("gate consumption has no request".into()))?;
            validate_stage_status(state, record.requested.request.params.stage).map_err(invalid)?;
            validate_permission_join(state, &record.requested).map_err(invalid)?;
            if let Some(id) = &record.requested.permission_request_id {
                let permission = &state.permissions[id];
                let actual = permission
                    .resolution
                    .as_ref()
                    .ok_or_else(|| invalid("permission consent is absent".into()))?;
                let consent = record
                    .resolution
                    .as_ref()
                    .and_then(|r| r.consent.as_ref())
                    .ok_or_else(|| invalid("permission consent join is absent".into()))?;
                if !actual.allow
                    || !consent.allow
                    || actual.actor != consent.actor
                    || consent.reference != *id
                {
                    return Err(invalid(
                        "gate cannot replace or relabel permission consent".into(),
                    ));
                }
            }
            if state
                .consumed_gate_resolutions
                .contains(consumption.resolution_id.as_str())
            {
                return Err(invalid("gate resolution was already consumed".into()));
            }
            state
                .gate_evaluations
                .get_mut(consumption.attempt_id.as_str())
                .ok_or_else(|| invalid("gate consumption has no request".into()))?
                .consume(consumption.clone(), event.ts)
                .map_err(invalid)?;
            state
                .consumed_gate_resolutions
                .insert(consumption.resolution_id.as_str().into());
        }
        _ => return Err(invalid("not a gate lifecycle event".into())),
    }
    Ok(())
}

fn validate_permission_join(
    state: &crate::types::MissionState,
    evaluation: &Requested,
) -> Result<(), String> {
    use super::protocol::Subject;
    match (
        &evaluation.request.params.subject,
        &evaluation.permission_request_id,
    ) {
        (
            Subject::Invocation {
                run_id,
                peer_session_id,
                tool_call_id,
                peer_request_id,
                action_digest,
                options_digest,
                cwd_id,
            },
            Some(id),
        ) => {
            let record = state
                .permissions
                .get(id)
                .ok_or("gate invocation has no live permission request")?;
            let permission = &record.request;
            let run = state
                .runs
                .get(&permission.binding.run_id)
                .ok_or("permission run is absent")?;
            let workspace = workspace_id(&permission.binding.workspace);
            let peer = serde_json::to_value(peer_request_id).map_err(|e| e.to_string())?;
            if record.closed.is_some()
                || record.delivery.is_some()
                || run.ended_at.is_some()
                || evaluation.request.params.binding.workspace_id != workspace
                || *cwd_id != workspace
                || evaluation.request.params.binding.plan_digest.as_str()
                    != format!("sha256:{}", permission.binding.plan_digest)
                || evaluation.policy.mission_policy_digest.as_str()
                    != format!("sha256:{}", permission.binding.policy_digest)
                || deadline(&evaluation.request)? > permission.proposal.deadline
                || permission.binding.run_id != run_id.as_str()
                || permission.proposal.peer_session_id != peer_session_id.as_str()
                || permission.proposal.tool_call_id != tool_call_id.as_str()
                || permission.proposal.peer_request_id != peer
                || action_digest.as_str() != format!("sha256:{}", permission.proposal.action_digest)
                || options_digest.as_str()
                    != format!("sha256:{}", permission.proposal.options_digest)
            {
                return Err("gate invocation does not match its permission request".into());
            }
        }
        (Subject::Invocation { .. }, None) => {
            return Err("gate invocation requires a permission join".into())
        }
        (_, Some(_)) => return Err("only invocation evaluations may join a permission".into()),
        _ => {}
    }
    Ok(())
}

pub fn workspace_id(path: &str) -> Id {
    Id::try_from(format!(
        "workspace-{}",
        &Digest::of(path.as_bytes()).as_str()[7..]
    ))
    .expect("a SHA-256 workspace label is a valid opaque ID")
}

fn validate_stage_status(state: &crate::types::MissionState, stage: Stage) -> Result<(), String> {
    use crate::types::MissionStatus;
    let ready = match stage {
        Stage::PlanApproval => state.mission.status == MissionStatus::Planning,
        Stage::CommandPermission | Stage::MilestoneValidation => matches!(
            state.mission.status,
            MissionStatus::Running | MissionStatus::Validating
        ),
        Stage::FinalGate => state.mission.status == MissionStatus::Validating,
        Stage::Merge => state.mission.status == MissionStatus::Complete,
    };
    if ready {
        Ok(())
    } else {
        Err("gate stage does not match the mission lifecycle".into())
    }
}
