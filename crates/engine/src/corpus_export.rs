//! Provenance-tagged training-corpus export (ticket
//! `.kranz/tickets/training-corpus-export.md`, KRZ-332 — the governance
//! evidence layer's flywheel feed): one JSONL stream joining the three
//! judged-artefact sources a local model can learn from —
//!
//! 1. **Validated worker traces** — [`crate::trace_export`]'s
//!    instruction pairs (a run qualifies only as validation-PASSED:
//!    `Role::Worker`, `RunResult::Pass`, feature Complete inside a Complete
//!    milestone), now carrying the provenance tags. A failed or unvalidated
//!    session is excluded by CONSTRUCTION — the selection fold is reused,
//!    not reimplemented, so the corpus can never widen past the trace
//!    export's gate.
//! 2. **Divergence records** — each `divergence.noted` comparison (every
//!    candidate ref verbatim: run id, branch, backend, tree hash) paired
//!    with its `divergence.resolved` judgement (selected index or none,
//!    reason, decider). These are the consent/judgement pairs: what the
//!    pool produced, and which side the human picked and why.
//! 3. **Escalation-ledger records** — the flight-surgeon fold
//!    ([`crate::escalation_metrics`]) as labeled human-judgment examples:
//!    grant parks (ask, decision, latency), steers, and milestone blocks
//!    the OPERATOR lifted.
//!
//! Every record carries provenance refs that RESOLVE via the provenance
//! replay ([`crate::provenance::provenance_chain`]) — they are taken FROM
//! the replay, not recomputed beside it: the trace's backend is the
//! replay's config-at-seq derivation, the gate-chain refs are the replay's
//! ladder seqs, the divergence seqs are the replay's ledger seqs, and an
//! escalation's decision seq joins the replay's human-decision chain. A
//! consumer can therefore walk any record back to the mission, the session,
//! and the gates that vouched for it without re-deriving anything.
//!
//! WHY a new `export-corpus` command rather than extending `export-traces`:
//! the trace export's line shape IS its consumer contract (one
//! instruction-pair object per line), and the two new sources are not
//! instruction pairs — a consent judgement or an escalation decision has no
//! instruction/response. Widening `export-traces` would either break that
//! contract or force a tagged union onto a command whose name promises
//! traces. A separate command keeps `export-traces` byte-stable and gives
//! the corpus one stream, one ordering rule, one determinism rule.
//!
//! Determinism, in the substrate's own discipline ([`crate::trace_export`],
//! [`crate::provenance`]): the export is a pure function of the event log —
//! no clock is consulted (the only time-derived value, a grant/block
//! latency, is a difference of RECORDED timestamps), no hashed map is
//! iterated (trace selection walks `state.runs`, a BTreeMap; every other
//! fold keeps log order in Vecs), and serialization is struct-order
//! stable. Same log → byte-identical JSONL.
//!
//! WHY the ordering key is what it is: records group by source in a fixed
//! order (worker traces, then divergences, then escalations), stable within
//! each group — run id for traces (the BTreeMap order), the noted event's
//! seq for divergences, the ask event's seq for escalations. Each
//! within-group key is already total over one log, while a merged seq
//! ordering would interleave sources for no gain: a corpus consumer filters
//! by `source` anyway. Across missions (`kranz export-corpus --all`),
//! mission ids sort (the [`crate::paths::MissionPaths::list_missions`]
//! order), then the same grouping applies per mission.

use crate::error::Result;
use crate::events::{Event, EventKind};
use crate::gate::{GateSurface, GateVerdict};
use crate::provenance::{DivergenceLink, ProvenanceChain};
use crate::types::{DivergenceCandidate, MissionState};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One gate-ladder link referenced by a worker-trace record: identity,
/// surface, and verdict of one `gate.result`, pinned by its event `seq` —
/// the join key into the provenance replay's ladder
/// ([`crate::provenance::GateLink`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateChainRef {
    pub seq: u64,
    pub gate: String,
    pub surface: GateSurface,
    pub verdict: GateVerdict,
}

/// A validation-PASSED worker trace: the
/// [`crate::trace_export::InstructionPair`] fields verbatim (so a corpus
/// line is a strict superset of an `export-traces` line) plus the
/// provenance tags the ticket demands — the backend the session ran on and
/// the mission's gate ladder by seq.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerTraceRecord {
    pub instruction: String,
    pub response: String,
    pub model: String,
    pub quant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_hash: Option<String>,
    pub mission_id: String,
    pub feature_id: String,
    pub run_id: String,
    /// DERIVED, never read from the event: the replay's config-at-seq
    /// backend derivation ([`crate::provenance::SessionLink::backend`]), so
    /// the corpus and the audit chain can never disagree about which
    /// backend drove the session. Serialized as `null` (never omitted) on
    /// hand-cut logs with no `mission.created` — a corpus consumer should
    /// see the gap, not guess at it.
    pub backend: Option<String>,
    /// The mission's gate ladder as replay refs. The ladder is
    /// mission-level (contract gates evaluate the floor, not an individual
    /// run), so every trace of one mission carries the same chain; empty on
    /// pre-`gate.result` logs.
    pub gate_chain: Vec<GateChainRef>,
}

