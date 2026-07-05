//! Pure, deterministic event-to-span mapping primitives.
//!
//! No OTLP or network types leak in here — this module is unit-testable in
//! isolation.

use chrono::{DateTime, Utc};
use kranz_engine::events::{Event, EventKind};
use kranz_engine::types::TokenUsage;
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// A single OTLP-agnostic span attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    String(String),
    I64(i64),
    F64(f64),
}

/// Span outcome, independent of any OTLP status code encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpanStatus {
    Unset,
    Ok,
    Error(String),
}

/// A neutral, transport-agnostic span record.
///
/// Attribute ordering is caller-determined and preserved (a `Vec`, not a
/// map) so mapping code can emit attributes in a deterministic order.
#[derive(Debug, Clone, PartialEq)]
pub struct MissionSpan {
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub parent_span_id: Option<[u8; 8]>,
    pub name: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub attributes: Vec<(String, AttrValue)>,
    pub status: SpanStatus,
}

/// Deterministic trace id for a mission: first 16 bytes of sha256(mission_id).
pub fn trace_id(mission_id: &str) -> [u8; 16] {
    let digest = Sha256::digest(mission_id.as_bytes());
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// Deterministic span id for a mission/opening-seq pair: first 8 bytes of
/// sha256("{mission_id}:{open_seq}"), where `open_seq` is the seq of the
/// span's opening event (mission.created / milestone.started /
/// worker.spawned).
pub fn span_id(mission_id: &str, open_seq: u64) -> [u8; 8] {
    let digest = Sha256::digest(format!("{mission_id}:{open_seq}").as_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&digest[..8]);
    out
}

fn tokens_attrs(tokens: &TokenUsage) -> [(String, AttrValue); 4] {
    [
        ("kranz.tokens.input".to_string(), AttrValue::I64(tokens.input as i64)),
        ("kranz.tokens.output".to_string(), AttrValue::I64(tokens.output as i64)),
        ("kranz.tokens.cache_read".to_string(), AttrValue::I64(tokens.cache_read as i64)),
        ("kranz.tokens.cache_write".to_string(), AttrValue::I64(tokens.cache_write as i64)),
    ]
}

/// Tracks a still-open (not-yet-closed) milestone span.
struct OpenMilestone {
    open_seq: u64,
    start: DateTime<Utc>,
    title: String,
    fix_cycles: u32,
    validating: bool,
}

/// Tracks a still-open (not-yet-closed) run span.
struct OpenRun {
    open_seq: u64,
    start: DateTime<Utc>,
    role: kranz_engine::types::Role,
    model: String,
    feature_id: Option<String>,
    milestone_id: Option<String>,
}

/// Fold a mission's event log into its finished spans (root, milestones,
/// runs). Pure and deterministic: every timestamp comes from the events'
/// `ts` fields, never from wall-clock time. A span is emitted only when its
/// closing event is present in `events` — mirroring OTel, which exports on
/// span end.
pub fn map_mission(events: &[Event]) -> Vec<MissionSpan> {
    let mut spans = Vec::new();

    let mission_id = match events.first() {
        Some(e) => e.mission_id.clone(),
        None => return spans,
    };

    let mut goal = String::new();
    let mut created_seq: Option<u64> = None;
    let mut created_ts: Option<DateTime<Utc>> = None;

    // milestone_id -> title, populated from plan.approved when present.
    let mut milestone_titles: HashMap<String, String> = HashMap::new();
    let mut open_milestones: HashMap<String, OpenMilestone> = HashMap::new();
    let mut open_runs: HashMap<String, OpenRun> = HashMap::new();

    let mut total_tokens = TokenUsage::default();
    let mut total_cost = 0.0_f64;

    for event in events {
        match &event.kind {
            EventKind::MissionCreated { goal: g, .. } => {
                goal = g.clone();
                created_seq = Some(event.seq);
                created_ts = Some(event.ts);
            }

            EventKind::PlanApproved { plan, .. } => {
                for (mi, pm) in plan.milestones.iter().enumerate() {
                    milestone_titles.insert(format!("ms-{}", mi + 1), pm.title.clone());
                }
            }

            EventKind::MilestoneStarted { milestone_id, .. } => {
                let title =
                    milestone_titles.get(milestone_id).cloned().unwrap_or_else(|| milestone_id.clone());
                open_milestones.insert(
                    milestone_id.clone(),
                    OpenMilestone {
                        open_seq: event.seq,
                        start: event.ts,
                        title,
                        fix_cycles: 0,
                        validating: false,
                    },
                );
            }

            EventKind::MilestoneValidating { milestone_id } => {
                if let Some(open) = open_milestones.get_mut(milestone_id) {
                    open.validating = true;
                }
            }

            EventKind::FixFeatureCreated { milestone_id, .. } => {
                if let Some(open) = open_milestones.get_mut(milestone_id) {
                    if open.validating {
                        open.fix_cycles += 1;
                        open.validating = false;
                    }
                }
            }

            EventKind::WorkerSpawned { run_id, role, feature_id, milestone_id, model, .. } => {
                open_runs.insert(
                    run_id.clone(),
                    OpenRun {
                        open_seq: event.seq,
                        start: event.ts,
                        role: *role,
                        model: model.clone(),
                        feature_id: feature_id.clone(),
                        milestone_id: milestone_id.clone(),
                    },
                );
            }

            EventKind::WorkerCompleted { run_id, result, tokens, cost_usd, .. } => {
                total_tokens.add(tokens);
                if let Some(c) = cost_usd {
                    total_cost += c;
                }

                if let Some(open) = open_runs.remove(run_id) {
                    let parent = if let Some(mid) = &open.milestone_id {
                        open_milestones
                            .get(mid)
                            .map(|m| span_id(&mission_id, m.open_seq))
                            .or_else(|| root_span_id(&mission_id, created_seq))
                    } else if let Some(fid) = &open.feature_id {
                        milestone_id_from_feature_id(fid)
                            .and_then(|mid| open_milestones.get(&mid))
                            .map(|m| span_id(&mission_id, m.open_seq))
                            .or_else(|| root_span_id(&mission_id, created_seq))
                    } else {
                        root_span_id(&mission_id, created_seq)
                    };

                    let (status, result_str) = match result {
                        kranz_engine::types::RunResult::Pass => (SpanStatus::Ok, "pass"),
                        kranz_engine::types::RunResult::Fail => {
                            (SpanStatus::Error("fail".to_string()), "fail")
                        }
                        kranz_engine::types::RunResult::Partial => {
                            (SpanStatus::Error("partial".to_string()), "partial")
                        }
                    };

                    let mut attrs = vec![
                        ("kranz.run.id".to_string(), AttrValue::String(run_id.clone())),
                        ("kranz.role".to_string(), AttrValue::String(role_str(open.role).to_string())),
                        ("kranz.model".to_string(), AttrValue::String(open.model.clone())),
                        ("kranz.run.result".to_string(), AttrValue::String(result_str.to_string())),
                    ];
                    if let Some(c) = cost_usd {
                        attrs.push(("kranz.cost.usd".to_string(), AttrValue::F64(*c)));
                    }
                    attrs.extend(tokens_attrs(tokens));
                    if let Some(fid) = &open.feature_id {
                        attrs.push(("kranz.feature.id".to_string(), AttrValue::String(fid.clone())));
                    }
                    if let Some(mid) = &open.milestone_id {
                        attrs.push(("kranz.milestone.id".to_string(), AttrValue::String(mid.clone())));
                    }

                    spans.push(MissionSpan {
                        trace_id: trace_id(&mission_id),
                        span_id: span_id(&mission_id, open.open_seq),
                        parent_span_id: parent,
                        name: format!("{} {}", role_str(open.role), run_id),
                        start: open.start,
                        end: event.ts,
                        attributes: attrs,
                        status,
                    });
                }
            }

            EventKind::MilestoneCompleted { milestone_id, .. } => {
                if let Some(open) = open_milestones.remove(milestone_id) {
                    spans.push(finished_milestone_span(
                        &mission_id,
                        milestone_id,
                        &open,
                        event.ts,
                        SpanStatus::Ok,
                        created_seq,
                    ));
                }
            }

            EventKind::MilestoneBlocked { milestone_id, reason } => {
                if let Some(open) = open_milestones.remove(milestone_id) {
                    spans.push(finished_milestone_span(
                        &mission_id,
                        milestone_id,
                        &open,
                        event.ts,
                        SpanStatus::Error(reason.clone()),
                        created_seq,
                    ));
                }
            }

            EventKind::MissionCompleted {} => {
                if let (Some(seq), Some(start)) = (created_seq, created_ts) {
                    spans.push(finished_root_span(
                        &mission_id,
                        &goal,
                        seq,
                        start,
                        event.ts,
                        SpanStatus::Ok,
                        "complete",
                        &total_tokens,
                        total_cost,
                    ));
                }
            }

            EventKind::MissionFailed { reason } => {
                if let (Some(seq), Some(start)) = (created_seq, created_ts) {
                    spans.push(finished_root_span(
                        &mission_id,
                        &goal,
                        seq,
                        start,
                        event.ts,
                        SpanStatus::Error(reason.clone()),
                        "failed",
                        &total_tokens,
                        total_cost,
                    ));
                }
            }

            EventKind::MissionAbandoned { reason } => {
                if let (Some(seq), Some(start)) = (created_seq, created_ts) {
                    spans.push(finished_root_span(
                        &mission_id,
                        &goal,
                        seq,
                        start,
                        event.ts,
                        SpanStatus::Error(reason.clone()),
                        "abandoned",
                        &total_tokens,
                        total_cost,
                    ));
                }
            }

            _ => {}
        }
    }

    spans
}

/// The root span id, if `mission.created` is among the events this fold saw.
///
/// A live tail may start mid-mission and observe a milestone or run's full
/// open/close pair without ever having seen `mission.created` (it happened
/// before the tail began) — such a span still gets exported, just parentless
/// rather than panicking.
fn root_span_id(mission_id: &str, created_seq: Option<u64>) -> Option<[u8; 8]> {
    created_seq.map(|seq| span_id(mission_id, seq))
}

fn role_str(role: kranz_engine::types::Role) -> &'static str {
    use kranz_engine::types::Role::*;
    match role {
        Orchestrator => "orchestrator",
        Worker => "worker",
        ValidatorScrutiny => "validator-scrutiny",
        ValidatorFunctional => "validator-functional",
    }
}

