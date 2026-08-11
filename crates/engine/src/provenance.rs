//! Provenance replay (ticket `.kranz/tickets/provenance-replay.md`, KRZ-325 —
//! the governance evidence layer's audit story): reconstruct WHY a mission's
//! unit passed from its event log ALONE — which gates in which order, which
//! artefacts, which model/backend, which prompt identity, which human
//! decisions, and the terminal outcome — as one typed, ordered
//! [`ProvenanceChain`] that the CLI renders as a text summary or `--json`.
//!
//! The fold's discipline, in the substrate's own rules:
//!
//! - **Log alone, plus the mission dir.** Everything here is folded from one
//!   mission's `events.jsonl`; the only other read is classifying artefact
//!   references against the mission dir via
//!   [`crate::gate_results::resolve_artefact`] (the total classifier — a
//!   reference whose bytes are gone reads [`ArtefactStatus::Unresolved`],
//!   never an error). No network, no git, no other missions, no persisted
//!   state: a pruned mission (a cleaned `runs/`) still replays end to end.
//! - **Deterministic machine form.** Same log → byte-identical `--json`:
//!   the chain keeps log order (Vec, never a hashed map), consults no clock,
//!   and carries NO host paths — artefact resolution is recorded as the
//!   classification alone; the absolute path the resolver probed never
//!   leaves [`crate::gate_results`]. (A resolved path would also leak the
//!   host layout into the audit record, the exact failure KRZ-312's
//!   mission-relative discipline exists to prevent.)
//! - **Pure-fold idiom.** Same shape as [`crate::escalation_metrics`]:
//!   [`provenance_chain`] is a pure function over an event slice (plus the
//!   artefact classification), [`compute_provenance`] the thin read wrapper.
//!   The replay writes nothing to the mission dir.
//!
//! WHY the gate ladder keeps LOG order rather than sorting on
//! (surface, kind, index): `gate.result` emission is already pipeline order
//! per surface batch ([`crate::gate_results::gate_result_events`]), so seq
//! order IS the ladder order — while a re-approval emits a SECOND approval
//! batch whose (kind, index) positions repeat, and sorting the whole surface
//! set would interleave the two evaluations. The chain therefore preserves
//! seq and carries the ladder position fields verbatim for any reader that
//! wants to re-derive pipeline structure.
//!
//! WHY `backend` is the one DERIVED field: `worker.spawned` records the
//! model, quant, weight hash, and prompt hash verbatim, but not the backend
//! that drove the session — the backend is a property of the mission's
//! config. The fold tracks the config the log itself records
//! (`mission.created`'s [`crate::types::MissionConfig`], evolved by each
//! `config.changed` patch through the reducer's OWN deep-merge, so the
//! replay can never drift from the state fold) and derives each spawn's
//! backend from the config in force AT THAT SEQ: a mid-mission backend flip
//! shows in exactly the spawns after it. An invalid patch fails the replay
//! exactly as it fails the reducer's fold — a log the reducer would reject
//! is corruption, not a provenance gap.
//!
//! WHY the decision set is what it is: it mirrors the flight-surgeon
//! intervention set ([`crate::escalation_metrics`]) so the two folds can
//! never disagree about what counts as a human acting — grant
//! approvals/denials, plan-revision decisions, operator milestone unblocks
//! (excluding the engine-owned workspace-gate lift via the SAME
//! [`crate::escalation_metrics::is_engine_lift`] classification), and
//! `user.message` split by SEQUENCE at `plan.approved` (at/after = steer,
//! before = drafting conversation that shaped the approved plan — both are
//! human acts a chain must name). Two additions the metrics fold can take
//! for granted but a chain cannot: `plan.approved` itself (the foundational
//! approval the whole mission rests on) and `mission.abandoned` (the
//! operator's terminal decision).

use crate::error::{EngineError, Result};
use crate::events::{Event, EventKind};
use crate::gate::{GateKind, GateSurface, GateVerdict};
use crate::gate_results::{file_artefact_ref, resolve_artefact, ArtefactResolution};
use crate::types::{MissionConfig, Role};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The artefact-resolution classification carried by the chain: the verdict
/// of [`crate::gate_results::resolve_artefact`] WITHOUT the probed path, so
/// the machine form stays host-layout free and byte-stable across machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtefactStatus {
    /// A `file:` reference whose mission-relative bytes are present.
    Resolved,
    /// A `file:` reference whose bytes are gone (or whose path could never
    /// resolve honestly inside a mission dir) — a classification, never an
    /// error; the replay continues.
    Unresolved,
    /// No `file:` scheme: the evidence is textual and travels in the event
    /// payload itself — there is nothing on disk that could go missing.
    Inline,
}

impl ArtefactStatus {
    /// Classify a resolution, dropping the probed path (see the type docs).
    fn classify(resolution: &ArtefactResolution) -> Self {
        match resolution {
            ArtefactResolution::Resolved { .. } => Self::Resolved,
            ArtefactResolution::Unresolved { .. } => Self::Unresolved,
            ArtefactResolution::Inline => Self::Inline,
        }
    }

    /// The wire/serde form (`resolved`/`unresolved`/`inline`) for text surfaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Resolved => "resolved",
            Self::Unresolved => "unresolved",
            Self::Inline => "inline",
        }
    }
}

/// One `gate.result` event, replayed: identity, ladder position, verdict,
/// the artefact handle verbatim, and its resolution against the mission dir.
/// The `seq` pins the evaluation into the chain's ordering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateLink {
    pub seq: u64,
    pub gate: String,
    pub surface: GateSurface,
    /// The gate's kind — doubling as the ladder section (events.rs).
    pub kind: GateKind,
    /// Zero-based position within the section, verbatim from the event.
    pub index: u32,
    pub verdict: GateVerdict,
    /// The artefact handle exactly as the gate stated it.
    pub artefact_ref: String,
    /// Evidence captured verbatim by the gate; absent when the reference
    /// alone is the evidence.
    pub artefact_detail: Option<String>,
    /// Gate-supplied confidence pair (KRZ-315), purely evidentiary.
    pub score: Option<f64>,
    pub threshold: Option<f64>,
    /// Resolution of `artefact_ref` against the mission dir.
    pub artefact: ArtefactStatus,
    /// The standards rule ids the evaluation joined (KRZ-343, design D-H),
    /// verbatim from the event. Additive: absent (never `[]`) on pre-field
    /// chains and for gates with no standards linkage, so those chains stay
    /// byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rule_ids: Vec<String>,
}

