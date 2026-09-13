//! The Flight Rules human waiver decision (ticket
//! `.kranz/tickets/flight-rules-waiver-decisions.md`, KRZ-344; design
//! `docs/scoping/flight-rules-engineering-standards.md`, decision D-I —
//! "waivers are narrow human decisions"): the ONE authorized exception path
//! for a standards failure, and the fail-closed binding checks every
//! consumer shares.
//!
//! WHY a structured event and never orchestrator prose: the ordinary
//! finding-waiver is a model-discretion `orchestrator.decision` — a human
//! reviews the summary, but the decision text is free-form and the
//! authority is the planner's. That is insufficient authority for an
//! enforced organizational MUST (D-I). `standards.waiver.approved` is
//! recorded ONLY by an authenticated human surface (`kranz standards
//! waive`); a model may request a waiver or propose a fix, and there is
//! deliberately no model-driven approval path — no engine or backend code
//! constructs this event.
//!
//! WHY the binding is this narrow (D-I): the waiver names the pinned rule
//! id + revision + manifest digest + the `plan.approved` seq, the
//! fingerprint of the EXACT finding it subtracts, the affected paths, and
//! the sha256 over the affected-path diff (the whole diff for an unscoped
//! rule), plus reason, approver, and expiry. A change to the affected-path
//! diff, the rule revision, the finding fingerprint, or the pin — or the
//! expiry passing — invalidates the waiver and restores the block.
//! Unrelated paths receive no authority: a path-scoped rule's digest covers
//! only the diff under its `when-paths`. One waiver subtracts EXACTLY ONE
//! matching failure (the coverage fold consumes each waiver at most once);
//! it never disables a checker, an RFC, a domain, or a class, and engine
//! floor gates have no waiver slot at all. There is no `--ignore-standards`
//! switch, no auto-waive, no wildcard, no permanent default.
//!
//! WHY the fold can verify every clause except the diff digest: the
//! coverage fold is log-only (no git, no filesystem), so it joins on the
//! recorded rule/revision/digest/approval-seq/fingerprint/surface/expiry.
//! The DIFF binding is re-derived where a decision is taken against the
//! live tree — the record path below computes it at approval time, and the
//! enforcement binding (KRZ-346) recomputes it at gate time and compares.
//! This module owns both halves so the two surfaces can never drift apart
//! on what a waiver covers.

use crate::error::EngineError;
use crate::event_log::{EventLog, LockForce};
use crate::events::{Event, EventKind};
use crate::paths::MissionPaths;
use crate::reducer;
use crate::types::{Finding, PinnedRule, StandardsPin};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::Path;
use std::time::Duration;

/// The invocation surfaces whose waiver events carry authority (D-I):
/// authenticated human surfaces only. `rest` is reserved for the host
/// mutation path; `cli` is this slice's surface. Any other spelling — a
/// hand-cut event claiming `model`, `orchestrator`, `worker`, or an empty
/// approver — is honored by NO consumer: a model can never approve a
/// standards waiver, and the check lives at consumption so a forged event
/// fails closed instead of laundering model discretion into human
/// authority.
pub const HUMAN_SURFACES: &[&str] = &["cli", "rest"];

/// The honest approver spelling when the local authority model cannot
/// identify a person (D-I): the CLI authenticates the operator by being
/// local, never by inventing a real-world identity.
pub const LOCAL_OPERATOR: &str = "local-operator";

/// Lowercase hex SHA-256 of `bytes`. Full 32-byte digest (unlike
/// [`crate::prompts::hash_text`]'s 12-char identity hash): waiver bindings
/// are an audit surface, so collisions must be cryptographic, not merely
/// unlikely (the evidence_bundle idiom).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The content fingerprint of the ONE finding a waiver subtracts (D-I's
/// "checker/finding fingerprint"): sha256 over the citing run id and every
/// finding field, including the full rule-citation join key. Any change to
/// the finding — a re-run rendering new evidence, a different subject, a
/// re-citation at another revision — fingerprints differently, so the
/// waiver cannot drift onto a failure the approver never saw. Fields are
/// length-prefixed and unit-separated so no pair of field lists can
/// concatenate to the same canonical text.
pub fn finding_fingerprint(run_id: &str, finding: &Finding) -> String {
    let mut canonical = String::new();
    let mut field = |value: &str| {
        let _ = write!(canonical, "{}\x1f{}\x1e", value.len(), value);
    };
    field(run_id);
    field(&finding.subject);
    field(&finding.severity);
    field(&finding.evidence);
    field(&finding.suggested_fix);
    field(&finding.class);
    match &finding.rule {
        Some(rule) => {
            field("cited");
            field(&rule.id);
            field(&rule.revision.to_string());
            field(&rule.source);
            field(&rule.digest);
            field(&rule.lifecycle);
            field(&rule.level);
            field(rule.checker.as_deref().unwrap_or(""));
        }
        None => field("uncited"),
    }
    sha256_hex(canonical.as_bytes())
}

/// The paths a waiver's diff digest covers (D-I): the rule's `when-paths`
/// intersected with the mission's changed paths — or the WHOLE changed set
/// for an unscoped rule. Sorted and deduped so the digest inputs are
/// canonical regardless of git's reporting order.
pub fn affected_paths(rule: &PinnedRule, changed_paths: &[String]) -> Vec<String> {
    let mut paths: Vec<String> = if rule.when_paths.is_empty() {
        changed_paths.to_vec()
    } else {
        changed_paths
            .iter()
            .filter(|path| {
                crate::merge_gate::when_paths_match(&rule.when_paths, std::slice::from_ref(path))
            })
            .cloned()
            .collect()
    };
    paths.sort();
    paths.dedup();
    paths
}

