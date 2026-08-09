//! Flight-surgeon outcomes fold: autonomy ratio, grant-latency distribution,
//! an escalation ledger, cost and cycle time — plus the KRZ-321/323/329
//! extensions (per-task-class rows, the context-reuse split, the rubber-stamp
//! flag, and cost per merged change), the KRZ-316 gate score distribution
//! flags beside the rubber-stamp signal, and the KRZ-333 industry-comparison
//! set ([`crate::comparison_metrics`]) attached as a clearly-separated
//! secondary section when the fold options pin its window — all computed
//! per-request from the
//! existing event log. Pure-fold style, mirroring [`crate::trace_export`]:
//! there is no second persisted source of truth, only a function over
//! `&[Event]` (the merged-change denominator adds the live ancestry probe at
//! fold time — derived, never stored).

use crate::events::{Event, EventKind};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutonomyRatio {
    pub closed_missions: u64,
    pub total_interventions: u64,
    pub interventions_per_closed_mission: f64,
    pub zero_intervention_missions: u64,
    pub zero_intervention_share: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyBucket {
    pub label: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantLatency {
    pub buckets: Vec<LatencyBucket>,
    pub total_decided: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EscalationKind {
    Block,
    Grant,
    Revision,
}

impl EscalationKind {
    /// The wire/serde form (`block`/`grant`/`revision`) for text surfaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Grant => "grant",
            Self::Revision => "revision",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EscalationRow {
    pub ts: DateTime<Utc>,
    pub mission_id: String,
    pub kind: EscalationKind,
    pub summary: String,
    pub decision: String,
    pub latency_ms: Option<u64>,
    /// Rubber-stamp marker (ticket `rubber-stamp-grant-flag`), stamped at
    /// aggregate time against the configured threshold: `Some(true)` when
    /// this is a grant APPROVED in under the threshold, `Some(false)` for an
    /// approved grant at/over it, `None` when the marker does not apply
    /// (denied or pending grants — a fast DENY is not a rubber stamp — and
    /// non-grant rows). A flag, never an enforcement.
    #[serde(default)]
    pub rubber_stamp: Option<bool>,
}

/// The divergence ledger of one mission (ticket
/// `divergence-first-class-event`, KRZ-304), folded from its
/// `divergence.noted` / `divergence.resolved` events. A ledger exists only
/// for missions that recorded pool activity — a mission without pools has
/// NO row (absent, never zeroed).
///
/// The counts are records, not verdicts: agreement between models is a
/// signal to log, never a criterion to trust, so `agreed` feeds the
/// escalation ledger and the training corpus but gates nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DivergenceOutcomes {
    /// Units whose sibling candidate streams were compared
    /// (`divergence.noted` events).
    pub noted: u64,
    /// Of those, units whose candidate branch trees differed.
    pub diverged: u64,
    /// Of those, units with identical candidate trees — the agreement
    /// records (logged, never trusted).
    pub agreed: u64,
    /// Resolutions that chose a candidate (first-wins per unit, the
    /// engine's own emission posture — a duplicated hand-written resolution
    /// counts once).
    pub resolved_selected: u64,
    /// Resolutions that chose NONE — the unit was judged and abandoned;
    /// itself a recorded judgement, distinct from "not yet judged".
    pub resolved_none: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcomes {
    pub autonomy_ratio: AutonomyRatio,
    pub grant_latency: GrantLatency,
    pub escalations: Vec<EscalationRow>,
    /// costUsd per merged non-meta commit (outcomes-view lagging metric).
    pub cost_per_change: CostPerChange,
    /// mission.created → terminal, minus paused spans (dashboard rule).
    pub cycle_time: CycleTime,
    /// The same fold grouped by task class (ticket
    /// `outcomes-report-task-class`): one row per class recovered from
    /// `mission.created` goals, plus an explicit "unclassified" row for
    /// missions whose goal carries none. Sorted by class name with
    /// "unclassified" last.
    #[serde(default)]
    pub task_classes: Vec<TaskClassRow>,
    /// Context-reuse split per backend (fresh vs cache-read vs cache-write
    /// input tokens) — only for backends whose wire reports cache fields at
    /// all; a backend that reports none yields NO row (absent, never a
    /// fabricated 0%).
    #[serde(default)]
    pub context_reuse: Vec<ContextReuseRow>,
    /// Rubber-stamp flag summary (ticket `rubber-stamp-grant-flag`), shown
    /// alongside the latency distribution.
    #[serde(default)]
    pub rubber_stamp: RubberStampReport,
    /// Gate score distribution flags (ticket
    /// `gate-score-distribution-flags`, KRZ-316): per-gate smells folded
    /// from the scored `gate.result` series across the same mission logs —
    /// the rubber-stamp signal's documented COMPLEMENT, presented together:
    /// block-to-grant timing catches an inattentive human, these catch a
    /// mis-specified gate whose threshold nothing approaches. Carried into
    /// the escalation ledger fold as this SUMMARY FIELD, never a per-row
    /// marker: a flag indicts the GATE's specification across all missions,
    /// so pinning it on one mission's grant/block/revision row would
    /// misattribute a cross-mission smell to one escalation.
    #[serde(default)]
    pub gate_score_flags: crate::gate_score_flags::GateScoreFlagsReport,
    /// Fleet divergence ledger (KRZ-304), summed over the missions that
    /// have one — `None` when NO mission recorded a divergence event
    /// (absent means "no pools", never a fabricated zero report), and
    /// omitted from the wire then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divergences: Option<DivergenceOutcomes>,
    /// The industry-comparison set (ticket `outcomes-comparison-metrics`,
    /// KRZ-333): assisted-change share, defect density per merged change,
    /// and defect resolution time — a clearly-separated SECONDARY section
    /// beside the kranz-native metrics above, each metric carrying its
    /// inline definition (the definition is the whole argument). `None` —
    /// and omitted from the wire — when the fold options pin no comparison
    /// window (the hermetic test seam); production resolve() pins one, so
    /// every served/printed report carries the section LAST.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<crate::comparison_metrics::ComparisonReport>,
}

/// One task class's row in the outcomes report (KRZ-321).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskClassRow {
    /// The class as written in ticket frontmatter / the mission goal, or
    /// [`UNCLASSIFIED_TASK_CLASS`] when the mission carried none.
    pub task_class: String,
    pub missions: u64,
    pub closed_missions: u64,
    /// Σ worker cost across the class's missions (same rule as
    /// [`CostPerChange::total_cost_usd`]).
    pub total_cost_usd: f64,
    pub non_meta_commits: u64,
    /// total_cost_usd / non_meta_commits — None when the class has no
    /// non-meta commits (the ratio is meaningless, not zero).
    pub usd_per_commit: Option<f64>,
    /// Grant + block + revision rows raised by the class's missions.
    pub escalations: u64,
    /// Of those, the grant parks — the advisor invocations.
    pub advisor_invocations: u64,
    /// escalations / missions (every row has at least one mission).
    pub escalations_per_mission: f64,
    /// Mean created→terminal (paused spans excluded) over the class's
    /// missions with a computable cycle — None when none closed.
    pub cycle_mean_ms: Option<f64>,
}

/// The task-class label missions without a `task-class` group under
/// (KRZ-321: an explicit row, never silently dropped).
pub const UNCLASSIFIED_TASK_CLASS: &str = "unclassified";

/// One backend's context-reuse split (KRZ-321). Emitted ONLY for backends
/// whose wire reports cache token fields
/// ([`crate::types::BackendKind::reports_cache_read_tokens`]); reuse shares
/// above ~95% are the cost pattern per-mission totals hide — a signal to
/// investigate carried context, not a target to optimize.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextReuseRow {
    /// [`crate::types::BackendKind::as_str`] of the backend the mission's
    /// config routed these runs to.
    pub backend: String,
    /// Missions contributing at least one completed run on this backend.
    pub missions: u64,
    /// Completed runs folded.
    pub runs: u64,
    /// Σ non-cache input tokens.
    pub fresh_input: u64,
    /// Σ cache-read input tokens.
    pub cache_read: u64,
    /// Σ cache-write (creation) input tokens — None for backends whose wire
    /// has no such field (codex), never zero-filled.
    pub cache_write: Option<u64>,
    /// (cache_read + cache_write) / (fresh + cache_read + cache_write) over
    /// reported fields — None when no input tokens were recorded at all.
    pub reuse_share: Option<f64>,
}

/// The rubber-stamp flag summary (KRZ-323): approved grants decided under
/// the configured threshold, counted against all approved decisions with a
/// computable latency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RubberStampReport {
    /// The threshold in effect (config `rubberStampThresholdMs`; default
    /// [`crate::types::DEFAULT_RUBBER_STAMP_THRESHOLD_MS`]).
    pub threshold_ms: u64,
    /// Approved grant decisions with a computable latency (the population).
    pub approved_decisions: u64,
    /// Approved decisions under `threshold_ms` (strictly under; at/over is
    /// not flagged).
    pub flagged: u64,
    /// flagged / approved_decisions — None when nothing was approved.
    pub share: Option<f64>,
}

impl Default for RubberStampReport {
    /// The serde-backfill / empty-history default carries the DOCUMENTED
    /// threshold, never a zero that would flag everything.
    fn default() -> Self {
        Self {
            threshold_ms: crate::types::DEFAULT_RUBBER_STAMP_THRESHOLD_MS,
            approved_decisions: 0,
            flagged: 0,
            share: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostPerChange {
    pub total_cost_usd: f64,
    /// Commits recorded on feature.completed whose subject is not an
    /// engine/meta template (contract_sweep::is_meta_commit, subject-level —
    /// the fold reads events only, never git).
    pub non_meta_commits: u64,
    /// total_cost_usd / non_meta_commits (None when no non-meta commits —
    /// the ratio is meaningless, not zero).
    pub usd_per_commit: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CycleTime {
    /// Closed missions with a computable cycle (terminal event present).
    pub closed_missions: u64,
    pub total_ms: u64,
    /// total_ms / closed_missions (None when nothing closed yet).
    pub mean_ms: Option<f64>,
}

/// Per-mission fold intermediate (never serialized — the report structs are
/// the wire surface; this lives in the memo cache and the aggregator).
#[derive(Debug, Clone, PartialEq)]
pub struct MissionOutcomes {
    pub interventions: u64,
    pub is_closed: bool,
    pub latencies_ms: Vec<u64>,
    pub escalations: Vec<EscalationRow>,
    /// Σ worker.completed costUsd, with token-priced fallback for runs that
    /// record none (mirrors cost::mission_total_cost's rule).
    pub cost_usd: f64,
    pub non_meta_commits: u64,
    /// created → terminal minus paused spans; None while no terminal event.
    pub cycle_time_ms: Option<u64>,
    /// The `task-class` recovered from the mission.created goal via
    /// [`crate::ticket::parse_task_class_from_goal`]; None when the goal
    /// carries no class heading (the "unclassified" row).
    pub task_class: Option<String>,
    /// Token usage summed per backend (as routed by the mission.created
    /// config for each run's role) — the context-reuse split's input.
    pub token_sums: Vec<BackendTokenSum>,
    /// The mission's divergence ledger (KRZ-304) — `None` when the mission
    /// recorded no divergence events at all (missions without pools:
    /// absent, never a zeroed ledger).
    pub divergences: Option<DivergenceOutcomes>,
    /// The mission's scored gate evaluations (KRZ-316): every `gate.result`
    /// carrying a score pair, folded to (gate, score, threshold) samples —
    /// the distribution flag fold's per-mission input, memoized with the
    /// rest of this struct so repeated requests never re-walk the log.
    pub gate_score_samples: Vec<crate::gate_score_flags::GateScoreSample>,
    /// The KRZ-333 comparison fold's log-derived inputs, folded in the SAME
    /// scan as every other field and memoized alongside them (14th-pass
    /// review: the comparison path used to re-read every events.jsonl the
    /// native fold had just parsed — two scans per log per request). Only
    /// the git probes stay outside this struct: branch tips move
    /// independently of the log, so a merged bit can never ride the memo
    /// entry — it is probed live at report time.
    pub comparison: ComparisonInputs,
}

/// The log-derived per-mission inputs the KRZ-333 comparison fold needs
/// ([`MissionOutcomes::comparison`]) — every field a pure function of the
/// log bytes, so the whole bundle memoizes with the native fold.
#[derive(Debug, Clone, PartialEq)]
pub struct ComparisonInputs {
    /// The first terminal event's timestamp — the comparison window's key;
    /// `None` while the mission is open (an open mission is in no closed
    /// window).
    pub terminal_ts: Option<DateTime<Utc>>,
    /// The `mission.created` base branch, recovered DIRECTLY from the
    /// event: the landed-changes denominator's anchor even when the strict
    /// reducer rejects the log (the standalone fold's recovery rule,
    /// unchanged).
    pub base_branch: Option<String>,
    /// The strict reducer's reading of the log — the merged-change
    /// derivation's inputs. `None` when the reducer rejects the log
    /// (hand-edited, non-contiguous, dangling refs): a corrupt log yields
    /// no merged change — an under-read, never an inflation. Folded over
    /// the event slice as passed; production callers pass one mission's
    /// log.
    pub folded: Option<FoldedMissionRefs>,
}

/// The strict-reducer mission facts the merged-change probe needs
/// ([`ComparisonInputs::folded`]).
#[derive(Debug, Clone, PartialEq)]
pub struct FoldedMissionRefs {
    pub status: crate::types::MissionStatus,
    pub base_branch: String,
    pub mission_branch: String,
}

/// One mission's token usage on one backend, summed over its completed runs
/// (KRZ-321 context-reuse split).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackendTokenSum {
    pub backend: crate::types::BackendKind,
    pub runs: u64,
    /// Non-cache input tokens.
    pub fresh_input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// The four fixed grant-latency bucket labels, in display order.
const BUCKET_LABELS: [&str; 4] = ["<10s", "<60s", "<10m", ">=10m"];

// ---------------------------------------------------------------------------
// Per-mission memoization (outcomes-fold-scaling ticket)
// ---------------------------------------------------------------------------

/// A cached fold keyed by the log's (len, mtime): events.jsonl is append-only
/// by design, so new events always grow `len` and invalidate deterministically.
/// A rewrite that preserves length and lands in the same mtime tick could
/// stale-hit — accepted for a display fold (and impossible via the engine's
/// append path). One entry per mission; trivially bounded. The computes/hits
/// counters let the invalidation test prove per-path behavior — global
/// counters would race across parallel tests.
#[derive(Clone)]
struct CachedMission {
    len: u64,
    mtime: std::time::SystemTime,
    outcomes: MissionOutcomes,
    computes: u64,
    hits: u64,
}

static MISSION_CACHE: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, CachedMission>>,
> = std::sync::OnceLock::new();

/// Per-entry (computes, hits) for the memoization test.
#[cfg(test)]
fn cache_entry_stats(events_path: &std::path::Path) -> Option<(u64, u64)> {
    MISSION_CACHE
        .get()?
        .lock()
        .ok()?
        .get(events_path)
        .map(|c| (c.computes, c.hits))
}

/// Fold one mission with per-(path, len, mtime) memoization. Returns None
/// when the log is missing or unreadable — the caller degrades per-row
/// exactly as before; the cache never changes the skip semantics.
/// Crate-internal: the KRZ-333 comparison fold ([`crate::comparison_metrics`])
/// rides the same memoized scan instead of re-reading every log the native
/// fold just parsed (14th-pass review).
pub(crate) fn cached_mission_outcomes(
    mission_id: &str,
    events_path: &std::path::Path,
) -> Option<MissionOutcomes> {
    let meta = std::fs::metadata(events_path).ok()?;
    let (len, mtime) = (meta.len(), meta.modified().ok()?);
    let cache =
        MISSION_CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    {
        let mut guard = cache.lock().ok()?;
        if let Some(hit) = guard.get_mut(events_path) {
            if hit.len == len && hit.mtime == mtime {
                hit.hits += 1;
                return Some(hit.outcomes.clone());
            }
        }
    }
    let events = crate::event_log::EventLog::read_events(events_path).ok()?;
    let outcomes = mission_outcomes(mission_id, &events);
    if let Ok(mut guard) = cache.lock() {
        guard
            .entry(events_path.to_path_buf())
            .and_modify(|entry| {
                entry.len = len;
                entry.mtime = mtime;
                entry.outcomes = outcomes.clone();
                entry.computes += 1;
            })
            .or_insert_with(|| CachedMission {
                len,
                mtime,
                outcomes: outcomes.clone(),
                computes: 1,
                hits: 0,
            });
    }
    Some(outcomes)
}

/// Fold a single mission's outcomes from its event slice. `events` may
/// contain events for other missions too (they are filtered out) but must be
/// in ascending `seq` order for the "earliest later" grant/unblock/revision
/// matching to be correct.
pub fn mission_outcomes(mission_id: &str, events: &[Event]) -> MissionOutcomes {
    let mission_events: Vec<&Event> = events
        .iter()
        .filter(|e| e.mission_id == mission_id)
        .collect();

    let is_closed = mission_events.iter().any(|e| {
        matches!(
            e.kind,
            EventKind::MissionCompleted {}
                | EventKind::MissionFailed { .. }
                | EventKind::MissionAbandoned { .. }
        )
    });

    let plan_approved_ts = mission_events
        .iter()
        .find(|e| matches!(e.kind, EventKind::PlanApproved { .. }))
        .map(|e| e.ts);

    let mut interventions: u64 = 0;
    for e in &mission_events {
        match &e.kind {
            EventKind::UserMessage { .. } => {
                if let Some(approved_ts) = plan_approved_ts {
                    if e.ts >= approved_ts {
                        interventions += 1;
                    }
                }
            }
            EventKind::GrantApproved { .. }
            | EventKind::GrantDenied { .. }
            | EventKind::PlanRevised { .. }
            | EventKind::PlanRevisionRejected { .. } => {
                interventions += 1;
            }
            _ => {}
        }
    }

    let mut latencies_ms = Vec::new();
    let mut escalations = Vec::new();

    // Grant requests: match each to the earliest later decision with the same
    // command (falling back to the next decision in seq order), consuming
    // each decision at most once so repeated requests don't double-match.
    let mut used_decisions = vec![false; mission_events.len()];
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
            // Fallback for partial/hand-edited logs: take the next unconsumed
            // decision in seq order even when it answers a different command —
            // the same-command pass above is authoritative whenever the engine
            // echoed `command` into the decision event.
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

        let (decision, latency_ms) = match matched {
            Some(i) => {
                used_decisions[i] = true;
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

        escalations.push(EscalationRow {
            ts: req.ts,
            mission_id: mission_id.to_string(),
            kind: EscalationKind::Grant,
            summary: command.clone(),
            decision,
            latency_ms,
            // Stamped at aggregate time against the configured threshold.
            rubber_stamp: None,
        });
    }

    // Milestone blocks: match each to the earliest later unblock on the same
    // milestone id.
    let mut used_unblocks = vec![false; mission_events.len()];
    for (idx, e) in mission_events.iter().enumerate() {
        let EventKind::MilestoneBlocked {
            milestone_id,
            reason,
        } = &e.kind
        else {
            continue;
        };
        let mut matched: Option<usize> = None;
        for (i, cand) in mission_events.iter().enumerate() {
            if i <= idx || used_unblocks[i] {
                continue;
            }
            if let EventKind::MilestoneUnblocked {
                milestone_id: mid, ..
            } = &cand.kind
            {
                if mid == milestone_id {
                    matched = Some(i);
                    break;
                }
            }
        }
        let decision = match matched {
            Some(i) => {
                used_unblocks[i] = true;
                let EventKind::MilestoneUnblocked {
                    reason: unblock_reason,
                    ..
                } = &mission_events[i].kind
                else {
                    unreachable!()
                };
                format!("unblocked: {unblock_reason}")
            }
            None => "open".to_string(),
        };
        escalations.push(EscalationRow {
            ts: e.ts,
            mission_id: mission_id.to_string(),
            kind: EscalationKind::Block,
            summary: reason.clone(),
            decision,
            latency_ms: None,
            rubber_stamp: None,
        });
    }

    // Plan revisions: match each proposal to a later plan.revised or
    // plan.revision.rejected on the same revision number.
    for (idx, e) in mission_events.iter().enumerate() {
        let EventKind::PlanRevisionProposed {
            revision,
            instructions,
            ..
        } = &e.kind
        else {
            continue;
        };
        let mut decision = "pending".to_string();
        for cand in mission_events.iter().skip(idx + 1) {
            match &cand.kind {
                EventKind::PlanRevised { revision: rev, .. } if rev == revision => {
                    decision = format!("accepted (rev {rev})");
                    break;
                }
                EventKind::PlanRevisionRejected {
                    revision: rev,
                    reason,
                } if rev == revision => {
                    decision = format!("rejected: {reason}");
                    break;
                }
                _ => {}
            }
        }
        escalations.push(EscalationRow {
            ts: e.ts,
            mission_id: mission_id.to_string(),
            kind: EscalationKind::Revision,
            summary: instructions.clone(),
            decision,
            latency_ms: None,
            rubber_stamp: None,
        });
    }

    // --- cost per change + cycle time (outcomes-view lagging metrics) ------
    // Non-meta commits: feature.completed commit strings are "<sha> <subject>";
    // classify by subject only — the fold reads events, never git.
    let mut non_meta_commits: u64 = 0;
    for e in &mission_events {
        if let EventKind::FeatureCompleted { commits, .. } = &e.kind {
            for commit in commits {
                let subject = commit.split_once(' ').map(|(_, s)| s).unwrap_or("");
                if !crate::contract_sweep::is_meta_commit(subject) {
                    non_meta_commits += 1;
                }
            }
        }
    }

    // Cost: Σ recorded costUsd, falling back to token pricing with the
    // spawned run's model and the config's backend for that role (mirrors
    // cost::mission_total_cost — including $0 for the local tier).
    let config = mission_events.iter().find_map(|e| match &e.kind {
        EventKind::MissionCreated { config, .. } => Some(config),
        _ => None,
    });
    // The task class travels in the goal (ticket.rs folds it in under a
    // fixed heading; create() only ever sees the folded goal).
    let task_class = mission_events.iter().find_map(|e| match &e.kind {
        EventKind::MissionCreated { goal, .. } => crate::ticket::parse_task_class_from_goal(goal),
        _ => None,
    });
    let mut run_models: std::collections::HashMap<&str, (&str, crate::types::Role)> =
        std::collections::HashMap::new();
    for e in &mission_events {
        if let EventKind::WorkerSpawned {
            run_id,
            role,
            model,
            ..
        } = &e.kind
        {
            run_models.insert(run_id.as_str(), (model.as_str(), *role));
        }
    }
    let mut cost_usd = 0.0;
    // Token usage summed per backend (keyed by its as_str for deterministic
    // output) — the context-reuse split's per-mission input. The backend is
    // the one the mission.created config routes the run's role to, the same
    // rule the cost fallback prices with.
    let mut token_sums: std::collections::BTreeMap<&'static str, BackendTokenSum> =
        std::collections::BTreeMap::new();
    for e in &mission_events {
        if let EventKind::WorkerCompleted {
            run_id,
            tokens,
            cost_usd: recorded,
            ..
        } = &e.kind
        {
            let (model, role) = run_models
                .get(run_id.as_str())
                .copied()
                .unwrap_or(("", crate::types::Role::Worker));
            let backend = config
                .map(|c| c.backend_kind(role))
                .unwrap_or(crate::types::BackendKind::Claude);
            cost_usd += recorded
                .unwrap_or_else(|| crate::cost::usage_cost_usd_for_backend(tokens, model, backend));
            let sum = token_sums
                .entry(backend.as_str())
                .or_insert(BackendTokenSum {
                    backend,
                    runs: 0,
                    fresh_input: 0,
                    cache_read: 0,
                    cache_write: 0,
                });
            sum.runs += 1;
            sum.fresh_input += tokens.input;
            sum.cache_read += tokens.cache_read;
            sum.cache_write += tokens.cache_write;
        }
    }

    // Cycle time: created → terminal minus paused spans (a pause never
    // resumed runs to the terminal timestamp — the dashboard's rule).
    let created_ts = mission_events
        .iter()
        .find(|e| matches!(e.kind, EventKind::MissionCreated { .. }))
        .map(|e| e.ts);
    let terminal_ts = mission_events.iter().find_map(|e| {
        matches!(
            e.kind,
            EventKind::MissionCompleted {}
                | EventKind::MissionFailed { .. }
                | EventKind::MissionAbandoned { .. }
        )
        .then_some(e.ts)
    });
    let cycle_time_ms = match (created_ts, terminal_ts) {
        (Some(start), Some(end)) => {
            let mut paused_ms: i64 = 0;
            let mut pause_start: Option<DateTime<Utc>> = None;
            for e in &mission_events {
                match &e.kind {
                    EventKind::MissionPaused {} => pause_start = Some(e.ts),
                    EventKind::MissionResumed {} => {
                        if let Some(p) = pause_start.take() {
                            paused_ms += (e.ts - p).num_milliseconds().max(0);
                        }
                    }
                    _ => {}
                }
            }
            if let Some(p) = pause_start {
                paused_ms += (end - p).num_milliseconds().max(0);
            }
            Some(((end - start).num_milliseconds() - paused_ms).max(0) as u64)
        }
        _ => None,
    };

    // --- divergence ledger (KRZ-304) --------------------------------------
    // The pool's judgement trail per mission: units compared, the diverged/
    // agreed split, and the resolution KINDS (a candidate chosen vs judged-
    // and-abandoned). Resolutions count first-wins per unit — the engine
    // emits at most one, and the fold dedupes a hand-written duplicate the
    // same way so a crafted log cannot inflate the ledger.
    let mut noted: u64 = 0;
    let mut diverged: u64 = 0;
    let mut resolved_units: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut resolved_selected: u64 = 0;
    let mut resolved_none: u64 = 0;
    for e in &mission_events {
        match &e.kind {
            EventKind::DivergenceNoted { diverged: d, .. } => {
                noted += 1;
                if *d {
                    diverged += 1;
                }
            }
            EventKind::DivergenceResolved { unit, selected, .. } => {
                let first_for_unit = resolved_units.insert(unit.as_str());
                match (first_for_unit, selected) {
                    (true, Some(_)) => resolved_selected += 1,
                    (true, None) => resolved_none += 1,
                    (false, _) => {}
                }
            }
            _ => {}
        }
    }
    let divergences = (noted > 0 || !resolved_units.is_empty()).then(|| DivergenceOutcomes {
        noted,
        diverged,
        agreed: noted - diverged,
        resolved_selected,
        resolved_none,
    });

    // --- gate score samples (KRZ-316) --------------------------------------
    // Every scored `gate.result` in this mission's slice, as (gate, score,
    // threshold) samples — the distribution flag fold's input. Unscored
    // (boolean-only) gates yield no sample: excluded, never zeroed.
    let gate_score_samples = crate::gate_score_flags::collect_scored_samples(&mission_events);

    // --- comparison-fold inputs (KRZ-333; 14th-pass review) ----------------
    // Everything the industry-comparison fold needs from the log, derived in
    // this same scan so a pinned comparison window never re-reads a log the
    // native fold just parsed. The base-branch anchor comes from
    // `mission.created` DIRECTLY (a log the strict reducer rejects still
    // anchors the denominator); the merged-change derivation reads the
    // strict reducer's status + branch refs (a rejected log yields no merged
    // change — the degrade rule the standalone fold documented).
    let comparison = ComparisonInputs {
        terminal_ts,
        base_branch: mission_events.iter().find_map(|e| match &e.kind {
            EventKind::MissionCreated { base_branch, .. } => Some(base_branch.clone()),
            _ => None,
        }),
        folded: crate::reducer::fold(events)
            .ok()
            .map(|state| FoldedMissionRefs {
                status: state.mission.status,
                base_branch: state.mission.base_branch,
                mission_branch: state.mission.mission_branch,
            }),
    };

    MissionOutcomes {
        interventions,
        is_closed,
        latencies_ms,
        escalations,
        cost_usd,
        non_meta_commits,
        cycle_time_ms,
        task_class,
        token_sums: token_sums.into_values().collect(),
        divergences,
        gate_score_samples,
        comparison,
    }
}

/// Per-task-class accumulator for the KRZ-321 grouping (fold-internal).
#[derive(Default)]
struct TaskClassAcc {
    missions: u64,
    closed_missions: u64,
    total_cost_usd: f64,
    non_meta_commits: u64,
    escalations: u64,
    advisor_invocations: u64,
    cycle_count: u64,
    cycle_total_ms: u64,
}

/// Per-backend context-reuse accumulator (fold-internal).
struct ReuseAcc {
    backend: crate::types::BackendKind,
    missions: u64,
    runs: u64,
    fresh_input: u64,
    cache_read: u64,
    cache_write: u64,
}

/// Fold-time options for the outcomes report (ticket
/// `rubber-stamp-grant-flag`). Pure-fold idiom preserved: the same log plus
/// the same options always yields byte-identical report data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutcomesOptions {
    /// Grants APPROVED in under this many ms are flagged as rubber-stamp
    /// signals (strictly under; at/over is not flagged).
    pub rubber_stamp_threshold_ms: u64,
    /// When `Some((days, now))`, the industry-comparison set (KRZ-333) is
    /// folded over that window and attached to the report
    /// ([`Outcomes::comparison`]). `None` keeps the fold hermetic — no git
    /// probe, no clock — which is exactly the test seam: production
    /// [`OutcomesOptions::resolve`] pins the documented default window and
    /// the request time, so the purity rule above holds with the window as
    /// an explicit input.
    pub comparison_window: Option<(u64, DateTime<Utc>)>,
}

impl Default for OutcomesOptions {
    fn default() -> Self {
        Self {
            rubber_stamp_threshold_ms: crate::types::DEFAULT_RUBBER_STAMP_THRESHOLD_MS,
            comparison_window: None,
        }
    }
}

impl OutcomesOptions {
    /// Resolve from the repo's layered config (`rubberStampThresholdMs`).
    /// A missing key falls back to the documented default; a broken config
    /// degrades to the default too — the report fold never fails on config
    /// (the engine proper rejects bad config at run start).
    pub fn resolve(repo_root: &std::path::Path) -> Self {
        match crate::config::load(repo_root) {
            Ok(cfg) => Self {
                rubber_stamp_threshold_ms: cfg.rubber_stamp_threshold_ms,
                ..Self::default()
            },
            Err(_) => Self::default(),
        }
        .with_comparison_window()
    }

    /// Pin the industry-comparison window to the documented default
    /// ([`DEFAULT_MERGED_CHANGE_WINDOW_DAYS`], the same window the
    /// merged-change fold publishes) ending at the request time.
    fn with_comparison_window(mut self) -> Self {
        self.comparison_window = Some((DEFAULT_MERGED_CHANGE_WINDOW_DAYS, Utc::now()));
        self
    }
}

/// Enumerate every mission under `repo_root` exactly as
/// [`crate::orchestrator`]'s REST-layer callers do — union
/// [`crate::paths::MissionPaths::list_missions`] with the ids recorded in
/// `.kranz/missions/index.md` — fold each mission's outcomes, and aggregate.
/// A mission with no `events.jsonl` or an unreadable/corrupt log is skipped
/// (degrade per-row); this never panics or fails the whole aggregate.
pub fn compute_outcomes(repo_root: &std::path::Path) -> anyhow::Result<Outcomes> {
    compute_outcomes_with_options(repo_root, &OutcomesOptions::resolve(repo_root))
}

/// [`compute_outcomes`] with explicit fold options (the hermetic test seam:
/// no config file is consulted).
pub fn compute_outcomes_with_options(
    repo_root: &std::path::Path,
    options: &OutcomesOptions,
) -> anyhow::Result<Outcomes> {
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

    let mut closed_missions: u64 = 0;
    let mut total_interventions: u64 = 0;
    let mut zero_intervention_missions: u64 = 0;
    let mut all_latencies_ms = Vec::new();
    let mut escalations = Vec::new();
    let mut total_cost_usd = 0.0;
    let mut total_non_meta_commits: u64 = 0;
    let mut cycle_closed: u64 = 0;
    let mut cycle_total_ms: u64 = 0;
    // Per-task-class accumulators, keyed by class name (BTreeMap: the fold's
    // output order must be a function of the log, never of hash iteration).
    let mut class_accs: std::collections::BTreeMap<String, TaskClassAcc> =
        std::collections::BTreeMap::new();
    // Per-backend context-reuse accumulators, keyed by the backend's as_str.
    let mut reuse_accs: std::collections::BTreeMap<&'static str, ReuseAcc> =
        std::collections::BTreeMap::new();
    // Fleet divergence ledger (KRZ-304): summed over missions that have one;
    // stays None when no mission recorded a divergence event.
    let mut divergence_acc: Option<DivergenceOutcomes> = None;
    // Scored gate evaluation samples (KRZ-316): concatenated across
    // missions into the distribution flag fold's input.
    let mut all_score_samples: Vec<crate::gate_score_flags::GateScoreSample> = Vec::new();
    // The comparison fold's per-mission inputs (KRZ-333), collected in this
    // same pass so the comparison section never re-reads a log this loop
    // just folded (14th-pass review — the double scan).
    let mut comparison_inputs: Vec<(String, ComparisonInputs)> = Vec::new();

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
        // Memoized fold (outcomes-fold-scaling): unchanged logs are not
        // re-parsed on repeated requests; new events grow the file and
        // invalidate deterministically.
        let Some(out) = cached_mission_outcomes(&id, &events_path) else {
            continue;
        };
        comparison_inputs.push((id.clone(), out.comparison.clone()));
        if out.is_closed {
            closed_missions += 1;
            total_interventions += out.interventions;
            if out.interventions == 0 {
                zero_intervention_missions += 1;
            }
        }
        all_latencies_ms.extend(out.latencies_ms);
        total_cost_usd += out.cost_usd;
        total_non_meta_commits += out.non_meta_commits;
        if let Some(ms) = out.cycle_time_ms {
            cycle_closed += 1;
            cycle_total_ms += ms;
        }

        // Same fold, grouped by task class (KRZ-321).
        let class_key = out
            .task_class
            .clone()
            .unwrap_or_else(|| UNCLASSIFIED_TASK_CLASS.to_string());
        let acc = class_accs.entry(class_key).or_default();
        acc.missions += 1;
        if out.is_closed {
            acc.closed_missions += 1;
        }
        acc.total_cost_usd += out.cost_usd;
        acc.non_meta_commits += out.non_meta_commits;
        if let Some(ms) = out.cycle_time_ms {
            acc.cycle_count += 1;
            acc.cycle_total_ms += ms;
        }
        acc.escalations += out.escalations.len() as u64;
        acc.advisor_invocations += out
            .escalations
            .iter()
            .filter(|r| r.kind == EscalationKind::Grant)
            .count() as u64;

        // Same fold, grouped by backend (KRZ-321 context-reuse split).
        for sum in &out.token_sums {
            let acc = reuse_accs
                .entry(sum.backend.as_str())
                .or_insert_with(|| ReuseAcc {
                    backend: sum.backend,
                    missions: 0,
                    runs: 0,
                    fresh_input: 0,
                    cache_read: 0,
                    cache_write: 0,
                });
            acc.missions += 1;
            acc.runs += sum.runs;
            acc.fresh_input += sum.fresh_input;
            acc.cache_read += sum.cache_read;
            acc.cache_write += sum.cache_write;
        }

        // The fleet divergence ledger sums only missions that HAVE one.
        if let Some(d) = &out.divergences {
            let acc = divergence_acc.get_or_insert_with(DivergenceOutcomes::default);
            acc.noted += d.noted;
            acc.diverged += d.diverged;
            acc.agreed += d.agreed;
            acc.resolved_selected += d.resolved_selected;
            acc.resolved_none += d.resolved_none;
        }

        all_score_samples.extend(out.gate_score_samples);

        escalations.extend(out.escalations);
    }

    let interventions_per_closed_mission = if closed_missions > 0 {
        total_interventions as f64 / closed_missions as f64
    } else {
        0.0
    };
    let zero_intervention_share = if closed_missions > 0 {
        zero_intervention_missions as f64 / closed_missions as f64
    } else {
        0.0
    };

    escalations.sort_by_key(|e| std::cmp::Reverse(e.ts));

    // Rubber-stamp flag (KRZ-323): stamp each approved grant row against the
    // configured threshold and count the share. Strictly under flags; at or
    // over does not. Denied/pending grants and non-grant rows keep `None` —
    // a fast deny is not a rubber stamp.
    let mut approved_decisions: u64 = 0;
    let mut flagged: u64 = 0;
    for row in &mut escalations {
        if row.kind != EscalationKind::Grant || row.decision != "approved" {
            continue;
        }
        let Some(latency) = row.latency_ms else {
            continue;
        };
        approved_decisions += 1;
        let is_flagged = latency < options.rubber_stamp_threshold_ms;
        if is_flagged {
            flagged += 1;
        }
        row.rubber_stamp = Some(is_flagged);
    }

    // Rows sorted by class name (BTreeMap order) with "unclassified" moved
    // last — documented and deterministic.
    let mut task_classes: Vec<TaskClassRow> = class_accs
        .into_iter()
        .map(|(task_class, acc)| TaskClassRow {
            escalations_per_mission: acc.escalations as f64 / acc.missions as f64,
            usd_per_commit: (acc.non_meta_commits > 0)
                .then(|| acc.total_cost_usd / acc.non_meta_commits as f64),
            cycle_mean_ms: (acc.cycle_count > 0)
                .then(|| acc.cycle_total_ms as f64 / acc.cycle_count as f64),
            task_class,
            missions: acc.missions,
            closed_missions: acc.closed_missions,
            total_cost_usd: acc.total_cost_usd,
            non_meta_commits: acc.non_meta_commits,
            escalations: acc.escalations,
            advisor_invocations: acc.advisor_invocations,
        })
        .collect();
    task_classes.sort_by_key(|row| {
        (
            row.task_class == UNCLASSIFIED_TASK_CLASS,
            row.task_class.clone(),
        )
    });

    // Context-reuse rows: ONLY backends whose wire reports cache fields —
    // a backend reporting none yields no row (absent, never a fabricated
    // 0% split).
    let context_reuse: Vec<ContextReuseRow> = reuse_accs
        .into_values()
        .filter(|acc| acc.backend.reports_cache_read_tokens())
        .map(|acc| {
            let cache_write = acc
                .backend
                .reports_cache_write_tokens()
                .then_some(acc.cache_write);
            let cached = acc.cache_read + cache_write.unwrap_or(0);
            let total = acc.fresh_input + cached;
            ContextReuseRow {
                backend: acc.backend.as_str().to_string(),
                missions: acc.missions,
                runs: acc.runs,
                fresh_input: acc.fresh_input,
                cache_read: acc.cache_read,
                cache_write,
                reuse_share: (total > 0).then(|| cached as f64 / total as f64),
            }
        })
        .collect();

    // Industry-comparison set (KRZ-333): folded and attached only when the
    // options pin a window (production resolve() does; the hermetic seam
    // leaves it off and the section is simply absent). Folded over the
    // per-mission inputs the native loop above already derived and memoized
    // — the log scan is shared, never repeated; only the git probes run
    // live (branch tips move independently of the logs). Derived, never
    // stored.
    let comparison = options
        .comparison_window
        .map(|(window_days, now)| {
            crate::comparison_metrics::comparison_report_from_inputs(
                repo_root,
                &comparison_inputs,
                window_days,
                now,
            )
        })
        .transpose()?;

    Ok(Outcomes {
        autonomy_ratio: AutonomyRatio {
            closed_missions,
            total_interventions,
            interventions_per_closed_mission,
            zero_intervention_missions,
            zero_intervention_share,
        },
        grant_latency: bucketize(&all_latencies_ms),
        escalations,
        cost_per_change: CostPerChange {
            total_cost_usd,
            non_meta_commits: total_non_meta_commits,
            usd_per_commit: (total_non_meta_commits > 0)
                .then(|| total_cost_usd / total_non_meta_commits as f64),
        },
        cycle_time: CycleTime {
            closed_missions: cycle_closed,
            total_ms: cycle_total_ms,
            mean_ms: (cycle_closed > 0).then(|| cycle_total_ms as f64 / cycle_closed as f64),
        },
        task_classes,
        context_reuse,
        rubber_stamp: RubberStampReport {
            threshold_ms: options.rubber_stamp_threshold_ms,
            approved_decisions,
            flagged,
            share: (approved_decisions > 0).then(|| flagged as f64 / approved_decisions as f64),
        },
        gate_score_flags: crate::gate_score_flags::score_distribution_report(&all_score_samples),
        divergences: divergence_acc,
        comparison,
    })
}

/// Default time window for [`compute_cost_per_merged_change`] (KRZ-329):
/// 30 days. The window selects missions by their terminal-event timestamp
/// and is inclusive at both ends (`cutoff <= terminal_ts <= now`).
pub const DEFAULT_MERGED_CHANGE_WINDOW_DAYS: u64 = 30;

/// Largest window [`compute_cost_per_merged_change`] accepts: 36,525 days
/// (100 years) — far past any real audit window. The bound exists so the
/// `u64 → i64` conversion and the chrono subtraction can never wrap, panic,
/// or push the cutoff out of representable range (12th-pass review): the
/// REST layer rejects over-bound values with 400, and the engine errors
/// here so ANY caller is safe.
pub const MAX_MERGED_CHANGE_WINDOW_DAYS: u64 = 36_525;

/// Cost per merged change for one repo (KRZ-329), beside the autonomy
/// ratio. The numerator is the existing cost fold over missions closed in
/// the window; the denominator is merged changes — missions that COMPLETED
/// in the window AND whose branch tip is an ancestor of the live base tip
/// ([`crate::merged::merged_bit`]), derived at fold time, never stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostPerMergedChange {
    /// The window in effect (days).
    pub window_days: u64,
    /// Missions with a terminal event inside the window (any terminal kind —
    /// the same closed set as [`AutonomyRatio`]).
    pub closed_in_window: u64,
    /// Σ worker cost over the windowed missions (same rule as
    /// [`CostPerChange::total_cost_usd`]).
    pub total_cost_usd: f64,
    /// Windowed missions that closed COMPLETE with their branch landed.
    pub merged_changes: u64,
    /// total_cost_usd / merged_changes — None when nothing merged in the
    /// window (absent, never zero: no fabricated numbers).
    pub usd_per_merged_change: Option<f64>,
    /// zero-intervention closed / closed over the same window — None when
    /// nothing closed in it.
    pub zero_intervention_share: Option<f64>,
}

/// Fold one repo's cost per merged change. Pure over (event logs, live git
/// refs, `now`): the same inputs always yield byte-identical data, and no
/// merge state is ever persisted — the ancestry probe runs at fold time.
/// A mission with an unreadable/corrupt log is skipped (degrade per-row);
/// a repo git fails to open simply yields no merged changes (the ratio
/// reads absent, never zero). A `window_days` over
/// [`MAX_MERGED_CHANGE_WINDOW_DAYS`] is an honest error — never a wrapped
/// or panicked computation.
pub fn compute_cost_per_merged_change(
    repo_root: &std::path::Path,
    window_days: u64,
    now: DateTime<Utc>,
) -> anyhow::Result<CostPerMergedChange> {
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

    // Bound the window BEFORE any arithmetic (12th-pass review): an
    // unbounded `window_days` wraps the `as i64` cast negative (a cutoff in
    // the future, silently windowing the wrong missions) or panics the
    // chrono arithmetic — a read-authorized request could crash its own
    // handler. The conversions stay checked so the failure mode is always
    // an honest error, for this and every other caller.
    if window_days > MAX_MERGED_CHANGE_WINDOW_DAYS {
        return Err(crate::error::EngineError::InvalidState(format!(
            "window_days {window_days} exceeds the maximum {MAX_MERGED_CHANGE_WINDOW_DAYS} days"
        ))
        .into());
    }
    let days = i64::try_from(window_days).map_err(|_| {
        crate::error::EngineError::InvalidState(format!(
            "window_days {window_days} is out of range"
        ))
    })?;
    let window = chrono::Duration::try_days(days).ok_or_else(|| {
        crate::error::EngineError::InvalidState(format!(
            "window_days {window_days} is out of range"
        ))
    })?;
    let cutoff = now - window;
    let repo = crate::git_ops::GitRepo::open(repo_root).ok();

    let mut closed_in_window: u64 = 0;
    let mut zero_intervention: u64 = 0;
    let mut total_cost_usd = 0.0;
    let mut merged_changes: u64 = 0;

    for id in ids {
        let paths = crate::paths::MissionPaths::new(repo_root, &id);
        let events_path = paths.events_file();
        if !events_path.is_file() {
            continue;
        }
        if paths.require_no_follow().is_err() {
            continue;
        }
        let events = match crate::event_log::EventLog::read_events(&events_path) {
            Ok(events) => events,
            Err(_) => continue, // corrupt log degrades per-mission, never fails
        };
        // The window keys on the terminal event's own timestamp (the same
        // "first terminal in seq order" the cycle-time fold uses).
        let Some(terminal_ts) = events.iter().find_map(|e| {
            matches!(
                e.kind,
                EventKind::MissionCompleted {}
                    | EventKind::MissionFailed { .. }
                    | EventKind::MissionAbandoned { .. }
            )
            .then_some(e.ts)
        }) else {
            continue; // still open — not in any closed window
        };
        if terminal_ts < cutoff || terminal_ts > now {
            continue;
        }
        let out = mission_outcomes(&id, &events);
        closed_in_window += 1;
        if out.interventions == 0 {
            zero_intervention += 1;
        }
        total_cost_usd += out.cost_usd;

        // Merged change: closed COMPLETE and the mission branch landed on the
        // live base (merged.rs's probe — the same derivation the mission rows
        // and ticket projection use, run at fold time). The strict-reducer
        // refs ride `mission_outcomes`' own fold — a log the reducer rejects
        // yields no merged change (degrade per-mission, never fail the fold).
        if let (Some(repo), Some(folded)) = (repo.as_ref(), out.comparison.folded.as_ref()) {
            if folded.status == crate::types::MissionStatus::Complete
                && crate::merged::merged_bit_for_branches(
                    repo,
                    &folded.mission_branch,
                    &folded.base_branch,
                ) == Some(true)
            {
                merged_changes += 1;
            }
        }
    }

    Ok(CostPerMergedChange {
        window_days,
        closed_in_window,
        total_cost_usd,
        merged_changes,
        usd_per_merged_change: (merged_changes > 0).then(|| total_cost_usd / merged_changes as f64),
        zero_intervention_share: (closed_in_window > 0)
            .then(|| zero_intervention as f64 / closed_in_window as f64),
    })
}

/// Bucket grant-decision latencies into the four fixed windows, always
/// present (count 0 when empty) and in fixed order.
pub fn bucketize(latencies_ms: &[u64]) -> GrantLatency {
    let mut counts = [0u64; 4];
    for &latency in latencies_ms {
        let idx = if latency < 10_000 {
            0
        } else if latency < 60_000 {
            1
        } else if latency < 600_000 {
            2
        } else {
            3
        };
        counts[idx] += 1;
    }
    let buckets = BUCKET_LABELS
        .iter()
        .zip(counts)
        .map(|(label, count)| LatencyBucket {
            label: label.to_string(),
            count,
        })
        .collect();
    GrantLatency {
        buckets,
        total_decided: latencies_ms.len() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::GrantKind;

    fn ev(seq: u64, mission_id: &str, ts_secs: i64, kind: EventKind) -> Event {
        Event {
            seq,
            ts: DateTime::from_timestamp(ts_secs, 0).unwrap(),
            mission_id: mission_id.to_string(),
            kind,
        }
    }

    fn ev_ms(seq: u64, mission_id: &str, ts_ms: i64, kind: EventKind) -> Event {
        Event {
            seq,
            ts: DateTime::from_timestamp_millis(ts_ms).unwrap(),
            mission_id: mission_id.to_string(),
            kind,
        }
    }

    #[test]
    fn outcomes_latency_bucket_boundaries() {
        let latencies = vec![9_999, 10_000, 59_999, 60_000, 599_999, 600_000];
        let result = bucketize(&latencies);
        assert_eq!(result.total_decided, 6);
        assert_eq!(result.buckets.len(), 4);
        assert_eq!(result.buckets[0].label, "<10s");
        assert_eq!(result.buckets[0].count, 1); // 9_999
        assert_eq!(result.buckets[1].label, "<60s");
        assert_eq!(result.buckets[1].count, 2); // 10_000, 59_999
        assert_eq!(result.buckets[2].label, "<10m");
        assert_eq!(result.buckets[2].count, 2); // 60_000, 599_999
        assert_eq!(result.buckets[3].label, ">=10m");
        assert_eq!(result.buckets[3].count, 1); // 600_000
    }

    #[test]
    fn outcomes_latency_empty_fills_all_zero_buckets() {
        let result = bucketize(&[]);
        assert_eq!(result.total_decided, 0);
        assert_eq!(result.buckets.len(), 4);
        assert!(result.buckets.iter().all(|b| b.count == 0));
    }

    #[test]
    fn interventions_ignore_pre_approval_messages_but_count_post_approval() {
        let events = vec![
            ev(
                1,
                "m-1",
                100,
                EventKind::UserMessage {
                    text: "before".into(),
                    interrupt: false,
                },
            ),
            ev(
                2,
                "m-1",
                200,
                EventKind::PlanApproved {
                    plan: sample_plan(),
                    base_sha: None,
                },
            ),
            ev(
                3,
                "m-1",
                300,
                EventKind::UserMessage {
                    text: "after".into(),
                    interrupt: false,
                },
            ),
        ];
        let out = mission_outcomes("m-1", &events);
        assert_eq!(out.interventions, 1);
    }

    #[test]
    fn interventions_count_each_decision_kind() {
        let events = vec![
            ev(
                1,
                "m-1",
                100,
                EventKind::PlanApproved {
                    plan: sample_plan(),
                    base_sha: None,
                },
            ),
            ev(
                2,
                "m-1",
                200,
                EventKind::GrantApproved {
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
            ),
            ev(
                3,
                "m-1",
                300,
                EventKind::GrantDenied {
                    kind: GrantKind::Command,
                    command: "rm -rf".into(),
                    reason: "no".into(),
                },
            ),
            ev(
                4,
                "m-1",
                400,
                EventKind::PlanRevised {
                    revision: 1,
                    plan: sample_plan(),
                },
            ),
            ev(
                5,
                "m-1",
                500,
                EventKind::PlanRevisionRejected {
                    revision: 2,
                    reason: "bad".into(),
                },
            ),
        ];
        let out = mission_outcomes("m-1", &events);
        assert_eq!(out.interventions, 4);
    }

    #[test]
    fn interventions_no_plan_approved_counts_zero_user_messages() {
        let events = vec![ev(
            1,
            "m-1",
            100,
            EventKind::UserMessage {
                text: "hi".into(),
                interrupt: false,
            },
        )];
        let out = mission_outcomes("m-1", &events);
        assert_eq!(out.interventions, 0);
    }

    #[test]
    fn is_closed_true_for_each_terminal_event() {
        for kind in [
            EventKind::MissionCompleted {},
            EventKind::MissionFailed { reason: "x".into() },
            EventKind::MissionAbandoned { reason: "x".into() },
        ] {
            let events = vec![ev(1, "m-1", 100, kind)];
            let out = mission_outcomes("m-1", &events);
            assert!(out.is_closed);
        }
    }

    #[test]
    fn is_closed_false_without_terminal_event() {
        let events = vec![ev(
            1,
            "m-1",
            100,
            EventKind::PlanApproved {
                plan: sample_plan(),
                base_sha: None,
            },
        )];
        let out = mission_outcomes("m-1", &events);
        assert!(!out.is_closed);
    }

    #[test]
    fn escalation_grant_row_approved_denied_pending() {
        let events = vec![
            ev_ms(
                1,
                "m-1",
                0,
                EventKind::GrantRequested {
                    milestone_id: "ms-1".into(),
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
            ),
            ev_ms(
                2,
                "m-1",
                5_000,
                EventKind::GrantApproved {
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
            ),
            ev_ms(
                3,
                "m-1",
                10_000,
                EventKind::GrantRequested {
                    milestone_id: "ms-1".into(),
                    kind: GrantKind::Command,
                    command: "rm -rf".into(),
                },
            ),
            ev_ms(
                4,
                "m-1",
                15_000,
                EventKind::GrantDenied {
                    kind: GrantKind::Command,
                    command: "rm -rf".into(),
                    reason: "unsafe".into(),
                },
            ),
            ev_ms(
                5,
                "m-1",
                20_000,
                EventKind::GrantRequested {
                    milestone_id: "ms-1".into(),
                    kind: GrantKind::Command,
                    command: "still pending".into(),
                },
            ),
        ];
        let out = mission_outcomes("m-1", &events);
        let grants: Vec<_> = out
            .escalations
            .iter()
            .filter(|r| r.kind == EscalationKind::Grant)
            .collect();
        assert_eq!(grants.len(), 3);
        assert_eq!(grants[0].summary, "cargo test");
        assert_eq!(grants[0].decision, "approved");
        assert_eq!(grants[0].latency_ms, Some(5_000));
        assert_eq!(grants[1].summary, "rm -rf");
        assert_eq!(grants[1].decision, "denied: unsafe");
        assert_eq!(grants[1].latency_ms, Some(5_000));
        assert_eq!(grants[2].summary, "still pending");
        assert_eq!(grants[2].decision, "pending");
        assert_eq!(grants[2].latency_ms, None);
    }

    #[test]
    fn escalation_block_row_open_and_unblocked() {
        let events = vec![
            ev(
                1,
                "m-1",
                0,
                EventKind::MilestoneBlocked {
                    milestone_id: "ms-1".into(),
                    reason: "waiting".into(),
                },
            ),
            ev(
                2,
                "m-1",
                10,
                EventKind::MilestoneUnblocked {
                    milestone_id: "ms-1".into(),
                    reason: "cap raised".into(),
                    validator_guidance: None,
                },
            ),
            ev(
                3,
                "m-1",
                20,
                EventKind::MilestoneBlocked {
                    milestone_id: "ms-2".into(),
                    reason: "still stuck".into(),
                },
            ),
        ];
        let out = mission_outcomes("m-1", &events);
        let blocks: Vec<_> = out
            .escalations
            .iter()
            .filter(|r| r.kind == EscalationKind::Block)
            .collect();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].summary, "waiting");
        assert_eq!(blocks[0].decision, "unblocked: cap raised");
        assert_eq!(blocks[1].summary, "still stuck");
        assert_eq!(blocks[1].decision, "open");
    }

    #[test]
    fn escalation_revision_row_accepted_rejected_pending() {
        let events = vec![
            ev(
                1,
                "m-1",
                0,
                EventKind::PlanRevisionProposed {
                    revision: 1,
                    plan: sample_plan(),
                    instructions: "add tests".into(),
                },
            ),
            ev(
                2,
                "m-1",
                10,
                EventKind::PlanRevised {
                    revision: 1,
                    plan: sample_plan(),
                },
            ),
            ev(
                3,
                "m-1",
                20,
                EventKind::PlanRevisionProposed {
                    revision: 2,
                    plan: sample_plan(),
                    instructions: "drop scope".into(),
                },
            ),
            ev(
                4,
                "m-1",
                30,
                EventKind::PlanRevisionRejected {
                    revision: 2,
                    reason: "too risky".into(),
                },
            ),
            ev(
                5,
                "m-1",
                40,
                EventKind::PlanRevisionProposed {
                    revision: 3,
                    plan: sample_plan(),
                    instructions: "pending one".into(),
                },
            ),
        ];
        let out = mission_outcomes("m-1", &events);
        let revisions: Vec<_> = out
            .escalations
            .iter()
            .filter(|r| r.kind == EscalationKind::Revision)
            .collect();
        assert_eq!(revisions.len(), 3);
        assert_eq!(revisions[0].summary, "add tests");
        assert_eq!(revisions[0].decision, "accepted (rev 1)");
        assert_eq!(revisions[1].summary, "drop scope");
        assert_eq!(revisions[1].decision, "rejected: too risky");
        assert_eq!(revisions[2].summary, "pending one");
        assert_eq!(revisions[2].decision, "pending");
    }

    fn sample_plan() -> crate::types::Plan {
        crate::types::Plan {
            goal: "g".into(),
            validation_contract: vec![],
            milestones: vec![],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
        }
    }

    /// Acceptance hint 2: the per-mission fold surfaces the divergence count
    /// and the resolution KINDS (a candidate chosen vs judged-and-abandoned),
    /// first-wins per unit — and a mission without pool activity has NO
    /// ledger at all (absent, never a zeroed row).
    #[test]
    fn divergence_event_outcomes_fold_surfaces_count_and_resolution_kind() {
        let candidate = |run_id: &str, tree: &str| crate::types::DivergenceCandidate {
            run_id: run_id.into(),
            branch: format!("kranz/pool/m-1/f-1-1-{run_id}"),
            backend: "claude".into(),
            tree: tree.into(),
        };
        let noted = |seq: u64, unit: &str, diverged: bool| {
            ev(
                seq,
                "m-1",
                seq as i64,
                EventKind::DivergenceNoted {
                    unit: unit.into(),
                    candidates: vec![candidate("r-c0", "aaa"), candidate("r-c1", "bbb")],
                    diverged,
                },
            )
        };
        let resolved = |seq: u64, unit: &str, selected: Option<u32>| {
            ev(
                seq,
                "m-1",
                seq as i64,
                EventKind::DivergenceResolved {
                    unit: unit.into(),
                    selected,
                    reason: "r".into(),
                    decided_by: "operator".into(),
                },
            )
        };
        let events = vec![
            noted(1, "f-1-1", true),       // diverged
            noted(2, "f-1-2", false),      // agreement record
            resolved(3, "f-1-1", Some(1)), // chose a candidate
            resolved(4, "f-1-2", None),    // judged, none chosen
            resolved(5, "f-1-1", Some(0)), // duplicate: first-wins
        ];
        let out = mission_outcomes("m-1", &events);
        let ledger = out.divergences.expect("a pool mission has a ledger");
        assert_eq!(ledger.noted, 2, "two units compared");
        assert_eq!(ledger.diverged, 1);
        assert_eq!(
            ledger.agreed, 1,
            "the agreement record counts — logged, never trusted"
        );
        assert_eq!(ledger.resolved_selected, 1, "first-wins dedupes the repeat");
        assert_eq!(ledger.resolved_none, 1);

        // A mission with NO divergence events has no ledger at all.
        let quiet = mission_outcomes(
            "m-1",
            &[ev(
                1,
                "m-1",
                1,
                EventKind::UserMessage {
                    text: "hi".into(),
                    interrupt: false,
                },
            )],
        );
        assert_eq!(
            quiet.divergences, None,
            "absent for missions without pools — never a zeroed row"
        );
    }

    mod compute_outcomes_tests {
        use super::*;
        use crate::event_log::{EventLog, LockForce};
        use crate::paths::MissionPaths;
        use crate::types::{GrantKind, MissionConfig};
        use std::time::Duration;
        use tempfile::TempDir;

        /// Seed a mission's `events.jsonl` with the given kinds, in order.
        fn seed_mission(repo_root: &std::path::Path, id: &str, kinds: Vec<EventKind>) {
            let paths = MissionPaths::new(repo_root, id);
            let mut log = EventLog::acquire(&paths, id, Duration::ZERO, LockForce::No).unwrap();
            for kind in kinds {
                log.append(kind).unwrap();
            }
        }

        /// Write events.jsonl lines by hand (fixed timestamps) — seed_mission
        /// stamps Utc::now(), which can't test pause-span subtraction.
        fn write_timed_log(repo_root: &std::path::Path, id: &str, events: Vec<Event>) {
            let paths = MissionPaths::new(repo_root, id);
            std::fs::create_dir_all(paths.mission_dir()).unwrap();
            let lines: Vec<String> = events
                .iter()
                .map(|e| serde_json::to_string(e).unwrap())
                .collect();
            std::fs::write(paths.events_file(), lines.join("\n") + "\n").unwrap();
        }

        fn timed(seq: u64, secs: i64, kind: EventKind) -> Event {
            Event {
                seq,
                ts: chrono::DateTime::from_timestamp(secs, 0).unwrap(),
                mission_id: "m-cost".into(),
                kind,
            }
        }

        #[test]
        fn cost_per_change_and_cycle_time_fold_from_events() {
            use crate::types::{Role, RunResult, TokenUsage};

            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            write_timed_log(
                root,
                "m-cost",
                vec![
                    timed(1, 0, created("g")),
                    timed(
                        2,
                        10,
                        EventKind::PlanApproved {
                            plan: sample_plan(),
                            base_sha: None,
                        },
                    ),
                    timed(
                        3,
                        20,
                        EventKind::WorkerSpawned {
                            run_id: "r-1".into(),
                            role: Role::Worker,
                            feature_id: Some("f-1-1".into()),
                            milestone_id: Some("ms-1".into()),
                            candidate: None,
                            executor_route: None,
                            sdk_session_id: "s".into(),
                            model: "sonnet".into(),
                            quant: "n/a".into(),
                            weight_hash: None,
                            prompt_hash: "h".into(),
                            transcript_path: "t".into(),
                        },
                    ),
                    timed(
                        4,
                        100,
                        EventKind::WorkerCompleted {
                            run_id: "r-1".into(),
                            result: RunResult::Pass,
                            tokens: TokenUsage {
                                input: 1,
                                output: 1,
                                cache_read: 0,
                                cache_write: 0,
                            },
                            cost_usd: Some(12.50),
                            report: None,
                        },
                    ),
                    timed(
                        5,
                        110,
                        EventKind::FeatureCompleted {
                            feature_id: "f-1-1".into(),
                            commits: vec![
                                "aaa [f-1-1] add the thing".to_string(),
                                "bbb [kranz] mission report for m-cost".to_string(),
                            ],
                        },
                    ),
                    timed(6, 120, EventKind::MissionPaused {}),
                    timed(7, 130, EventKind::MissionResumed {}),
                    timed(8, 160, EventKind::MissionCompleted {}),
                ],
            );

            let outcomes = compute_outcomes(root).unwrap();
            let cost = &outcomes.cost_per_change;
            assert_eq!(cost.total_cost_usd, 12.50);
            // Two commits recorded; the "[kranz]" engine-meta one is excluded.
            assert_eq!(cost.non_meta_commits, 1);
            assert_eq!(cost.usd_per_commit, Some(12.50));

            let cycle = &outcomes.cycle_time;
            assert_eq!(cycle.closed_missions, 1);
            // created@0s → completed@160s = 160s, minus the 10s paused span.
            assert_eq!(cycle.total_ms, 150_000);
            assert_eq!(cycle.mean_ms, Some(150_000.0));
        }

        #[test]
        fn cost_fallback_prices_tokens_with_spawned_model_and_local_is_zero() {
            use crate::types::{Role, RunResult, TokenUsage};

            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            // Local-tier mission: no recorded costUsd, and the local backend
            // prices every token at $0 — never the opus fallback.
            let mut local_cfg = MissionConfig::default();
            local_cfg.worker.backend = Some("local".into());
            let tokens = TokenUsage {
                input: 2_000_000,
                output: 100_000,
                cache_read: 0,
                cache_write: 0,
            };
            write_timed_log(
                root,
                "m-cost",
                vec![
                    timed(
                        1,
                        0,
                        EventKind::MissionCreated {
                            goal: "g".into(),
                            base_branch: "main".into(),
                            mission_branch: "kranz/mission-x".into(),
                            config: local_cfg,
                        },
                    ),
                    timed(
                        2,
                        10,
                        EventKind::WorkerSpawned {
                            run_id: "r-1".into(),
                            role: Role::Worker,
                            feature_id: None,
                            milestone_id: Some("ms-1".into()),
                            candidate: None,
                            executor_route: None,
                            sdk_session_id: "s".into(),
                            model: "my-local-model".into(),
                            quant: "n/a".into(),
                            weight_hash: None,
                            prompt_hash: "h".into(),
                            transcript_path: "t".into(),
                        },
                    ),
                    timed(
                        3,
                        20,
                        EventKind::WorkerCompleted {
                            run_id: "r-1".into(),
                            result: RunResult::Pass,
                            tokens,
                            cost_usd: None,
                            report: None,
                        },
                    ),
                    timed(4, 30, EventKind::MissionCompleted {}),
                ],
            );

            let outcomes = compute_outcomes(root).unwrap();
            assert_eq!(
                outcomes.cost_per_change.total_cost_usd, 0.0,
                "the local tier must price at $0, never the frontier fallback"
            );
        }

        #[test]
        fn memoized_fold_skips_unchanged_logs_and_invalidates_on_new_events() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            let events_path = MissionPaths::new(root, "m-cache").events_file();

            seed_mission(
                root,
                "m-cache",
                vec![
                    created("g"),
                    EventKind::PlanApproved {
                        plan: sample_plan(),
                        base_sha: None,
                    },
                    EventKind::MissionCompleted {},
                ],
            );
            let first = compute_outcomes(root).unwrap();
            assert_eq!(
                cache_entry_stats(&events_path),
                Some((1, 0)),
                "first fold computes once, no hits"
            );

            let second = compute_outcomes(root).unwrap();
            assert_eq!(
                cache_entry_stats(&events_path),
                Some((1, 1)),
                "an unchanged log is served from the cache — no re-parse"
            );
            assert_eq!(first, second);

            // New events appended (events.jsonl is append-only, so len
            // grows) must invalidate the memo entry deterministically.
            seed_mission(
                root,
                "m-cache",
                vec![
                    EventKind::GrantRequested {
                        milestone_id: "ms-1".into(),
                        kind: GrantKind::Command,
                        command: "cargo test".into(),
                    },
                    EventKind::GrantApproved {
                        kind: GrantKind::Command,
                        command: "cargo test".into(),
                    },
                ],
            );
            let third = compute_outcomes(root).unwrap();
            assert_eq!(
                cache_entry_stats(&events_path),
                Some((2, 1)),
                "appended events invalidate the memo entry"
            );
            assert_ne!(third, second);
        }

        /// 14th-pass review: the KRZ-333 comparison fold rides the memoized
        /// native fold — one scan per log, not two per request. The cache
        /// stats are the observable: a standalone comparison-report call
        /// after a full outcomes fold must be a cache HIT (before the fix
        /// the comparison path re-read every events.jsonl from disk,
        /// invisible to the cache).
        #[test]
        fn comparison_fold_reuses_the_memoized_native_fold_scan() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            let events_path = MissionPaths::new(root, "m-cmp").events_file();
            seed_mission(
                root,
                "m-cmp",
                vec![
                    created("g"),
                    EventKind::PlanApproved {
                        plan: sample_plan(),
                        base_sha: None,
                    },
                    EventKind::MissionCompleted {},
                ],
            );

            let outcomes = compute_outcomes(root).unwrap();
            assert_eq!(
                cache_entry_stats(&events_path),
                Some((1, 0)),
                "the native fold computes once"
            );
            // The comparison section attached to the SAME fold (a tempdir is
            // no git repo, so the denominator reads absent; the base anchor
            // from mission.created still records).
            let attached = outcomes
                .comparison
                .as_ref()
                .expect("resolve() pins the comparison window");
            assert_eq!(
                attached.assisted_change_share.base_branch.as_deref(),
                Some("main")
            );
            assert_eq!(attached.window_days, DEFAULT_MERGED_CHANGE_WINDOW_DAYS);

            // The standalone entry point reuses the memoized scan too.
            let report = crate::comparison_metrics::compute_comparison_report(
                root,
                DEFAULT_MERGED_CHANGE_WINDOW_DAYS,
                chrono::Utc::now(),
            )
            .unwrap();
            assert_eq!(
                cache_entry_stats(&events_path),
                Some((1, 1)),
                "the comparison fold must ride the memoized scan, not re-read the log"
            );
            assert_eq!(&report, attached, "same inputs, same report");
        }

        fn created(goal: &str) -> EventKind {
            EventKind::MissionCreated {
                goal: goal.into(),
                base_branch: "main".into(),
                mission_branch: "kranz/mission-x".into(),
                config: MissionConfig::default(),
            }
        }

        #[test]
        fn outcomes_ratio_denominator_is_closed_missions() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();

            // Closed mission with two interventions after plan approval.
            seed_mission(
                root,
                "m-closed",
                vec![
                    created("closed one"),
                    EventKind::PlanApproved {
                        plan: sample_plan(),
                        base_sha: None,
                    },
                    EventKind::GrantApproved {
                        kind: GrantKind::Command,
                        command: "cargo test".into(),
                    },
                    EventKind::GrantDenied {
                        kind: GrantKind::Command,
                        command: "rm -rf".into(),
                        reason: "no".into(),
                    },
                    EventKind::MissionCompleted {},
                ],
            );

            // Open mission — must be excluded from the ratio denominator
            // even though it has interventions recorded.
            seed_mission(
                root,
                "m-open",
                vec![
                    created("still running"),
                    EventKind::PlanApproved {
                        plan: sample_plan(),
                        base_sha: None,
                    },
                    EventKind::GrantApproved {
                        kind: GrantKind::Command,
                        command: "echo hi".into(),
                    },
                ],
            );

            let outcomes = compute_outcomes(root).unwrap();
            let ratio = outcomes.autonomy_ratio;
            assert_eq!(ratio.closed_missions, 1);
            assert_eq!(ratio.total_interventions, 2);
            assert_eq!(ratio.interventions_per_closed_mission, 2.0);
            assert_eq!(ratio.zero_intervention_missions, 0);
            assert_eq!(ratio.zero_intervention_share, 0.0);
        }

        #[test]
        fn outcomes_ratio_zero_intervention_share_counts_clean_closed_missions() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();

            seed_mission(
                root,
                "m-clean",
                vec![
                    created("clean"),
                    EventKind::PlanApproved {
                        plan: sample_plan(),
                        base_sha: None,
                    },
                    EventKind::MissionCompleted {},
                ],
            );
            seed_mission(
                root,
                "m-dirty",
                vec![
                    created("dirty"),
                    EventKind::PlanApproved {
                        plan: sample_plan(),
                        base_sha: None,
                    },
                    EventKind::GrantApproved {
                        kind: GrantKind::Command,
                        command: "cargo test".into(),
                    },
                    EventKind::MissionCompleted {},
                ],
            );

            let outcomes = compute_outcomes(root).unwrap();
            let ratio = outcomes.autonomy_ratio;
            assert_eq!(ratio.closed_missions, 2);
            assert_eq!(ratio.zero_intervention_missions, 1);
            assert_eq!(ratio.zero_intervention_share, 0.5);
        }

        #[test]
        fn outcomes_ledger_newest_first() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();

            // m-a's grant request/decision happen earliest; m-b's happen later.
            seed_mission(
                root,
                "m-a",
                vec![
                    created("a"),
                    EventKind::GrantRequested {
                        milestone_id: "ms-1".into(),
                        kind: GrantKind::Command,
                        command: "cargo test".into(),
                    },
                    EventKind::GrantApproved {
                        kind: GrantKind::Command,
                        command: "cargo test".into(),
                    },
                ],
            );
            seed_mission(
                root,
                "m-b",
                vec![
                    created("b"),
                    EventKind::GrantRequested {
                        milestone_id: "ms-1".into(),
                        kind: GrantKind::Command,
                        command: "npm test".into(),
                    },
                    EventKind::GrantDenied {
                        kind: GrantKind::Command,
                        command: "npm test".into(),
                        reason: "no".into(),
                    },
                ],
            );

            let outcomes = compute_outcomes(root).unwrap();
            assert!(outcomes.escalations.len() >= 2);
            for pair in outcomes.escalations.windows(2) {
                assert!(pair[0].ts >= pair[1].ts);
            }
            // m-b's rows were appended later (later real-time `ts`), so they
            // must sort ahead of m-a's in the newest-first ledger.
            let mission_order: Vec<&str> = outcomes
                .escalations
                .iter()
                .map(|r| r.mission_id.as_str())
                .collect();
            assert_eq!(mission_order[0], "m-b");
        }

        #[test]
        fn outcomes_empty_repo_yields_all_zero_defaults() {
            let tmp = TempDir::new().unwrap();
            let outcomes = compute_outcomes(tmp.path()).unwrap();

            let ratio = outcomes.autonomy_ratio;
            assert_eq!(ratio.closed_missions, 0);
            assert_eq!(ratio.interventions_per_closed_mission, 0.0);
            assert_eq!(ratio.zero_intervention_share, 0.0);

            assert_eq!(outcomes.grant_latency.buckets.len(), 4);
            assert!(outcomes.grant_latency.buckets.iter().all(|b| b.count == 0));
            assert_eq!(outcomes.grant_latency.total_decided, 0);

            assert!(outcomes.escalations.is_empty());
        }

        /// 12th-pass review: an unbounded `window_days` once wrapped the
        /// `as i64` cast negative or panicked the chrono arithmetic — a
        /// read-authorized request could crash its handler. Over the
        /// documented maximum is now an honest error for ANY caller;
        /// the maximum itself still computes.
        #[test]
        fn window_days_bound_over_max_errors_instead_of_panicking() {
            let tmp = TempDir::new().unwrap();
            let now = Utc::now();
            let err = compute_cost_per_merged_change(tmp.path(), u64::MAX, now)
                .expect_err("u64::MAX must error, never wrap or panic");
            assert!(err.to_string().contains("exceeds the maximum"), "{err}");
            let err =
                compute_cost_per_merged_change(tmp.path(), MAX_MERGED_CHANGE_WINDOW_DAYS + 1, now)
                    .expect_err("just over the bound errors");
            assert!(err.to_string().contains("exceeds the maximum"), "{err}");
            let report =
                compute_cost_per_merged_change(tmp.path(), MAX_MERGED_CHANGE_WINDOW_DAYS, now)
                    .expect("the documented maximum computes");
            assert_eq!(report.window_days, MAX_MERGED_CHANGE_WINDOW_DAYS);
            assert_eq!(report.closed_in_window, 0);
        }

        #[test]
        fn outcomes_skips_mission_with_corrupt_event_log() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();

            seed_mission(
                root,
                "m-good",
                vec![
                    created("good"),
                    EventKind::PlanApproved {
                        plan: sample_plan(),
                        base_sha: None,
                    },
                    EventKind::MissionCompleted {},
                ],
            );

            // Corrupt mission: events.jsonl exists but is not valid JSONL.
            let bad_paths = MissionPaths::new(root, "m-bad");
            std::fs::create_dir_all(bad_paths.mission_dir()).unwrap();
            std::fs::write(bad_paths.events_file(), "not valid json\n").unwrap();

            let outcomes = compute_outcomes(root).unwrap();
            assert_eq!(outcomes.autonomy_ratio.closed_missions, 1);
        }

        /// The fleet ledger sums only missions that HAVE one; a repo with no
        /// pool activity reports None (absent — never a fabricated zero).
        #[test]
        fn divergence_event_fleet_ledger_sums_only_pool_missions() {
            let candidate = |run_id: &str| crate::types::DivergenceCandidate {
                run_id: run_id.into(),
                branch: format!("kranz/pool/m-pool/f-1-1-{run_id}"),
                backend: "claude".into(),
                tree: "aaa".into(),
            };
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            seed_mission(
                root,
                "m-pool",
                vec![
                    created("pool"),
                    EventKind::DivergenceNoted {
                        unit: "f-1-1".into(),
                        candidates: vec![candidate("r-c0"), candidate("r-c1")],
                        diverged: true,
                    },
                    EventKind::DivergenceResolved {
                        unit: "f-1-1".into(),
                        selected: Some(0),
                        reason: "kept".into(),
                        decided_by: "operator".into(),
                    },
                ],
            );
            seed_mission(root, "m-quiet", vec![created("quiet")]);

            let outcomes = compute_outcomes(root).unwrap();
            let ledger = outcomes
                .divergences
                .expect("a repo with a pool mission reports a fleet ledger");
            assert_eq!(ledger.noted, 1);
            assert_eq!(ledger.diverged, 1);
            assert_eq!(ledger.agreed, 0);
            assert_eq!(ledger.resolved_selected, 1);
            assert_eq!(ledger.resolved_none, 0);

            // No pool activity anywhere → the fleet ledger is absent, and
            // stays off the wire (additive: pool-less reports are unchanged).
            let tmp2 = TempDir::new().unwrap();
            seed_mission(tmp2.path(), "m-quiet", vec![created("quiet")]);
            let outcomes = compute_outcomes(tmp2.path()).unwrap();
            assert_eq!(outcomes.divergences, None);
            let value = serde_json::to_value(&outcomes).unwrap();
            assert!(
                !value.as_object().unwrap().contains_key("divergences"),
                "no divergences key on the wire without pools: {value}"
            );
        }
    }

    /// KRZ-321/323 fold extensions: per-task-class rows, the context-reuse
    /// split, and the rubber-stamp flag — all derived from the same event
    /// log at fold time.
    mod outcomes_report_tests {
        use super::*;
        use crate::paths::MissionPaths;
        use crate::types::{GrantKind, MissionConfig, Role, RunResult, TokenUsage};
        use tempfile::TempDir;

        fn ev_ms(seq: u64, mission_id: &str, ts_ms: i64, kind: EventKind) -> Event {
            Event {
                seq,
                ts: DateTime::from_timestamp_millis(ts_ms).unwrap(),
                mission_id: mission_id.to_string(),
                kind,
            }
        }

        fn write_log(repo_root: &std::path::Path, id: &str, events: Vec<Event>) {
            let paths = MissionPaths::new(repo_root, id);
            std::fs::create_dir_all(paths.mission_dir()).unwrap();
            let lines: Vec<String> = events
                .iter()
                .map(|e| serde_json::to_string(e).unwrap())
                .collect();
            std::fs::write(paths.events_file(), lines.join("\n") + "\n").unwrap();
        }

        fn sample_plan() -> crate::types::Plan {
            crate::types::Plan {
                goal: "g".into(),
                validation_contract: vec![],
                milestones: vec![],
                considered_alternatives: None,
                command_grants: vec![],
                touch_set: vec![],
            }
        }

        /// A mission.created whose goal carries a `task-class` heading in the
        /// exact layout [`crate::ticket::Ticket::mission_goal`] folds it in.
        fn created_with_class(mission_branch: &str, task_class: Option<&str>) -> EventKind {
            let goal = match task_class {
                Some(class) => format!("do the thing\n\n## Task class\n{class}\n"),
                None => "do the thing".to_string(),
            };
            created_with_config(mission_branch, goal, MissionConfig::default())
        }

        fn created_with_config(
            mission_branch: &str,
            goal: String,
            config: MissionConfig,
        ) -> EventKind {
            EventKind::MissionCreated {
                goal,
                base_branch: "main".into(),
                mission_branch: mission_branch.into(),
                config,
            }
        }

        fn worker_spawned(run_id: &str) -> EventKind {
            EventKind::WorkerSpawned {
                run_id: run_id.into(),
                role: Role::Worker,
                feature_id: Some("f-1-1".into()),
                milestone_id: Some("ms-1".into()),
                candidate: None,
                executor_route: None,
                sdk_session_id: "s".into(),
                model: "sonnet".into(),
                quant: "n/a".into(),
                weight_hash: None,
                prompt_hash: "h".into(),
                transcript_path: "t".into(),
            }
        }

        fn worker_completed(run_id: &str, tokens: TokenUsage, cost_usd: f64) -> EventKind {
            EventKind::WorkerCompleted {
                run_id: run_id.into(),
                result: RunResult::Pass,
                tokens,
                cost_usd: Some(cost_usd),
                report: None,
            }
        }

        fn grant_req(seq: u64, mission_id: &str, ts_ms: i64, command: &str) -> Event {
            ev_ms(
                seq,
                mission_id,
                ts_ms,
                EventKind::GrantRequested {
                    milestone_id: "ms-1".into(),
                    kind: GrantKind::Command,
                    command: command.into(),
                },
            )
        }

        fn grant_yes(seq: u64, mission_id: &str, ts_ms: i64, command: &str) -> Event {
            ev_ms(
                seq,
                mission_id,
                ts_ms,
                EventKind::GrantApproved {
                    kind: GrantKind::Command,
                    command: command.into(),
                },
            )
        }

        #[test]
        fn outcomes_report_task_class_rows_group_and_unclassified_last() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();

            // m-a: execution-class, closed, $10 spend, one non-meta commit,
            // one approved grant park, a 100s cycle.
            write_log(
                root,
                "m-a",
                vec![
                    ev_ms(
                        1,
                        "m-a",
                        0,
                        created_with_class("kranz/m-a", Some("execution-class")),
                    ),
                    ev_ms(2, "m-a", 1_000, worker_spawned("r-a")),
                    ev_ms(
                        3,
                        "m-a",
                        2_000,
                        worker_completed(
                            "r-a",
                            TokenUsage {
                                input: 1,
                                output: 1,
                                cache_read: 0,
                                cache_write: 0,
                            },
                            10.0,
                        ),
                    ),
                    ev_ms(
                        4,
                        "m-a",
                        3_000,
                        EventKind::FeatureCompleted {
                            feature_id: "f-1-1".into(),
                            commits: vec!["aaa [f-1-1] add the thing".to_string()],
                        },
                    ),
                    grant_req(5, "m-a", 4_000, "cargo test"),
                    grant_yes(6, "m-a", 64_000, "cargo test"),
                    ev_ms(7, "m-a", 100_000, EventKind::MissionCompleted {}),
                ],
            );
            // m-b: same class, still open (no terminal), $5 spend, one
            // pending grant park.
            write_log(
                root,
                "m-b",
                vec![
                    ev_ms(
                        1,
                        "m-b",
                        0,
                        created_with_class("kranz/m-b", Some("execution-class")),
                    ),
                    ev_ms(2, "m-b", 1_000, worker_spawned("r-b")),
                    ev_ms(
                        3,
                        "m-b",
                        2_000,
                        worker_completed(
                            "r-b",
                            TokenUsage {
                                input: 1,
                                output: 1,
                                cache_read: 0,
                                cache_write: 0,
                            },
                            5.0,
                        ),
                    ),
                    grant_req(4, "m-b", 3_000, "cargo clippy"),
                ],
            );
            // m-c: no task class in its goal, closed with a 50s cycle, no
            // escalations and no spend.
            write_log(
                root,
                "m-c",
                vec![
                    ev_ms(1, "m-c", 0, created_with_class("kranz/m-c", None)),
                    ev_ms(2, "m-c", 50_000, EventKind::MissionCompleted {}),
                ],
            );

            let outcomes =
                compute_outcomes_with_options(root, &OutcomesOptions::default()).unwrap();
            assert_eq!(outcomes.task_classes.len(), 2);
            let exec = &outcomes.task_classes[0];
            assert_eq!(exec.task_class, "execution-class");
            assert_eq!(exec.missions, 2);
            assert_eq!(exec.closed_missions, 1);
            assert_eq!(exec.total_cost_usd, 15.0);
            assert_eq!(exec.non_meta_commits, 1);
            assert_eq!(exec.usd_per_commit, Some(15.0));
            assert_eq!(exec.escalations, 2);
            assert_eq!(exec.advisor_invocations, 2);
            assert_eq!(exec.escalations_per_mission, 1.0);
            assert_eq!(exec.cycle_mean_ms, Some(100_000.0));

            let unclassified = &outcomes.task_classes[1];
            assert_eq!(unclassified.task_class, UNCLASSIFIED_TASK_CLASS);
            assert_eq!(unclassified.missions, 1);
            assert_eq!(unclassified.closed_missions, 1);
            assert_eq!(unclassified.non_meta_commits, 0);
            // Missing data is absent, never zero-filled.
            assert_eq!(unclassified.usd_per_commit, None);
            assert_eq!(unclassified.escalations, 0);
            assert_eq!(unclassified.advisor_invocations, 0);
            assert_eq!(unclassified.escalations_per_mission, 0.0);
            assert_eq!(unclassified.cycle_mean_ms, Some(50_000.0));
        }

        #[test]
        fn outcomes_report_context_reuse_split_absent_for_unreporting_backends() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();

            // Claude run with cache fields reported.
            write_log(
                root,
                "m-claude",
                vec![
                    ev_ms(1, "m-claude", 0, created_with_class("kranz/m-c", None)),
                    ev_ms(2, "m-claude", 1_000, worker_spawned("r-1")),
                    ev_ms(
                        3,
                        "m-claude",
                        2_000,
                        worker_completed(
                            "r-1",
                            TokenUsage {
                                input: 500,
                                output: 10,
                                cache_read: 800,
                                cache_write: 200,
                            },
                            1.0,
                        ),
                    ),
                    ev_ms(4, "m-claude", 3_000, EventKind::MissionCompleted {}),
                ],
            );
            // Local-tier mission: the local backend's wire carries no cache
            // fields at all, so it must yield NO reuse row (absent — never a
            // fabricated 0% split).
            let mut local_cfg = MissionConfig::default();
            local_cfg.worker.backend = Some("local".into());
            write_log(
                root,
                "m-local",
                vec![
                    ev_ms(
                        1,
                        "m-local",
                        0,
                        created_with_config("kranz/m-l", "g".into(), local_cfg),
                    ),
                    ev_ms(2, "m-local", 1_000, worker_spawned("r-2")),
                    ev_ms(
                        3,
                        "m-local",
                        2_000,
                        worker_completed(
                            "r-2",
                            TokenUsage {
                                input: 100,
                                output: 10,
                                cache_read: 0,
                                cache_write: 0,
                            },
                            0.0,
                        ),
                    ),
                    ev_ms(4, "m-local", 3_000, EventKind::MissionCompleted {}),
                ],
            );

            let outcomes =
                compute_outcomes_with_options(root, &OutcomesOptions::default()).unwrap();
            assert_eq!(outcomes.context_reuse.len(), 1);
            let row = &outcomes.context_reuse[0];
            assert_eq!(row.backend, "claude");
            assert_eq!(row.missions, 1);
            assert_eq!(row.runs, 1);
            assert_eq!(row.fresh_input, 500);
            assert_eq!(row.cache_read, 800);
            assert_eq!(row.cache_write, Some(200));
            assert_eq!(row.reuse_share, Some(1_000.0 / 1_500.0));
        }

        #[test]
        fn outcomes_report_context_reuse_codex_cache_write_is_absent() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();

            // Codex reports cached input tokens but has no cache-write field
            // on its wire: cache_read is real, cache_write must be absent
            // (None), never zero-filled.
            let mut codex_cfg = MissionConfig::default();
            codex_cfg.worker.backend = Some("codex".into());
            write_log(
                root,
                "m-codex",
                vec![
                    ev_ms(
                        1,
                        "m-codex",
                        0,
                        created_with_config("kranz/m-x", "g".into(), codex_cfg),
                    ),
                    ev_ms(2, "m-codex", 1_000, worker_spawned("r-1")),
                    ev_ms(
                        3,
                        "m-codex",
                        2_000,
                        worker_completed(
                            "r-1",
                            TokenUsage {
                                input: 900,
                                output: 10,
                                cache_read: 100,
                                cache_write: 0,
                            },
                            1.0,
                        ),
                    ),
                    ev_ms(4, "m-codex", 3_000, EventKind::MissionCompleted {}),
                ],
            );

            let outcomes =
                compute_outcomes_with_options(root, &OutcomesOptions::default()).unwrap();
            assert_eq!(outcomes.context_reuse.len(), 1);
            let row = &outcomes.context_reuse[0];
            assert_eq!(row.backend, "codex");
            assert_eq!(row.cache_read, 100);
            assert_eq!(row.cache_write, None);
            assert_eq!(row.reuse_share, Some(100.0 / 1_000.0));
        }

        #[test]
        fn outcomes_report_rubber_stamp_boundary_at_threshold() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();

            // Five parks against the default 10s threshold:
            // - 9_999ms approval → flagged (strictly under);
            // - 10_000ms approval → NOT flagged (at the threshold);
            // - 15_000ms approval → NOT flagged (over);
            // - 5_000ms DENIAL → marker does not apply (a fast deny is not a
            //   rubber stamp) and is not in the population;
            // - pending → no marker, not in the population.
            write_log(
                root,
                "m-1",
                vec![
                    ev_ms(1, "m-1", 0, created_with_class("kranz/m-1", None)),
                    ev_ms(
                        2,
                        "m-1",
                        1_000,
                        EventKind::PlanApproved {
                            plan: sample_plan(),
                            base_sha: None,
                        },
                    ),
                    grant_req(3, "m-1", 2_000, "under"),
                    grant_yes(4, "m-1", 11_999, "under"),
                    grant_req(5, "m-1", 20_000, "at"),
                    grant_yes(6, "m-1", 30_000, "at"),
                    grant_req(7, "m-1", 40_000, "over"),
                    grant_yes(8, "m-1", 55_000, "over"),
                    grant_req(9, "m-1", 60_000, "denied-fast"),
                    ev_ms(
                        10,
                        "m-1",
                        65_000,
                        EventKind::GrantDenied {
                            kind: GrantKind::Command,
                            command: "denied-fast".into(),
                            reason: "no".into(),
                        },
                    ),
                    grant_req(11, "m-1", 70_000, "pending"),
                    ev_ms(12, "m-1", 80_000, EventKind::MissionCompleted {}),
                ],
            );

            let outcomes =
                compute_outcomes_with_options(root, &OutcomesOptions::default()).unwrap();
            let stamp = &outcomes.rubber_stamp;
            assert_eq!(
                stamp.threshold_ms,
                crate::types::DEFAULT_RUBBER_STAMP_THRESHOLD_MS
            );
            assert_eq!(stamp.approved_decisions, 3);
            assert_eq!(stamp.flagged, 1);
            assert_eq!(stamp.share, Some(1.0 / 3.0));

            let marker = |summary: &str| {
                outcomes
                    .escalations
                    .iter()
                    .find(|r| r.summary == summary)
                    .unwrap()
                    .rubber_stamp
            };
            assert_eq!(marker("under"), Some(true));
            assert_eq!(
                marker("at"),
                Some(false),
                "at the threshold is not under it"
            );
            assert_eq!(marker("over"), Some(false));
            assert_eq!(marker("denied-fast"), None);
            assert_eq!(marker("pending"), None);
        }

        #[test]
        fn outcomes_report_rubber_stamp_threshold_resolves_from_config() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();

            // The threshold is a config key (rubberStampThresholdMs); the
            // project layer sets 60s here, so a 15s approval flags.
            std::fs::create_dir_all(root.join(".kranz")).unwrap();
            std::fs::write(
                root.join(".kranz").join("config.json"),
                "{\"rubberStampThresholdMs\": 60000}",
            )
            .unwrap();
            assert_eq!(
                OutcomesOptions::resolve(root).rubber_stamp_threshold_ms,
                60_000
            );

            write_log(
                root,
                "m-1",
                vec![
                    ev_ms(1, "m-1", 0, created_with_class("kranz/m-1", None)),
                    ev_ms(
                        2,
                        "m-1",
                        1_000,
                        EventKind::PlanApproved {
                            plan: sample_plan(),
                            base_sha: None,
                        },
                    ),
                    grant_req(3, "m-1", 2_000, "fifteen seconds"),
                    grant_yes(4, "m-1", 17_000, "fifteen seconds"),
                    ev_ms(5, "m-1", 20_000, EventKind::MissionCompleted {}),
                ],
            );

            // compute_outcomes is the config-reading entry point the CLI and
            // REST surfaces call.
            let outcomes = compute_outcomes(root).unwrap();
            assert_eq!(outcomes.rubber_stamp.threshold_ms, 60_000);
            assert_eq!(outcomes.rubber_stamp.flagged, 1);
            assert_eq!(outcomes.rubber_stamp.share, Some(1.0));
            assert_eq!(outcomes.escalations[0].rubber_stamp, Some(true));
        }

        #[test]
        fn outcomes_report_rubber_stamp_absent_without_approvals() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();

            write_log(
                root,
                "m-1",
                vec![
                    ev_ms(1, "m-1", 0, created_with_class("kranz/m-1", None)),
                    ev_ms(
                        2,
                        "m-1",
                        1_000,
                        EventKind::PlanApproved {
                            plan: sample_plan(),
                            base_sha: None,
                        },
                    ),
                    grant_req(3, "m-1", 2_000, "only-denied"),
                    ev_ms(
                        4,
                        "m-1",
                        3_000,
                        EventKind::GrantDenied {
                            kind: GrantKind::Command,
                            command: "only-denied".into(),
                            reason: "no".into(),
                        },
                    ),
                    ev_ms(5, "m-1", 4_000, EventKind::MissionCompleted {}),
                ],
            );

            let outcomes =
                compute_outcomes_with_options(root, &OutcomesOptions::default()).unwrap();
            assert_eq!(outcomes.rubber_stamp.approved_decisions, 0);
            assert_eq!(outcomes.rubber_stamp.flagged, 0);
            assert_eq!(outcomes.rubber_stamp.share, None);
            assert_eq!(outcomes.escalations[0].rubber_stamp, None);
        }

        #[test]
        fn outcomes_report_fold_is_byte_identical_across_repeated_computes() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();

            write_log(
                root,
                "m-1",
                vec![
                    ev_ms(
                        1,
                        "m-1",
                        0,
                        created_with_class("kranz/m-1", Some("execution-class")),
                    ),
                    ev_ms(2, "m-1", 1_000, worker_spawned("r-1")),
                    ev_ms(
                        3,
                        "m-1",
                        2_000,
                        worker_completed(
                            "r-1",
                            TokenUsage {
                                input: 500,
                                output: 10,
                                cache_read: 800,
                                cache_write: 200,
                            },
                            3.0,
                        ),
                    ),
                    grant_req(4, "m-1", 3_000, "cargo test"),
                    grant_yes(5, "m-1", 6_000, "cargo test"),
                    ev_ms(6, "m-1", 10_000, EventKind::MissionCompleted {}),
                ],
            );

            let options = OutcomesOptions::default();
            let first = compute_outcomes_with_options(root, &options).unwrap();
            let second = compute_outcomes_with_options(root, &options).unwrap();
            assert_eq!(first, second);
            assert_eq!(
                serde_json::to_string(&first).unwrap(),
                serde_json::to_string(&second).unwrap(),
                "the same log plus the same options yields byte-identical data"
            );
        }

        /// A `gate.result` event; `score` is the (score, threshold) pair a
        /// scored gate reports, `None` for a boolean-only gate (the
        /// gate_scores.rs fixture idiom).
        fn gate_scored(
            seq: u64,
            mission_id: &str,
            ts_ms: i64,
            gate: &str,
            score: Option<(f64, f64)>,
        ) -> Event {
            ev_ms(
                seq,
                mission_id,
                ts_ms,
                EventKind::GateResult {
                    gate: gate.into(),
                    surface: crate::gate::GateSurface::Approval,
                    kind: crate::gate::GateKind::Deterministic,
                    index: 0,
                    verdict: crate::gate::GateVerdict::Pass,
                    artefact_ref: format!("contract gate {gate}"),
                    artefact_detail: None,
                    score: score.map(|(score, _)| score),
                    threshold: score.map(|(_, threshold)| threshold),
                },
            )
        }

        /// KRZ-316: the distribution flags fold beside the rubber-stamp
        /// signal in ONE report — the documented complement. Ten constant
        /// far-from-threshold scores across two missions flag the gate
        /// (never-approaches AND near-constant) while a sub-10s grant
        /// approval flags the human side; the ledger rows stay untouched
        /// (the gate smell is the summary field, never a row marker).
        #[test]
        fn score_distribution_flag_outcomes_fold_flags_beside_rubber_stamp() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();

            let mut m1 = vec![
                ev_ms(1, "m-1", 0, created_with_class("kranz/m-1", None)),
                ev_ms(
                    2,
                    "m-1",
                    1_000,
                    EventKind::PlanApproved {
                        plan: sample_plan(),
                        base_sha: None,
                    },
                ),
                grant_req(3, "m-1", 2_000, "cargo test"),
                grant_yes(4, "m-1", 4_000, "cargo test"),
            ];
            for i in 0..5 {
                m1.push(gate_scored(
                    5 + i,
                    "m-1",
                    5_000 + i as i64,
                    "vacuous-filter",
                    Some((0.5, 1.0)),
                ));
            }
            m1.push(ev_ms(10, "m-1", 10_000, EventKind::MissionCompleted {}));
            write_log(root, "m-1", m1);

            let mut m2 = vec![ev_ms(1, "m-2", 0, created_with_class("kranz/m-2", None))];
            for i in 0..5 {
                m2.push(gate_scored(
                    2 + i,
                    "m-2",
                    5_000 + i as i64,
                    "vacuous-filter",
                    Some((0.5, 1.0)),
                ));
            }
            m2.push(ev_ms(7, "m-2", 10_000, EventKind::MissionCompleted {}));
            write_log(root, "m-2", m2);

            let outcomes =
                compute_outcomes_with_options(root, &OutcomesOptions::default()).unwrap();

            // The human-side signal, as before.
            assert_eq!(outcomes.rubber_stamp.flagged, 1);
            assert_eq!(outcomes.escalations[0].rubber_stamp, Some(true));

            // The gate-side complement beside it.
            let report = &outcomes.gate_score_flags;
            assert_eq!(report.scored_gates, 1);
            assert_eq!(report.assessed_gates, 1);
            assert_eq!(report.flags.len(), 2);
            assert!(
                report.flags.iter().all(|f| f.gate == "vacuous-filter"),
                "the flag names the gate: {report:?}"
            );
            let kinds: Vec<_> = report.flags.iter().map(|f| f.kind).collect();
            assert_eq!(
                kinds,
                [
                    crate::gate_score_flags::GateScoreFlagKind::NeverApproachesThreshold,
                    crate::gate_score_flags::GateScoreFlagKind::NearConstant,
                ]
            );
            // The flag carries the distribution that triggered it: ten
            // samples over BOTH missions, closest approach 0.5, variance 0.
            let d = &report.flags[0].distribution;
            assert_eq!(d.samples, 10);
            assert_eq!(d.closest_approach, 0.5);
            assert_eq!(d.variance, 0.0);
            // The rule constants ride the wire (the rubber-stamp idiom).
            assert_eq!(
                report.min_samples,
                crate::gate_score_flags::MIN_SAMPLE_COUNT
            );
        }

        /// KRZ-316 absence rules in the outcomes fold: a scored gate below
        /// the minimum sample is counted but NEVER assessed (no flags, no
        /// zero-filled distribution), and a boolean-only gate produces no
        /// population at all — it appears nowhere in the report.
        #[test]
        fn score_distribution_flag_outcomes_fold_sub_minimum_and_unscored_absent() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();

            // m-1: three scored evaluations — under the 10-sample minimum.
            let mut m1 = vec![ev_ms(1, "m-1", 0, created_with_class("kranz/m-1", None))];
            for i in 0..3 {
                m1.push(gate_scored(
                    2 + i,
                    "m-1",
                    5_000 + i as i64,
                    "vacuous-filter",
                    Some((0.5, 1.0)),
                ));
            }
            m1.push(ev_ms(5, "m-1", 10_000, EventKind::MissionCompleted {}));
            write_log(root, "m-1", m1);

            // m-2: only boolean-only gate events — no score pair at all.
            write_log(
                root,
                "m-2",
                vec![
                    ev_ms(1, "m-2", 0, created_with_class("kranz/m-2", None)),
                    gate_scored(2, "m-2", 5_000, "env-sensitive", None),
                    gate_scored(3, "m-2", 6_000, "env-sensitive", None),
                    ev_ms(4, "m-2", 10_000, EventKind::MissionCompleted {}),
                ],
            );

            let outcomes =
                compute_outcomes_with_options(root, &OutcomesOptions::default()).unwrap();
            let report = &outcomes.gate_score_flags;
            assert_eq!(
                report.scored_gates, 1,
                "the unscored gate adds no population: {report:?}"
            );
            assert_eq!(report.assessed_gates, 0, "under the minimum: unassessed");
            assert!(
                report.flags.is_empty(),
                "absent, never a zero-filled row: {report:?}"
            );
        }
    }
}