/// One `worker.spawned`, replayed: who ran, with what, under which prompt
/// identity. Carries whatever the log records — the prompt hash is a
/// required field on the event, so any log that parses surfaces it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionLink {
    pub seq: u64,
    pub run_id: String,
    pub role: Role,
    /// DERIVED, not recorded (module docs): the backend the mission's
    /// recorded config named for this role at this seq. `None` only when the
    /// log carries no `mission.created` before the spawn (a hand-cut log).
    pub backend: Option<String>,
    pub model: String,
    pub quant: String,
    pub weight_hash: Option<String>,
    /// The prompt identity as recorded (first 12 hex chars of the prompt
    /// text's SHA-256 — [`crate::prompts::hash_text`]), verbatim from the log.
    pub prompt_hash: String,
    pub feature_id: Option<String>,
    pub milestone_id: Option<String>,
    /// The mission-relative transcript path recorded on the event.
    pub transcript_ref: String,
    /// Its resolution against the mission dir — transcripts live under the
    /// prunable `runs/`, so this degrades to unresolved exactly like a gate
    /// artefact.
    pub transcript: ArtefactStatus,
}

/// What kind of human act one chain entry records (module docs for the set).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DecisionKind {
    PlanApproval,
    PlanRevision,
    PlanRevisionRejection,
    GrantApproval,
    GrantDenial,
    MilestoneUnblock,
    /// A `user.message` at/after `plan.approved` (by seq — the
    /// escalation-metrics steer rule).
    Steer,
    /// A `user.message` before plan approval: drafting, not a steer.
    OperatorMessage,
    MissionAbandoned,
}

impl DecisionKind {
    /// The wire/serde form for text surfaces (the `as_str` idiom of
    /// [`crate::escalation_metrics::LedgerKind`]).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PlanApproval => "plan-approval",
            Self::PlanRevision => "plan-revision",
            Self::PlanRevisionRejection => "plan-revision-rejection",
            Self::GrantApproval => "grant-approval",
            Self::GrantDenial => "grant-denial",
            Self::MilestoneUnblock => "milestone-unblock",
            Self::Steer => "steer",
            Self::OperatorMessage => "operator-message",
            Self::MissionAbandoned => "mission-abandoned",
        }
    }
}

/// One human decision, in log order, pinned to its event `seq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionLink {
    pub seq: u64,
    pub kind: DecisionKind,
    /// One line stating what was decided ("approved command: cargo test",
    /// the steer's text, "unblocked ms-1: user skipped findings").
    pub summary: String,
}

/// One divergence-ledger entry, replayed (ticket
/// `divergence-first-class-event`, KRZ-304): the comparison the pool
/// parked on, or the resolution that later landed — pinned to its `seq`
/// so the record and its resolution interleave with the rest of the chain
/// in log order. The candidate refs (run id, branch, backend, tree hash)
/// ride verbatim, so the resolution's `selected` index resolves against
/// the SAME replayed record without git.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum DivergenceLink {
    /// A `divergence.noted` — the comparison record. `diverged: false` is
    /// the agreement record: logged, never trusted.
    Noted {
        seq: u64,
        unit: String,
        candidates: Vec<crate::types::DivergenceCandidate>,
        diverged: bool,
    },
    /// A `divergence.resolved` — which candidate (or none), why, decided
    /// by whom.
    Resolved {
        seq: u64,
        unit: String,
        selected: Option<u32>,
        reason: String,
        decided_by: String,
    },
}

/// How the mission ended, when it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalStatus {
    Completed,
    Failed,
    Abandoned,
}

impl TerminalStatus {
    /// The wire/serde form for text surfaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
        }
    }
}

/// The terminal event, pinned to its `seq`. `reason` rides along for
/// failed/abandoned; `None` on completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalLink {
    pub seq: u64,
    pub status: TerminalStatus,
    pub reason: Option<String>,
}

/// The replayed chain: mission identity, the gate ladder, the sessions, the
/// human decisions, and the terminal outcome — every vec in log (seq) order.
/// `None` identity fields mean the log lacked the event that records them
/// (a hand-cut log); the chain still reconstructs around the gap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceChain {
    pub mission_id: String,
    pub goal: Option<String>,
    pub base_branch: Option<String>,
    pub mission_branch: Option<String>,
    /// Base-branch commit SHA pinned at approval; `None` on pre-`baseSha`
    /// logs.
    pub base_sha: Option<String>,
    pub gates: Vec<GateLink>,
    pub sessions: Vec<SessionLink>,
    pub decisions: Vec<DecisionLink>,
    /// The divergence ledger (KRZ-304): comparison records and their
    /// resolutions, each pinned to its seq. Empty on pre-pool logs
    /// (`#[serde(default)]` keeps a pre-field chain.json readable).
    #[serde(default)]
    pub divergences: Vec<DivergenceLink>,
    /// The Flight Rules rule coverage matrix (KRZ-343, design D-H), folded
    /// from the same log by [`crate::standards_coverage`]: every applicable
    /// pinned rule's disposition with its mechanism and evidence joins, the
    /// resolution provenance, and any drift refusals. `None` — and absent
    /// from the machine form — on missions with no approved standards pin
    /// (every pre-Flight-Rules log), so those chains stay byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standards: Option<crate::standards_coverage::StandardsCoverage>,
    /// The FIRST terminal event (a well-formed log has exactly one); `None`
    /// while the mission is still in flight.
    pub outcome: Option<TerminalLink>,
}