/// [`affected_paths`] for a rule selected by immutable context rather than a
/// changed path. In that case its exception/attestation binds the whole
/// deliverable diff; binding an empty diff would let later review edits reuse
/// authority granted for different output bytes.
pub fn affected_paths_with_context(
    rule: &PinnedRule,
    changed_paths: &[String],
    context_paths: &[String],
) -> Vec<String> {
    let paths = affected_paths(rule, changed_paths);
    if !paths.is_empty() || rule.when_paths.is_empty() {
        return paths;
    }
    if crate::merge_gate::when_paths_match(&rule.when_paths, context_paths) {
        let mut all = changed_paths.to_vec();
        all.sort();
        all.dedup();
        all
    } else {
        paths
    }
}

/// One `standards.waiver.approved` event folded into its binding record —
/// the shape the coverage fold joins and the record path returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaiverRecord {
    /// The event's seq — the audit anchor into the log.
    pub seq: u64,
    pub rule_id: String,
    pub rule_revision: u64,
    pub manifest_digest: String,
    pub approval_seq: u64,
    pub finding_fingerprint: String,
    pub paths: Vec<String>,
    pub diff_digest: String,
    pub reason: String,
    pub approver: String,
    pub surface: String,
    pub expires_at: DateTime<Utc>,
}

impl WaiverRecord {
    /// Fold one event into its waiver record; `None` for every other kind.
    pub fn from_event(event: &Event) -> Option<Self> {
        let EventKind::StandardsWaiverApproved {
            rule_id,
            rule_revision,
            manifest_digest,
            approval_seq,
            finding_fingerprint,
            paths,
            diff_digest,
            reason,
            approver,
            surface,
            expires_at,
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
            finding_fingerprint: finding_fingerprint.clone(),
            paths: paths.clone(),
            diff_digest: diff_digest.clone(),
            reason: reason.clone(),
            approver: approver.clone(),
            surface: surface.clone(),
            expires_at: *expires_at,
        })
    }
}

/// The log-derivable half of the D-I binding: does this waiver cover THIS
/// failing finding for THIS pinned rule, at this instant? Every clause
/// fails closed:
///
/// - the pinned rule must declare `waivable: true` — a hand-cut waiver
///   against a non-waivable rule joins nothing, exactly like the record
///   path's refusal;
/// - rule id, pinned revision, manifest digest, and the `plan.approved`
///   seq must match EXACTLY — a revision bump, a re-approval, or a
///   substituted manifest invalidates the waiver;
/// - the finding fingerprint must match — the waiver names one failure,
///   never a class of them;
/// - `now` must precede the expiry — an expired waiver excepts nothing;
/// - the surface must be a recognized human one with a non-empty approver
///   — a model-authored or anonymous event carries no authority.
///
/// The diff-digest clause is deliberately NOT here: the fold has no git.
/// It is bound at record time (below) and re-derived at enforcement time
/// (KRZ-346) by comparing `waiver.diff_digest` against a fresh digest over
/// the affected-path diff.
pub fn waiver_covers(
    waiver: &WaiverRecord,
    rule: &PinnedRule,
    pin: &StandardsPin,
    approval_seq: u64,
    fingerprint: &str,
    now: DateTime<Utc>,
) -> bool {
    rule.waivable
        && waiver.seq > approval_seq
        && waiver.rule_id == rule.id
        && waiver.rule_revision == rule.revision
        && waiver.manifest_digest == pin.digest
        && waiver.approval_seq == approval_seq
        && waiver.finding_fingerprint == fingerprint
        && now < waiver.expires_at
        && HUMAN_SURFACES.contains(&waiver.surface.as_str())
        && !waiver.approver.trim().is_empty()
}

/// Enforcement-time half of D-I: find the live human waiver that covers this
/// freshly-computed failing checker finding AND the current affected-path
/// diff. The log-only coverage fold cannot re-read git; final/merge decisions
/// must call this function before treating a failure as waived.
#[allow(clippy::too_many_arguments)]
pub fn active_waiver_for_finding(
    repo: &crate::git_ops::GitRepo,
    events: &[Event],
    mission_id: &str,
    pin: &StandardsPin,
    rule: &PinnedRule,
    finding: &Finding,
    base_ref: &str,
    head_ref: &str,
    now: DateTime<Utc>,
) -> crate::error::Result<Option<WaiverRecord>> {
    let approval_seq = events
        .iter()
        .filter(|event| event.mission_id == mission_id)
        .filter_map(|event| match &event.kind {
            EventKind::PlanApproved { plan, .. }
                if plan
                    .standards_manifest
                    .as_deref()
                    .is_some_and(|approved| approved == pin) =>
            {
                Some(event.seq)
            }
            _ => None,
        })
        .next_back();
    let Some(approval_seq) = approval_seq else {
        return Ok(None);
    };
    let changed = repo.changed_paths(base_ref, head_ref)?;
    let paths = affected_paths_with_context(rule, &changed, &pin.context_paths);
    let diff = if rule.when_paths.is_empty() {
        repo.diff_full(base_ref, head_ref)?
    } else if paths.is_empty() {
        String::new()
    } else {
        repo.diff_range_paths(base_ref, head_ref, &paths)?
    };
    let diff_digest = sha256_hex(diff.as_bytes());
    let fingerprint = finding_fingerprint(crate::reducer::ENGINE_RUN_ID, finding);
    Ok(events
        .iter()
        .filter(|event| event.mission_id == mission_id)
        .filter_map(WaiverRecord::from_event)
        .rfind(|waiver| {
            waiver_covers(waiver, rule, pin, approval_seq, &fingerprint, now)
                && waiver.paths == paths
                && waiver.diff_digest == diff_digest
        }))
}

