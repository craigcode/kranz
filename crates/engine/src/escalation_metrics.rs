//! Flight-surgeon console (ticket `.kranz/tickets/flight-surgeon-dashboard.md`):
//! escalation health metrics computed per-request from the data kranz already
//! writes — per-mission `events.jsonl` plus the optional `traced-from-mission`
//! ticket frontmatter field. Pure-fold style, mirroring [`crate::outcomes`] /
//! [`crate::contract_health`]: no second persisted source of truth, only
//! functions over event slices and the ticket list.
//!
//! The four metrics:
//!
//! 1. **Autonomy ratio** — closed missions (COMPLETED or FAILED honestly; an
//!    ABANDONED mission was operator-retired, so it is neither and stays out
//!    of the denominator) with zero operator interventions / all closed
//!    missions, split by completion outcome. The intervention set:
//!    - `grant.approved` / `grant.denied` — any grant decision means the run
//!      parked at the consent boundary (a deny-default timeout is still a
//!      park that needed the operator, so denials count too);
//!    - `user.message` at/after `plan.approved` — control-command steers
//!      (pre-approval messages are drafting, not steers; same rule as
//!      [`crate::outcomes`]);
//!    - `plan.revised` / `plan.revision.rejected` — the operator deciding a
//!      plan revision;
//!    - `milestone.unblocked` whose reason is an operator decision — i.e.
//!      every unblock EXCEPT the workspace-gate lift
//!      ([`crate::workspace_gate::GATE_LIFT_REASON`], which is engine-owned).
//! 2. **Rubber-stamp signal** — the park→grant latency distribution (p50/p90,
//!    nearest-rank; and the count under 10s) over decided grant requests.
//! 3. **False greens** — missions that closed COMPLETED with ≥1 defect ticket
//!    tracing back via `traced-from-mission` frontmatter; rate over all
//!    completed missions, split by whether the mission had interventions.
//! 4. **Escalation ledger** — every grant park (paired with its decision and
//!    latency) and every steer, newest first: mission, milestone, what was
//!    asked, what the operator decided.

use crate::events::{Event, EventKind};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Latency under which a grant decision reads as rubber-stamped (the
/// tight-boundary failure made visible): 10 seconds.
const RUBBER_STAMP_THRESHOLD_MS: u64 = 10_000;

/// A defect→mission link from a ticket's `traced-from-mission` frontmatter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TracedDefect {
    /// The defect ticket's slug (file stem under `.kranz/tickets/`).
    pub ticket: String,
    /// The mission the defect was traced back to.
    pub mission_id: String,
}

/// Autonomy ratio over all closed missions, plus the per-outcome split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutonomyMetric {
    pub closed_missions: u64,
    pub zero_intervention_missions: u64,
    /// None when nothing closed (the share is meaningless, not zero).
    pub zero_intervention_share: Option<f64>,
    pub completed: AutonomyOutcomeSplit,
    pub failed: AutonomyOutcomeSplit,
}

/// One outcome arm of the autonomy split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutonomyOutcomeSplit {
    pub missions: u64,
    pub zero_intervention: u64,
    /// None when the arm has no missions.
    pub zero_intervention_share: Option<f64>,
}

/// Park→grant latency distribution — the rubber-stamp signal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RubberStamp {
    /// Grant requests with a matching decision (the percentile population).
    pub decided_grants: u64,
    /// Nearest-rank percentiles in ms; None when nothing was decided.
    pub p50_ms: Option<u64>,
    pub p90_ms: Option<u64>,
    /// Decisions under [`RUBBER_STAMP_THRESHOLD_MS`] — the wall of
    /// sub-ten-second approvals, counted.
    pub under_ten_seconds: u64,
}

/// One intervention arm of the false-green split.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FalseGreenSplit {
    pub completed_missions: u64,
    pub false_greens: u64,
    /// None when the arm has no completed missions.
    pub rate: Option<f64>,
}

/// Missions that closed green and later produced a traced defect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FalseGreens {
    pub completed_missions: u64,
    pub false_greens: u64,
    /// None when nothing completed.
    pub false_green_rate: Option<f64>,
    /// The autonomy-quality test: does an intervention-free completion
    /// produce fewer defects than an operator-steered one?
    pub with_interventions: FalseGreenSplit,
    pub zero_intervention: FalseGreenSplit,
    /// Every defect→mission link that joined (auditable, never inferred —
    /// only frontmatter-traced links appear).
    pub traced_defects: Vec<TracedDefect>,
}

/// Ledger row kind: a grant park (paired with its decision) or a steer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LedgerKind {
    Grant,
    Steer,
}

impl LedgerKind {
    /// The wire/serde form (`grant`/`steer`) for text surfaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Grant => "grant",
            Self::Steer => "steer",
        }
    }
}

