//! Gate score series (ticket `.kranz/tickets/gate-confidence-score.md`,
//! KRZ-315 — the persistence-and-query half of scored gates): the recorded
//! evaluation series for ONE gate identity, folded from every mission log
//! in the repo and surfaced as `kranz gate-scores <gate>` (text or
//! `--json`). The `gate-score-distribution-flags` slice
//! ([`crate::gate_score_flags`], KRZ-316) folds the distribution flags over
//! the same `gate.result` events, sharing this module's extraction
//! discipline and verbatim-score contract.
//!
//! The ticket's contract rules, and where this module stands on them:
//!
//! - **kranz RECORDS, never normalizes.** Score and threshold travel from
//!   the `gate.result` event into the series verbatim — nothing here scales,
//!   buckets, clamps, or reinterprets them; the scale is the gate's to
//!   define ([`crate::gate::GateScore`]). What the gate reported is exactly
//!   what a reader gets back.
//! - **The verdict stays authoritative.** Every point carries `verdict`
//!   verbatim beside the score pair; nothing derives one from the other. A
//!   gate may pass with a low score or fail with a high one — both land in
//!   the series exactly as stated (gate.rs enforces this structurally:
//!   there is no constructor that computes a verdict from a score).
//! - **Absence is the normal case.** Boolean-only gates emit no score, and
//!   the series carries `None` for them — never a zero. A `0.0` would
//!   invent a worst-possible reading the gate never stated; a gate that
//!   says nothing about confidence says NOTHING, and the query surface
//!   renders that honestly (no score column, not a column of zeros).
//! - **Clean-room boundary.** The series vocabulary is the substrate's own
//!   (gate, verdict, score, threshold) — no consumer-specific naming; that
//!   lives in packs and downstream slices (positioning ADR).
//!
//! Pure-fold idiom, mirroring [`crate::escalation_metrics`] and
//! [`crate::provenance`]: [`gate_score_series`] is a pure function over
//! `(mission id, events)` pairs, [`compute_gate_score_series`] the thin
//! read wrapper that enumerates the repo's mission logs. No persisted
//! state, no reads outside the logs, no clock: the series is ordered by
//! event timestamp with (mission id, seq) tie-breaks — a total order folded
//! from the logs alone, so identical logs always yield an identical series.

use crate::events::{Event, EventKind};
use crate::gate::{GateSurface, GateVerdict};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One gate evaluation in the series: the replayable identity of one
/// `gate.result` event carrying the queried gate identity — which mission,
/// which event seq, which surface, the stated verdict, and the
/// gate-supplied score pair verbatim (absent for boolean-only gates).
/// `ts` rides along so the series' chronological order is inspectable and
/// the text surface can say WHEN, like the escalation ledger does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateScorePoint {
    pub mission_id: String,
    pub seq: u64,
    pub ts: DateTime<Utc>,
    /// Which evaluation surface ran the pipeline (approval / final-gate) —
    /// the same gate id is evaluated more than once per mission (events.rs),
    /// so the surface is part of the evaluation's identity.
    pub surface: GateSurface,
    /// The verdict the gate stated — never derived from `score`.
    pub verdict: GateVerdict,
    /// Gate-supplied confidence score, recorded verbatim. `None` for
    /// boolean-only gates — never a zero.
    pub score: Option<f64>,
    /// The threshold the gate judged `score` against; present exactly when
    /// `score` is (the pair travels together from the event).
    pub threshold: Option<f64>,
}

/// The ordered evaluation series for one gate identity across every folded
/// mission log. The gate identity is echoed back so the machine form
/// self-describes (the escalation-metrics/provenance JSON idiom).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateScoreSeries {
    pub gate: String,
    pub evaluations: Vec<GateScorePoint>,
}

