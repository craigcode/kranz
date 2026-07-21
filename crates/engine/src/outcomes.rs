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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissionOutcomes {
    pub interventions: u64,
    pub is_closed: bool,
    pub latencies_ms: Vec<u64>,
    pub escalations: Vec<EscalationRow>,
}

/// The four fixed grant-latency bucket labels, in display order.
const BUCKET_LABELS: [&str; 4] = ["<10s", "<60s", "<10m", ">=10m"];

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

    MissionOutcomes {
        interventions,
        is_closed,
        latencies_ms,
        escalations,
    }
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
}