/// The judgement half of a divergence record: which candidate was selected
/// (an index into the record's `candidates`; `null` = judged-and-abandoned,
/// serialized explicitly so it cannot be confused with a real index), why,
/// by whom, and at which event seq (the join key into the replay's
/// divergence ledger).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DivergenceResolution {
    pub selected: Option<u32>,
    pub reason: String,
    pub decided_by: String,
    pub resolved_seq: u64,
}

/// One `divergence.noted`, paired with its resolution: the comparison
/// record (every candidate ref verbatim — the run ids and branches the
/// judgement chose between) plus the judgement that landed, or `null`
/// while the unit awaits one. `diverged: false` is the agreement record:
/// logged, never trusted — it exports like any other noted, the consumer
/// decides what to learn from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DivergenceRecord {
    pub mission_id: String,
    pub unit: String,
    /// The noted event's seq — the record's own position in the log and its
    /// join key into the replay.
    pub noted_seq: u64,
    pub diverged: bool,
    pub candidates: Vec<DivergenceCandidate>,
    pub resolution: Option<DivergenceResolution>,
}

/// Which kind of escalation one record labels (the ledger's grant/steer set
/// plus blocks — a milestone block the operator lifted is a human judgement
/// the ledger only counts, and the corpus names it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EscalationKind {
    Grant,
    Steer,
    Block,
}

/// One escalation as a labeled human-judgment example: what was asked, what
/// was decided, how long the decision took, and the seq refs that join it
/// to the replay. Grant/block rows mirror the ledger's vocabulary
/// (`approved` / `denied: <reason>` / `pending`); a block's decision is
/// `unblocked: <reason>`. Pending rows export with a `null` decision seq —
/// the ask is real log data the consumer may want, the missing label is
/// visible rather than silently dropped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationRecord {
    pub mission_id: String,
    pub kind: EscalationKind,
    /// Grant/block: the milestone that parked; `null` on steers.
    pub milestone_id: Option<String>,
    /// Grant: `<kind>: <command>`; steer: the operator's message; block:
    /// the milestone.blocked reason.
    pub ask: String,
    pub decision: String,
    /// Ask→decision latency from the RECORDED timestamps (no clock is
    /// consulted); `null` while pending and on steers.
    pub latency_ms: Option<u64>,
    /// Seq of the ask event (grant.requested / user.message /
    /// milestone.blocked) — the record's ordering key.
    pub ask_seq: u64,
    /// Seq of the decision event — joins the replay's human-decision chain
    /// ([`crate::provenance::DecisionLink`]); `null` while pending.
    pub decision_seq: Option<u64>,
}

/// One corpus line: a tagged union over the three sources (module docs).
/// The `source` tag is the consumer's filter key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case")]
pub enum CorpusRecord {
    WorkerTrace(WorkerTraceRecord),
    Divergence(DivergenceRecord),
    Escalation(EscalationRecord),
}

/// Derive one mission's corpus records from its event log. `events` must be
/// that mission's log (the [`crate::reducer::fold`] discipline, same as
/// `export-traces`); `mission_dir` anchors the provenance replay's artefact
/// probes. Fallible exactly where its two folds are: a log the reducer
/// rejects, or a `config.changed` patch the replay cannot merge — both are
/// corruption, never a corpus gap.
///
/// Pure apart from the replay's read-only artefact probes: no clock, no
/// network, no git, nothing persisted — calling this again over the same
/// log yields the same records.
pub fn export_corpus(
    mission_dir: &Path,
    mission_id: &str,
    events: &[Event],
) -> Result<Vec<CorpusRecord>> {
    let state = crate::reducer::fold(events)?;
    let chain = crate::provenance::provenance_chain(mission_dir, mission_id, events)?;
    let mut records = Vec::new();
    records.extend(
        worker_trace_records(&state, events, &chain)
            .into_iter()
            .map(CorpusRecord::WorkerTrace),
    );
    records.extend(
        divergence_records(mission_id, &chain)
            .into_iter()
            .map(CorpusRecord::Divergence),
    );
    records.extend(
        escalation_records(mission_id, events)
            .into_iter()
            .map(CorpusRecord::Escalation),
    );
    Ok(records)
}