/// The pure fold: every `gate.result` event whose gate identity matches
/// `gate`, across all given mission logs, as one ordered series.
///
/// `missions` is `(mission_id, events)` pairs in any order, each mission's
/// events in log (seq) order; events recorded under a DIFFERENT mission id
/// than the log they sit in are filtered out (the
/// [`crate::escalation_metrics::mission_escalation`] discipline — the
/// enumerated mission is the truth, a stray event folds nowhere).
///
/// Ordering: ascending event `ts`, ties broken by (mission id, seq). WHY
/// not seq alone: `seq` is per-mission, so it cannot order a cross-mission
/// series — and WHY the tie-break matters: two missions can append within
/// the same millisecond, and a distribution consumer must never see the
/// order drift with input enumeration. The order is a total function of
/// the logs; no clock, no hash iteration.
pub fn gate_score_series(gate: &str, missions: &[(String, Vec<Event>)]) -> GateScoreSeries {
    let mut evaluations = Vec::new();
    for (mission_id, events) in missions {
        for event in events {
            if event.mission_id != *mission_id {
                continue;
            }
            let EventKind::GateResult {
                gate: event_gate,
                surface,
                verdict,
                score,
                threshold,
                ..
            } = &event.kind
            else {
                continue;
            };
            if event_gate != gate {
                continue;
            }
            evaluations.push(GateScorePoint {
                mission_id: mission_id.clone(),
                seq: event.seq,
                ts: event.ts,
                surface: *surface,
                verdict: *verdict,
                score: *score,
                threshold: *threshold,
            });
        }
    }
    evaluations.sort_by(|a, b| {
        a.ts.cmp(&b.ts)
            .then_with(|| a.mission_id.cmp(&b.mission_id))
            .then_with(|| a.seq.cmp(&b.seq))
    });
    GateScoreSeries {
        gate: gate.to_string(),
        evaluations,
    }
}