/// One escalation-ledger row: what was asked, what was decided, how long it
/// took. Tabular across REST/CLI/dashboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerRow {
    pub ts: DateTime<Utc>,
    pub mission_id: String,
    pub kind: LedgerKind,
    /// The milestone a grant parked for; None on steers.
    pub milestone_id: Option<String>,
    /// Grant: `<kind>: <command>`; steer: the operator's message text.
    pub ask: String,
    /// Grant: `approved` / `denied: <reason>` / `pending`; steer: `steered`.
    pub decision: String,
    /// Park→decision latency; None while pending (and on steers).
    pub latency_ms: Option<u64>,
}

/// The full flight-surgeon aggregate over one host's missions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationMetrics {
    pub autonomy: AutonomyMetric,
    pub rubber_stamp: RubberStamp,
    pub false_greens: FalseGreens,
    pub ledger: Vec<LedgerRow>,
}

/// How a mission closed, for the autonomy denominator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalOutcome {
    Completed,
    Failed,
    /// Operator-retired (`kranz abandon`): terminal but neither a completion
    /// nor an honest failure, so it never enters the autonomy ratio.
    Abandoned,
}

/// One mission's fold: terminal outcome, intervention count, grant-decision
/// latencies, and its ledger rows.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionEscalation {
    terminal: Option<TerminalOutcome>,
    interventions: u64,
    latencies_ms: Vec<u64>,
    ledger: Vec<LedgerRow>,
}

/// Kebab-case wire name of a [`crate::types::GrantKind`] (mirrors its serde
/// rename) for the ledger's `ask` text. `pub(crate)` so the provenance
/// replay's grant-decision summaries spell the kind identically.
pub(crate) fn grant_kind_str(kind: &crate::types::GrantKind) -> &'static str {
    match kind {
        crate::types::GrantKind::Command => "command",
        crate::types::GrantKind::TouchPath => "touch-path",
        crate::types::GrantKind::WorkerDeny => "worker-deny",
        crate::types::GrantKind::Egress => "egress",
    }
}

/// True for an engine-owned workspace-gate lift. Typed context is authoritative;
/// only legacy events without context use the historical exact reason. `pub(crate)` so the provenance
/// replay excludes the same lift from its human-decision chain — one
/// classification rule, no drift between the two folds.
pub(crate) fn is_engine_lift(reason: &str, context: Option<&crate::types::BlockContext>) -> bool {
    match context {
        Some(context) => context.is_workspace_gate(),
        None => reason == crate::workspace_gate::GATE_LIFT_REASON,
    }
}

/// Pair each `grant.requested` (in seq order) with the `grant.approved` /
/// `grant.denied` that answered it, returning `(request_index,
/// Option<decision_index>)` pairs indexing `mission_events`: the earliest
/// LATER decision for the same command, falling back to the next unconsumed
/// decision in seq order (partial/hand-edited logs), each decision consumed
/// at most once. `None` means the park was never answered (pending).
///
/// Extracted for the training-corpus export ([`crate::corpus_export`]) — the
/// THIRD user of this rule (outcomes.rs carries the first, the ledger fold
/// below the second) — so the ledger and the corpus pair identical logs
/// identically and can never drift. `mission_events` must be in ascending
/// `seq` order or "earliest later" is meaningless.
pub(crate) fn pair_grant_decisions(mission_events: &[&Event]) -> Vec<(usize, Option<usize>)> {
    let mut used_decisions = vec![false; mission_events.len()];
    let mut pairs = Vec::new();
    for (req_idx, req) in mission_events.iter().enumerate() {
        let EventKind::GrantRequested { command, .. } = &req.kind else {
            continue;
        };

        let mut matched: Option<usize> = None;
        for (i, cand) in mission_events.iter().enumerate() {
            if i <= req_idx || used_decisions[i] {
                continue;
            }
            let cand_command = match &cand.kind {
                EventKind::GrantApproved { command, .. }
                | EventKind::GrantDenied { command, .. } => command,
                _ => continue,
            };
            if cand_command == command {
                matched = Some(i);
                break;
            }
        }
        if matched.is_none() {
            // Fallback for partial/hand-edited logs (mirrors outcomes.rs):
            // take the next unconsumed decision in seq order even when it
            // answers a different command.
            for (i, cand) in mission_events.iter().enumerate() {
                if i <= req_idx || used_decisions[i] {
                    continue;
                }
                if matches!(
                    cand.kind,
                    EventKind::GrantApproved { .. } | EventKind::GrantDenied { .. }
                ) {
                    matched = Some(i);
                    break;
                }
            }
        }
        if let Some(i) = matched {
            used_decisions[i] = true;
        }
        pairs.push((req_idx, matched));
    }
    pairs
}