/// Source 1: the validation-PASSED instruction pairs, tagged. Selection
/// stays with [`crate::trace_export::export_validated_traces`] — a failed
/// or unvalidated session never enters the corpus because it never enters
/// THAT fold; this only decorates what it returns with the replay's
/// backend derivation and ladder refs.
fn worker_trace_records(
    state: &MissionState,
    events: &[Event],
    chain: &ProvenanceChain,
) -> Vec<WorkerTraceRecord> {
    let gate_chain: Vec<GateChainRef> = chain
        .gates
        .iter()
        .map(|gate| GateChainRef {
            seq: gate.seq,
            gate: gate.gate.clone(),
            surface: gate.surface,
            verdict: gate.verdict,
        })
        .collect();
    crate::trace_export::export_validated_traces(state, events)
        .into_iter()
        .map(|pair| {
            let backend = chain
                .sessions
                .iter()
                .find(|session| session.run_id == pair.run_id)
                .and_then(|session| session.backend.clone());
            WorkerTraceRecord {
                instruction: pair.instruction,
                response: pair.response,
                model: pair.model,
                quant: pair.quant,
                weight_hash: pair.weight_hash,
                mission_id: pair.mission_id,
                feature_id: pair.feature_id,
                run_id: pair.run_id,
                backend,
                gate_chain: gate_chain.clone(),
            }
        })
        .collect()
}

/// Source 2: each noted divergence with its resolution. Pairing rule: a
/// noted pairs with the earliest LATER (by seq) unconsumed resolution for
/// the same unit — "at most one resolution per unit, the first judgement
/// stands" (events.rs) means a re-noted unit must NOT inherit the old
/// resolution, and consuming each resolution once keeps the pairing total
/// and deterministic. The replay's ledger is already in log order.
fn divergence_records(mission_id: &str, chain: &ProvenanceChain) -> Vec<DivergenceRecord> {
    let mut used_resolutions = vec![false; chain.divergences.len()];
    let mut records = Vec::new();
    for link in &chain.divergences {
        let DivergenceLink::Noted {
            seq,
            unit,
            candidates,
            diverged,
        } = link
        else {
            continue;
        };
        let mut resolution = None;
        for (i, candidate) in chain.divergences.iter().enumerate() {
            if used_resolutions[i] {
                continue;
            }
            if let DivergenceLink::Resolved {
                seq: resolved_seq,
                unit: resolved_unit,
                selected,
                reason,
                decided_by,
            } = candidate
            {
                if resolved_unit == unit && resolved_seq > seq {
                    used_resolutions[i] = true;
                    resolution = Some(DivergenceResolution {
                        selected: *selected,
                        reason: reason.clone(),
                        decided_by: decided_by.clone(),
                        resolved_seq: *resolved_seq,
                    });
                    break;
                }
            }
        }
        records.push(DivergenceRecord {
            mission_id: mission_id.to_string(),
            unit: unit.clone(),
            noted_seq: *seq,
            diverged: *diverged,
            candidates: candidates.clone(),
            resolution,
        });
    }
    records
}