/// Enumerate every mission under `repo_root` exactly as
/// [`crate::escalation_metrics::compute_escalation_metrics`] does (union of
/// [`crate::paths::MissionPaths::list_missions`] and the ids in
/// `.kranz/missions/index.md`), read each log, and fold the series for
/// `gate`. A mission with no `events.jsonl` or an unreadable/corrupt log is
/// skipped (degrade per-row); this never panics or fails the whole query.
pub fn compute_gate_score_series(
    repo_root: &std::path::Path,
    gate: &str,
) -> anyhow::Result<GateScoreSeries> {
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

    Ok(gate_score_series(gate, &missions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{EventLog, LockForce};
    use crate::gate::GateKind;
    use crate::paths::MissionPaths;
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

    /// A `gate.result` payload; `score` is the (score, threshold) pair a
    /// scored gate reports, `None` for a boolean-only gate.
    fn gate_result(
        gate: &str,
        surface: GateSurface,
        verdict: GateVerdict,
        score: Option<(f64, f64)>,
    ) -> EventKind {
        EventKind::GateResult {
            gate: gate.to_string(),
            surface,
            kind: GateKind::Deterministic,
            index: 0,
            verdict,
            artefact_ref: format!("contract gate {gate}"),
            artefact_detail: None,
            score: score.map(|(score, _)| score),
            threshold: score.map(|(_, threshold)| threshold),
        }
    }

    /// Replay: a scored gate event folds to the verdict AND the score pair
    /// EXACTLY as stated — kranz records, never normalizes. The verdict is
    /// Fail with a HIGH score (0.99): the series must carry Fail verbatim,
    /// proving nothing derives the verdict from the score at query time
    /// either.
    #[test]
    fn gate_score_series_replays_verdict_score_threshold_verbatim() {
        let events = vec![ev(
            7,
            "m-1",
            1_000,
            gate_result(
                "vacuous-filter",
                GateSurface::FinalGate,
                GateVerdict::Fail,
                Some((0.99, 0.5)),
            ),
        )];
        let series = gate_score_series("vacuous-filter", &[("m-1".to_string(), events)]);
        assert_eq!(series.gate, "vacuous-filter");
        assert_eq!(series.evaluations.len(), 1);
        let point = &series.evaluations[0];
        assert_eq!(point.mission_id, "m-1");
        assert_eq!(point.seq, 7);
        assert_eq!(point.surface, GateSurface::FinalGate);
        assert_eq!(
            point.verdict,
            GateVerdict::Fail,
            "verdict is stated, never derived"
        );
        assert_eq!(point.score, Some(0.99));
        assert_eq!(point.threshold, Some(0.5));
    }

    /// A score-absent event (a boolean-only gate) replays cleanly: the
    /// point carries `None`, never a zero — and the threshold is absent
    /// exactly when the score is (the pair travels together).
    #[test]
    fn gate_score_series_score_absent_event_replays_clean() {
        let events = vec![ev(
            3,
            "m-1",
            1_000,
            gate_result(
                "env-sensitive",
                GateSurface::Approval,
                GateVerdict::Pass,
                None,
            ),
        )];
        let series = gate_score_series("env-sensitive", &[("m-1".to_string(), events)]);
        assert_eq!(series.evaluations.len(), 1);
        let point = &series.evaluations[0];
        assert_eq!(point.verdict, GateVerdict::Pass);
        assert_eq!(point.score, None, "absence is never a zero");
        assert_eq!(point.threshold, None);
    }

    /// Old logs fold: a `gate.result` line written before the score fields
    /// existed (no `score`/`threshold` keys on the wire) deserializes with
    /// serde defaults and folds to `None` — the additive-fields contract
    /// (events.rs) exercised through the series fold, on the wire bytes.
    #[test]
    fn gate_score_series_old_log_lines_without_score_fields_fold() {
        let old_line = r#"{
            "seq": 5,
            "ts": "2026-01-02T03:04:05Z",
            "missionId": "m-1",
            "type": "gate.result",
            "payload": {
                "gate": "vacuous-filter",
                "surface": "approval",
                "kind": "deterministic",
                "index": 0,
                "verdict": "pass",
                "artefactRef": "contract gate vacuous-filter"
            }
        }"#;
        let event: Event = serde_json::from_str(old_line).unwrap();
        let series = gate_score_series("vacuous-filter", &[("m-1".to_string(), vec![event])]);
        assert_eq!(series.evaluations.len(), 1);
        let point = &series.evaluations[0];
        assert_eq!(point.verdict, GateVerdict::Pass);
        assert_eq!(point.score, None);
        assert_eq!(point.threshold, None);
    }

    /// The cross-mission query: two missions' logs fold into ONE series for
    /// the queried gate identity — other gates' events and non-gate events
    /// are ignored, an event recorded under a foreign mission id is
    /// filtered out, and the series is ordered by (ts, mission id, seq)
    /// regardless of the order the missions were handed in (determinism is
    /// the replay contract).
    #[test]
    fn gate_score_series_cross_mission_query_orders_by_log_time() {
        let m1 = vec![
            ev(
                4,
                "m-1",
                2_000,
                gate_result(
                    "vacuous-filter",
                    GateSurface::Approval,
                    GateVerdict::Pass,
                    Some((1.0, 1.0)),
                ),
            ),
            ev(
                9,
                "m-1",
                3_000,
                gate_result(
                    "vacuous-filter",
                    GateSurface::FinalGate,
                    GateVerdict::Pass,
                    Some((1.0, 1.0)),
                ),
            ),
            // Another gate's event in the same log: ignored.
            ev(
                5,
                "m-1",
                2_500,
                gate_result(
                    "env-sensitive",
                    GateSurface::Approval,
                    GateVerdict::Pass,
                    None,
                ),
            ),
            // A stray event recorded under a different mission id inside
            // this log: filtered out (the enumerated mission is the truth).
            ev(
                6,
                "m-elsewhere",
                2_600,
                gate_result(
                    "vacuous-filter",
                    GateSurface::Approval,
                    GateVerdict::Fail,
                    Some((0.1, 1.0)),
                ),
            ),
        ];
        let m2 = vec![
            // Same ms as m-1's seq 4: the mission-id tie-break orders it.
            ev(
                2,
                "m-2",
                2_000,
                gate_result(
                    "vacuous-filter",
                    GateSurface::Approval,
                    GateVerdict::Pass,
                    Some((0.5, 1.0)),
                ),
            ),
            // A non-gate event: ignored.
            ev(3, "m-2", 2_100, EventKind::MissionCompleted {}),
        ];
        // Handed in reverse mission order; the series must not care.
        let series = gate_score_series(
            "vacuous-filter",
            &[("m-2".to_string(), m2), ("m-1".to_string(), m1)],
        );
        let shape: Vec<(&str, u64, GateSurface, Option<f64>)> = series
            .evaluations
            .iter()
            .map(|p| (p.mission_id.as_str(), p.seq, p.surface, p.score))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("m-1", 4, GateSurface::Approval, Some(1.0)),
                ("m-2", 2, GateSurface::Approval, Some(0.5)),
                ("m-1", 9, GateSurface::FinalGate, Some(1.0)),
            ],
            "ordered by ts with the mission-id tie-break at the 2_000ms tie"
        );
    }

    /// A gate identity with no recorded evaluations yields an empty series,
    /// not an error (the outcomes empty-history rule: a query over no
    /// history is not a failure).
    #[test]
    fn gate_score_series_unknown_gate_is_empty() {
        let events = vec![ev(
            1,
            "m-1",
            0,
            gate_result(
                "env-sensitive",
                GateSurface::Approval,
                GateVerdict::Pass,
                None,
            ),
        )];
        let series = gate_score_series("no-such-gate", &[("m-1".to_string(), events)]);
        assert_eq!(series.gate, "no-such-gate");
        assert!(series.evaluations.is_empty());
        let empty = gate_score_series("vacuous-filter", &[]);
        assert!(empty.evaluations.is_empty());
    }

    // -- compute_gate_score_series over a fixture repo ---------------------

    /// Seed a mission's `events.jsonl` with the given kinds, in order (the
    /// escalation_metrics fixture idiom).
    fn seed_mission(repo_root: &std::path::Path, id: &str, kinds: Vec<EventKind>) {
        let paths = MissionPaths::new(repo_root, id);
        let mut log = EventLog::acquire(&paths, id, Duration::ZERO, LockForce::No).unwrap();
        for kind in kinds {
            log.append(kind).unwrap();
        }
    }

    /// End to end over real logs: two missions' `events.jsonl` files fold
    /// into the queried gate's series, and a mission with a corrupt log
    /// degrades per-row instead of failing the query.
    #[test]
    fn gate_score_series_compute_end_to_end_over_fixture_repo() {
        let tmp = TempDir::new().unwrap();
        seed_mission(
            tmp.path(),
            "m-1",
            vec![
                gate_result(
                    "vacuous-filter",
                    GateSurface::Approval,
                    GateVerdict::Pass,
                    Some((1.0, 1.0)),
                ),
                gate_result(
                    "env-sensitive",
                    GateSurface::Approval,
                    GateVerdict::Pass,
                    None,
                ),
                EventKind::MissionCompleted {},
            ],
        );
        seed_mission(
            tmp.path(),
            "m-2",
            vec![
                gate_result(
                    "vacuous-filter",
                    GateSurface::FinalGate,
                    GateVerdict::Fail,
                    Some((0.5, 1.0)),
                ),
                EventKind::MissionCompleted {},
            ],
        );
        // A mission whose log is corrupt: skipped, never fatal.
        let corrupt = MissionPaths::new(tmp.path(), "m-corrupt");
        std::fs::create_dir_all(corrupt.mission_dir()).unwrap();
        std::fs::write(corrupt.events_file(), b"{not json}\n").unwrap();

        let series = compute_gate_score_series(tmp.path(), "vacuous-filter").unwrap();
        assert_eq!(series.gate, "vacuous-filter");
        // m-1 was appended before m-2, so ts orders the series; identical
        // ms would tie-break on the mission id — either way m-1 leads.
        assert_eq!(series.evaluations.len(), 2);
        let first = &series.evaluations[0];
        assert_eq!(first.mission_id, "m-1");
        assert_eq!(first.surface, GateSurface::Approval);
        assert_eq!(first.verdict, GateVerdict::Pass);
        assert_eq!(first.score, Some(1.0));
        assert_eq!(first.threshold, Some(1.0));
        let second = &series.evaluations[1];
        assert_eq!(second.mission_id, "m-2");
        assert_eq!(second.surface, GateSurface::FinalGate);
        assert_eq!(second.verdict, GateVerdict::Fail);
        assert_eq!(second.score, Some(0.5));
        assert_eq!(second.threshold, Some(1.0));

        // A boolean-only gate's series: recorded, with the score pair absent.
        let series = compute_gate_score_series(tmp.path(), "env-sensitive").unwrap();
        assert_eq!(series.evaluations.len(), 1);
        assert_eq!(series.evaluations[0].score, None);
        assert_eq!(series.evaluations[0].threshold, None);

        // A gate nothing recorded: empty, not an error.
        let series = compute_gate_score_series(tmp.path(), "no-such-gate").unwrap();
        assert!(series.evaluations.is_empty());
    }
}