/// Fold one mission's escalation data from its event slice. `events` may
/// contain events for other missions too (they are filtered out) but must be
/// in ascending `seq` order for the "earliest later" grant matching to be
/// correct. Same discipline as [`crate::outcomes::mission_outcomes`].
pub fn mission_escalation(mission_id: &str, events: &[Event]) -> MissionEscalation {
    let mission_events: Vec<&Event> = events
        .iter()
        .filter(|e| e.mission_id == mission_id)
        .collect();

    let terminal = mission_events.iter().find_map(|e| match &e.kind {
        EventKind::MissionCompleted {} => Some(TerminalOutcome::Completed),
        EventKind::MissionFailed { .. } => Some(TerminalOutcome::Failed),
        EventKind::MissionAbandoned { .. } => Some(TerminalOutcome::Abandoned),
        _ => None,
    });

    let plan_approved_seq = mission_events
        .iter()
        .find(|e| matches!(e.kind, EventKind::PlanApproved { .. }))
        .map(|e| e.seq);

    let mut interventions: u64 = 0;
    let mut ledger = Vec::new();

    // Steers and decision interventions (the autonomy numerator set).
    for e in &mission_events {
        match &e.kind {
            EventKind::UserMessage { text, .. } => {
                // Classify by SEQUENCE, not wall clock (5th-pass review):
                // the event log's seq is the order of truth — timestamps
                // can tie or move backward across a clock step, and either
                // would silently misclassify a steer as drafting.
                if let Some(approved_seq) = plan_approved_seq {
                    if e.seq >= approved_seq {
                        interventions += 1;
                        ledger.push(LedgerRow {
                            ts: e.ts,
                            mission_id: mission_id.to_string(),
                            kind: LedgerKind::Steer,
                            milestone_id: None,
                            ask: text.clone(),
                            decision: "steered".to_string(),
                            latency_ms: None,
                        });
                    }
                }
            }
            EventKind::GrantApproved { .. }
            | EventKind::GrantDenied { .. }
            | EventKind::PlanRevised { .. }
            | EventKind::PlanRevisionRejected { .. } => {
                interventions += 1;
            }
            EventKind::MilestoneUnblocked {
                reason,
                block_context,
                ..
            } if !is_engine_lift(reason, block_context.as_ref()) => {
                interventions += 1;
            }
            _ => {}
        }
    }

    // Grant parks: pair each request with its decision via the shared rule
    // ([`pair_grant_decisions`]) so this fold, outcomes.rs, and the
    // training-corpus export pair identical logs identically.
    let mut latencies_ms = Vec::new();
    for (req_idx, matched) in pair_grant_decisions(&mission_events) {
        let req = mission_events[req_idx];
        let EventKind::GrantRequested {
            milestone_id,
            kind,
            command,
        } = &req.kind
        else {
            unreachable!("pair_grant_decisions only returns grant.requested indices")
        };

        let (decision, latency_ms) = match matched {
            Some(i) => {
                let decided = mission_events[i];
                let latency = (decided.ts - req.ts).num_milliseconds();
                let latency_ms = if latency >= 0 {
                    Some(latency as u64)
                } else {
                    None
                };
                if let Some(l) = latency_ms {
                    latencies_ms.push(l);
                }
                let decision = match &decided.kind {
                    EventKind::GrantApproved { .. } => "approved".to_string(),
                    EventKind::GrantDenied { reason, .. } => format!("denied: {reason}"),
                    _ => unreachable!(),
                };
                (decision, latency_ms)
            }
            None => ("pending".to_string(), None),
        };

        ledger.push(LedgerRow {
            ts: req.ts,
            mission_id: mission_id.to_string(),
            kind: LedgerKind::Grant,
            milestone_id: Some(milestone_id.clone()),
            ask: format!("{}: {command}", grant_kind_str(kind)),
            decision,
            latency_ms,
        });
    }

    MissionEscalation {
        terminal,
        interventions,
        latencies_ms,
        ledger,
    }
}

/// Nearest-rank percentile over a sorted ascending slice: the value at rank
/// `ceil(p/100 * n)` (1-indexed, clamped to `n`). None for an empty
/// population. Deterministic and exact for the small n these folds see.
fn percentile_nearest_rank(sorted: &[u64], p: u64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len() as u64;
    let rank = p.saturating_mul(n).saturating_add(99) / 100;
    let rank = rank.clamp(1, n);
    Some(sorted[(rank - 1) as usize])
}