/// Source 3: the escalation ledger as judgment examples. Grant pairing is
/// [`crate::escalation_metrics::pair_grant_decisions`] itself and the steer
/// rule is its SEQUENCE rule (a `user.message` at/after `plan.approved`) —
/// the corpus and the flight surgeon read the same log identically. Blocks
/// pair each `milestone.blocked` with the earliest later unconsumed unblock
/// for the same milestone; a block the ENGINE lifted (the workspace gate,
/// [`crate::escalation_metrics::is_engine_lift`]) is not a human judgement
/// and yields no record, consuming its unblock so it cannot pair again.
fn escalation_records(mission_id: &str, events: &[Event]) -> Vec<EscalationRecord> {
    let mission_events: Vec<&Event> = events
        .iter()
        .filter(|e| e.mission_id == mission_id)
        .collect();
    let plan_approved_seq = mission_events
        .iter()
        .find(|e| matches!(e.kind, EventKind::PlanApproved { .. }))
        .map(|e| e.seq);
    let mut records = Vec::new();

    // Steers: the message is both ask and decision.
    for e in &mission_events {
        if let EventKind::UserMessage { text, .. } = &e.kind {
            if let Some(approved) = plan_approved_seq {
                if e.seq >= approved {
                    records.push(EscalationRecord {
                        mission_id: mission_id.to_string(),
                        kind: EscalationKind::Steer,
                        milestone_id: None,
                        ask: text.clone(),
                        decision: "steered".to_string(),
                        latency_ms: None,
                        ask_seq: e.seq,
                        decision_seq: Some(e.seq),
                    });
                }
            }
        }
    }

    // Grant parks, via the shared pairing rule.
    for (req_idx, matched) in crate::escalation_metrics::pair_grant_decisions(&mission_events) {
        let req = mission_events[req_idx];
        let EventKind::GrantRequested {
            milestone_id,
            kind,
            command,
        } = &req.kind
        else {
            unreachable!("pair_grant_decisions only returns grant.requested indices")
        };
        let (decision, latency_ms, decision_seq) = match matched {
            Some(i) => {
                let decided = mission_events[i];
                let latency = (decided.ts - req.ts).num_milliseconds();
                let latency_ms = if latency >= 0 {
                    Some(latency as u64)
                } else {
                    None
                };
                let decision = match &decided.kind {
                    EventKind::GrantApproved { .. } => "approved".to_string(),
                    EventKind::GrantDenied { reason, .. } => format!("denied: {reason}"),
                    _ => unreachable!("pair_grant_decisions only matches grant decisions"),
                };
                (decision, latency_ms, Some(decided.seq))
            }
            None => ("pending".to_string(), None, None),
        };
        records.push(EscalationRecord {
            mission_id: mission_id.to_string(),
            kind: EscalationKind::Grant,
            milestone_id: Some(milestone_id.clone()),
            ask: format!(
                "{}: {command}",
                crate::escalation_metrics::grant_kind_str(kind)
            ),
            decision,
            latency_ms,
            ask_seq: req.seq,
            decision_seq,
        });
    }

    // Milestone blocks the operator lifted.
    let mut used_unblocks = vec![false; mission_events.len()];
    for (block_idx, block) in mission_events.iter().enumerate() {
        let EventKind::MilestoneBlocked {
            milestone_id,
            reason,
        } = &block.kind
        else {
            continue;
        };
        let mut matched = None;
        for (i, candidate) in mission_events.iter().enumerate() {
            if i <= block_idx || used_unblocks[i] {
                continue;
            }
            if let EventKind::MilestoneUnblocked {
                milestone_id: unblocked,
                ..
            } = &candidate.kind
            {
                if unblocked == milestone_id {
                    used_unblocks[i] = true;
                    matched = Some(i);
                    break;
                }
            }
        }
        let (decision, latency_ms, decision_seq) = match matched {
            Some(i) => {
                let decided = mission_events[i];
                let EventKind::MilestoneUnblocked {
                    reason: unblock_reason,
                    ..
                } = &decided.kind
                else {
                    unreachable!("matched only milestone.unblocked above")
                };
                if crate::escalation_metrics::is_engine_lift(unblock_reason) {
                    continue;
                }
                let latency = (decided.ts - block.ts).num_milliseconds();
                let latency_ms = if latency >= 0 {
                    Some(latency as u64)
                } else {
                    None
                };
                (
                    format!("unblocked: {unblock_reason}"),
                    latency_ms,
                    Some(decided.seq),
                )
            }
            None => ("pending".to_string(), None, None),
        };
        records.push(EscalationRecord {
            mission_id: mission_id.to_string(),
            kind: EscalationKind::Block,
            milestone_id: Some(milestone_id.clone()),
            ask: reason.clone(),
            decision,
            latency_ms,
            ask_seq: block.seq,
            decision_seq,
        });
    }

    // Merge the three kinds into one lane by the ask event's seq — unique
    // per record (event seqs are unique within a log), so the order is
    // total and stable.
    records.sort_by_key(|record| record.ask_seq);
    records
}