// ---------------------------------------------------------------------------
// The record path — `kranz standards waive` (the one approval surface)
// ---------------------------------------------------------------------------

/// What the operator asks to waive. The approver is never an input: the
/// local authority model cannot name a person, so the record honestly
/// carries [`LOCAL_OPERATOR`] plus the invocation surface (D-I).
#[derive(Debug, Clone)]
pub struct WaiverRequest {
    /// The pinned rule id to except.
    pub rule_id: String,
    /// The revision the operator believes they are waiving; `None` takes
    /// the pinned revision. A mismatch refuses — never silently rebind.
    pub revision: Option<u64>,
    /// Optional finding-subject disambiguator when several findings cite
    /// the rule; the latest matching finding is the target.
    pub finding_subject: Option<String>,
    /// Why the exception is granted (recorded verbatim; scrubbed at write).
    pub reason: String,
    /// The expiry instant. Must be in the future — waivers are never
    /// permanent (D-I).
    pub expires_at: DateTime<Utc>,
}

/// The recorded waiver plus the evidence it binds, returned so the caller
/// can display exactly what was approved.
#[derive(Debug, Clone)]
pub struct WaiverOutcome {
    /// The appended, seq-stamped event.
    pub event: Event,
    pub rule: PinnedRule,
    /// The finding the waiver subtracts (subject, evidence, citing run).
    pub finding_subject: String,
    pub finding_evidence: String,
    pub run_id: String,
    pub affected_paths: Vec<String>,
    pub diff_digest: String,
    pub finding_fingerprint: String,
}