/// `f-<m>-<f>` -> `ms-<m>`.
fn milestone_id_from_feature_id(feature_id: &str) -> Option<String> {
    let rest = feature_id.strip_prefix("f-")?;
    let m = rest.split('-').next()?;
    Some(format!("ms-{m}"))
}

fn finished_milestone_span(
    mission_id: &str,
    milestone_id: &str,
    open: &OpenMilestone,
    end: DateTime<Utc>,
    status: SpanStatus,
    created_seq: Option<u64>,
) -> MissionSpan {
    let attrs = vec![
        ("kranz.milestone.id".to_string(), AttrValue::String(milestone_id.to_string())),
        ("kranz.milestone.title".to_string(), AttrValue::String(open.title.clone())),
        (
            "kranz.milestone.status".to_string(),
            AttrValue::String(if status == SpanStatus::Ok { "complete".to_string() } else { "blocked".to_string() }),
        ),
        ("kranz.milestone.fix_cycles".to_string(), AttrValue::I64(open.fix_cycles as i64)),
    ];

    MissionSpan {
        trace_id: trace_id(mission_id),
        span_id: span_id(mission_id, open.open_seq),
        parent_span_id: root_span_id(mission_id, created_seq),
        name: format!("milestone {milestone_id}: {}", open.title),
        start: open.start,
        end,
        attributes: attrs,
        status,
    }
}

