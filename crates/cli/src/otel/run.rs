//! `kranz otel` runtime loop: poll mission event logs, fold spans, export.
//!
//! Mirrors [`crate::tail::tail_events`] and `kranz_slack::outbound_engine::run_bridge`
//! — a read-only poll over `events.jsonl`, self-healing on read errors (log
//! and retry next tick, never wedge the loop). No engine changes: everything
//! here consumes `kranz_engine::event_log`/`paths` read-side APIs only.

use super::emit::build_exporter;
use super::map::{map_mission, MissionSpan};
use anyhow::Result;
use kranz_engine::event_log::EventLog;
use kranz_engine::events::Event;
use kranz_engine::paths::MissionPaths;
use opentelemetry_otlp::SpanExporter;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

/// Poll interval, matching [`crate::tail::POLL_INTERVAL`] and the Slack bridge.
pub const POLL_INTERVAL: Duration = Duration::from_millis(400);

/// Per-mission tailing cursor.
struct MissionCursor {
    /// Events accumulated since this mission was first seen (live mode:
    /// starts empty at first sighting's head seq; --from-start: the whole
    /// log). Kept around because [`map_mission`] needs a span's opening
    /// event as well as its closing one to build it.
    events: Vec<Event>,
    last_seq: u64,
    exported: HashSet<[u8; 8]>,
}

/// Run the `kranz otel` sidecar until Ctrl-C. Returns the process exit code
/// (always 0 — Ctrl-C is a normal stop, not a failure).
pub async fn run_otel(
    repo: PathBuf,
    mission_scope: Option<String>,
    endpoint: String,
    from_start: bool,
) -> Result<i32> {
    let scope_desc = mission_scope.as_deref().unwrap_or("all missions");
    eprintln!("kranz otel: exporting spans to {endpoint} ({scope_desc}, from-start={from_start})");

    let exporter = build_exporter(&endpoint)?;
    let mut cursors: HashMap<String, MissionCursor> = HashMap::new();
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("kranz otel: shutting down");
                return Ok(0);
            }
            _ = ticker.tick() => {
                let missions = match &mission_scope {
                    Some(id) => vec![id.clone()],
                    None => MissionPaths::list_missions(&repo),
                };
                for mission_id in missions {
                    if let Err(error) =
                        poll_mission(&repo, &mission_id, from_start, &exporter, &mut cursors).await
                    {
                        tracing::warn!(mission = %mission_id, %error, "kranz otel: poll failed, retrying next tick");
                    }
                }
            }
        }
    }
}