/// Record one human standards waiver (KRZ-344, D-I). Reads the mission's
/// log and git state, refuses every invalid shape naming the reason, and —
/// only then — appends `standards.waiver.approved`. The pin, the finding,
/// and the diff are resolved exactly as the coverage fold will join them,
/// so a recorded waiver can never be inert-on-arrival.
///
/// Refusals (all fail closed, all named):
/// - no approved standards pin on the mission;
/// - the rule is absent from the pin — an expired/retired rule or RFC is
///   never pinned (KRZ-342's resolver), so it has no waiver slot;
/// - the operator-named revision disagrees with the pinned one;
/// - the pinned rule declares `waivable: false`;
/// - no recorded finding cites the rule at the pinned revision + digest
///   (a waiver subtracts a failure; there is nothing to except);
/// - a live waiver already covers that exact finding (one waiver
///   subtracts exactly one failure);
/// - the expiry is not in the future, or the reason is empty;
/// - the mission has no pinned base sha to diff against.
///
/// Locking mirrors [`crate::mission_catalog::abandon_mission`]: a live
/// engine holds the mission lock, so a waiver against a RUNNING mission is
/// refused with `LockHeld` — pause or stop the engine first. CLI and REST
/// surfaces share this same record path and authority checks.
pub fn approve_standards_waiver(
    repo_root: &Path,
    mission_id: &str,
    request: &WaiverRequest,
    surface: &str,
    force: LockForce,
) -> crate::error::Result<WaiverOutcome> {
    if !MissionPaths::is_safe_id(mission_id) {
        return Err(EngineError::InvalidState(format!(
            "unsafe mission id `{mission_id}` — waiver targets must be one local mission id"
        )));
    }
    let paths = MissionPaths::new(repo_root, mission_id);
    // The initial fold supplies only the configured throttle needed to acquire
    // the single-writer lock. Re-read and re-fold after acquisition: two
    // simultaneous human surfaces must not both validate against the same
    // stale pre-lock log and append duplicate or superseded authority.
    let initial_events = EventLog::read_events(&paths.events_file())?;
    let initial_state = reducer::fold(&initial_events)?;
    let mut log = EventLog::acquire(
        &paths,
        mission_id,
        Duration::from_millis(initial_state.config.event_stream_throttle_ms),
        force,
    )?;
    let events = EventLog::read_events(&paths.events_file())?;
    let state = reducer::fold(&events)?;

    // The pin joins EXACTLY as the coverage fold resolves it: the latest
    // plan.approved carrying a manifest (a revision never re-pins). The
    // waiver records that approval's seq so a re-approval supersedes it.
    let (approval_seq, pin) = events
        .iter()
        .filter(|event| event.mission_id == mission_id)
        .filter_map(|event| match &event.kind {
            EventKind::PlanApproved { plan, .. } => plan
                .standards_manifest
                .as_deref()
                .map(|manifest| (event.seq, manifest.clone())),
            _ => None,
        })
        .next_back()
        .ok_or_else(|| {
            EngineError::InvalidState(format!(
                "mission '{mission_id}' has no approved standards pin — there is no rule \
                 failure to except"
            ))
        })?;

    let rule = pin
        .rules
        .iter()
        .find(|rule| rule.id == request.rule_id)
        .ok_or_else(|| {
            EngineError::InvalidState(format!(
                "rule '{}' is not in mission '{mission_id}'s approved standards pin — a \
                 waiver binds the pinned consent artifact; an absent, retired, or \
                 never-applicable rule carries no waiver slot",
                request.rule_id
            ))
        })?;
    if let Some(revision) = request.revision {
        if revision != rule.revision {
            return Err(EngineError::InvalidState(format!(
                "rule '{}' is pinned at revision r{} but the waiver names r{revision} — a \
                 mismatched revision never joins (D-I)",
                rule.id, rule.revision
            )));
        }
    }
    if !rule.waivable {
        return Err(EngineError::InvalidState(format!(
            "rule '{}' declares waivable: false — an organizational MUST without a waiver \
             declaration is never waivable (D-I); fix the finding or change the rule's \
             posture through the pack lifecycle",
            rule.id
        )));
    }

    // The target finding: the latest recorded failure citing this rule at
    // the pinned revision AND digest — the coverage fold's full citation
    // join key, so the waived evidence is exactly the joined evidence.
    let (_, finding, run_id) = events
        .iter()
        .filter(|event| event.mission_id == mission_id)
        .filter_map(|event| match &event.kind {
            EventKind::ValidationFinding {
                finding, run_id, ..
            } => Some((event.seq, finding, run_id)),
            _ => None,
        })
        .rfind(|(_, finding, _)| {
            finding.rule.as_ref().is_some_and(|citation| {
                citation.id == rule.id
                    && citation.revision == rule.revision
                    && citation.digest == pin.digest
            }) && request
                .finding_subject
                .as_ref()
                .is_none_or(|subject| &finding.subject == subject)
        })
        .ok_or_else(|| {
            EngineError::InvalidState(format!(
                "no recorded finding cites rule '{}' r{} at the approved digest — a waiver \
                 subtracts an exact recorded failure; there is nothing to except",
                rule.id, rule.revision
            ))
        })?;
    let fingerprint = finding_fingerprint(run_id, finding);

    let now = Utc::now();
    if request.expires_at <= now {
        return Err(EngineError::InvalidState(format!(
            "the expiry {} is not in the future — an expired waiver excepts nothing (D-I: \
             waivers are never permanent)",
            request.expires_at.to_rfc3339()
        )));
    }
    if request.reason.trim().is_empty() {
        return Err(EngineError::InvalidState(
            "a waiver requires a reason — the audit names why the exception was granted"
                .to_string(),
        ));
    }
    // One waiver subtracts exactly one failure: a finding already covered
    // by a live waiver cannot be waived again.
    let already = events
        .iter()
        .filter(|event| event.mission_id == mission_id)
        .filter_map(WaiverRecord::from_event)
        .any(|waiver| waiver_covers(&waiver, rule, &pin, approval_seq, &fingerprint, now));
    if already {
        return Err(EngineError::InvalidState(format!(
            "finding '{}' is already covered by a live waiver — one waiver subtracts \
             exactly one standards failure (D-I)",
            finding.subject
        )));
    }

    // The diff binding: the mission diff (pinned base .. mission branch),
    // scoped to the rule's affected paths. An unscoped rule binds the
    // whole diff; a scoped rule binds only what its when-paths cover, so
    // unrelated paths receive no authority.
    let base_sha = state.mission.base_sha.clone().ok_or_else(|| {
        EngineError::InvalidState(format!(
            "mission '{mission_id}' has no pinned base sha — the diff a waiver binds is \
             undefined"
        ))
    })?;
    let git = crate::git_ops::GitRepo::open(repo_root)?;
    let branch = &state.mission.mission_branch;
    let changed = git.changed_paths(&base_sha, branch).map_err(|e| {
        EngineError::Git(format!(
            "cannot diff the pinned base {base_sha} against mission branch `{branch}`: {e}"
        ))
    })?;
    let affected = affected_paths_with_context(rule, &changed, &pin.context_paths);
    let diff_text = if rule.when_paths.is_empty() {
        git.diff_full(&base_sha, branch)?
    } else if affected.is_empty() {
        // A scoped rule with no changed path under its when-paths binds
        // the EMPTY scoped diff — any later change inside the scope
        // invalidates the waiver.
        String::new()
    } else {
        git.diff_range_paths(&base_sha, branch, &affected)?
    };
    let diff_digest = sha256_hex(diff_text.as_bytes());

    let (event, audits) = log.append_with_redaction_audits(EventKind::StandardsWaiverApproved {
        rule_id: rule.id.clone(),
        rule_revision: rule.revision,
        manifest_digest: pin.digest.clone(),
        approval_seq,
        finding_fingerprint: fingerprint.clone(),
        paths: affected.clone(),
        diff_digest: diff_digest.clone(),
        reason: request.reason.clone(),
        approver: LOCAL_OPERATOR.to_string(),
        surface: surface.to_string(),
        expires_at: request.expires_at,
    })?;
    // The reducer arm is audit-only, but fold the append (+ any redaction
    // audits) and refresh the snapshot anyway, so state.json's last_seq
    // never lags the log (the abandon_mission idiom).
    let mut state = state;
    reducer::apply(&mut state, &event)?;
    for audit in &audits {
        reducer::apply(&mut state, audit)?;
    }
    reducer::write_snapshot(&state, &paths.state_file())?;
    log.flush()?;

    Ok(WaiverOutcome {
        event,
        rule: rule.clone(),
        finding_subject: finding.subject.clone(),
        finding_evidence: finding.evidence.clone(),
        run_id: run_id.clone(),
        affected_paths: affected,
        diff_digest,
        finding_fingerprint: fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::RuleCitation;

    /// A fixed instant for fixtures that must not consult a wall clock.
    fn ts() -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 1, 2, 3, 4, 5).unwrap()
    }

    fn cited_finding(subject: &str, evidence: &str) -> Finding {
        Finding {
            subject: subject.to_string(),
            severity: "major".to_string(),
            evidence: evidence.to_string(),
            suggested_fix: String::new(),
            class: String::new(),
            rule: Some(RuleCitation {
                id: "ZZ-FAIL-001".to_string(),
                revision: 2,
                source: "zz-pack standards".to_string(),
                digest: "ab".repeat(32),
                lifecycle: "enforced".to_string(),
                level: "must".to_string(),
                checker: Some("gate:zz-gate".to_string()),
            }),
        }
    }

    fn pinned(waivable: bool) -> PinnedRule {
        PinnedRule {
            id: "ZZ-FAIL-001".to_string(),
            revision: 2,
            rfc: "RFC-001".to_string(),
            level: "must".to_string(),
            effective_status: "enforced".to_string(),
            statement: "zz statement".to_string(),
            domains: Vec::new(),
            stages: vec!["validation".to_string()],
            when_paths: vec!["crates/".to_string()],
            task_classes: Vec::new(),
            checker: Some("gate:zz-gate".to_string()),
            waivable,
        }
    }

    fn pin() -> StandardsPin {
        StandardsPin {
            pack_name: "zz-pack".to_string(),
            pack_dir: "vendor/pack".to_string(),
            standards_root: "standards".to_string(),
            digest: "ab".repeat(32),
            source: crate::types::StandardsPinSource::RepoTracked,
            task_class: None,
            touch_set: vec!["crates/**".to_string()],
            context_paths: Vec::new(),
            gates: Vec::new(),
            rules: vec![pinned(true)],
        }
    }

    fn record() -> WaiverRecord {
        WaiverRecord {
            seq: 7,
            rule_id: "ZZ-FAIL-001".to_string(),
            rule_revision: 2,
            manifest_digest: "ab".repeat(32),
            approval_seq: 1,
            finding_fingerprint: finding_fingerprint("v-1", &cited_finding("a-1", "broke")),
            paths: vec!["crates/engine/src/x.rs".to_string()],
            diff_digest: "cd".repeat(32),
            reason: "accepted risk".to_string(),
            approver: LOCAL_OPERATOR.to_string(),
            surface: "cli".to_string(),
            expires_at: ts() + chrono::Duration::hours(1),
        }
    }

    /// The fingerprint is content-bound: identical findings fingerprint
    /// identically, and every single-field change — subject, evidence,
    /// severity, class, the citing run, any citation field — fingerprints
    /// differently, so a waiver can never drift onto a failure the
    /// approver never saw.
    #[test]
    fn flight_rules_waiver_fingerprint_is_stable_and_content_bound() {
        let base = finding_fingerprint("v-1", &cited_finding("a-1", "broke"));
        assert_eq!(base.len(), 64, "full sha256 hex");
        assert_eq!(
            base,
            finding_fingerprint("v-1", &cited_finding("a-1", "broke")),
            "same inputs, same fingerprint"
        );

        let mut changed = cited_finding("a-1", "broke");
        changed.evidence = "broke differently".to_string();
        assert_ne!(base, finding_fingerprint("v-1", &changed));
        let mut changed = cited_finding("a-1", "broke");
        changed.subject = "a-2".to_string();
        assert_ne!(base, finding_fingerprint("v-1", &changed));
        let mut changed = cited_finding("a-1", "broke");
        changed.severity = "minor".to_string();
        assert_ne!(base, finding_fingerprint("v-1", &changed));
        let mut changed = cited_finding("a-1", "broke");
        changed.class = "out-of-contract-write".to_string();
        assert_ne!(base, finding_fingerprint("v-1", &changed));
        let mut changed = cited_finding("a-1", "broke");
        changed.rule.as_mut().unwrap().revision = 3;
        assert_ne!(base, finding_fingerprint("v-1", &changed));
        // The citing run is part of the fingerprint: two runs rendering
        // identical findings stay distinct failures.
        assert_ne!(
            base,
            finding_fingerprint("v-2", &cited_finding("a-1", "broke"))
        );
        // An uncited finding never collides with its cited twin.
        let mut uncited = cited_finding("a-1", "broke");
        uncited.rule = None;
        assert_ne!(base, finding_fingerprint("v-1", &uncited));
    }

    /// Length-prefixing closes the concatenation ambiguity: ("ab", "c")
    /// and ("a", "bc") must never share a canonical form.
    #[test]
    fn flight_rules_waiver_fingerprint_fields_are_unambiguous() {
        let mut a = cited_finding("ab", "c");
        a.rule = None;
        let mut b = cited_finding("a", "bc");
        b.rule = None;
        assert_ne!(
            finding_fingerprint("v-1", &a),
            finding_fingerprint("v-1", &b)
        );
    }

    /// Affected paths: an unscoped rule binds the whole changed set; a
    /// scoped rule binds exactly the changed paths under its when-paths —
    /// unrelated paths receive no authority (D-I).
    #[test]
    fn flight_rules_waiver_affected_paths_scope_to_when_paths() {
        let changed = vec![
            "docs/notes.md".to_string(),
            "crates/engine/src/x.rs".to_string(),
            "crates/cli/src/main.rs".to_string(),
        ];
        let mut unscoped = pinned(true);
        unscoped.when_paths = Vec::new();
        assert_eq!(
            affected_paths(&unscoped, &changed),
            vec![
                "crates/cli/src/main.rs".to_string(),
                "crates/engine/src/x.rs".to_string(),
                "docs/notes.md".to_string(),
            ],
            "unscoped binds the whole set, canonical order"
        );
        assert_eq!(
            affected_paths(&pinned(true), &changed),
            vec![
                "crates/cli/src/main.rs".to_string(),
                "crates/engine/src/x.rs".to_string(),
            ],
            "scoped binds the when-paths intersection only"
        );
        let mut scoped = pinned(true);
        scoped.when_paths = vec!["apps/".to_string()];
        assert!(
            affected_paths(&scoped, &changed).is_empty(),
            "no changed path under the scope binds the empty diff"
        );
    }

    #[test]
    fn flight_rules_review_class_context_rule_binds_the_review_diff() {
        let mut rule = pinned(true);
        rule.when_paths = vec!["docs/spec.md".to_string()];
        let changed = vec!["reviews/spec-review.md".to_string()];
        assert!(affected_paths(&rule, &changed).is_empty());
        assert_eq!(
            affected_paths_with_context(&rule, &changed, &["docs/spec.md".to_string()]),
            changed
        );
    }

    /// The join predicate, one clause at a time: the valid record covers;
    /// every single-clause mutation — rule id, revision, manifest digest,
    /// approval seq, fingerprint, expiry, surface, approver, waivable —
    /// fails closed.
    #[test]
    fn flight_rules_waiver_covers_requires_the_exact_binding() {
        let rule = pinned(true);
        let pin = pin();
        let fingerprint = finding_fingerprint("v-1", &cited_finding("a-1", "broke"));
        let now = ts();
        assert!(waiver_covers(&record(), &rule, &pin, 1, &fingerprint, now));

        let mut w = record();
        w.rule_id = "ZZ-OTHER-001".to_string();
        assert!(!waiver_covers(&w, &rule, &pin, 1, &fingerprint, now));
        let mut w = record();
        w.rule_revision = 3;
        assert!(!waiver_covers(&w, &rule, &pin, 1, &fingerprint, now));
        let mut w = record();
        w.manifest_digest = "ff".repeat(32);
        assert!(!waiver_covers(&w, &rule, &pin, 1, &fingerprint, now));
        let mut w = record();
        w.approval_seq = 9;
        assert!(!waiver_covers(&w, &rule, &pin, 1, &fingerprint, now));
        assert!(
            !waiver_covers(&record(), &rule, &pin, 1, "deadbeef", now),
            "a different finding fingerprint joins nothing"
        );
        assert!(
            !waiver_covers(
                &record(),
                &rule,
                &pin,
                1,
                &fingerprint,
                ts() + chrono::Duration::hours(2)
            ),
            "expired"
        );
        let mut w = record();
        w.surface = "model".to_string();
        assert!(!waiver_covers(&w, &rule, &pin, 1, &fingerprint, now));
        let mut w = record();
        w.approver = "  ".to_string();
        assert!(!waiver_covers(&w, &rule, &pin, 1, &fingerprint, now));
        assert!(
            !waiver_covers(&record(), &pinned(false), &pin, 1, &fingerprint, now),
            "a hand-cut waiver against a waivable:false rule joins nothing"
        );
    }

    // ---- the record path (approve_standards_waiver) -------------------------

    /// A temp git repo: base commit on `main` (x.rs v1 + README), then a
    /// mission branch `kranz/mission-m-1` changing x.rs (scoped, under
    /// `crates/`) and adding `docs/notes.md` (unscoped). Returns the
    /// TempDir, the repo root, and the base sha; None when git is
    /// unavailable (the resolution.rs fixture idiom).
    fn mission_repo() -> Option<(tempfile::TempDir, std::path::PathBuf, String)> {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(&root)
            .output()
            .ok()?;
        if !init.status.success() {
            crate::test_capability::skip(
                crate::test_capability::capability::GIT,
                "git is not on PATH",
            );
            return None;
        }
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
            out
        };
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::create_dir_all(root.join("crates/engine/src")).unwrap();
        std::fs::write(root.join("crates/engine/src/x.rs"), "v1\n").unwrap();
        std::fs::write(root.join("README.md"), "seed\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        let base = git(&["rev-parse", "main"]);
        let base_sha = String::from_utf8(base.stdout).unwrap().trim().to_string();
        git(&["checkout", "-q", "-b", "kranz/mission-m-1"]);
        std::fs::write(root.join("crates/engine/src/x.rs"), "v2\n").unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/notes.md"), "n\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "mission work"]);
        Some((tmp, root, base_sha))
    }

    /// The record-path pin: ZZ-FAIL-001 r2 (waivable, scoped to `crates/`),
    /// ZZ-HARD-001 r1 (NOT waivable — the refusal probe), ZZ-QUIET-001 r1
    /// (waivable but never cited — the absent-finding probe).
    fn record_pin() -> StandardsPin {
        let mut hard = pinned(false);
        hard.id = "ZZ-HARD-001".to_string();
        hard.revision = 1;
        hard.when_paths = Vec::new();
        let mut quiet = pinned(true);
        quiet.id = "ZZ-QUIET-001".to_string();
        quiet.revision = 1;
        quiet.when_paths = Vec::new();
        StandardsPin {
            rules: vec![pinned(true), hard, quiet],
            ..pin()
        }
    }

    fn record_finding(id: &str, revision: u64, subject: &str) -> Finding {
        Finding {
            subject: subject.to_string(),
            severity: "major".to_string(),
            evidence: format!("evidence for {subject}"),
            suggested_fix: String::new(),
            class: String::new(),
            rule: Some(RuleCitation {
                id: id.to_string(),
                revision,
                source: "zz-pack standards".to_string(),
                digest: "ab".repeat(32),
                lifecycle: "enforced".to_string(),
                level: "must".to_string(),
                checker: Some("gate:zz-gate".to_string()),
            }),
        }
    }

    /// Seed m-1's log: created → approved (pinned, base sha) → one finding
    /// per enforced rule that has one. Returns the mission paths.
    fn seed_record_mission(root: &Path, base_sha: &str) -> crate::paths::MissionPaths {
        let paths = MissionPaths::new(root, "m-1");
        let mut log = EventLog::acquire(&paths, "m-1", Duration::ZERO, LockForce::No).unwrap();
        log.append(EventKind::MissionCreated {
            goal: "g".to_string(),
            base_branch: "main".to_string(),
            mission_branch: "kranz/mission-m-1".to_string(),
            config: crate::types::MissionConfig::default(),
        })
        .unwrap();
        log.append(EventKind::PlanApproved {
            plan: crate::types::Plan {
                goal: "g".to_string(),
                validation_contract: Vec::new(),
                // One milestone so the seeded findings' `ms-1` reference
                // folds (the reducer rejects unknown milestone ids).
                milestones: vec![crate::types::PlanMilestone {
                    title: "ms".to_string(),
                    features: Vec::new(),
                }],
                considered_alternatives: None,
                command_grants: Vec::new(),
                touch_set: vec!["crates/**".to_string()],
                standards_manifest: Some(Box::new(record_pin())),
                reviewer_independence: None,
            },
            base_sha: Some(base_sha.to_string()),
        })
        .unwrap();
        for (id, revision, subject) in [("ZZ-FAIL-001", 2, "a-1"), ("ZZ-HARD-001", 1, "a-2")] {
            log.append(EventKind::ValidationFinding {
                milestone_id: "ms-1".to_string(),
                // The reserved engine run id: the reducer skips the
                // run-exists check for it, so the fixture needs no worker
                // spawn events.
                run_id: crate::reducer::ENGINE_RUN_ID.to_string(),
                finding: record_finding(id, revision, subject),
            })
            .unwrap();
        }
        drop(log);
        paths
    }

    fn record_request() -> WaiverRequest {
        WaiverRequest {
            rule_id: "ZZ-FAIL-001".to_string(),
            revision: Some(2),
            finding_subject: None,
            reason: "accepted risk: matches the documented exception".to_string(),
            expires_at: Utc::now() + chrono::Duration::hours(1),
        }
    }

    /// The record → fold loop: a valid request appends the fully-bound
    /// event, displays exactly the evidence it binds, and the coverage
    /// fold then renders the rule waived — the path report.md, provenance
    /// replay, and the evidence bundle all read.
    #[test]
    fn flight_rules_waiver_record_path_appends_and_the_fold_renders_waived() {
        let Some((_tmp, root, base_sha)) = mission_repo() else {
            return;
        };
        let paths = seed_record_mission(&root, &base_sha);
        let outcome =
            approve_standards_waiver(&root, "m-1", &record_request(), "cli", LockForce::No)
                .expect("a valid waiver records");

        // The event carries the full D-I binding, seq-stamped by the log.
        assert_eq!(outcome.event.seq, 5);
        assert_eq!(outcome.finding_subject, "a-1");
        assert_eq!(outcome.run_id, crate::reducer::ENGINE_RUN_ID);
        assert_eq!(
            outcome.affected_paths,
            vec!["crates/engine/src/x.rs".to_string()],
            "the scoped rule binds only the changed path under crates/"
        );
        assert_eq!(outcome.diff_digest.len(), 64);
        let EventKind::StandardsWaiverApproved {
            rule_id,
            rule_revision,
            manifest_digest,
            approval_seq,
            finding_fingerprint: recorded_fingerprint,
            paths: waiver_paths,
            diff_digest,
            approver,
            surface,
            ..
        } = &outcome.event.kind
        else {
            panic!("wrong variant");
        };
        assert_eq!(rule_id, "ZZ-FAIL-001");
        assert_eq!(*rule_revision, 2);
        assert_eq!(*manifest_digest, "ab".repeat(32));
        assert_eq!(*approval_seq, 2, "the plan.approved seq the pin rode");
        assert_eq!(
            *recorded_fingerprint,
            finding_fingerprint(
                crate::reducer::ENGINE_RUN_ID,
                &record_finding("ZZ-FAIL-001", 2, "a-1")
            )
        );
        assert_eq!(waiver_paths, &outcome.affected_paths);
        assert_eq!(*diff_digest, outcome.diff_digest);
        assert_eq!(approver, LOCAL_OPERATOR);
        assert_eq!(surface, "cli");

        // The persisted log folds to `waived` — the exact join the report,
        // the replay, and the bundle render.
        let events = EventLog::read_events(&paths.events_file()).unwrap();
        let coverage =
            crate::standards_coverage::standards_coverage("m-1", &events).expect("a pin folds");
        let row = coverage
            .rules
            .iter()
            .find(|row| row.id == "ZZ-FAIL-001")
            .expect("the row");
        assert_eq!(
            row.disposition,
            crate::standards_coverage::RuleDisposition::Waived
        );
        assert_eq!(row.evidence[0].waiver.as_ref().expect("named").seq, 5);

        // A second waiver over the same finding refuses: one waiver
        // subtracts exactly one failure.
        let err = approve_standards_waiver(&root, "m-1", &record_request(), "cli", LockForce::No)
            .expect_err("already waived");
        assert!(
            err.to_string().contains("already covered by a live waiver"),
            "{err}"
        );
    }

    /// Every refusal shape fails closed and names its cause — and appends
    /// NOTHING to the log.
    #[test]
    fn flight_rules_waiver_record_path_refusals_fail_closed() {
        let Some((_tmp, root, base_sha)) = mission_repo() else {
            return;
        };
        let paths = seed_record_mission(&root, &base_sha);
        let refuse = |request: &WaiverRequest| -> String {
            approve_standards_waiver(&root, "m-1", request, "cli", LockForce::No)
                .expect_err("must refuse")
                .to_string()
        };

        // Rule absent from the pin (an expired/retired rule or RFC is
        // never pinned — it carries no waiver slot).
        let err = refuse(&WaiverRequest {
            rule_id: "ZZ-GONE-001".to_string(),
            ..record_request()
        });
        assert!(
            err.contains("not in mission 'm-1's approved standards pin"),
            "{err}"
        );
        // Mismatched revision.
        let err = refuse(&WaiverRequest {
            revision: Some(3),
            ..record_request()
        });
        assert!(
            err.contains("pinned at revision r2 but the waiver names r3"),
            "{err}"
        );
        // waivable: false.
        let err = refuse(&WaiverRequest {
            rule_id: "ZZ-HARD-001".to_string(),
            revision: Some(1),
            ..record_request()
        });
        assert!(err.contains("waivable: false"), "{err}");
        // Absent finding.
        let err = refuse(&WaiverRequest {
            rule_id: "ZZ-QUIET-001".to_string(),
            revision: Some(1),
            ..record_request()
        });
        assert!(err.contains("no recorded finding cites"), "{err}");
        // Past expiry.
        let err = refuse(&WaiverRequest {
            expires_at: Utc::now() - chrono::Duration::hours(1),
            ..record_request()
        });
        assert!(err.contains("not in the future"), "{err}");
        // Empty reason.
        let err = refuse(&WaiverRequest {
            reason: "   ".to_string(),
            ..record_request()
        });
        assert!(err.contains("requires a reason"), "{err}");
        // No approved standards pin at all: a second mission seeded
        // without a manifest.
        let paths2 = MissionPaths::new(&root, "m-2");
        {
            let mut log = EventLog::acquire(&paths2, "m-2", Duration::ZERO, LockForce::No).unwrap();
            log.append(EventKind::MissionCreated {
                goal: "g".to_string(),
                base_branch: "main".to_string(),
                mission_branch: "kranz/mission-m-1".to_string(),
                config: crate::types::MissionConfig::default(),
            })
            .unwrap();
        }
        let err = approve_standards_waiver(&root, "m-2", &record_request(), "cli", LockForce::No)
            .expect_err("no pin")
            .to_string();
        assert!(err.contains("no approved standards pin"), "{err}");

        // Nothing was appended by any refusal.
        let events = EventLog::read_events(&paths.events_file()).unwrap();
        assert_eq!(events.len(), 4, "refusals append nothing");
        let events2 = EventLog::read_events(&paths2.events_file()).unwrap();
        assert_eq!(events2.len(), 1, "refusals append nothing");
    }

    #[test]
    fn flight_rules_waiver_refuses_unsafe_mission_id_before_path_access() {
        let root = tempfile::tempdir().unwrap();
        let err = approve_standards_waiver(
            root.path(),
            "../m-victim",
            &record_request(),
            "cli",
            LockForce::No,
        )
        .expect_err("a waiver cannot escape the local mission namespace");
        assert!(err.to_string().contains("unsafe mission id"), "{err}");
    }

    /// The diff binding (D-I): the recorded digest covers the
    /// affected-path diff, so an unrelated-path change leaves the binding
    /// intact (no authority was granted over it) while any change under
    /// the rule's when-paths digests differently and invalidates the
    /// waiver — the comparison enforcement (KRZ-346) re-derives.
    #[test]
    fn flight_rules_waiver_diff_digest_tracks_only_affected_paths() {
        let Some((_tmp, root, base_sha)) = mission_repo() else {
            return;
        };
        seed_record_mission(&root, &base_sha);
        let outcome =
            approve_standards_waiver(&root, "m-1", &record_request(), "cli", LockForce::No)
                .expect("a valid waiver records");

        // Recompute exactly as the record path did.
        let recompute = || {
            let git = crate::git_ops::GitRepo::open(&root).unwrap();
            let changed = git.changed_paths(&base_sha, "kranz/mission-m-1").unwrap();
            let affected = affected_paths(&pinned(true), &changed);
            let text = git
                .diff_range_paths(&base_sha, "kranz/mission-m-1", &affected)
                .unwrap();
            sha256_hex(text.as_bytes())
        };
        assert_eq!(recompute(), outcome.diff_digest, "stable recompute");

        // An unrelated-path change (docs/, outside crates/) does not alter
        // the affected-path diff: the waiver neither covers it nor dies.
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        std::fs::write(root.join("docs/more.md"), "more\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "unrelated"]);
        assert_eq!(
            recompute(),
            outcome.diff_digest,
            "unrelated paths receive no authority and take none away"
        );

        // A change under the rule's when-paths digests differently: the
        // binding is invalidated and the block is restored at the next
        // recompute.
        std::fs::write(root.join("crates/engine/src/x.rs"), "v3\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "affected"]);
        assert_ne!(
            recompute(),
            outcome.diff_digest,
            "an affected-path change invalidates the waiver"
        );
    }
}
