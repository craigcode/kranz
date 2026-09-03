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
        (
            "kranz.tokens.input".to_string(),
            AttrValue::I64(tokens.input as i64),
        ),
        (
            "kranz.tokens.output".to_string(),
            AttrValue::I64(tokens.output as i64),
        ),
        (
            "kranz.tokens.cache_read".to_string(),
            AttrValue::I64(tokens.cache_read as i64),
        ),
        (
            "kranz.tokens.cache_write".to_string(),
            AttrValue::I64(tokens.cache_write as i64),
        ),
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

/// Cap on any free-text string this module turns into a span attribute or a
/// span name. Goals, milestone titles, and failure reasons are agent- or
/// repo-authored and otherwise unbounded.
pub const OTEL_TEXT_MAX_CHARS: usize = 512;

/// Redact and bound one piece of free text before it becomes an attribute.
///
/// `EventLog::append_redacting` scrubs at write time, but the export's read
/// path does not get to assume that writer: `otel::run` enumerates
/// `<repo>/.kranz/missions/` with a bare `read_dir` and no provenance check,
/// so a repo that commits a hand-written `events.jsonl` gets its strings
/// into the operator's observability backend verbatim. Scrub here too, and
/// cap the length while we are at it — an OTLP exporter is not the place to
/// discover a megabyte attribute.
fn clean_text(text: &str) -> String {
    kranz_engine::scrub::truncate_chars(&kranz_engine::scrub::scrub(text), OTEL_TEXT_MAX_CHARS)
}

/// Cap on an IDENTIFIER attribute or span name (mission/run/feature/
/// milestone ids). Ids are short by construction; a long one is a
/// hand-written log getting creative.
pub const OTEL_ID_MAX_CHARS: usize = 128;

/// Redact and bound one identifier before it becomes an attribute or part of
/// a span name.
///
/// Threat (follow-up review M-14): the module claimed every free-text field
/// crossed [`clean_text`], and the ids did not. `otel::run` enumerates
/// `<repo>/.kranz/missions/` with a bare `read_dir` and no provenance check,
/// so a repo that commits a hand-written `events.jsonl` shipped its `runId`,
/// `featureId`, `milestoneId` and mission id into the operator's
/// observability backend verbatim and unbounded. Ids get the same scrub as
/// prose with a tighter cap.
///
/// Deliberately NOT applied to `trace_id`/`span_id`: those hash the RAW id,
/// and cleaning one side of that derivation would break parent linkage
/// against every span already exported.
fn clean_id(id: &str) -> String {
    kranz_engine::scrub::truncate_chars(&kranz_engine::scrub::scrub(id), OTEL_ID_MAX_CHARS)
}