/// Aggregate per-mission folds (plus the traced-defect links) into the four
/// flight-surgeon metrics. `missions` is `(mission_id, events)` pairs in any
/// order; the ledger comes out newest-first. Pure: no I/O, no clock.
pub fn aggregate(
    missions: &[(String, Vec<Event>)],
    traced_defects: &[TracedDefect],
) -> EscalationMetrics {
    let mut closed_missions: u64 = 0;
    let mut zero_intervention_missions: u64 = 0;
    let mut completed_missions: u64 = 0;
    let mut completed_zero_intervention: u64 = 0;
    let mut failed_missions: u64 = 0;
    let mut failed_zero_intervention: u64 = 0;
    let mut all_latencies_ms = Vec::new();
    let mut ledger = Vec::new();
    // Completed missions by intervention arm, for the false-green join.
    let mut completed_with_interventions: Vec<&str> = Vec::new();
    let mut completed_clean: Vec<&str> = Vec::new();

    for (mission_id, events) in missions {
        let fold = mission_escalation(mission_id, events);
        match fold.terminal {
            Some(TerminalOutcome::Completed) => {
                closed_missions += 1;
                completed_missions += 1;
                if fold.interventions == 0 {
                    zero_intervention_missions += 1;
                    completed_zero_intervention += 1;
                    completed_clean.push(mission_id);
                } else {
                    completed_with_interventions.push(mission_id);
                }
            }
            Some(TerminalOutcome::Failed) => {
                closed_missions += 1;
                failed_missions += 1;
                if fold.interventions == 0 {
                    zero_intervention_missions += 1;
                    failed_zero_intervention += 1;
                }
            }
            Some(TerminalOutcome::Abandoned) | None => {}
        }
        all_latencies_ms.extend(fold.latencies_ms);
        ledger.extend(fold.ledger);
    }

    // False greens: a traced defect joins only against a mission that closed
    // COMPLETED (a defect on a failed mission is not a false green; a trace
    // to an unknown mission joins nothing). Each completed mission with ≥1
    // traced defect counts once, however many defects trace to it.
    let mut joined: Vec<TracedDefect> = Vec::new();
    let mut false_green_with_interventions: u64 = 0;
    let mut false_green_clean: u64 = 0;
    for defect in traced_defects {
        let id = defect.mission_id.as_str();
        let is_completed =
            completed_with_interventions.contains(&id) || completed_clean.contains(&id);
        if is_completed {
            joined.push(defect.clone());
        }
    }
    let counted: std::collections::HashSet<&str> =
        joined.iter().map(|d| d.mission_id.as_str()).collect();
    for id in &completed_with_interventions {
        if counted.contains(id) {
            false_green_with_interventions += 1;
        }
    }
    for id in &completed_clean {
        if counted.contains(id) {
            false_green_clean += 1;
        }
    }
    joined.sort_by(|a, b| {
        a.mission_id
            .cmp(&b.mission_id)
            .then_with(|| a.ticket.cmp(&b.ticket))
    });
    let false_greens_total = false_green_with_interventions + false_green_clean;

    all_latencies_ms.sort_unstable();
    let decided = all_latencies_ms.len() as u64;
    let under_ten_seconds = all_latencies_ms
        .iter()
        .filter(|&&l| l < RUBBER_STAMP_THRESHOLD_MS)
        .count() as u64;

    ledger.sort_by_key(|row| std::cmp::Reverse(row.ts));

    EscalationMetrics {
        autonomy: AutonomyMetric {
            closed_missions,
            zero_intervention_missions,
            zero_intervention_share: (closed_missions > 0)
                .then(|| zero_intervention_missions as f64 / closed_missions as f64),
            completed: AutonomyOutcomeSplit {
                missions: completed_missions,
                zero_intervention: completed_zero_intervention,
                zero_intervention_share: (completed_missions > 0)
                    .then(|| completed_zero_intervention as f64 / completed_missions as f64),
            },
            failed: AutonomyOutcomeSplit {
                missions: failed_missions,
                zero_intervention: failed_zero_intervention,
                zero_intervention_share: (failed_missions > 0)
                    .then(|| failed_zero_intervention as f64 / failed_missions as f64),
            },
        },
        rubber_stamp: RubberStamp {
            decided_grants: decided,
            p50_ms: percentile_nearest_rank(&all_latencies_ms, 50),
            p90_ms: percentile_nearest_rank(&all_latencies_ms, 90),
            under_ten_seconds,
        },
        false_greens: FalseGreens {
            completed_missions,
            false_greens: false_greens_total,
            false_green_rate: (completed_missions > 0)
                .then(|| false_greens_total as f64 / completed_missions as f64),
            with_interventions: FalseGreenSplit {
                completed_missions: completed_with_interventions.len() as u64,
                false_greens: false_green_with_interventions,
                rate: (!completed_with_interventions.is_empty()).then(|| {
                    false_green_with_interventions as f64
                        / completed_with_interventions.len() as f64
                }),
            },
            zero_intervention: FalseGreenSplit {
                completed_missions: completed_clean.len() as u64,
                false_greens: false_green_clean,
                rate: (!completed_clean.is_empty())
                    .then(|| false_green_clean as f64 / completed_clean.len() as f64),
            },
            traced_defects: joined,
        },
        ledger,
    }
}

