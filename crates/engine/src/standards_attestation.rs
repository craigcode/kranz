//! Authorized `manual-attestation` checker decisions (KRZ-346, D-F).
//!
//! An attestation is a positive checker verdict, not a waiver. It is bound to
//! one approval pin, rule revision, affected path set, and diff digest, so it
//! becomes inert after any relevant change or re-approval. Only recognized
//! human surfaces may record or satisfy it.

use crate::error::EngineError;
use crate::event_log::{EventLog, LockForce};
use crate::events::{Event, EventKind};
use crate::paths::MissionPaths;
use crate::types::{PinnedRule, StandardsPin};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttestationRecord {
    pub seq: u64,
    pub rule_id: String,
    pub rule_revision: u64,
    pub manifest_digest: String,
    pub approval_seq: u64,
    pub paths: Vec<String>,
    pub diff_digest: String,
    pub reason: String,
    pub approver: String,
    pub surface: String,
}

impl AttestationRecord {
    pub fn from_event(event: &Event) -> Option<Self> {
        let EventKind::StandardsAttestationApproved {
            rule_id,
            rule_revision,
            manifest_digest,
            approval_seq,
            paths,
            diff_digest,
            reason,
            approver,
            surface,
        } = &event.kind
        else {
            return None;
        };
        Some(Self {
            seq: event.seq,
            rule_id: rule_id.clone(),
            rule_revision: *rule_revision,
            manifest_digest: manifest_digest.clone(),
            approval_seq: *approval_seq,
            paths: paths.clone(),
            diff_digest: diff_digest.clone(),
            reason: reason.clone(),
            approver: approver.clone(),
            surface: surface.clone(),
        })
    }
}

fn approval_pin(events: &[Event], mission_id: &str) -> Option<(u64, StandardsPin)> {
    events
        .iter()
        .filter(|event| event.mission_id == mission_id)
        .filter_map(|event| match &event.kind {
            EventKind::PlanApproved { plan, .. } => plan
                .standards_manifest
                .as_deref()
                .map(|pin| (event.seq, pin.clone())),
            _ => None,
        })
        .next_back()
}

fn current_binding(
    repo: &crate::git_ops::GitRepo,
    pin: &StandardsPin,
    rule: &PinnedRule,
    base_ref: &str,
    head_ref: &str,
) -> crate::error::Result<(Vec<String>, String)> {
    let changed = repo.changed_paths(base_ref, head_ref)?;
    let paths =
        crate::standards_waiver::affected_paths_with_context(rule, &changed, &pin.context_paths);
    let diff = if rule.when_paths.is_empty() {
        repo.diff_full(base_ref, head_ref)?
    } else if paths.is_empty() {
        String::new()
    } else {
        repo.diff_range_paths(base_ref, head_ref, &paths)?
    };
    Ok((paths, crate::standards_waiver::sha256_hex(diff.as_bytes())))
}

#[allow(clippy::too_many_arguments)]
pub fn active_attestation(
    repo: &crate::git_ops::GitRepo,
    events: &[Event],
    mission_id: &str,
    pin: &StandardsPin,
    rule: &PinnedRule,
    base_ref: &str,
    head_ref: &str,
) -> crate::error::Result<Option<AttestationRecord>> {
    let Some((approval_seq, approved)) = approval_pin(events, mission_id) else {
        return Ok(None);
    };
    if approved != *pin || rule.checker.as_deref() != Some("manual-attestation") {
        return Ok(None);
    }
    let (paths, diff_digest) = current_binding(repo, pin, rule, base_ref, head_ref)?;
    Ok(events
        .iter()
        .filter(|event| event.mission_id == mission_id)
        .filter_map(AttestationRecord::from_event)
        .rfind(|record| {
            record.seq > approval_seq
                && record.rule_id == rule.id
                && record.rule_revision == rule.revision
                && record.manifest_digest == pin.digest
                && record.approval_seq == approval_seq
                && record.paths == paths
                && record.diff_digest == diff_digest
                && crate::standards_waiver::HUMAN_SURFACES.contains(&record.surface.as_str())
                && !record.approver.trim().is_empty()
        }))
}