#[allow(clippy::too_many_arguments)]
fn finished_root_span(
    mission_id: &str,
    goal: &str,
    created_seq: u64,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    status: SpanStatus,
    status_str: &str,
    tokens: &TokenUsage,
    cost: f64,
) -> MissionSpan {
    let mut attrs = vec![
        ("kranz.mission.id".to_string(), AttrValue::String(mission_id.to_string())),
        ("kranz.mission.goal".to_string(), AttrValue::String(goal.to_string())),
        ("kranz.mission.status".to_string(), AttrValue::String(status_str.to_string())),
        ("kranz.cost.usd".to_string(), AttrValue::F64(cost)),
    ];
    attrs.extend(tokens_attrs(tokens));

    MissionSpan {
        trace_id: trace_id(mission_id),
        span_id: span_id(mission_id, created_seq),
        parent_span_id: None,
        name: format!("mission {mission_id}"),
        start,
        end,
        attributes: attrs,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_deterministic_and_idempotent() {
        let t1 = trace_id("m-01");
        let t2 = trace_id("m-01");
        assert_eq!(t1, t2, "trace_id must be idempotent for the same mission_id");

        // Pin the exact derivation: first 16 bytes of sha256(mission_id),
        // computed independently of `trace_id` via sha2 directly.
        let expected_trace: [u8; 16] = {
            let digest = Sha256::digest(b"m-01");
            digest[..16].try_into().unwrap()
        };
        assert_eq!(t1, expected_trace);

        // And against a hardcoded hex vector (`printf 'm-01' | shasum -a
        // 256` => 71677172fe9630d25271b72337b5caa4...), so the test also
        // guards against the hash algorithm itself changing.
        let expected_trace_hex: [u8; 16] = [
            0x71, 0x67, 0x71, 0x72, 0xfe, 0x96, 0x30, 0xd2, 0x52, 0x71, 0xb7, 0x23, 0x37, 0xb5,
            0xca, 0xa4,
        ];
        assert_eq!(t1, expected_trace_hex);

        let s1 = span_id("m-01", 42);
        let s2 = span_id("m-01", 42);
        assert_eq!(s1, s2, "span_id must be idempotent for the same (mission_id, seq)");

        // Pin span_id to first 8 bytes of sha256("{mission_id}:{seq}"),
        // independently computed via sha2 directly.
        let expected_span: [u8; 8] = {
            let digest = Sha256::digest(b"m-01:42");
            digest[..8].try_into().unwrap()
        };
        assert_eq!(s1, expected_span);

        let t_other = trace_id("m-02");
        assert_ne!(t1, t_other, "distinct missions must get distinct trace ids");

        let s_other_seq = span_id("m-01", 43);
        assert_ne!(s1, s_other_seq, "distinct seqs must get distinct span ids");

        let s_other_mission = span_id("m-02", 42);
        assert_ne!(s1, s_other_mission, "distinct missions must get distinct span ids even with the same seq");
    }

    use kranz_engine::types::{MissionConfig, Role, RunResult, TokenUsage};
    use chrono::TimeZone;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn ev(seq: u64, secs: i64, kind: EventKind) -> Event {
        Event { seq, ts: ts(secs), mission_id: "m-01".to_string(), kind }
    }

    fn created(seq: u64, secs: i64) -> Event {
        ev(
            seq,
            secs,
            EventKind::MissionCreated {
                goal: "ship the thing".to_string(),
                base_branch: "main".to_string(),
                mission_branch: "kranz/mission-m-01".to_string(),
                config: MissionConfig::default(),
            },
        )
    }

    fn spawned(
        seq: u64,
        secs: i64,
        run_id: &str,
        role: Role,
        feature_id: Option<&str>,
        milestone_id: Option<&str>,
        model: &str,
    ) -> Event {
        ev(
            seq,
            secs,
            EventKind::WorkerSpawned {
                run_id: run_id.to_string(),
                role,
                feature_id: feature_id.map(|s| s.to_string()),
                milestone_id: milestone_id.map(|s| s.to_string()),
                sdk_session_id: "sdk-1".to_string(),
                model: model.to_string(),
                prompt_hash: "hash".to_string(),
                transcript_path: "path".to_string(),
            },
        )
    }

    fn completed(seq: u64, secs: i64, run_id: &str, result: RunResult, tokens: TokenUsage, cost_usd: Option<f64>) -> Event {
        ev(
            seq,
            secs,
            EventKind::WorkerCompleted { run_id: run_id.to_string(), result, tokens, cost_usd, report: None },
        )
    }

    fn milestone_started(seq: u64, secs: i64, milestone_id: &str) -> Event {
        ev(
            seq,
            secs,
            EventKind::MilestoneStarted { milestone_id: milestone_id.to_string(), start_sha: "abc123".to_string() },
        )
    }

    fn milestone_completed(seq: u64, secs: i64, milestone_id: &str) -> Event {
        ev(seq, secs, EventKind::MilestoneCompleted { milestone_id: milestone_id.to_string(), tag: None })
    }

    fn sample_tokens() -> TokenUsage {
        TokenUsage { input: 100, output: 50, cache_read: 10, cache_write: 5 }
    }

    #[test]
    fn mission_maps_to_trace_root() {
        let events = vec![
            created(1, 0),
            milestone_started(2, 10, "ms-1"),
            spawned(3, 20, "run-1", Role::Worker, Some("f-1-1"), None, "sonnet"),
            completed(4, 30, "run-1", RunResult::Pass, sample_tokens(), Some(0.5)),
            milestone_completed(5, 40, "ms-1"),
            ev(6, 50, EventKind::MissionCompleted {}),
        ];

        let spans = map_mission(&events);
        let roots: Vec<_> = spans.iter().filter(|s| s.parent_span_id.is_none()).collect();
        assert_eq!(roots.len(), 1, "expected exactly one root span, got: {spans:#?}");

        let root = roots[0];
        assert_eq!(root.trace_id, trace_id("m-01"));
        assert_eq!(root.start, ts(0));
        assert_eq!(root.end, ts(50));
        assert_eq!(root.status, SpanStatus::Ok);
    }

    #[test]
    fn mission_without_terminal_event_emits_no_root() {
        let events = vec![created(1, 0), milestone_started(2, 10, "ms-1")];
        let spans = map_mission(&events);
        assert!(spans.iter().all(|s| s.parent_span_id.is_some()), "no root span should be emitted for a still-running mission");
    }

    #[test]
    fn run_span_carries_cost_and_token_attributes() {
        let events = vec![
            created(1, 0),
            spawned(2, 10, "run-1", Role::Worker, None, None, "sonnet"),
            completed(3, 20, "run-1", RunResult::Pass, sample_tokens(), Some(1.25)),
        ];

        let spans = map_mission(&events);
        let run_span = spans.iter().find(|s| s.name.contains("run-1")).expect("run span expected");

        assert_eq!(
            run_span.attributes.iter().find(|(k, _)| k == "kranz.cost.usd").map(|(_, v)| v.clone()),
            Some(AttrValue::F64(1.25))
        );
        assert_eq!(
            run_span.attributes.iter().find(|(k, _)| k == "kranz.tokens.input").map(|(_, v)| v.clone()),
            Some(AttrValue::I64(100))
        );
        assert_eq!(
            run_span.attributes.iter().find(|(k, _)| k == "kranz.tokens.output").map(|(_, v)| v.clone()),
            Some(AttrValue::I64(50))
        );
        assert_eq!(
            run_span.attributes.iter().find(|(k, _)| k == "kranz.tokens.cache_read").map(|(_, v)| v.clone()),
            Some(AttrValue::I64(10))
        );
        assert_eq!(
            run_span.attributes.iter().find(|(k, _)| k == "kranz.tokens.cache_write").map(|(_, v)| v.clone()),
            Some(AttrValue::I64(5))
        );
        assert_eq!(
            run_span.attributes.iter().find(|(k, _)| k == "kranz.role").map(|(_, v)| v.clone()),
            Some(AttrValue::String("worker".to_string()))
        );
        assert_eq!(
            run_span.attributes.iter().find(|(k, _)| k == "kranz.model").map(|(_, v)| v.clone()),
            Some(AttrValue::String("sonnet".to_string()))
        );
    }

    #[test]
    fn terminal_status_maps_to_span_status() {
        // Run statuses.
        for (result, expected) in [
            (RunResult::Pass, SpanStatus::Ok),
            (RunResult::Fail, SpanStatus::Error("fail".to_string())),
            (RunResult::Partial, SpanStatus::Error("partial".to_string())),
        ] {
            let events = vec![
                created(1, 0),
                spawned(2, 10, "run-1", Role::Worker, None, None, "sonnet"),
                completed(3, 20, "run-1", result, sample_tokens(), None),
            ];
            let spans = map_mission(&events);
            let run_span = spans.iter().find(|s| s.name.contains("run-1")).unwrap();
            assert_eq!(run_span.status, expected, "result {result:?} should map to {expected:?}");
        }

        // Mission statuses.
        let complete = vec![created(1, 0), ev(2, 10, EventKind::MissionCompleted {})];
        let root = map_mission(&complete).into_iter().find(|s| s.parent_span_id.is_none()).unwrap();
        assert_eq!(root.status, SpanStatus::Ok);

        let failed = vec![created(1, 0), ev(2, 10, EventKind::MissionFailed { reason: "boom".to_string() })];
        let root = map_mission(&failed).into_iter().find(|s| s.parent_span_id.is_none()).unwrap();
        assert_eq!(root.status, SpanStatus::Error("boom".to_string()));

        let abandoned = vec![created(1, 0), ev(2, 10, EventKind::MissionAbandoned { reason: "retired".to_string() })];
        let root = map_mission(&abandoned).into_iter().find(|s| s.parent_span_id.is_none()).unwrap();
        assert_eq!(root.status, SpanStatus::Error("retired".to_string()));

        // Milestone statuses.
        let ms_complete = vec![created(1, 0), milestone_started(2, 10, "ms-1"), milestone_completed(3, 20, "ms-1")];
        let ms_span = map_mission(&ms_complete).into_iter().find(|s| s.name.contains("ms-1")).unwrap();
        assert_eq!(ms_span.status, SpanStatus::Ok);

        let ms_blocked = vec![
            created(1, 0),
            milestone_started(2, 10, "ms-1"),
            ev(3, 20, EventKind::MilestoneBlocked { milestone_id: "ms-1".to_string(), reason: "too many fix cycles".to_string() }),
        ];
        let ms_span = map_mission(&ms_blocked).into_iter().find(|s| s.name.contains("ms-1")).unwrap();
        assert_eq!(ms_span.status, SpanStatus::Error("too many fix cycles".to_string()));
    }

    #[test]
    fn spans_parent_correctly() {
        let events = vec![
            created(1, 0),
            // orchestrator run — parents to root.
            spawned(2, 5, "orch-1", Role::Orchestrator, None, None, "opus"),
            completed(3, 6, "orch-1", RunResult::Pass, sample_tokens(), None),
            // milestone ms-1, and a milestone-scoped validator run.
            milestone_started(4, 10, "ms-1"),
            spawned(5, 15, "val-1", Role::ValidatorFunctional, None, Some("ms-1"), "sonnet"),
            completed(6, 16, "val-1", RunResult::Pass, sample_tokens(), None),
            // milestone ms-2, and a worker run parented via featureId f-2-1.
            milestone_started(7, 20, "ms-2"),
            spawned(8, 25, "run-2-1", Role::Worker, Some("f-2-1"), None, "sonnet"),
            completed(9, 26, "run-2-1", RunResult::Pass, sample_tokens(), None),
            milestone_completed(10, 30, "ms-1"),
            milestone_completed(11, 31, "ms-2"),
            ev(12, 40, EventKind::MissionCompleted {}),
        ];

        let spans = map_mission(&events);
        let root = spans.iter().find(|s| s.parent_span_id.is_none()).expect("root span");
        let ms1 = spans.iter().find(|s| s.name.contains("ms-1")).expect("ms-1 span");
        let ms2 = spans.iter().find(|s| s.name.contains("ms-2")).expect("ms-2 span");
        let orch = spans.iter().find(|s| s.name.contains("orch-1")).expect("orch span");
        let val = spans.iter().find(|s| s.name.contains("val-1")).expect("val span");
        let run2 = spans.iter().find(|s| s.name.contains("run-2-1")).expect("run-2-1 span");

        assert_eq!(orch.parent_span_id, Some(root.span_id), "orchestrator run parents to root");
        assert_eq!(ms1.parent_span_id, Some(root.span_id), "milestone parents to root");
        assert_eq!(ms2.parent_span_id, Some(root.span_id), "milestone parents to root");
        assert_eq!(val.parent_span_id, Some(ms1.span_id), "validator run with milestoneId parents to that milestone");
        assert_eq!(run2.parent_span_id, Some(ms2.span_id), "run with featureId f-2-1 parents to ms-2");
    }

    #[test]
    fn replay_uses_event_timestamps() {
        let events = vec![
            created(1, 1000),
            milestone_started(2, 1010, "ms-1"),
            spawned(3, 1020, "run-1", Role::Worker, Some("f-1-1"), None, "sonnet"),
            completed(4, 1030, "run-1", RunResult::Pass, sample_tokens(), Some(0.1)),
            milestone_completed(5, 1040, "ms-1"),
            ev(6, 1050, EventKind::MissionCompleted {}),
        ];

        let spans = map_mission(&events);
        assert_eq!(spans.len(), 3, "expected root, milestone, and run spans");

        let root = spans.iter().find(|s| s.parent_span_id.is_none()).unwrap();
        assert_eq!(root.start, ts(1000));
        assert_eq!(root.end, ts(1050));

        let ms = spans.iter().find(|s| s.name.contains("ms-1")).unwrap();
        assert_eq!(ms.start, ts(1010));
        assert_eq!(ms.end, ts(1040));

        let run = spans.iter().find(|s| s.name.contains("run-1")).unwrap();
        assert_eq!(run.start, ts(1020));
        assert_eq!(run.end, ts(1030));
    }
}