/// Read every ticket's `traced-from-mission` frontmatter into defect→mission
/// links. Absent field = not a traced defect (no false positives); tickets
/// that fail to parse are already skipped by [`crate::ticket::Ticket::list`].
/// `pub(crate)` so the industry-comparison fold
/// ([`crate::comparison_metrics`]) joins the SAME recorded links for its
/// defect density — one linkage source, no drift between the two folds.
pub(crate) fn traced_defects_from_tickets(repo_root: &std::path::Path) -> Vec<TracedDefect> {
    let mut out: Vec<TracedDefect> = crate::ticket::Ticket::list(repo_root)
        .into_iter()
        .filter_map(|ticket| {
            ticket.traced_from_mission.map(|mission_id| TracedDefect {
                ticket: ticket.slug,
                mission_id,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.mission_id
            .cmp(&b.mission_id)
            .then_with(|| a.ticket.cmp(&b.ticket))
    });
    out
}

/// Enumerate every mission under `repo_root` exactly as
/// [`crate::outcomes::compute_outcomes`] does (union of
/// [`crate::paths::MissionPaths::list_missions`] and the ids in
/// `.kranz/missions/index.md`), read each log, join the tickets'
/// `traced-from-mission` links, and aggregate. A mission with no
/// `events.jsonl` or an unreadable/corrupt log is skipped (degrade per-row);
/// this never panics or fails the whole aggregate.
pub fn compute_escalation_metrics(
    repo_root: &std::path::Path,
) -> anyhow::Result<EscalationMetrics> {
    let index_contents = std::fs::read_to_string(
        crate::paths::MissionPaths::new(repo_root, "_")
            .missions_dir()
            .join("index.md"),
    )
    .unwrap_or_default();

    let mut ids = crate::paths::MissionPaths::list_missions(repo_root);
    for id in crate::mission_catalog::mission_index_ids(&index_contents) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.sort();

    let mut missions = Vec::new();
    for id in ids {
        let paths = crate::paths::MissionPaths::new(repo_root, &id);
        let events_path = paths.events_file();
        if !events_path.is_file() {
            continue;
        }
        // Never fold a mission reached through a symlinked path component
        // (P1 mission-path-no-follow).
        if paths.require_no_follow().is_err() {
            continue;
        }
        let events = match crate::event_log::EventLog::read_events(&events_path) {
            Ok(events) => events,
            Err(_) => continue, // corrupt log degrades per-mission, never fails
        };
        missions.push((id, events));
    }

    Ok(aggregate(
        &missions,
        &traced_defects_from_tickets(repo_root),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{EventLog, LockForce};
    use crate::paths::MissionPaths;
    use crate::types::{GrantKind, MissionConfig, Plan};
    use std::time::Duration;
    use tempfile::TempDir;

    fn ev(seq: u64, mission_id: &str, ts_ms: i64, kind: EventKind) -> Event {
        Event {
            seq,
            ts: DateTime::from_timestamp_millis(ts_ms).unwrap(),
            mission_id: mission_id.to_string(),
            kind,
        }
    }

    fn sample_plan() -> Plan {
        Plan {
            goal: "g".into(),
            validation_contract: vec![],
            milestones: vec![],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
            standards_manifest: None,
            reviewer_independence: None,
        }
    }

    fn created() -> EventKind {
        EventKind::MissionCreated {
            goal: "g".into(),
            base_branch: "main".into(),
            mission_branch: "kranz/mission-x".into(),
            config: MissionConfig::default(),
        }
    }

    /// The anti-vacuity fixture (ticket: flight-surgeon-dashboard): three
    /// closed missions —
    /// - m-1: COMPLETED, zero interventions;
    /// - m-2: COMPLETED, with grant decisions (parks at 5s and 900s);
    /// - m-3: FAILED honestly, with a control steer.
    ///
    /// Every metric must land exactly: ratio 1/3, completed split 1/2,
    /// failed split 0/1, p50 5s, p90 900s, under-10s 1.
    fn anti_vacuity_missions() -> Vec<(String, Vec<Event>)> {
        let m1 = vec![
            ev(1, "m-1", 0, created()),
            ev(2, "m-1", 1_000, EventKind::MissionCompleted {}),
        ];
        let m2 = vec![
            ev(1, "m-2", 0, created()),
            ev(
                2,
                "m-2",
                1_000,
                EventKind::PlanApproved {
                    plan: sample_plan(),
                    base_sha: None,
                },
            ),
            ev(
                3,
                "m-2",
                10_000,
                EventKind::GrantRequested {
                    milestone_id: "ms-1".into(),
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
            ),
            ev(
                4,
                "m-2",
                15_000,
                EventKind::GrantApproved {
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
            ),
            ev(
                5,
                "m-2",
                20_000,
                EventKind::GrantRequested {
                    milestone_id: "ms-1".into(),
                    kind: GrantKind::Egress,
                    command: "registry.npmjs.org:443".into(),
                },
            ),
            ev(
                6,
                "m-2",
                920_000,
                EventKind::GrantDenied {
                    kind: GrantKind::Egress,
                    command: "registry.npmjs.org:443".into(),
                    reason: "not needed".into(),
                },
            ),
            ev(7, "m-2", 921_000, EventKind::MissionCompleted {}),
        ];
        let m3 = vec![
            ev(1, "m-3", 0, created()),
            ev(
                2,
                "m-3",
                1_000,
                EventKind::PlanApproved {
                    plan: sample_plan(),
                    base_sha: None,
                },
            ),
            ev(
                3,
                "m-3",
                2_000,
                EventKind::UserMessage {
                    text: "skip the flaky test".into(),
                    interrupt: false,
                },
            ),
            ev(
                4,
                "m-3",
                3_000,
                EventKind::MissionFailed {
                    reason: "honest failure".into(),
                },
            ),
        ];
        vec![
            ("m-1".to_string(), m1),
            ("m-2".to_string(), m2),
            ("m-3".to_string(), m3),
        ]
    }

    #[test]
    fn escalation_metrics_autonomy_ratio_split_by_outcome() {
        let metrics = aggregate(&anti_vacuity_missions(), &[]);
        let autonomy = &metrics.autonomy;
        assert_eq!(autonomy.closed_missions, 3);
        assert_eq!(autonomy.zero_intervention_missions, 1);
        assert_eq!(autonomy.zero_intervention_share, Some(1.0 / 3.0));
        assert_eq!(autonomy.completed.missions, 2);
        assert_eq!(autonomy.completed.zero_intervention, 1);
        assert_eq!(autonomy.completed.zero_intervention_share, Some(0.5));
        assert_eq!(autonomy.failed.missions, 1);
        assert_eq!(autonomy.failed.zero_intervention, 0);
        assert_eq!(autonomy.failed.zero_intervention_share, Some(0.0));
    }

    #[test]
    fn escalation_metrics_rubber_stamp_percentiles_and_under_10s() {
        let metrics = aggregate(&anti_vacuity_missions(), &[]);
        let stamp = &metrics.rubber_stamp;
        assert_eq!(stamp.decided_grants, 2);
        // [5_000, 900_000] nearest-rank: p50 → rank 1 → 5s; p90 → rank 2 → 900s.
        assert_eq!(stamp.p50_ms, Some(5_000));
        assert_eq!(stamp.p90_ms, Some(900_000));
        assert_eq!(stamp.under_ten_seconds, 1);
    }

    #[test]
    fn escalation_metrics_percentiles_empty_population_is_none() {
        let metrics = aggregate(&[], &[]);
        assert_eq!(metrics.rubber_stamp.decided_grants, 0);
        assert_eq!(metrics.rubber_stamp.p50_ms, None);
        assert_eq!(metrics.rubber_stamp.p90_ms, None);
        assert_eq!(metrics.autonomy.zero_intervention_share, None);
        assert_eq!(metrics.false_greens.false_green_rate, None);
    }

    #[test]
    fn escalation_metrics_false_greens_join_and_split() {
        let traced = vec![
            // Joins: m-1 closed COMPLETED with zero interventions.
            TracedDefect {
                ticket: "defect-login-regression".into(),
                mission_id: "m-1".into(),
            },
            // Does NOT join: m-3 closed FAILED (not a green).
            TracedDefect {
                ticket: "defect-on-a-failure".into(),
                mission_id: "m-3".into(),
            },
            // Does NOT join: no such mission.
            TracedDefect {
                ticket: "defect-unknown-mission".into(),
                mission_id: "m-999".into(),
            },
        ];
        let metrics = aggregate(&anti_vacuity_missions(), &traced);
        let fg = &metrics.false_greens;
        assert_eq!(fg.completed_missions, 2);
        assert_eq!(fg.false_greens, 1);
        assert_eq!(fg.false_green_rate, Some(0.5));
        assert_eq!(fg.with_interventions.completed_missions, 1);
        assert_eq!(fg.with_interventions.false_greens, 0);
        assert_eq!(fg.with_interventions.rate, Some(0.0));
        assert_eq!(fg.zero_intervention.completed_missions, 1);
        assert_eq!(fg.zero_intervention.false_greens, 1);
        assert_eq!(fg.zero_intervention.rate, Some(1.0));
        assert_eq!(
            fg.traced_defects,
            vec![TracedDefect {
                ticket: "defect-login-regression".into(),
                mission_id: "m-1".into(),
            }]
        );
    }

    #[test]
    fn escalation_metrics_two_defects_one_mission_count_once() {
        let traced = vec![
            TracedDefect {
                ticket: "defect-a".into(),
                mission_id: "m-1".into(),
            },
            TracedDefect {
                ticket: "defect-b".into(),
                mission_id: "m-1".into(),
            },
        ];
        let metrics = aggregate(&anti_vacuity_missions(), &traced);
        assert_eq!(metrics.false_greens.false_greens, 1);
        assert_eq!(metrics.false_greens.traced_defects.len(), 2);
    }

    #[test]
    fn escalation_metrics_ledger_rows_grants_and_steers() {
        let metrics = aggregate(&anti_vacuity_missions(), &[]);
        // Newest first: m-2's denied egress park (20s) leads, then the
        // approved command park (10s), then m-3's steer (2s).
        assert_eq!(metrics.ledger.len(), 3);
        let egress = &metrics.ledger[0];
        assert_eq!(egress.kind, LedgerKind::Grant);
        assert_eq!(egress.mission_id, "m-2");
        assert_eq!(egress.milestone_id.as_deref(), Some("ms-1"));
        assert_eq!(egress.ask, "egress: registry.npmjs.org:443");
        assert_eq!(egress.decision, "denied: not needed");
        assert_eq!(egress.latency_ms, Some(900_000));
        let command = &metrics.ledger[1];
        assert_eq!(command.kind, LedgerKind::Grant);
        assert_eq!(command.ask, "command: cargo test");
        assert_eq!(command.decision, "approved");
        assert_eq!(command.latency_ms, Some(5_000));
        let steer = &metrics.ledger[2];
        assert_eq!(steer.kind, LedgerKind::Steer);
        assert_eq!(steer.mission_id, "m-3");
        assert_eq!(steer.milestone_id, None);
        assert_eq!(steer.ask, "skip the flaky test");
        assert_eq!(steer.decision, "steered");
        assert_eq!(steer.latency_ms, None);
    }

    #[test]
    fn escalation_metrics_pending_grant_rows_and_pre_approval_messages() {
        // A pre-approval message is drafting, not a steer; an unanswered
        // park is a pending ledger row with no latency and no intervention.
        let events = vec![
            ev(
                1,
                "m-1",
                0,
                EventKind::UserMessage {
                    text: "draft note".into(),
                    interrupt: false,
                },
            ),
            ev(
                2,
                "m-1",
                1_000,
                EventKind::PlanApproved {
                    plan: sample_plan(),
                    base_sha: None,
                },
            ),
            ev(
                3,
                "m-1",
                2_000,
                EventKind::GrantRequested {
                    milestone_id: "ms-1".into(),
                    kind: GrantKind::TouchPath,
                    command: "docs/**".into(),
                },
            ),
        ];
        let fold = mission_escalation("m-1", &events);
        assert_eq!(fold.interventions, 0);
        assert!(fold.latencies_ms.is_empty());
        assert_eq!(fold.ledger.len(), 1);
        assert_eq!(fold.ledger[0].decision, "pending");
        assert_eq!(fold.ledger[0].latency_ms, None);
        assert_eq!(fold.ledger[0].ask, "touch-path: docs/**");
    }

    #[test]
    fn escalation_metrics_operator_unblocks_count_engine_lifts_do_not() {
        let events = vec![
            ev(
                1,
                "m-1",
                0,
                EventKind::MilestoneBlocked {
                    block_context: None,
                    milestone_id: "ms-1".into(),
                    reason: "workspace gate: bootstrap failed".into(),
                },
            ),
            // Engine-owned lift (workspace gate passing) — NOT an operator
            // decision, so it must not break the mission's clean record.
            ev(
                2,
                "m-1",
                1_000,
                EventKind::MilestoneUnblocked {
                    block_context: None,
                    milestone_id: "ms-1".into(),
                    reason: crate::workspace_gate::GATE_LIFT_REASON.to_string(),
                    validator_guidance: None,
                },
            ),
            ev(
                3,
                "m-1",
                2_000,
                EventKind::MilestoneBlocked {
                    block_context: None,
                    milestone_id: "ms-1".into(),
                    reason: "fix-cycle cap".into(),
                },
            ),
            // Operator decision — counts.
            ev(
                4,
                "m-1",
                3_000,
                EventKind::MilestoneUnblocked {
                    block_context: None,
                    milestone_id: "ms-1".into(),
                    reason: "user skipped findings".into(),
                    validator_guidance: None,
                },
            ),
        ];
        let fold = mission_escalation("m-1", &events);
        assert_eq!(fold.interventions, 1);
    }

    #[test]
    fn escalation_metrics_revision_decisions_count_as_interventions() {
        let events = vec![
            ev(
                1,
                "m-1",
                0,
                EventKind::PlanRevised {
                    revision: 1,
                    plan: sample_plan(),
                },
            ),
            ev(
                2,
                "m-1",
                1_000,
                EventKind::PlanRevisionRejected {
                    revision: 2,
                    reason: "too risky".into(),
                },
            ),
            ev(3, "m-1", 2_000, EventKind::MissionCompleted {}),
        ];
        let fold = mission_escalation("m-1", &events);
        assert_eq!(fold.interventions, 2);
        let metrics = aggregate(&[("m-1".to_string(), events)], &[]);
        assert_eq!(metrics.autonomy.zero_intervention_missions, 0);
        assert_eq!(metrics.autonomy.completed.zero_intervention, 0);
    }

    #[test]
    fn escalation_metrics_abandoned_is_not_a_closed_mission() {
        let events = vec![
            ev(1, "m-1", 0, created()),
            ev(
                2,
                "m-1",
                1_000,
                EventKind::MissionAbandoned {
                    reason: "operator retired it".into(),
                },
            ),
        ];
        let metrics = aggregate(&[("m-1".to_string(), events)], &[]);
        assert_eq!(metrics.autonomy.closed_missions, 0);
        assert_eq!(metrics.autonomy.zero_intervention_share, None);
        assert_eq!(metrics.false_greens.completed_missions, 0);
    }

    #[test]
    fn escalation_metrics_other_missions_events_are_filtered_out() {
        let events = vec![
            ev(1, "m-1", 0, created()),
            ev(
                2,
                "m-2",
                500,
                EventKind::GrantApproved {
                    kind: GrantKind::Command,
                    command: "other".into(),
                },
            ),
            ev(3, "m-1", 1_000, EventKind::MissionCompleted {}),
        ];
        let fold = mission_escalation("m-1", &events);
        assert_eq!(fold.interventions, 0);
        assert!(fold.terminal == Some(TerminalOutcome::Completed));
    }

    // -- compute_escalation_metrics over a fixture repo ----------------------

    /// Seed a mission's `events.jsonl` with the given kinds, in order.
    fn seed_mission(repo_root: &std::path::Path, id: &str, kinds: Vec<EventKind>) {
        let paths = MissionPaths::new(repo_root, id);
        let mut log = EventLog::acquire(&paths, id, Duration::ZERO, LockForce::No).unwrap();
        for kind in kinds {
            log.append(kind).unwrap();
        }
    }

    fn seed_ticket(repo_root: &std::path::Path, slug: &str, frontmatter: &str) {
        let dir = crate::ticket::Ticket::tickets_dir(repo_root);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{slug}.md")),
            format!("{frontmatter}\n## Goal\nfix it\n"),
        )
        .unwrap();
    }

    #[test]
    fn escalation_metrics_compute_end_to_end_over_fixture_repo() {
        let tmp = TempDir::new().unwrap();
        seed_mission(
            tmp.path(),
            "m-1",
            vec![created(), EventKind::MissionCompleted {}],
        );
        seed_mission(
            tmp.path(),
            "m-2",
            vec![
                created(),
                EventKind::GrantRequested {
                    milestone_id: "ms-1".into(),
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
                EventKind::GrantApproved {
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
                EventKind::MissionCompleted {},
            ],
        );
        // Hand-edited traced-from-mission frontmatter joins m-1; a ticket
        // without the field joins nothing.
        seed_ticket(
            tmp.path(),
            "defect-regression",
            "---\ntitle: Login regression\ntraced-from-mission: m-1\n---\n",
        );
        seed_ticket(
            tmp.path(),
            "ordinary-task",
            "---\ntitle: Ordinary task\n---\n",
        );

        let metrics = compute_escalation_metrics(tmp.path()).unwrap();
        assert_eq!(metrics.autonomy.closed_missions, 2);
        assert_eq!(metrics.autonomy.zero_intervention_missions, 1);
        assert_eq!(metrics.false_greens.false_greens, 1);
        assert_eq!(metrics.false_greens.false_green_rate, Some(0.5));
        assert_eq!(
            metrics.false_greens.traced_defects,
            vec![TracedDefect {
                ticket: "defect-regression".into(),
                mission_id: "m-1".into(),
            }]
        );
        // m-2's grant decision landed within the same ms (appended back to
        // back) — a decided sub-10s grant.
        assert_eq!(metrics.rubber_stamp.decided_grants, 1);
        assert_eq!(metrics.rubber_stamp.under_ten_seconds, 1);
        assert_eq!(metrics.ledger.len(), 1);
        assert_eq!(metrics.ledger[0].decision, "approved");
    }
}