/// Fold one mission's provenance chain from its event slice, classifying
/// artefact references against `mission_dir`. `events` may contain other
/// missions' events (filtered out, the [`crate::escalation_metrics`]
/// discipline) but must be in ascending `seq` order — the chain's ordering
/// IS the log's. Pure: no clock, no network, no git; the only I/O is the
/// resolver's metadata probes under `mission_dir`.
///
/// Fallible in exactly one place, by design: a `config.changed` patch that
/// does not merge into a valid [`MissionConfig`] fails here exactly as it
/// fails the reducer's fold (corruption, not a provenance gap). Artefact
/// resolution is total and never contributes an error.
pub fn provenance_chain(
    mission_dir: &Path,
    mission_id: &str,
    events: &[Event],
) -> Result<ProvenanceChain> {
    let mut chain = ProvenanceChain {
        mission_id: mission_id.to_string(),
        goal: None,
        base_branch: None,
        mission_branch: None,
        base_sha: None,
        gates: Vec::new(),
        sessions: Vec::new(),
        decisions: Vec::new(),
        divergences: Vec::new(),
        standards: None,
        outcome: None,
    };
    // The config in force at the current seq (backend derivation); set by
    // mission.created, evolved by config.changed through the reducer's merge.
    let mut config: Option<MissionConfig> = None;
    let mut plan_approved_seq: Option<u64> = None;

    for event in events.iter().filter(|e| e.mission_id == mission_id) {
        match &event.kind {
            EventKind::MissionCreated {
                goal,
                base_branch,
                mission_branch,
                config: created,
            } => {
                chain.goal = Some(goal.clone());
                chain.base_branch = Some(base_branch.clone());
                chain.mission_branch = Some(mission_branch.clone());
                config = Some(created.clone());
            }
            EventKind::PlanApproved { base_sha, .. } => {
                chain.base_sha = base_sha.clone();
                plan_approved_seq = Some(event.seq);
                chain.decisions.push(DecisionLink {
                    seq: event.seq,
                    kind: DecisionKind::PlanApproval,
                    summary: "plan approved".to_string(),
                });
            }
            EventKind::PlanRevised { revision, .. } => chain.decisions.push(DecisionLink {
                seq: event.seq,
                kind: DecisionKind::PlanRevision,
                summary: format!("plan revision {revision} approved"),
            }),
            EventKind::PlanRevisionRejected { revision, reason } => {
                chain.decisions.push(DecisionLink {
                    seq: event.seq,
                    kind: DecisionKind::PlanRevisionRejection,
                    summary: format!("plan revision {revision} rejected: {reason}"),
                });
            }
            EventKind::GrantApproved { kind, command } => chain.decisions.push(DecisionLink {
                seq: event.seq,
                kind: DecisionKind::GrantApproval,
                summary: format!(
                    "approved {}: {command}",
                    crate::escalation_metrics::grant_kind_str(kind)
                ),
            }),
            EventKind::GrantDenied {
                kind,
                command,
                reason,
            } => chain.decisions.push(DecisionLink {
                seq: event.seq,
                kind: DecisionKind::GrantDenial,
                summary: format!(
                    "denied {}: {command} ({reason})",
                    crate::escalation_metrics::grant_kind_str(kind)
                ),
            }),
            EventKind::MilestoneUnblocked {
                milestone_id,
                reason,
                ..
            } if !crate::escalation_metrics::is_engine_lift(reason) => {
                chain.decisions.push(DecisionLink {
                    seq: event.seq,
                    kind: DecisionKind::MilestoneUnblock,
                    summary: format!("unblocked {milestone_id}: {reason}"),
                });
            }
            EventKind::UserMessage { text, .. } => {
                // Classify by SEQUENCE, not wall clock (the escalation-metrics
                // steer rule): the event log's seq is the order of truth.
                let kind = match plan_approved_seq {
                    Some(approved) if event.seq >= approved => DecisionKind::Steer,
                    _ => DecisionKind::OperatorMessage,
                };
                chain.decisions.push(DecisionLink {
                    seq: event.seq,
                    kind,
                    summary: text.clone(),
                });
            }
            EventKind::MissionAbandoned { reason } => {
                chain.decisions.push(DecisionLink {
                    seq: event.seq,
                    kind: DecisionKind::MissionAbandoned,
                    summary: format!("abandoned: {reason}"),
                });
                if chain.outcome.is_none() {
                    chain.outcome = Some(TerminalLink {
                        seq: event.seq,
                        status: TerminalStatus::Abandoned,
                        reason: Some(reason.clone()),
                    });
                }
            }
            EventKind::MissionCompleted {} => {
                if chain.outcome.is_none() {
                    chain.outcome = Some(TerminalLink {
                        seq: event.seq,
                        status: TerminalStatus::Completed,
                        reason: None,
                    });
                }
            }
            EventKind::MissionFailed { reason } => {
                if chain.outcome.is_none() {
                    chain.outcome = Some(TerminalLink {
                        seq: event.seq,
                        status: TerminalStatus::Failed,
                        reason: Some(reason.clone()),
                    });
                }
            }
            EventKind::GateResult {
                gate,
                surface,
                kind,
                index,
                verdict,
                artefact_ref,
                artefact_detail,
                score,
                threshold,
                rule_ids,
            } => chain.gates.push(GateLink {
                seq: event.seq,
                gate: gate.clone(),
                surface: *surface,
                kind: *kind,
                index: *index,
                verdict: *verdict,
                artefact_ref: artefact_ref.clone(),
                artefact_detail: artefact_detail.clone(),
                score: *score,
                threshold: *threshold,
                artefact: ArtefactStatus::classify(&resolve_artefact(mission_dir, artefact_ref)),
                rule_ids: rule_ids.clone(),
            }),
            EventKind::DivergenceNoted {
                unit,
                candidates,
                diverged,
            } => chain.divergences.push(DivergenceLink::Noted {
                seq: event.seq,
                unit: unit.clone(),
                candidates: candidates.clone(),
                diverged: *diverged,
            }),
            EventKind::DivergenceResolved {
                unit,
                selected,
                reason,
                decided_by,
            } => chain.divergences.push(DivergenceLink::Resolved {
                seq: event.seq,
                unit: unit.clone(),
                selected: *selected,
                reason: reason.clone(),
                decided_by: decided_by.clone(),
            }),
            EventKind::WorkerSpawned {
                run_id,
                role,
                feature_id,
                milestone_id,
                model,
                quant,
                weight_hash,
                prompt_hash,
                transcript_path,
                ..
            } => chain.sessions.push(SessionLink {
                seq: event.seq,
                run_id: run_id.clone(),
                role: *role,
                backend: config
                    .as_ref()
                    .map(|config| config.backend_kind(*role).as_str().to_string()),
                model: model.clone(),
                quant: quant.clone(),
                weight_hash: weight_hash.clone(),
                prompt_hash: prompt_hash.clone(),
                feature_id: feature_id.clone(),
                milestone_id: milestone_id.clone(),
                transcript_ref: transcript_path.clone(),
                transcript: ArtefactStatus::classify(&resolve_artefact(
                    mission_dir,
                    &file_artefact_ref(transcript_path),
                )),
            }),
            EventKind::ConfigChanged { patch } => {
                if let Some(current) = &mut config {
                    // The reducer's own merge + error, verbatim: the replay's
                    // tracked config cannot drift from the state fold's.
                    let mut value = serde_json::to_value(&*current)?;
                    crate::reducer::deep_merge(&mut value, patch);
                    *current = serde_json::from_value(value).map_err(|e| {
                        EngineError::Config(format!(
                            "config.changed patch produced invalid config: {e}"
                        ))
                    })?;
                }
            }
            _ => {}
        }
    }
    // The standards coverage matrix (KRZ-343, D-H): one fold over the same
    // slice, attached to the chain so the replay and the evidence bundle
    // render rule dispositions without a second pass. `None` on
    // pre-Flight-Rules logs — their chains stay byte-identical.
    chain.standards = crate::standards_coverage::standards_coverage(mission_id, events);
    Ok(chain)
}