/// Fold a mission's event log into its finished spans (root, milestones,
/// runs). Pure and deterministic: every timestamp comes from the events'
/// `ts` fields, never from wall-clock time. A span is emitted only when its
/// closing event is present in `events` — mirroring OTel, which exports on
/// span end.
///
/// Every free-text field the log carries crosses [`clean_text`], and every
/// identifier crosses [`clean_id`], on the way in, so no caller can build an
/// unscrubbed or unbounded attribute or span name.
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
                goal = clean_text(g);
                created_seq = Some(event.seq);
                created_ts = Some(event.ts);
            }

            EventKind::PlanApproved { plan, .. } => {
                for (mi, pm) in plan.milestones.iter().enumerate() {
                    milestone_titles.insert(format!("ms-{}", mi + 1), clean_text(&pm.title));
                }
            }

            EventKind::MilestoneStarted { milestone_id, .. } => {
                let title = milestone_titles
                    .get(milestone_id)
                    .cloned()
                    .unwrap_or_else(|| clean_id(milestone_id));
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

            EventKind::WorkerSpawned {
                run_id,
                role,
                feature_id,
                milestone_id,
                model,
                ..
            } => {
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

            EventKind::WorkerCompleted {
                run_id,
                result,
                tokens,
                cost_usd,
                ..
            } => {
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
                        (
                            "kranz.run.id".to_string(),
                            AttrValue::String(clean_id(run_id)),
                        ),
                        (
                            "kranz.role".to_string(),
                            AttrValue::String(role_str(open.role).to_string()),
                        ),
                        (
                            "kranz.model".to_string(),
                            AttrValue::String(clean_text(&open.model)),
                        ),
                        (
                            "kranz.run.result".to_string(),
                            AttrValue::String(result_str.to_string()),
                        ),
                    ];
                    if let Some(c) = cost_usd {
                        attrs.push(("kranz.cost.usd".to_string(), AttrValue::F64(*c)));
                    }
                    attrs.extend(tokens_attrs(tokens));
                    if let Some(fid) = &open.feature_id {
                        attrs.push((
                            "kranz.feature.id".to_string(),
                            AttrValue::String(clean_id(fid)),
                        ));
                    }
                    if let Some(mid) = &open.milestone_id {
                        attrs.push((
                            "kranz.milestone.id".to_string(),
                            AttrValue::String(clean_id(mid)),
                        ));
                    }

                    spans.push(MissionSpan {
                        trace_id: trace_id(&mission_id),
                        span_id: span_id(&mission_id, open.open_seq),
                        parent_span_id: parent,
                        name: format!("{} {}", role_str(open.role), clean_id(run_id)),
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

            EventKind::MilestoneBlocked {
                milestone_id,
                reason,
            } => {
                if let Some(open) = open_milestones.remove(milestone_id) {
                    spans.push(finished_milestone_span(
                        &mission_id,
                        milestone_id,
                        &open,
                        event.ts,
                        SpanStatus::Error(clean_text(reason)),
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
                        SpanStatus::Error(clean_text(reason)),
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
                        SpanStatus::Error(clean_text(reason)),
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
        (
            "kranz.milestone.id".to_string(),
            AttrValue::String(clean_id(milestone_id)),
        ),
        (
            "kranz.milestone.title".to_string(),
            AttrValue::String(open.title.clone()),
        ),
        (
            "kranz.milestone.status".to_string(),
            AttrValue::String(if status == SpanStatus::Ok {
                "complete".to_string()
            } else {
                "blocked".to_string()
            }),
        ),
        (
            "kranz.milestone.fix_cycles".to_string(),
            AttrValue::I64(open.fix_cycles as i64),
        ),
    ];

    MissionSpan {
        trace_id: trace_id(mission_id),
        span_id: span_id(mission_id, open.open_seq),
        parent_span_id: root_span_id(mission_id, created_seq),
        name: format!("milestone {}: {}", clean_id(milestone_id), open.title),
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
        (
            "kranz.mission.id".to_string(),
            AttrValue::String(clean_id(mission_id)),
        ),
        (
            "kranz.mission.goal".to_string(),
            AttrValue::String(goal.to_string()),
        ),
        (
            "kranz.mission.status".to_string(),
            AttrValue::String(status_str.to_string()),
        ),
        ("kranz.cost.usd".to_string(), AttrValue::F64(cost)),
    ];
    attrs.extend(tokens_attrs(tokens));

    MissionSpan {
        trace_id: trace_id(mission_id),
        span_id: span_id(mission_id, created_seq),
        parent_span_id: None,
        name: format!("mission {}", clean_id(mission_id)),
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
        assert_eq!(
            t1, t2,
            "trace_id must be idempotent for the same mission_id"
        );

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
        assert_eq!(
            s1, s2,
            "span_id must be idempotent for the same (mission_id, seq)"
        );

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
        assert_ne!(
            s1, s_other_mission,
            "distinct missions must get distinct span ids even with the same seq"
        );
    }

    use chrono::TimeZone;
    use kranz_engine::types::{MissionConfig, Role, RunResult, TokenUsage};

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn ev(seq: u64, secs: i64, kind: EventKind) -> Event {
        Event {
            seq,
            ts: ts(secs),
            mission_id: "m-01".to_string(),
            kind,
        }
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
                candidate: None,
                executor_route: None,
                sdk_session_id: "sdk-1".to_string(),
                model: model.to_string(),
                quant: "n/a".to_string(),
                weight_hash: None,
                prompt_hash: "hash".to_string(),
                transcript_path: "path".to_string(),
            },
        )
    }

    fn completed(
        seq: u64,
        secs: i64,
        run_id: &str,
        result: RunResult,
        tokens: TokenUsage,
        cost_usd: Option<f64>,
    ) -> Event {
        ev(
            seq,
            secs,
            EventKind::WorkerCompleted {
                run_id: run_id.to_string(),
                result,
                tokens,
                cost_usd,
                report: None,
            },
        )
    }

    fn milestone_started(seq: u64, secs: i64, milestone_id: &str) -> Event {
        ev(
            seq,
            secs,
            EventKind::MilestoneStarted {
                milestone_id: milestone_id.to_string(),
                start_sha: "abc123".to_string(),
            },
        )
    }

    fn milestone_completed(seq: u64, secs: i64, milestone_id: &str) -> Event {
        ev(
            seq,
            secs,
            EventKind::MilestoneCompleted {
                milestone_id: milestone_id.to_string(),
                tag: None,
            },
        )
    }

    fn sample_tokens() -> TokenUsage {
        TokenUsage {
            input: 100,
            output: 50,
            cache_read: 10,
            cache_write: 5,
        }
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
        let roots: Vec<_> = spans
            .iter()
            .filter(|s| s.parent_span_id.is_none())
            .collect();
        assert_eq!(
            roots.len(),
            1,
            "expected exactly one root span, got: {spans:#?}"
        );

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
        assert!(
            spans.iter().all(|s| s.parent_span_id.is_some()),
            "no root span should be emitted for a still-running mission"
        );
    }

    #[test]
    fn run_span_carries_cost_and_token_attributes() {
        let events = vec![
            created(1, 0),
            spawned(2, 10, "run-1", Role::Worker, None, None, "sonnet"),
            completed(3, 20, "run-1", RunResult::Pass, sample_tokens(), Some(1.25)),
        ];

        let spans = map_mission(&events);
        let run_span = spans
            .iter()
            .find(|s| s.name.contains("run-1"))
            .expect("run span expected");

        assert_eq!(
            run_span
                .attributes
                .iter()
                .find(|(k, _)| k == "kranz.cost.usd")
                .map(|(_, v)| v.clone()),
            Some(AttrValue::F64(1.25))
        );
        assert_eq!(
            run_span
                .attributes
                .iter()
                .find(|(k, _)| k == "kranz.tokens.input")
                .map(|(_, v)| v.clone()),
            Some(AttrValue::I64(100))
        );
        assert_eq!(
            run_span
                .attributes
                .iter()
                .find(|(k, _)| k == "kranz.tokens.output")
                .map(|(_, v)| v.clone()),
            Some(AttrValue::I64(50))
        );
        assert_eq!(
            run_span
                .attributes
                .iter()
                .find(|(k, _)| k == "kranz.tokens.cache_read")
                .map(|(_, v)| v.clone()),
            Some(AttrValue::I64(10))
        );
        assert_eq!(
            run_span
                .attributes
                .iter()
                .find(|(k, _)| k == "kranz.tokens.cache_write")
                .map(|(_, v)| v.clone()),
            Some(AttrValue::I64(5))
        );
        assert_eq!(
            run_span
                .attributes
                .iter()
                .find(|(k, _)| k == "kranz.role")
                .map(|(_, v)| v.clone()),
            Some(AttrValue::String("worker".to_string()))
        );
        assert_eq!(
            run_span
                .attributes
                .iter()
                .find(|(k, _)| k == "kranz.model")
                .map(|(_, v)| v.clone()),
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
            assert_eq!(
                run_span.status, expected,
                "result {result:?} should map to {expected:?}"
            );
        }

        // Mission statuses.
        let complete = vec![created(1, 0), ev(2, 10, EventKind::MissionCompleted {})];
        let root = map_mission(&complete)
            .into_iter()
            .find(|s| s.parent_span_id.is_none())
            .unwrap();
        assert_eq!(root.status, SpanStatus::Ok);

        let failed = vec![
            created(1, 0),
            ev(
                2,
                10,
                EventKind::MissionFailed {
                    reason: "boom".to_string(),
                },
            ),
        ];
        let root = map_mission(&failed)
            .into_iter()
            .find(|s| s.parent_span_id.is_none())
            .unwrap();
        assert_eq!(root.status, SpanStatus::Error("boom".to_string()));

        let abandoned = vec![
            created(1, 0),
            ev(
                2,
                10,
                EventKind::MissionAbandoned {
                    reason: "retired".to_string(),
                },
            ),
        ];
        let root = map_mission(&abandoned)
            .into_iter()
            .find(|s| s.parent_span_id.is_none())
            .unwrap();
        assert_eq!(root.status, SpanStatus::Error("retired".to_string()));

        // Milestone statuses.
        let ms_complete = vec![
            created(1, 0),
            milestone_started(2, 10, "ms-1"),
            milestone_completed(3, 20, "ms-1"),
        ];
        let ms_span = map_mission(&ms_complete)
            .into_iter()
            .find(|s| s.name.contains("ms-1"))
            .unwrap();
        assert_eq!(ms_span.status, SpanStatus::Ok);

        let ms_blocked = vec![
            created(1, 0),
            milestone_started(2, 10, "ms-1"),
            ev(
                3,
                20,
                EventKind::MilestoneBlocked {
                    milestone_id: "ms-1".to_string(),
                    reason: "too many fix cycles".to_string(),
                },
            ),
        ];
        let ms_span = map_mission(&ms_blocked)
            .into_iter()
            .find(|s| s.name.contains("ms-1"))
            .unwrap();
        assert_eq!(
            ms_span.status,
            SpanStatus::Error("too many fix cycles".to_string())
        );
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
            spawned(
                5,
                15,
                "val-1",
                Role::ValidatorFunctional,
                None,
                Some("ms-1"),
                "sonnet",
            ),
            completed(6, 16, "val-1", RunResult::Pass, sample_tokens(), None),
            // milestone ms-2, and a worker run parented via featureId f-2-1.
            milestone_started(7, 20, "ms-2"),
            spawned(
                8,
                25,
                "run-2-1",
                Role::Worker,
                Some("f-2-1"),
                None,
                "sonnet",
            ),
            completed(9, 26, "run-2-1", RunResult::Pass, sample_tokens(), None),
            milestone_completed(10, 30, "ms-1"),
            milestone_completed(11, 31, "ms-2"),
            ev(12, 40, EventKind::MissionCompleted {}),
        ];

        let spans = map_mission(&events);
        let root = spans
            .iter()
            .find(|s| s.parent_span_id.is_none())
            .expect("root span");
        let ms1 = spans
            .iter()
            .find(|s| s.name.contains("ms-1"))
            .expect("ms-1 span");
        let ms2 = spans
            .iter()
            .find(|s| s.name.contains("ms-2"))
            .expect("ms-2 span");
        let orch = spans
            .iter()
            .find(|s| s.name.contains("orch-1"))
            .expect("orch span");
        let val = spans
            .iter()
            .find(|s| s.name.contains("val-1"))
            .expect("val span");
        let run2 = spans
            .iter()
            .find(|s| s.name.contains("run-2-1"))
            .expect("run-2-1 span");

        assert_eq!(
            orch.parent_span_id,
            Some(root.span_id),
            "orchestrator run parents to root"
        );
        assert_eq!(
            ms1.parent_span_id,
            Some(root.span_id),
            "milestone parents to root"
        );
        assert_eq!(
            ms2.parent_span_id,
            Some(root.span_id),
            "milestone parents to root"
        );
        assert_eq!(
            val.parent_span_id,
            Some(ms1.span_id),
            "validator run with milestoneId parents to that milestone"
        );
        assert_eq!(
            run2.parent_span_id,
            Some(ms2.span_id),
            "run with featureId f-2-1 parents to ms-2"
        );
    }

    #[test]
    fn replay_uses_event_timestamps() {
        let events = vec![
            created(1, 1000),
            milestone_started(2, 1010, "ms-1"),
            spawned(
                3,
                1020,
                "run-1",
                Role::Worker,
                Some("f-1-1"),
                None,
                "sonnet",
            ),
            completed(
                4,
                1030,
                "run-1",
                RunResult::Pass,
                sample_tokens(),
                Some(0.1),
            ),
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

    /// Audit (backends MEDIUM): the export reads mission logs off disk with
    /// no provenance check, so goal/title/reason text is untrusted. It
    /// crosses the scrubber and a length cap before it becomes a span
    /// attribute or a span name — the observability backend is not a place
    /// to discover a repo's planted credentials.
    #[test]
    fn otel_mission_text_is_scrubbed_and_bounded() {
        let secret = "sk-ant-F00barBazQuux9_7";
        let long = "g".repeat(OTEL_TEXT_MAX_CHARS + 500);
        let events = vec![
            ev(
                1,
                0,
                EventKind::MissionCreated {
                    goal: format!("api_key={secret} {long}"),
                    base_branch: "main".to_string(),
                    mission_branch: "kranz/mission-m-01".to_string(),
                    config: MissionConfig::default(),
                },
            ),
            ev(
                2,
                10,
                EventKind::MissionFailed {
                    reason: format!("failed with api_key={secret}"),
                },
            ),
        ];

        let root = map_mission(&events)
            .into_iter()
            .find(|s| s.parent_span_id.is_none())
            .unwrap();
        let goal = root
            .attributes
            .iter()
            .find(|(key, _)| key == "kranz.mission.goal")
            .map(|(_, value)| value.clone())
            .unwrap();
        let AttrValue::String(goal) = goal else {
            panic!("goal is a string attribute");
        };
        assert!(!goal.contains(secret), "goal leaked a secret: {goal}");
        assert!(goal.contains("[REDACTED]"));
        assert!(
            goal.chars().count() <= OTEL_TEXT_MAX_CHARS + 32,
            "goal is unbounded: {} chars",
            goal.chars().count()
        );
        match &root.status {
            SpanStatus::Error(reason) => {
                assert!(!reason.contains(secret), "reason leaked a secret: {reason}");
                assert!(reason.contains("[REDACTED]"));
            }
            other => panic!("expected an error status, got {other:?}"),
        }
    }

    /// M-14 (follow-up review): the module's own claim was that EVERY
    /// free-text field crosses the scrubber, and the ids did not: `run_id`
    /// (attribute and span name), `model`, `feature_id`, `milestone_id` and
    /// the mission id all shipped verbatim and uncapped from a hand-written
    /// `events.jsonl`.
    #[test]
    fn otel_run_ids_model_and_mission_id_are_scrubbed_and_bounded() {
        let secret = "sk-ant-F00barBazQuux9_7";
        let long = "z".repeat(OTEL_ID_MAX_CHARS + 200);
        let run_id = format!("run-{secret}-{long}");
        let events = vec![
            created(1, 0),
            spawned(
                2,
                5,
                &run_id,
                Role::Worker,
                Some(&format!("f-1-1-{secret}")),
                Some(&format!("ms-1-{secret}")),
                &format!("claude-{secret}"),
            ),
            completed(3, 9, &run_id, RunResult::Pass, TokenUsage::default(), None),
        ];

        let run = map_mission(&events)
            .into_iter()
            .find(|s| s.name.starts_with("worker"))
            .expect("a finished run span");

        assert!(
            !run.name.contains(secret),
            "span name leaked a secret: {}",
            run.name
        );
        assert!(
            run.name.chars().count() <= OTEL_ID_MAX_CHARS + 32,
            "span name is unbounded: {} chars",
            run.name.chars().count()
        );

        let attr = |key: &str| {
            run.attributes
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| match v {
                    AttrValue::String(s) => s.clone(),
                    other => panic!("{key} is not a string attribute: {other:?}"),
                })
                .unwrap_or_else(|| panic!("missing attribute {key}"))
        };
        for key in [
            "kranz.run.id",
            "kranz.model",
            "kranz.feature.id",
            "kranz.milestone.id",
        ] {
            let value = attr(key);
            assert!(!value.contains(secret), "{key} leaked a secret: {value}");
            assert!(
                value.contains("[REDACTED]"),
                "{key} was not scrubbed: {value}"
            );
        }
        let id = attr("kranz.run.id");
        assert!(
            id.chars().count() <= OTEL_ID_MAX_CHARS,
            "kranz.run.id is unbounded: {} chars",
            id.chars().count()
        );

        // The mission id rides the root span's attribute and name.
        let secret_mission = format!("m-{secret}");
        let mut events = vec![created(1, 0), ev(2, 10, EventKind::MissionCompleted {})];
        for e in &mut events {
            e.mission_id = secret_mission.clone();
        }
        let root = map_mission(&events)
            .into_iter()
            .find(|s| s.parent_span_id.is_none())
            .expect("a root span");
        assert!(
            !root.name.contains(secret),
            "root name leaked: {}",
            root.name
        );
        let mission_attr = root
            .attributes
            .iter()
            .find(|(k, _)| k == "kranz.mission.id")
            .map(|(_, v)| format!("{v:?}"))
            .unwrap();
        assert!(
            !mission_attr.contains(secret),
            "kranz.mission.id leaked: {mission_attr}"
        );
    }

    /// The milestone title travels the same way, into both the attribute and
    /// the span NAME.
    #[test]
    fn otel_milestone_title_is_scrubbed() {
        let secret = "sk-ant-F00barBazQuux9_7";
        let plan = kranz_engine::types::Plan {
            goal: "ship".to_string(),
            validation_contract: vec![],
            milestones: vec![kranz_engine::types::PlanMilestone {
                title: format!("do it with api_key={secret}"),
                features: vec![],
            }],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
            standards_manifest: None,
        };
        let events = vec![
            created(1, 0),
            ev(
                2,
                5,
                EventKind::PlanApproved {
                    plan,
                    base_sha: None,
                },
            ),
            milestone_started(3, 10, "ms-1"),
            milestone_completed(4, 20, "ms-1"),
        ];

        let ms = map_mission(&events)
            .into_iter()
            .find(|s| s.name.contains("ms-1"))
            .unwrap();
        assert!(!ms.name.contains(secret), "span name leaked: {}", ms.name);
        let title = ms
            .attributes
            .iter()
            .find(|(key, _)| key == "kranz.milestone.title")
            .map(|(_, value)| value.clone())
            .unwrap();
        assert_eq!(
            title,
            AttrValue::String(kranz_engine::scrub::scrub(&format!(
                "do it with api_key={secret}"
            )))
        );
    }
}