/// Record the current positive human verdict for one manual-attestation rule.
pub fn approve_attestation(
    repo_root: &Path,
    mission_id: &str,
    rule_id: &str,
    reason: &str,
    surface: &str,
    force: LockForce,
) -> crate::error::Result<AttestationRecord> {
    if !MissionPaths::is_safe_id(mission_id) {
        return Err(EngineError::InvalidState(format!(
            "unsafe mission id `{mission_id}` — attestation targets must be one local mission id"
        )));
    }
    if !crate::standards_waiver::HUMAN_SURFACES.contains(&surface) {
        return Err(EngineError::InvalidState(format!(
            "surface `{surface}` is not authorized to approve a manual attestation"
        )));
    }
    if reason.trim().is_empty() {
        return Err(EngineError::InvalidState(
            "a manual attestation requires a reason".to_string(),
        ));
    }
    let paths = MissionPaths::new(repo_root, mission_id);
    let initial = EventLog::read_events(&paths.events_file())?;
    let initial_state = crate::reducer::fold(&initial)?;
    let mut log = EventLog::acquire(
        &paths,
        mission_id,
        Duration::from_millis(initial_state.config.event_stream_throttle_ms),
        force,
    )?;
    let events = EventLog::read_events(&paths.events_file())?;
    let mut state = crate::reducer::fold(&events)?;
    let (approval_seq, pin) = approval_pin(&events, mission_id).ok_or_else(|| {
        EngineError::InvalidState(format!(
            "mission `{mission_id}` has no approved standards pin"
        ))
    })?;
    let rule = pin
        .rules
        .iter()
        .find(|rule| rule.id == rule_id)
        .ok_or_else(|| {
            EngineError::InvalidState(format!(
                "rule `{rule_id}` is not in mission `{mission_id}`'s approved standards pin"
            ))
        })?;
    if rule.checker.as_deref() != Some("manual-attestation") {
        return Err(EngineError::InvalidState(format!(
            "rule `{rule_id}` uses checker `{}` rather than manual-attestation",
            rule.checker.as_deref().unwrap_or("<missing>")
        )));
    }
    let base = state.mission.base_sha.as_deref().ok_or_else(|| {
        EngineError::InvalidState(format!("mission `{mission_id}` has no pinned base sha"))
    })?;
    let repo = crate::git_ops::GitRepo::open(repo_root)?;
    let (affected, diff_digest) =
        current_binding(&repo, &pin, rule, base, &state.mission.mission_branch)?;
    if active_attestation(
        &repo,
        &events,
        mission_id,
        &pin,
        rule,
        base,
        &state.mission.mission_branch,
    )?
    .is_some()
    {
        return Err(EngineError::InvalidState(format!(
            "rule `{rule_id}` already has a current manual attestation for this exact diff"
        )));
    }
    let (event, audits) =
        log.append_with_redaction_audits(EventKind::StandardsAttestationApproved {
            rule_id: rule.id.clone(),
            rule_revision: rule.revision,
            manifest_digest: pin.digest.clone(),
            approval_seq,
            paths: affected,
            diff_digest,
            reason: reason.to_string(),
            approver: crate::standards_waiver::LOCAL_OPERATOR.to_string(),
            surface: surface.to_string(),
        })?;
    crate::reducer::apply(&mut state, &event)?;
    for audit in &audits {
        crate::reducer::apply(&mut state, audit)?;
    }
    crate::reducer::write_snapshot(&state, &paths.state_file())?;
    log.flush()?;
    AttestationRecord::from_event(&event).ok_or_else(|| {
        EngineError::InvalidState("recorded event was not a standards attestation".to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flight_rules_enforcement_attestation_event_round_trips() {
        let kind = EventKind::StandardsAttestationApproved {
            rule_id: "ZZ-MANUAL-001".to_string(),
            rule_revision: 2,
            manifest_digest: "ab".repeat(32),
            approval_seq: 3,
            paths: vec!["src/x.rs".to_string()],
            diff_digest: "cd".repeat(32),
            reason: "reviewed deployment evidence".to_string(),
            approver: crate::standards_waiver::LOCAL_OPERATOR.to_string(),
            surface: "cli".to_string(),
        };
        let json = serde_json::to_string(&kind).unwrap();
        assert!(json.contains("standards.attestation.approved"), "{json}");
        let back: EventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back.type_name(), "standards.attestation.approved");
    }
}