/// Render corpus records as JSONL: one compact JSON object per line, each
/// terminated by `\n`. Pure function of its input — the
/// [`crate::trace_export::to_jsonl`] discipline — so it is byte-identical
/// across repeated calls on the same records.
pub fn to_jsonl(records: &[CorpusRecord]) -> String {
    let mut out = String::new();
    for record in records {
        out.push_str(&serde_json::to_string(record).expect("CorpusRecord always serializes"));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventKind;
    use crate::gate::GateKind;
    use crate::types::*;
    use chrono::{DateTime, TimeZone, Utc};

    const MISSION: &str = "m-1";

    fn base_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
    }

    /// One event per fixture line: ts = base + seq seconds, so a one-seq gap
    /// between an ask and its decision reads as exactly 1000ms of latency.
    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            ts: base_ts() + chrono::Duration::seconds(seq as i64),
            mission_id: MISSION.to_string(),
            kind,
        }
    }

    fn plan_feature(title: &str) -> PlanFeature {
        PlanFeature {
            title: title.to_string(),
            spec: format!("spec for {title}"),
            validation_criteria: vec![format!("{title} works")],
        }
    }

    /// One milestone, two features: f-1-1 (passes) and f-1-2 (fails).
    fn plan() -> Plan {
        Plan {
            goal: "build the thing".to_string(),
            validation_contract: vec![],
            milestones: vec![PlanMilestone {
                title: "milestone one".to_string(),
                features: vec![plan_feature("alpha"), plan_feature("beta")],
            }],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
            standards_manifest: None,
            reviewer_independence: None,
        }
    }

    fn gate(index: u32) -> EventKind {
        EventKind::GateResult {
            gate: "merge-gate-suite".to_string(),
            surface: GateSurface::Approval,
            kind: GateKind::Deterministic,
            index,
            verdict: GateVerdict::Pass,
            artefact_ref: format!("contract gate {index}"),
            artefact_detail: None,
            score: None,
            threshold: None,
            rule_ids: Vec::new(),
        }
    }

    fn spawn(run_id: &str, feature_id: &str, model: &str) -> EventKind {
        EventKind::WorkerSpawned {
            backend: None,
            run_id: run_id.to_string(),
            role: Role::Worker,
            feature_id: Some(feature_id.to_string()),
            milestone_id: None,
            candidate: None,
            executor_route: None,
            sdk_session_id: format!("sess-{run_id}"),
            model: model.to_string(),
            quant: "n/a".to_string(),
            weight_hash: None,
            prompt_hash: "deadbeef".to_string(),
            transcript_path: format!("runs/{run_id}.jsonl"),
        }
    }

    fn completed(run_id: &str, result: RunResult, summary: &str) -> EventKind {
        EventKind::WorkerCompleted {
            run_id: run_id.to_string(),
            result,
            tokens: TokenUsage::default(),
            cost_usd: None,
            report: Some(WorkerReport {
                result,
                summary: summary.to_string(),
                files_touched: vec![],
                tests_added: vec![],
                test_evidence: "cargo test: ok".to_string(),
                dependencies_added: vec![],
                known_gaps: vec![],
                commits: vec!["deadbeef commit".to_string()],
                commands_run: vec![],
                escalation: None,
                questions: None,
            }),
        }
    }

    fn candidates() -> Vec<DivergenceCandidate> {
        vec![
            DivergenceCandidate {
                run_id: "r-pass".to_string(),
                branch: format!("kranz/pool/{MISSION}/f-1-1-c0"),
                backend: "claude".to_string(),
                tree: "aaa".to_string(),
            },
            DivergenceCandidate {
                run_id: "r-cand".to_string(),
                branch: format!("kranz/pool/{MISSION}/f-1-1-c1"),
                backend: "codex".to_string(),
                tree: "bbb".to_string(),
            },
        ]
    }

    /// The anti-vacuity fixture: one mission touching all three sources.
    ///
    /// - Traces: r-pass qualifies (Pass on a Complete feature); r-fail
    ///   (failed feature) and r-cand (spawned, never completed — an
    ///   unvalidated session) must never enter the corpus.
    /// - Divergences: f-1-1 noted diverged and resolved (selected 0), then
    ///   noted AGAIN as an agreement record that is never resolved — the
    ///   second noted must NOT inherit the first's resolution.
    /// - Escalations: an operator-lifted block, an approved grant, a steer,
    ///   a pending grant, and an ENGINE-lifted block (no record) — in log
    ///   order with 1s ask→decision gaps (1000ms latencies).
    fn fixture_events() -> Vec<Event> {
        vec![
            ev(
                1,
                EventKind::MissionCreated {
                    goal: "build the thing".to_string(),
                    base_branch: "main".to_string(),
                    mission_branch: format!("kranz/mission-{MISSION}"),
                    config: MissionConfig::default(),
                },
            ),
            ev(
                2,
                EventKind::PlanApproved {
                    plan: plan(),
                    base_sha: Some("deadbeef".to_string()),
                },
            ),
            ev(3, gate(0)),
            ev(4, gate(1)),
            ev(
                5,
                EventKind::MilestoneStarted {
                    milestone_id: "ms-1".to_string(),
                    start_sha: "abc123".to_string(),
                },
            ),
            ev(
                6,
                EventKind::FeatureStarted {
                    feature_id: "f-1-1".to_string(),
                },
            ),
            ev(7, spawn("r-pass", "f-1-1", "sonnet")),
            ev(
                8,
                completed("r-pass", RunResult::Pass, "did the alpha thing"),
            ),
            ev(
                9,
                EventKind::FeatureCompleted {
                    feature_id: "f-1-1".to_string(),
                    commits: vec!["deadbeef".to_string()],
                },
            ),
            // The pool's second candidate stream: spawned, never completed.
            ev(10, spawn("r-cand", "f-1-1", "gpt-5")),
            ev(
                11,
                EventKind::DivergenceNoted {
                    unit: "f-1-1".to_string(),
                    candidates: candidates(),
                    diverged: true,
                },
            ),
            ev(
                12,
                EventKind::MilestoneBlocked {
                    milestone_id: "ms-1".to_string(),
                    reason: "divergence on f-1-1: candidates disagree".to_string(),
                },
            ),
            ev(
                13,
                EventKind::MilestoneUnblocked {
                    milestone_id: "ms-1".to_string(),
                    reason: "kept candidate 0".to_string(),
                    validator_guidance: None,
                },
            ),
            ev(
                14,
                EventKind::DivergenceResolved {
                    unit: "f-1-1".to_string(),
                    selected: Some(0),
                    reason: "kept candidate 0".to_string(),
                    decided_by: "operator".to_string(),
                },
            ),
            ev(
                15,
                EventKind::GrantRequested {
                    milestone_id: "ms-1".to_string(),
                    kind: GrantKind::Command,
                    command: "cargo test".to_string(),
                },
            ),
            ev(
                16,
                EventKind::GrantApproved {
                    kind: GrantKind::Command,
                    command: "cargo test".to_string(),
                },
            ),
            ev(
                17,
                EventKind::UserMessage {
                    text: "ship it as-is".to_string(),
                    interrupt: false,
                },
            ),
            ev(
                18,
                EventKind::FeatureStarted {
                    feature_id: "f-1-2".to_string(),
                },
            ),
            ev(19, spawn("r-fail", "f-1-2", "sonnet")),
            ev(
                20,
                completed("r-fail", RunResult::Fail, "could not do the beta thing"),
            ),
            ev(
                21,
                EventKind::FeatureFailed {
                    feature_id: "f-1-2".to_string(),
                    reason: "gave up".to_string(),
                    commits: Vec::new(),
                },
            ),
            // Engine-owned workspace-gate lift: NOT a human judgement.
            ev(
                22,
                EventKind::MilestoneBlocked {
                    milestone_id: "ms-1".to_string(),
                    reason: "workspace gate: bootstrap failed".to_string(),
                },
            ),
            ev(
                23,
                EventKind::MilestoneUnblocked {
                    milestone_id: "ms-1".to_string(),
                    reason: crate::workspace_gate::GATE_LIFT_REASON.to_string(),
                    validator_guidance: None,
                },
            ),
            // Never decided: a pending grant.
            ev(
                24,
                EventKind::GrantRequested {
                    milestone_id: "ms-1".to_string(),
                    kind: GrantKind::Egress,
                    command: "example.com:443".to_string(),
                },
            ),
            // The agreement record (diverged: false), never resolved.
            ev(
                25,
                EventKind::DivergenceNoted {
                    unit: "f-1-1".to_string(),
                    candidates: candidates(),
                    diverged: false,
                },
            ),
            ev(
                26,
                EventKind::MilestoneCompleted {
                    milestone_id: "ms-1".to_string(),
                    tag: None,
                },
            ),
            ev(27, EventKind::MissionCompleted {}),
        ]
    }

    fn export(dir: &std::path::Path, events: &[Event]) -> Vec<CorpusRecord> {
        export_corpus(dir, MISSION, events).unwrap()
    }

    #[test]
    fn corpus_export_emits_traces_divergences_and_escalations_with_provenance() {
        let tmp = tempfile::TempDir::new().unwrap();
        let events = fixture_events();
        let records = export(tmp.path(), &events);

        // 1 trace + 2 divergences + 4 escalations, grouped by source.
        assert_eq!(records.len(), 7, "record count: {records:?}");

        // -- worker-trace: the pair fields plus the provenance tags.
        let CorpusRecord::WorkerTrace(trace) = &records[0] else {
            panic!("records[0] must be the worker trace: {records:?}")
        };
        assert_eq!(trace.run_id, "r-pass");
        assert_eq!(trace.mission_id, MISSION);
        assert_eq!(trace.feature_id, "f-1-1");
        assert_eq!(trace.model, "sonnet");
        assert!(trace.instruction.contains("spec for alpha"));
        assert!(trace.response.contains("did the alpha thing"));
        // Backend DERIVED exactly as the replay derives it (default config →
        // claude); the gate chain is the mission's ladder, pinned by seq.
        assert_eq!(trace.backend.as_deref(), Some("claude"));
        let ladder: Vec<(u64, GateSurface, GateVerdict)> = trace
            .gate_chain
            .iter()
            .map(|r| (r.seq, r.surface, r.verdict))
            .collect();
        assert_eq!(
            ladder,
            vec![
                (3, GateSurface::Approval, GateVerdict::Pass),
                (4, GateSurface::Approval, GateVerdict::Pass),
            ]
        );

        // -- divergences: both candidates AND the resolution; the re-noted
        // agreement record stays unresolved (no inherited judgement).
        let CorpusRecord::Divergence(resolved) = &records[1] else {
            panic!("records[1] must be the resolved divergence: {records:?}")
        };
        assert_eq!(resolved.unit, "f-1-1");
        assert_eq!(resolved.noted_seq, 11);
        assert!(resolved.diverged);
        let candidate_refs: Vec<(&str, &str, &str)> = resolved
            .candidates
            .iter()
            .map(|c| (c.run_id.as_str(), c.branch.as_str(), c.backend.as_str()))
            .collect();
        assert_eq!(
            candidate_refs,
            vec![
                ("r-pass", "kranz/pool/m-1/f-1-1-c0", "claude"),
                ("r-cand", "kranz/pool/m-1/f-1-1-c1", "codex"),
            ]
        );
        let resolution = resolved.resolution.as_ref().expect("resolved at 14");
        assert_eq!(resolution.selected, Some(0));
        assert_eq!(resolution.reason, "kept candidate 0");
        assert_eq!(resolution.decided_by, "operator");
        assert_eq!(resolution.resolved_seq, 14);

        let CorpusRecord::Divergence(pending) = &records[2] else {
            panic!("records[2] must be the pending divergence: {records:?}")
        };
        assert_eq!(pending.noted_seq, 25);
        assert!(!pending.diverged, "the agreement record exports verbatim");
        assert_eq!(pending.resolution, None);

        // -- escalations, merged into ask-seq order: block(12), grant(15),
        // steer(17), pending grant(24). The engine-lifted block (22) is gone.
        let escalations: Vec<&EscalationRecord> = records[3..]
            .iter()
            .map(|r| match r {
                CorpusRecord::Escalation(e) => e,
                other => panic!("expected escalation, got {other:?}"),
            })
            .collect();
        let lane: Vec<(u64, EscalationKind, &str, &str)> = escalations
            .iter()
            .map(|e| (e.ask_seq, e.kind, e.ask.as_str(), e.decision.as_str()))
            .collect();
        assert_eq!(
            lane,
            vec![
                (
                    12,
                    EscalationKind::Block,
                    "divergence on f-1-1: candidates disagree",
                    "unblocked: kept candidate 0"
                ),
                (15, EscalationKind::Grant, "command: cargo test", "approved"),
                (17, EscalationKind::Steer, "ship it as-is", "steered"),
                (
                    24,
                    EscalationKind::Grant,
                    "egress: example.com:443",
                    "pending"
                ),
            ]
        );
        assert_eq!(escalations[0].latency_ms, Some(1000));
        assert_eq!(escalations[0].decision_seq, Some(13));
        assert_eq!(escalations[0].milestone_id.as_deref(), Some("ms-1"));
        assert_eq!(escalations[1].latency_ms, Some(1000));
        assert_eq!(escalations[1].decision_seq, Some(16));
        assert_eq!(escalations[2].milestone_id, None);
        assert_eq!(escalations[2].decision_seq, Some(17));
        assert_eq!(escalations[3].latency_ms, None);
        assert_eq!(escalations[3].decision_seq, None);

        // The wire shape: tagged lines, camelCase keys.
        let jsonl = to_jsonl(&records);
        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 7);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["source"], "worker-trace");
        assert!(first["gateChain"].is_array());
        assert_eq!(first["gateChain"][0]["surface"], "approval");
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["source"], "divergence");
        assert_eq!(second["notedSeq"], 11);
        assert_eq!(second["resolution"]["decidedBy"], "operator");
        let fourth: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
        assert_eq!(fourth["source"], "escalation");
        assert_eq!(fourth["kind"], "block");
        assert_eq!(fourth["askSeq"], 12);
    }

    #[test]
    fn corpus_export_excludes_failed_and_unvalidated_sessions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let events = fixture_events();
        let records = export(tmp.path(), &events);

        let traces: Vec<&WorkerTraceRecord> = records
            .iter()
            .filter_map(|r| match r {
                CorpusRecord::WorkerTrace(t) => Some(t),
                _ => None,
            })
            .collect();
        assert_eq!(
            traces.len(),
            1,
            "only the validation-PASSED run qualifies: {traces:?}"
        );
        assert_eq!(traces[0].run_id, "r-pass");
        // The failed run appears nowhere in the corpus at all; the
        // unvalidated candidate stream appears ONLY as a divergence
        // candidate ref (a comparison anchor), never as a trace.
        let jsonl = to_jsonl(&records);
        assert!(!jsonl.contains("r-fail"), "failed run leaked: {jsonl}");
        assert!(traces.iter().all(|t| t.run_id != "r-cand"));
    }

    #[test]
    fn corpus_export_is_byte_identical_across_regeneration() {
        let tmp = tempfile::TempDir::new().unwrap();
        let events = fixture_events();

        let first = to_jsonl(&export(tmp.path(), &events));
        let second = to_jsonl(&export(tmp.path(), &events));
        assert_eq!(first, second, "same log must yield byte-identical JSONL");
        assert!(!first.is_empty());
        for tag in ["worker-trace", "divergence", "escalation"] {
            assert!(
                first.contains(&format!("\"source\":\"{tag}\"")),
                "missing {tag} records: {first}"
            );
        }
    }

    #[test]
    fn corpus_export_provenance_refs_resolve_via_the_replay() {
        let tmp = tempfile::TempDir::new().unwrap();
        let events = fixture_events();
        let records = export(tmp.path(), &events);
        // An INDEPENDENT replay of the same log: every ref a record carries
        // must join it, by seq, without re-deriving anything.
        let chain = crate::provenance::provenance_chain(tmp.path(), MISSION, &events).unwrap();

        for record in &records {
            match record {
                CorpusRecord::WorkerTrace(trace) => {
                    let session = chain
                        .sessions
                        .iter()
                        .find(|s| s.run_id == trace.run_id)
                        .expect("trace run id must resolve to a replayed session");
                    assert_eq!(session.backend, trace.backend);
                    assert_eq!(session.model, trace.model);
                    for gate_ref in &trace.gate_chain {
                        let gate = chain
                            .gates
                            .iter()
                            .find(|g| g.seq == gate_ref.seq)
                            .expect("gate-chain seq must resolve to a ladder link");
                        assert_eq!(gate.gate, gate_ref.gate);
                        assert_eq!(gate.surface, gate_ref.surface);
                        assert_eq!(gate.verdict, gate_ref.verdict);
                    }
                }
                CorpusRecord::Divergence(divergence) => {
                    assert!(chain.divergences.iter().any(|link| matches!(
                        link,
                        DivergenceLink::Noted { seq, unit, .. }
                            if *seq == divergence.noted_seq && *unit == divergence.unit
                    )));
                    if let Some(resolution) = &divergence.resolution {
                        assert!(chain.divergences.iter().any(|link| matches!(
                            link,
                            DivergenceLink::Resolved { seq, unit, .. }
                                if *seq == resolution.resolved_seq && *unit == divergence.unit
                        )));
                        // The selected index names one of the record's own
                        // candidates — the both-candidates reference is real.
                        if let Some(selected) = resolution.selected {
                            assert!((selected as usize) < divergence.candidates.len());
                        }
                    }
                }
                CorpusRecord::Escalation(escalation) => {
                    if let Some(decision_seq) = escalation.decision_seq {
                        assert!(
                            chain.decisions.iter().any(|d| d.seq == decision_seq),
                            "decision seq {decision_seq} of {escalation:?} must resolve \
                             to a replayed human decision"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn corpus_export_grant_and_steer_rows_match_the_ledger_fold() {
        let tmp = tempfile::TempDir::new().unwrap();
        let events = fixture_events();
        let records = export(tmp.path(), &events);

        // The flight surgeon's own fold over the same log: its grant/steer
        // rows must equal the corpus's (ask, decision, latency) — one rule,
        // no drift. Blocks have no ledger row kind and sit this join out.
        let ledger =
            crate::escalation_metrics::aggregate(&[(MISSION.to_string(), events.clone())], &[])
                .ledger;
        let mut from_ledger: Vec<(String, String, Option<u64>)> = ledger
            .iter()
            .map(|row| (row.ask.clone(), row.decision.clone(), row.latency_ms))
            .collect();
        let mut from_corpus: Vec<(String, String, Option<u64>)> = records
            .iter()
            .filter_map(|r| match r {
                CorpusRecord::Escalation(e)
                    if matches!(e.kind, EscalationKind::Grant | EscalationKind::Steer) =>
                {
                    Some((e.ask.clone(), e.decision.clone(), e.latency_ms))
                }
                _ => None,
            })
            .collect();
        from_ledger.sort();
        from_corpus.sort();
        assert_eq!(from_corpus, from_ledger);
        assert_eq!(from_corpus.len(), 3, "two grants + one steer");
    }
}
