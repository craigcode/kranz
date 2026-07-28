//! Flight-surgeon outcomes fold: autonomy ratio, grant-latency distribution,
//! and an escalation ledger — all computed per-request from the existing
//! event log. Pure-fold style, mirroring [`crate::trace_export`]: there is no
//! second persisted source of truth, only a function over `&[Event]`.

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
fn cached_mission_outcomes(
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
    for e in &mission_events {
        if let EventKind::WorkerCompleted {
            run_id,
            tokens,
            cost_usd: recorded,
            ..
        } = &e.kind
        {
            cost_usd += recorded.unwrap_or_else(|| {
                let (model, role) = run_models
                    .get(run_id.as_str())
                    .copied()
                    .unwrap_or(("", crate::types::Role::Worker));
                let backend = config
                    .map(|c| c.backend_kind(role))
                    .unwrap_or(crate::types::BackendKind::Claude);
                crate::cost::usage_cost_usd_for_backend(tokens, model, backend)
            });
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

    MissionOutcomes {
        interventions,
        is_closed,
        latencies_ms,
        escalations,
        cost_usd,
        non_meta_commits,
        cycle_time_ms,
    }
}

/// Enumerate every mission under `repo_root` exactly as
/// [`crate::orchestrator`]'s REST-layer callers do — union
/// [`crate::paths::MissionPaths::list_missions`] with the ids recorded in
/// `.kranz/missions/index.md` — fold each mission's outcomes, and aggregate.
/// A mission with no `events.jsonl` or an unreadable/corrupt log is skipped
/// (degrade per-row); this never panics or fails the whole aggregate.
pub fn compute_outcomes(repo_root: &std::path::Path) -> anyhow::Result<Outcomes> {
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
        if out.is_closed {
            closed_missions += 1;
            total_interventions += out.interventions;
            if out.interventions == 0 {
                zero_intervention_missions += 1;
            }
        }
        all_latencies_ms.extend(out.latencies_ms);
        escalations.extend(out.escalations);
        total_cost_usd += out.cost_usd;
        total_non_meta_commits += out.non_meta_commits;
        if let Some(ms) = out.cycle_time_ms {
            cycle_closed += 1;
            cycle_total_ms += ms;
        }
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
    }
}