/// Advance one mission's cursor, exporting any span whose closing event was
/// newly observed. Self-healing: read errors are surfaced to the caller,
/// which logs and retries next tick without touching the cursor.
async fn poll_mission(
    repo: &std::path::Path,
    mission_id: &str,
    from_start: bool,
    exporter: &SpanExporter,
    cursors: &mut HashMap<String, MissionCursor>,
) -> Result<()> {
    let paths = MissionPaths::new(repo, mission_id);
    let events_path = paths.events_file();
    if !events_path.is_file() {
        return Ok(());
    }

    if !cursors.contains_key(mission_id) {
        if from_start {
            let events = EventLog::read_events(&events_path)?;
            let last_seq = events.last().map(|e| e.seq).unwrap_or(0);
            let spans = map_mission(&events);
            let exported: HashSet<[u8; 8]> = spans.iter().map(|s| s.span_id).collect();
            super::emit::export_spans(exporter, spans).await;
            cursors.insert(
                mission_id.to_string(),
                MissionCursor {
                    events,
                    last_seq,
                    exported,
                },
            );
        } else {
            // First sighting in live mode: seed the cursor at the current
            // head without replaying — only spans whose opening AND closing
            // events arrive during the tail get built and exported.
            let events = EventLog::read_events(&events_path)?;
            let last_seq = events.last().map(|e| e.seq).unwrap_or(0);
            cursors.insert(
                mission_id.to_string(),
                MissionCursor {
                    events: Vec::new(),
                    last_seq,
                    exported: HashSet::new(),
                },
            );
        }
        return Ok(());
    }

    let cursor = cursors
        .get_mut(mission_id)
        .expect("just checked contains_key");
    let new_events = EventLog::read_events_after(&events_path, cursor.last_seq)?;
    if new_events.is_empty() {
        return Ok(());
    }

    cursor.last_seq = new_events.last().map(|e| e.seq).unwrap_or(cursor.last_seq);
    cursor.events.extend(new_events);

    let spans: Vec<MissionSpan> = map_mission(&cursor.events)
        .into_iter()
        .filter(|s| !cursor.exported.contains(&s.span_id))
        .collect();
    for span in &spans {
        cursor.exported.insert(span.span_id);
    }
    super::emit::export_spans(exporter, spans).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use kranz_engine::events::EventKind;
    use kranz_engine::paths::MissionPaths;
    use kranz_engine::types::MissionConfig;
    use std::io::Write;

    fn ts(secs: i64) -> chrono::DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    /// Append one hand-built event line to a raw events.jsonl (no engine
    /// lock — these tests only ever read the log, mirroring
    /// `EventLog::read_events`/`read_events_after`).
    fn append(events_path: &std::path::Path, seq: u64, secs: i64, kind: EventKind) {
        let event = Event {
            seq,
            ts: ts(secs),
            mission_id: "m-01".to_string(),
            kind,
        };
        let mut line = serde_json::to_string(&event).unwrap();
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(events_path)
            .unwrap();
        file.write_all(line.as_bytes()).unwrap();
    }

    fn created_kind() -> EventKind {
        EventKind::MissionCreated {
            goal: "ship it".to_string(),
            base_branch: "main".to_string(),
            mission_branch: "kranz/mission-m-01".to_string(),
            config: MissionConfig::default(),
        }
    }

    #[tokio::test]
    async fn live_mode_first_sighting_seeds_cursor_at_head_without_exporting() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = MissionPaths::new(tmp.path(), "m-01");
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        let events_path = paths.events_file();

        append(&events_path, 1, 0, created_kind());
        append(&events_path, 2, 10, EventKind::MissionCompleted {});

        let exporter = build_exporter("http://127.0.0.1:1/v1/traces").unwrap();
        let mut cursors: HashMap<String, MissionCursor> = HashMap::new();

        poll_mission(tmp.path(), "m-01", false, &exporter, &mut cursors)
            .await
            .unwrap();

        let cursor = cursors.get("m-01").unwrap();
        assert_eq!(
            cursor.last_seq, 2,
            "cursor should seed at the current head seq"
        );
        assert!(
            cursor.events.is_empty(),
            "live mode must not replay historic events"
        );
        assert!(
            cursor.exported.is_empty(),
            "nothing should be exported on first sighting in live mode"
        );
    }

    #[tokio::test]
    async fn live_mode_exports_spans_whose_open_and_close_both_arrive_during_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = MissionPaths::new(tmp.path(), "m-01");
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        let events_path = paths.events_file();

        append(&events_path, 1, 0, created_kind());

        let exporter = build_exporter("http://127.0.0.1:1/v1/traces").unwrap();
        let mut cursors: HashMap<String, MissionCursor> = HashMap::new();

        // First sighting: seeds at seq 1, nothing to replay.
        poll_mission(tmp.path(), "m-01", false, &exporter, &mut cursors)
            .await
            .unwrap();
        assert!(cursors.get("m-01").unwrap().exported.is_empty());

        // Both the opening (already seeded away) and the closing event for
        // the mission root arrive: since mission.created is before the seed
        // point, the root span is NOT built in live mode (its open event
        // never entered `cursor.events`).
        append(&events_path, 2, 10, EventKind::MissionCompleted {});
        poll_mission(tmp.path(), "m-01", false, &exporter, &mut cursors)
            .await
            .unwrap();
        assert!(
            cursors.get("m-01").unwrap().exported.is_empty(),
            "root span's opening event predates the tail, so it must not be exported"
        );

        // A milestone whose open AND close both arrive during the tail IS
        // exported.
        append(
            &events_path,
            3,
            20,
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".to_string(),
                start_sha: "abc123".to_string(),
            },
        );
        append(
            &events_path,
            4,
            30,
            EventKind::MilestoneCompleted {
                milestone_id: "ms-1".to_string(),
                tag: None,
            },
        );
        poll_mission(tmp.path(), "m-01", false, &exporter, &mut cursors)
            .await
            .unwrap();
        assert_eq!(
            cursors.get("m-01").unwrap().exported.len(),
            1,
            "milestone opened and closed during the tail should be exported exactly once"
        );
    }

    #[tokio::test]
    async fn from_start_replays_already_closed_spans_from_event_timestamps() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = MissionPaths::new(tmp.path(), "m-01");
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        let events_path = paths.events_file();

        append(&events_path, 1, 0, created_kind());
        append(&events_path, 2, 10, EventKind::MissionCompleted {});

        let exporter = build_exporter("http://127.0.0.1:1/v1/traces").unwrap();
        let mut cursors: HashMap<String, MissionCursor> = HashMap::new();

        poll_mission(tmp.path(), "m-01", true, &exporter, &mut cursors)
            .await
            .unwrap();

        let cursor = cursors.get("m-01").unwrap();
        assert_eq!(cursor.last_seq, 2);
        assert_eq!(
            cursor.events.len(),
            2,
            "--from-start replays the whole log into the cursor"
        );
        assert_eq!(
            cursor.exported.len(),
            1,
            "the already-closed root span should be replayed and exported"
        );
    }

    #[tokio::test]
    async fn already_exported_span_is_not_re_exported_on_a_later_tick() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = MissionPaths::new(tmp.path(), "m-01");
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        let events_path = paths.events_file();

        append(&events_path, 1, 0, created_kind());
        append(
            &events_path,
            2,
            10,
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".to_string(),
                start_sha: "abc123".to_string(),
            },
        );
        append(
            &events_path,
            3,
            20,
            EventKind::MilestoneCompleted {
                milestone_id: "ms-1".to_string(),
                tag: None,
            },
        );

        let exporter = build_exporter("http://127.0.0.1:1/v1/traces").unwrap();
        let mut cursors: HashMap<String, MissionCursor> = HashMap::new();
        poll_mission(tmp.path(), "m-01", true, &exporter, &mut cursors)
            .await
            .unwrap();
        assert_eq!(cursors.get("m-01").unwrap().exported.len(), 1);

        // A later tick with no new events must not touch the cursor further.
        poll_mission(tmp.path(), "m-01", true, &exporter, &mut cursors)
            .await
            .unwrap();
        assert_eq!(
            cursors.get("m-01").unwrap().exported.len(),
            1,
            "no new events, no re-export"
        );
    }
}