/// Locate one mission under `repo_root` and replay its log into a
/// [`ProvenanceChain`]. Read-only, no lock (§4.3 read-only observers): opens
/// the log no-follow, refuses a symlinked mission path component (P1 — the
/// artefact resolver's probe anchor must be the real mission dir), and
/// writes nothing. A missing/unreadable/corrupt log surfaces as the error,
/// mirroring `load_state`'s posture for single-mission reads.
pub fn compute_provenance(repo_root: &Path, mission_id: &str) -> anyhow::Result<ProvenanceChain> {
    let paths = crate::paths::MissionPaths::new(repo_root, mission_id);
    paths.require_no_follow()?;
    let events = crate::event_log::EventLog::read_events(&paths.events_file())?;
    Ok(provenance_chain(&paths.mission_dir(), mission_id, &events)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{EventLog, LockForce};
    use crate::paths::MissionPaths;
    use crate::types::{GrantKind, Plan};
    use std::time::Duration;
    use tempfile::TempDir;

    /// Seed a mission's `events.jsonl` with the given kinds, in order (the
    /// escalation_metrics fixture idiom); the log handle drops — and flushes
    /// — before any replay reads.
    fn seed_mission(repo_root: &Path, id: &str, kinds: Vec<EventKind>) -> MissionPaths {
        let paths = MissionPaths::new(repo_root, id);
        let mut log = EventLog::acquire(&paths, id, Duration::ZERO, LockForce::No).unwrap();
        for kind in kinds {
            log.append(kind).unwrap();
        }
        paths
    }

    fn sample_plan() -> Plan {
        Plan {
            goal: "ship the thing".into(),
            validation_contract: vec![],
            milestones: vec![],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
            standards_manifest: None,
        }
    }

    /// A config whose Worker runs on codex — the backend the replay must
    /// derive for worker spawns (until a config.changed flips it).
    fn created_config() -> MissionConfig {
        let mut config = MissionConfig::default();
        config.worker.backend = Some("codex".to_string());
        config
    }

    fn created() -> EventKind {
        EventKind::MissionCreated {
            goal: "ship the thing".into(),
            base_branch: "main".into(),
            mission_branch: "kranz/mission-x".into(),
            config: created_config(),
        }
    }

    fn gate_result(
        gate: &str,
        surface: GateSurface,
        kind: GateKind,
        index: u32,
        verdict: GateVerdict,
        artefact_ref: &str,
    ) -> EventKind {
        EventKind::GateResult {
            gate: gate.to_string(),
            surface,
            kind,
            index,
            verdict,
            artefact_ref: artefact_ref.to_string(),
            artefact_detail: None,
            score: None,
            threshold: None,
            rule_ids: Vec::new(),
        }
    }

    fn worker_spawned(run_id: &str, role: Role, model: &str, prompt_hash: &str) -> EventKind {
        EventKind::WorkerSpawned {
            run_id: run_id.to_string(),
            role,
            feature_id: None,
            milestone_id: None,
            candidate: None,
            executor_route: None,
            sdk_session_id: format!("sess-{run_id}"),
            model: model.to_string(),
            quant: "n/a".to_string(),
            weight_hash: None,
            prompt_hash: prompt_hash.to_string(),
            transcript_path: MissionPaths::transcript_rel(run_id),
        }
    }

    /// The anti-vacuity fixture (ticket acceptance hint 1): a mission with
    /// the full gate ladder (both surfaces; an inline ref, a resolved file
    /// ref, a file ref whose bytes were never written), three sessions
    /// straddling a mid-mission backend flip, and the decision set — a plan
    /// approval, a grant park (request is NOT a decision) + approval, an
    /// operator unblock plus the engine-owned lift (NOT a decision), and a
    /// steer — ending COMPLETED.
    fn seed_full_mission(root: &Path) -> MissionPaths {
        let mut judged = gate_result(
            "plan-review",
            GateSurface::Approval,
            GateKind::ModelJudged,
            0,
            GateVerdict::Pass,
            "file:runs/gone.jsonl",
        );
        if let EventKind::GateResult {
            artefact_detail,
            score,
            threshold,
            ..
        } = &mut judged
        {
            *artefact_detail = Some("looks sound".to_string());
            *score = Some(0.9);
            *threshold = Some(0.5);
        }
        let paths = seed_mission(
            root,
            "m-1",
            vec![
                created(),
                EventKind::PlanApproved {
                    plan: sample_plan(),
                    base_sha: Some("deadbeef".to_string()),
                },
                gate_result(
                    "vacuous-filter",
                    GateSurface::Approval,
                    GateKind::Deterministic,
                    0,
                    GateVerdict::Pass,
                    "contract gate vacuous-filter",
                ),
                gate_result(
                    "merge-gate-suite",
                    GateSurface::Approval,
                    GateKind::Deterministic,
                    1,
                    GateVerdict::Pass,
                    "file:runs/gate-base.jsonl",
                ),
                judged,
                {
                    let mut spawn = worker_spawned("r-1", Role::Worker, "gpt-5", "aaaabbbbcccc");
                    if let EventKind::WorkerSpawned {
                        feature_id,
                        milestone_id,
                        ..
                    } = &mut spawn
                    {
                        *feature_id = Some("f-1-1".to_string());
                        *milestone_id = Some("ms-1".to_string());
                    }
                    spawn
                },
                EventKind::GrantRequested {
                    milestone_id: "ms-1".into(),
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
                EventKind::GrantApproved {
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
                EventKind::ConfigChanged {
                    patch: serde_json::json!({"worker": {"backend": "local"}}),
                },
                worker_spawned("r-2", Role::Worker, "my-local-model", "dddd11112222"),
                worker_spawned("r-3", Role::ValidatorScrutiny, "sonnet", "ffff33334444"),
                EventKind::MilestoneBlocked {
                    milestone_id: "ms-1".into(),
                    reason: "fix-cycle cap".into(),
                },
                EventKind::MilestoneUnblocked {
                    milestone_id: "ms-1".into(),
                    reason: "user skipped findings".into(),
                    validator_guidance: None,
                },
                EventKind::MilestoneUnblocked {
                    milestone_id: "ms-1".into(),
                    reason: crate::workspace_gate::GATE_LIFT_REASON.to_string(),
                    validator_guidance: None,
                },
                EventKind::UserMessage {
                    text: "skip the flaky test".into(),
                    interrupt: false,
                },
                gate_result(
                    "merge-gate-suite",
                    GateSurface::FinalGate,
                    GateKind::Deterministic,
                    0,
                    GateVerdict::Pass,
                    ".kranz/merge-gates.json",
                ),
                EventKind::MissionCompleted {},
            ],
        );
        // Bytes for the resolved refs: one gate artefact and one transcript.
        std::fs::write(paths.runs_dir().join("gate-base.jsonl"), b"{}").unwrap();
        std::fs::write(paths.runs_dir().join("r-1.jsonl"), b"{}").unwrap();
        paths
    }

    /// Ticket acceptance hint 1, in one fold: every gate verdict in order,
    /// every artefact ref with its resolution, the backend/model per session
    /// (including the derived backend across a mid-mission flip), the prompt
    /// identity, and each human decision with its event seq — then the
    /// terminal outcome.
    #[test]
    fn provenance_replay_names_ladder_sessions_decisions_and_outcome_in_order() {
        let tmp = TempDir::new().unwrap();
        let paths = seed_full_mission(tmp.path());
        let events = EventLog::read_events(&paths.events_file()).unwrap();
        let chain = provenance_chain(&paths.mission_dir(), "m-1", &events).unwrap();

        // Mission identity from mission.created + plan.approved.
        assert_eq!(chain.mission_id, "m-1");
        assert_eq!(chain.goal.as_deref(), Some("ship the thing"));
        assert_eq!(chain.base_branch.as_deref(), Some("main"));
        assert_eq!(chain.mission_branch.as_deref(), Some("kranz/mission-x"));
        assert_eq!(chain.base_sha.as_deref(), Some("deadbeef"));

        // The ladder, in log order, with resolutions classified.
        let ladder: Vec<(
            u64,
            &str,
            GateSurface,
            GateKind,
            u32,
            GateVerdict,
            ArtefactStatus,
        )> = chain
            .gates
            .iter()
            .map(|gate| {
                (
                    gate.seq,
                    gate.gate.as_str(),
                    gate.surface,
                    gate.kind,
                    gate.index,
                    gate.verdict,
                    gate.artefact,
                )
            })
            .collect();
        assert_eq!(
            ladder,
            vec![
                (
                    3,
                    "vacuous-filter",
                    GateSurface::Approval,
                    GateKind::Deterministic,
                    0,
                    GateVerdict::Pass,
                    ArtefactStatus::Inline
                ),
                (
                    4,
                    "merge-gate-suite",
                    GateSurface::Approval,
                    GateKind::Deterministic,
                    1,
                    GateVerdict::Pass,
                    ArtefactStatus::Resolved
                ),
                (
                    5,
                    "plan-review",
                    GateSurface::Approval,
                    GateKind::ModelJudged,
                    0,
                    GateVerdict::Pass,
                    ArtefactStatus::Unresolved
                ),
                (
                    16,
                    "merge-gate-suite",
                    GateSurface::FinalGate,
                    GateKind::Deterministic,
                    0,
                    GateVerdict::Pass,
                    ArtefactStatus::Inline
                ),
            ]
        );
        // Refs and captured evidence arrive verbatim.
        assert_eq!(chain.gates[0].artefact_ref, "contract gate vacuous-filter");
        assert_eq!(chain.gates[1].artefact_ref, "file:runs/gate-base.jsonl");
        assert_eq!(chain.gates[2].artefact_ref, "file:runs/gone.jsonl");
        assert_eq!(
            chain.gates[2].artefact_detail.as_deref(),
            Some("looks sound")
        );
        assert_eq!(chain.gates[2].score, Some(0.9));
        assert_eq!(chain.gates[2].threshold, Some(0.5));

        // Sessions: model + prompt identity verbatim; backend DERIVED from the
        // recorded config at each seq (codex → local across config.changed;
        // the validator untouched by the worker patch).
        assert_eq!(chain.sessions.len(), 3);
        let r1 = &chain.sessions[0];
        assert_eq!(r1.seq, 6);
        assert_eq!(r1.role, Role::Worker);
        assert_eq!(r1.backend.as_deref(), Some("codex"));
        assert_eq!(r1.model, "gpt-5");
        assert_eq!(r1.prompt_hash, "aaaabbbbcccc");
        assert_eq!(r1.feature_id.as_deref(), Some("f-1-1"));
        assert_eq!(r1.milestone_id.as_deref(), Some("ms-1"));
        assert_eq!(r1.transcript_ref, "runs/r-1.jsonl");
        assert_eq!(r1.transcript, ArtefactStatus::Resolved);
        let r2 = &chain.sessions[1];
        assert_eq!(r2.backend.as_deref(), Some("local"));
        assert_eq!(r2.model, "my-local-model");
        assert_eq!(r2.prompt_hash, "dddd11112222");
        // runs/r-2.jsonl was never written: unresolved, never an error.
        assert_eq!(r2.transcript, ArtefactStatus::Unresolved);
        let r3 = &chain.sessions[2];
        assert_eq!(r3.role, Role::ValidatorScrutiny);
        assert_eq!(r3.backend.as_deref(), Some("claude"));

        // Decisions in seq order: the grant REQUEST (seq 7) and the
        // engine-owned lift (seq 14) are absent by construction.
        let decisions: Vec<(u64, DecisionKind, &str)> = chain
            .decisions
            .iter()
            .map(|d| (d.seq, d.kind, d.summary.as_str()))
            .collect();
        assert_eq!(
            decisions,
            vec![
                (2, DecisionKind::PlanApproval, "plan approved"),
                (
                    8,
                    DecisionKind::GrantApproval,
                    "approved command: cargo test"
                ),
                (
                    13,
                    DecisionKind::MilestoneUnblock,
                    "unblocked ms-1: user skipped findings"
                ),
                (15, DecisionKind::Steer, "skip the flaky test"),
            ]
        );

        assert_eq!(
            chain.outcome,
            Some(TerminalLink {
                seq: 17,
                status: TerminalStatus::Completed,
                reason: None,
            })
        );
    }

    /// Ticket acceptance hint 2: with `runs/` removed the chain still
    /// reconstructs end to end — file-backed refs (gate artefact AND session
    /// transcript) read unresolved, never an error.
    #[test]
    fn provenance_replay_without_runs_dir_reconstructs_with_unresolved_refs() {
        let tmp = TempDir::new().unwrap();
        let paths = seed_full_mission(tmp.path());
        std::fs::remove_dir_all(paths.runs_dir()).unwrap();

        let chain = compute_provenance(tmp.path(), "m-1").unwrap();
        assert_eq!(chain.gates.len(), 4);
        assert_eq!(chain.gates[1].artefact, ArtefactStatus::Unresolved);
        // Textual refs are inline no matter what the filesystem holds.
        assert_eq!(chain.gates[0].artefact, ArtefactStatus::Inline);
        assert_eq!(chain.gates[3].artefact, ArtefactStatus::Inline);
        assert_eq!(chain.sessions.len(), 3);
        for session in &chain.sessions {
            assert_eq!(
                session.transcript,
                ArtefactStatus::Unresolved,
                "{} must read unresolved with runs/ gone",
                session.run_id
            );
        }
        assert_eq!(chain.decisions.len(), 4);
        assert_eq!(
            chain.outcome.map(|o| o.status),
            Some(TerminalStatus::Completed)
        );
    }

    /// Ticket acceptance hint 3: same log → byte-identical machine output,
    /// across two independent compute passes (read + fold + resolve each).
    #[test]
    fn provenance_replay_machine_form_is_byte_identical_across_replays() {
        let tmp = TempDir::new().unwrap();
        seed_full_mission(tmp.path());
        let first = compute_provenance(tmp.path(), "m-1").unwrap();
        let second = compute_provenance(tmp.path(), "m-1").unwrap();
        assert_eq!(first, second);
        let first_json = serde_json::to_string_pretty(&first).unwrap();
        let second_json = serde_json::to_string_pretty(&second).unwrap();
        assert_eq!(first_json, second_json);
        // The machine form carries no host layout: the temp dir's absolute
        // path appears nowhere in the serialization.
        assert!(
            !first_json.contains(&tmp.path().to_string_lossy().to_string()),
            "host path leaked into the machine form: {first_json}"
        );
    }

    /// Old logs (pre-`gate.result`, pre-`baseSha`) still fold: the ladder is
    /// empty, the identity falls back to what the log carries, and the
    /// failure outcome surfaces with its reason.
    #[test]
    fn provenance_replay_pre_gate_logs_still_fold() {
        let tmp = TempDir::new().unwrap();
        let paths = seed_mission(
            tmp.path(),
            "m-old",
            vec![
                EventKind::MissionCreated {
                    goal: "legacy goal".into(),
                    base_branch: "main".into(),
                    mission_branch: "kranz/mission-old".into(),
                    config: MissionConfig::default(),
                },
                EventKind::PlanApproved {
                    plan: sample_plan(),
                    base_sha: None,
                },
                worker_spawned("r-1", Role::Worker, "sonnet", "9999aaaabbbb"),
                EventKind::MissionFailed {
                    reason: "honest failure".into(),
                },
            ],
        );
        let events = EventLog::read_events(&paths.events_file()).unwrap();
        let chain = provenance_chain(&paths.mission_dir(), "m-old", &events).unwrap();
        assert!(chain.gates.is_empty());
        assert_eq!(chain.base_sha, None);
        assert_eq!(chain.sessions.len(), 1);
        assert_eq!(chain.sessions[0].backend.as_deref(), Some("claude"));
        assert_eq!(chain.sessions[0].prompt_hash, "9999aaaabbbb");
        assert_eq!(
            chain.outcome,
            Some(TerminalLink {
                seq: 4,
                status: TerminalStatus::Failed,
                reason: Some("honest failure".to_string()),
            })
        );
        // A pre-approval message is drafting, not a steer — but still a
        // named human act in the chain.
        let paths = seed_mission(
            tmp.path(),
            "m-draft",
            vec![
                EventKind::UserMessage {
                    text: "make it smaller".into(),
                    interrupt: false,
                },
                EventKind::PlanApproved {
                    plan: sample_plan(),
                    base_sha: None,
                },
            ],
        );
        let events = EventLog::read_events(&paths.events_file()).unwrap();
        let chain = provenance_chain(&paths.mission_dir(), "m-draft", &events).unwrap();
        assert_eq!(
            chain
                .decisions
                .iter()
                .map(|d| (d.seq, d.kind))
                .collect::<Vec<_>>(),
            vec![
                (1, DecisionKind::OperatorMessage),
                (2, DecisionKind::PlanApproval)
            ]
        );
        // No mission.created: identity fields stay None and the chain still
        // reconstructs; in flight, so no outcome.
        assert_eq!(chain.goal, None);
        assert_eq!(chain.outcome, None);
    }

    /// The one fallible path, by design: a config.changed patch that cannot
    /// merge into a valid MissionConfig fails the replay with the reducer's
    /// own error — corruption, not a provenance gap.
    #[test]
    fn provenance_replay_invalid_config_patch_fails_like_the_reducer() {
        let tmp = TempDir::new().unwrap();
        let paths = seed_mission(
            tmp.path(),
            "m-1",
            vec![
                created(),
                EventKind::ConfigChanged {
                    patch: serde_json::json!({"maxFixCyclesPerMilestone": "not-a-number"}),
                },
            ],
        );
        let events = EventLog::read_events(&paths.events_file()).unwrap();
        let result = provenance_chain(&paths.mission_dir(), "m-1", &events);
        assert!(
            matches!(result, Err(EngineError::Config(_))),
            "expected the reducer's Config error, got {result:?}"
        );
    }

    /// The divergence record and its resolution survive provenance replay
    /// (ticket divergence-first-class-event, KRZ-304): both appear in the
    /// chain pinned to their seqs, the candidate refs verbatim, and a
    /// pre-pool log folds with an empty ledger (`#[serde(default)]` keeps a
    /// pre-field chain.json readable too).
    #[test]
    fn divergence_event_provenance_chain_carries_record_and_resolution() {
        let tmp = TempDir::new().unwrap();
        let candidate = |run_id: &str, tree: &str| crate::types::DivergenceCandidate {
            run_id: run_id.into(),
            branch: format!("kranz/pool/m-1/f-1-1-{run_id}"),
            backend: "claude".into(),
            tree: tree.into(),
        };
        let paths = seed_mission(
            tmp.path(),
            "m-1",
            vec![
                created(),
                EventKind::DivergenceNoted {
                    unit: "f-1-1".into(),
                    candidates: vec![candidate("r-c0", "aaa"), candidate("r-c1", "bbb")],
                    diverged: true,
                },
                EventKind::DivergenceResolved {
                    unit: "f-1-1".into(),
                    selected: Some(1),
                    reason: "codex kept it total".into(),
                    decided_by: "operator".into(),
                },
            ],
        );
        let events = EventLog::read_events(&paths.events_file()).unwrap();
        let chain = provenance_chain(&paths.mission_dir(), "m-1", &events).unwrap();
        assert_eq!(chain.divergences.len(), 2);
        match &chain.divergences[0] {
            DivergenceLink::Noted {
                seq,
                unit,
                candidates,
                diverged,
            } => {
                assert_eq!(*seq, 2);
                assert_eq!(unit, "f-1-1");
                assert!(diverged);
                assert_eq!(candidates.len(), 2);
                assert_eq!(candidates[1].tree, "bbb");
            }
            other => panic!("expected the noted link first: {other:?}"),
        }
        match &chain.divergences[1] {
            DivergenceLink::Resolved {
                seq,
                unit,
                selected,
                reason,
                decided_by,
            } => {
                assert_eq!(*seq, 3);
                assert_eq!(unit, "f-1-1");
                assert_eq!(*selected, Some(1));
                assert_eq!(reason, "codex kept it total");
                assert_eq!(decided_by, "operator");
            }
            other => panic!("expected the resolution link: {other:?}"),
        }

        // A pre-pool log folds with an empty ledger, and a chain.json
        // predating the field still deserializes (serde default).
        let quiet = provenance_chain(&paths.mission_dir(), "m-1", &[]).unwrap();
        assert!(quiet.divergences.is_empty());
        let json = serde_json::to_value(&chain).unwrap();
        let mut stripped = json.clone();
        stripped.as_object_mut().unwrap().remove("divergences");
        let back: ProvenanceChain = serde_json::from_value(stripped).unwrap();
        assert!(back.divergences.is_empty());
    }

    // ---- KRZ-343: the standards coverage matrix rides the chain -----------

    /// A plan carrying a two-rule standards pin (KRZ-342's consent shape):
    /// one enforced must, one approved should.
    fn pinned_plan() -> Plan {
        let rule = |id: &str, revision: u64, status: &str, level: &str| crate::types::PinnedRule {
            id: id.to_string(),
            revision,
            rfc: "RFC-001".to_string(),
            level: level.to_string(),
            effective_status: status.to_string(),
            statement: format!("statement for {id}"),
            domains: Vec::new(),
            stages: vec!["validation".to_string()],
            when_paths: Vec::new(),
            task_classes: Vec::new(),
            checker: Some("gate:zz-gate".to_string()),
            waivable: false,
        };
        Plan {
            standards_manifest: Some(Box::new(crate::types::StandardsPin {
                pack_name: "zz-pack".to_string(),
                pack_dir: "vendor/pack".to_string(),
                standards_root: "standards".to_string(),
                digest: "ab".repeat(32),
                source: crate::types::StandardsPinSource::RepoTracked,
                task_class: None,
                touch_set: vec!["crates/**".to_string()],
                gates: Vec::new(),
                rules: vec![
                    rule("ZZ-FAIL-001", 2, "enforced", "must"),
                    rule("ZZ-QUIET-001", 1, "approved", "should"),
                ],
            })),
            ..sample_plan()
        }
    }

    /// KRZ-343 (D-H): the replay folds the coverage matrix from the same
    /// log — a finding's rule citation joins its pinned row, the untouched
    /// rule reads not-evaluated, and the machine form carries the section.
    #[test]
    fn flight_rules_provenance_replay_folds_the_coverage_matrix() {
        let tmp = TempDir::new().unwrap();
        seed_mission(
            tmp.path(),
            "m-1",
            vec![
                created(),
                EventKind::PlanApproved {
                    plan: pinned_plan(),
                    base_sha: Some("deadbeef".to_string()),
                },
                EventKind::StandardsResolved {
                    source: "repo-tracked".to_string(),
                    pack_name: "zz-pack".to_string(),
                    standards_root: "standards".to_string(),
                    digest: "ab".repeat(32),
                    stage: "approval".to_string(),
                    task_class: None,
                    touch_set: vec!["crates/**".to_string()],
                    rules: vec![
                        crate::types::StandardsRuleRef {
                            id: "ZZ-FAIL-001".to_string(),
                            revision: 2,
                            effective_status: "enforced".to_string(),
                        },
                        crate::types::StandardsRuleRef {
                            id: "ZZ-QUIET-001".to_string(),
                            revision: 1,
                            effective_status: "approved".to_string(),
                        },
                    ],
                    approval_seq: 2,
                },
                EventKind::ValidationFinding {
                    milestone_id: "ms-1".into(),
                    run_id: "v-1".into(),
                    finding: crate::types::Finding {
                        subject: "a-1".into(),
                        severity: "major".into(),
                        evidence: "broke the rule".into(),
                        suggested_fix: String::new(),
                        class: String::new(),
                        rule: Some(crate::types::RuleCitation {
                            id: "ZZ-FAIL-001".to_string(),
                            revision: 2,
                            source: "zz-pack standards".to_string(),
                            digest: "ab".repeat(32),
                            lifecycle: "enforced".to_string(),
                            level: "must".to_string(),
                            checker: Some("gate:zz-gate".to_string()),
                        }),
                    },
                },
                EventKind::MissionCompleted {},
            ],
        );
        let chain = compute_provenance(tmp.path(), "m-1").unwrap();
        let coverage = chain
            .standards
            .as_ref()
            .expect("the matrix rides the chain");
        assert_eq!(coverage.pack_name, "zz-pack");
        assert_eq!(coverage.approval_seq, 2);
        assert_eq!(coverage.resolution_seq, Some(3));
        assert_eq!(coverage.rules.len(), 2);
        let failed = &coverage.rules[0];
        assert_eq!(failed.id, "ZZ-FAIL-001");
        assert_eq!(
            failed.disposition,
            crate::standards_coverage::RuleDisposition::Failed
        );
        assert_eq!(failed.evidence.len(), 1);
        assert_eq!(failed.evidence[0].seq, 4);
        assert_eq!(failed.evidence[0].mechanism, "v-1");
        let quiet = &coverage.rules[1];
        assert_eq!(quiet.id, "ZZ-QUIET-001");
        assert_eq!(
            quiet.disposition,
            crate::standards_coverage::RuleDisposition::NotEvaluated
        );

        // The machine form carries the section; a chain.json predating the
        // field still deserializes (serde default).
        let json = serde_json::to_value(&chain).unwrap();
        assert_eq!(json["standards"]["digest"], "ab".repeat(32));
        assert_eq!(json["standards"]["rules"][0]["disposition"], "failed");
        let mut stripped = json.clone();
        stripped.as_object_mut().unwrap().remove("standards");
        let back: ProvenanceChain = serde_json::from_value(stripped).unwrap();
        assert!(back.standards.is_none());
        // …and the pinned manifest stays inspectable through the machine
        // form: id, revision, lifecycle, level, checker, statement all ride
        // the row.
        let row = &json["standards"]["rules"][1];
        assert_eq!(row["id"], "ZZ-QUIET-001");
        assert_eq!(row["revision"], 1);
        assert_eq!(row["lifecycle"], "approved");
        assert_eq!(row["level"], "should");
        assert_eq!(row["checker"], "gate:zz-gate");
        assert_eq!(row["statement"], "statement for ZZ-QUIET-001");
    }

    /// The byte-compat regression contract: a pre-Flight-Rules log (the
    /// full KRZ-325 fixture — no pin, no standards events) folds with NO
    /// standards section, so its chain JSON is byte-identical to what the
    /// replay produced before this field existed.
    #[test]
    fn flight_rules_provenance_pre_flight_rules_chain_is_unchanged() {
        let tmp = TempDir::new().unwrap();
        seed_full_mission(tmp.path());
        let chain = compute_provenance(tmp.path(), "m-1").unwrap();
        assert!(chain.standards.is_none());
        let json = serde_json::to_string_pretty(&chain).unwrap();
        assert!(
            !json.contains("\"standards\""),
            "a pre-Flight-Rules chain carries no standards key: {json}"
        );
        // A chain.json written before the field existed still deserializes.
        let mut value = serde_json::to_value(&chain).unwrap();
        value.as_object_mut().unwrap().remove("divergences");
        let back: ProvenanceChain = serde_json::from_value(value).unwrap();
        assert!(back.standards.is_none());
    }
}
