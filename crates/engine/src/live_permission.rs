//! One-call ACP consent. Persisted records are evidence, never response handles.
//!
//! The backend owns the live invocation; the engine owns approval authority.
//! A bounded channel connects them without borrowing the session's output pump.

use crate::error::{EngineError, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

pub const MAX_PENDING: usize = 16;
pub const MAX_MISSION_REQUESTS: usize = 1024;
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const REQUEST_TTL_SECS: i64 = 300;

pub fn digest(value: &impl Serialize) -> Result<String> {
    Ok(Sha256::digest(serde_json::to_vec(value)?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Adapter observations, before the engine attaches mission authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub id: String,
    pub engine_session_id: String,
    pub peer_session_id: String,
    pub peer_request_id: Value,
    pub tool_call_id: String,
    pub action: Value,
    pub options: Vec<Value>,
    pub action_digest: String,
    pub options_digest: String,
    pub observed_at: DateTime<Utc>,
    pub deadline: DateTime<Utc>,
    /// A prohibition is not overridable by the consent UI.
    pub prohibition: Option<String>,
}

impl Proposal {
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty()
            || self.engine_session_id.is_empty()
            || self.peer_session_id.is_empty()
            || (self.tool_call_id.is_empty() && self.prohibition.is_none())
            || self.action_digest != digest(&self.action)?
            || self.options_digest != digest(&self.options)?
            || self.deadline <= self.observed_at
            || self.deadline > self.observed_at + chrono::Duration::seconds(REQUEST_TTL_SECS)
            || serde_json::to_vec(self)?.len() > MAX_REQUEST_BYTES
        {
            return Err(EngineError::Backend(
                "invalid live permission proposal".into(),
            ));
        }
        Ok(())
    }

    /// Check source values, including JSON keys and adapter option labels.
    /// Kept out of historical validation so old event logs still replay.
    pub fn ambiguous_display(&self) -> bool {
        serde_json::to_value(self)
            .map(|value| crate::presentation::has_ambiguous_json(&value))
            .unwrap_or(true)
    }

    /// IDs are opaque. Only a unique, offered, one-time kind supplies semantics.
    pub fn option(&self, allow: bool) -> Option<String> {
        let mut ids = std::collections::BTreeSet::new();
        if self.options.is_empty() || self.options.len() > 32 {
            return None;
        }
        for option in &self.options {
            let id = option.get("optionId")?.as_str()?;
            if id.trim().is_empty() || id.len() > 256 || !ids.insert(id) {
                return None;
            }
        }
        let kind = if allow { "allow_once" } else { "reject_once" };
        let mut candidates = self.options.iter().filter(|o| o["kind"] == kind);
        let selected = candidates.next()?.get("optionId")?.as_str()?;
        candidates.next().is_none().then(|| selected.to_owned())
    }
}

/// Engine-authored identity of the exact policy and workspace that spawned a run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    pub mission_id: String,
    pub run_id: String,
    pub workspace: String,
    pub plan_digest: String,
    pub policy_digest: String,
}

impl Binding {
    fn validate(&self) -> Result<()> {
        let hash = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if self.mission_id.trim().is_empty()
            || self.run_id.trim().is_empty()
            || !std::path::Path::new(&self.workspace).is_absolute()
            || !hash(&self.plan_digest)
            || !hash(&self.policy_digest)
        {
            return Err(EngineError::Backend(
                "incomplete live permission authority binding".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub proposal: Proposal,
    pub binding: Binding,
    pub binding_digest: String,
}

impl Request {
    pub fn new(proposal: Proposal, binding: Binding) -> Result<Self> {
        proposal.validate()?;
        binding.validate()?;
        let binding_digest = digest(&(&proposal, &binding))?;
        Ok(Self {
            proposal,
            binding,
            binding_digest,
        })
    }

    pub fn ambiguous_display(&self) -> bool {
        self.proposal.ambiguous_display()
            || serde_json::to_value(&self.binding)
                .map(|value| crate::presentation::has_ambiguous_json(&value))
                .unwrap_or(true)
    }

    pub fn validate(&self) -> Result<()> {
        self.proposal.validate()?;
        self.binding.validate()?;
        if self.binding_digest != digest(&(&self.proposal, &self.binding))? {
            return Err(EngineError::Backend("permission binding changed".into()));
        }
        Ok(())
    }
}

/// Capability attribution is deliberately distinct from a verified Slack user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "id")]
pub enum Actor {
    Policy,
    LocalRepositoryAuthority,
    LocalMutationCapability,
    SlackUser(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    pub request_id: String,
    pub binding_digest: String,
    pub allow: bool,
    pub actor: Actor,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Delivery {
    Sent,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub request: Request,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<Resolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responded_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<String>,
}

impl Record {
    pub fn pending(&self, now: DateTime<Utc>) -> bool {
        self.resolution.is_none() && self.closed.is_none() && now < self.request.proposal.deadline
    }

    pub fn validate_answer(
        &self,
        binding_digest: &str,
        allow: bool,
        now: DateTime<Utc>,
    ) -> Result<()> {
        if !self.pending(now)
            || self.request.binding_digest != binding_digest
            || (allow
                && (self.request.ambiguous_display()
                    || self.request.proposal.prohibition.is_some()
                    || self.request.proposal.option(true).is_none()))
        {
            return Err(EngineError::InvalidState(
                "permission is stale, expired, already resolved or prohibited".into(),
            ));
        }
        Ok(())
    }
}

pub(crate) fn fold(
    state: &mut crate::types::MissionState,
    event: &crate::events::Event,
) -> Result<()> {
    use crate::events::EventKind;
    let invalid =
        || EngineError::InvalidState("invalid or stale one-call permission transition".into());
    match &event.kind {
        EventKind::PermissionRequested { request } => {
            request.validate()?;
            let run = state
                .runs
                .get(&request.binding.run_id)
                .ok_or_else(invalid)?;
            if request.binding.mission_id != state.mission.id
                || request.proposal.engine_session_id != run.sdk_session_id
                || run.role != crate::types::Role::Worker
                || run.ended_at.is_some()
                || event.ts < request.proposal.observed_at
                || event.ts >= request.proposal.deadline
                || state.permissions.contains_key(&request.proposal.id)
                || state.permissions.len() >= MAX_MISSION_REQUESTS
                || state
                    .permissions
                    .values()
                    .filter(|r| r.closed.is_none() && r.delivery.is_none())
                    .count()
                    >= MAX_PENDING
            {
                return Err(invalid());
            }
            state.permissions.insert(
                request.proposal.id.clone(),
                Record {
                    request: request.clone(),
                    resolution: None,
                    resolved_at: None,
                    delivery: None,
                    responded_at: None,
                    closed: None,
                },
            );
        }
        EventKind::PermissionResolved { resolution } => {
            let record = state
                .permissions
                .get_mut(&resolution.request_id)
                .ok_or_else(invalid)?;
            if !record.pending(event.ts)
                || resolution.binding_digest != record.request.binding_digest
                || (resolution.allow
                    && (record.request.proposal.prohibition.is_some()
                        || record.request.proposal.option(true).is_none()
                        || resolution.actor == Actor::Policy))
                || resolution.reason.trim().is_empty()
                || matches!(&resolution.actor, Actor::SlackUser(id) if id.trim().is_empty())
            {
                return Err(invalid());
            }
            record.resolution = Some(resolution.clone());
            record.resolved_at = Some(event.ts);
        }
        EventKind::PermissionResponseRecorded {
            request_id,
            delivery,
        } => {
            let record = state.permissions.get_mut(request_id).ok_or_else(invalid)?;
            if record.resolution.is_none() || record.delivery.is_some() || record.closed.is_some() {
                return Err(invalid());
            }
            record.delivery = Some(delivery.clone());
            record.responded_at = Some(event.ts);
        }
        EventKind::PermissionClosed { request_id, reason } => {
            let record = state.permissions.get_mut(request_id).ok_or_else(invalid)?;
            if record.closed.is_some() || reason.trim().is_empty() {
                return Err(invalid());
            }
            record.closed = Some(reason.clone());
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

#[derive(Debug)]
pub(crate) struct Answer {
    pub proposal: Proposal,
    pub allow: bool,
}

#[derive(Debug, Clone)]
pub struct PermissionResponder(mpsc::Sender<Answer>);

impl PermissionResponder {
    /// The caller must durably record the matching resolution before queuing.
    /// Queued is not sent: the session emits a separate delivery receipt.
    pub fn respond(&self, proposal: &Proposal, allow: bool) -> Result<()> {
        if allow && (proposal.ambiguous_display() || proposal.prohibition.is_some()) {
            return Err(EngineError::InvalidState(
                "ambiguous or prohibited permission cannot be allowed".into(),
            ));
        }
        self.0
            .try_send(Answer {
                proposal: proposal.clone(),
                allow,
            })
            .map_err(|_| {
                EngineError::Backend("permission response channel unavailable or full".into())
            })
    }

    pub(crate) fn channel() -> (Self, mpsc::Receiver<Answer>) {
        let (tx, rx) = mpsc::channel(MAX_PENDING);
        (Self(tx), rx)
    }
}
