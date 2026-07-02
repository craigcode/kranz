//! Integration tests for the append-only JSONL event log (plan §4.3).

use chrono::Utc;
use kranz_engine::error::EngineError;
use kranz_engine::event_log::EventLog;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::paths::MissionPaths;
use std::io::Write;
use std::time::Duration;

const MISSION: &str = "m-test";

/// A throttle long enough that deltas never auto-flush during a test.
const NEVER: Duration = Duration::from_secs(3600);

fn paths(dir: &std::path::Path) -> MissionPaths {
    MissionPaths::new(dir, MISSION)
}

fn lifecycle(text: &str) -> EventKind {
    EventKind::UserMessage { text: text.to_string(), interrupt: false }
}

fn delta(content: &str) -> EventKind {
    EventKind::WorkerMessage {
        run_id: "r-1".to_string(),
        tag: "text".to_string(),
        content: content.to_string(),
    }
}

/// Hand-write raw event lines (for corruption fixtures).
fn write_raw_log(path: &std::path::Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut f = std::fs::File::create(path).unwrap();
    for line in lines {
        writeln!(f, "{line}").unwrap();
    }
}

fn raw_event(seq: u64, kind: EventKind) -> String {
    serde_json::to_string(&Event {
        seq,
        ts: Utc::now(),
        mission_id: MISSION.to_string(),
        kind,
    })
    .unwrap()
}

// ---------------------------------------------------------------------------
// Locking
// ---------------------------------------------------------------------------

#[test]
fn acquire_creates_dirs_and_lock() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let log = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();

    assert!(p.mission_dir().is_dir());
    assert!(p.runs_dir().is_dir());
    assert!(p.control_dir().is_dir());
    assert!(p.lock_file().is_file());
    let pid = std::fs::read_to_string(p.lock_file()).unwrap();
    assert_eq!(pid, std::process::id().to_string());
    assert_eq!(log.last_seq(), 0);
}

#[test]
fn second_acquire_fails_with_lock_held_naming_pid() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let _held = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();

    let err = EventLog::acquire(&p, MISSION, NEVER, false).unwrap_err();
    match err {
        EngineError::LockHeld(msg) => {
            assert!(
                msg.contains(&std::process::id().to_string()),
                "message should name the holding pid: {msg}"
            );
        }
        other => panic!("expected LockHeld, got {other:?}"),
    }
}

#[test]
fn force_steals_lock() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let first = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();

    let mut stolen = EventLog::acquire(&p, MISSION, NEVER, true).unwrap();
    stolen.append(lifecycle("after steal")).unwrap();
    drop(first);
    drop(stolen);

    let events = EventLog::read_events(&p.events_file()).unwrap();
    assert_eq!(events.len(), 1);
}

#[test]
fn drop_removes_lock_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let log = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();
    assert!(p.lock_file().exists());
    drop(log);
    assert!(!p.lock_file().exists());
    // Reacquire works after release.
    let _again = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();
}

#[test]
fn failed_acquire_releases_lock() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    {
        let mut log = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();
        log.append(lifecycle("hello")).unwrap();
    }
    // Wrong mission id: acquire must fail AND must not leave the lock behind.
    let err = EventLog::acquire(&p, "m-other", NEVER, false).unwrap_err();
    assert!(matches!(err, EngineError::InvalidState(_)), "got {err:?}");
    assert!(!p.lock_file().exists());
    let _ok = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();
}

// ---------------------------------------------------------------------------
// Appending
// ---------------------------------------------------------------------------

#[test]
fn append_assigns_contiguous_seq_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();

    let e1 = log.append(lifecycle("one")).unwrap();
    let e2 = log.append(lifecycle("two")).unwrap();
    let e3 = log
        .append(EventKind::MilestoneStarted {
            milestone_id: "ms-1".to_string(),
            start_sha: "abc123".to_string(),
        })
        .unwrap();
    assert_eq!((e1.seq, e2.seq, e3.seq), (1, 2, 3));
    assert_eq!(e1.mission_id, MISSION);
    drop(log);

    let events = EventLog::read_events(&p.events_file()).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].seq, 1);
    match &events[2].kind {
        EventKind::MilestoneStarted { milestone_id, start_sha } => {
            assert_eq!(milestone_id, "ms-1");
            assert_eq!(start_sha, "abc123");
        }
        other => panic!("wrong kind round-tripped: {other:?}"),
    }
}

#[test]
fn lifecycle_events_are_durable_immediately() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();

    log.append(lifecycle("durable")).unwrap();
    // No flush(), no drop: a lifecycle append is already on disk (fsynced).
    let events = EventLog::read_events(&p.events_file()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].seq, 1);
}

#[test]
fn deltas_buffer_and_lifecycle_drains_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();

    log.append(lifecycle("L1")).unwrap();
    log.append(delta("d1")).unwrap();
    log.append(delta("d2")).unwrap();
    // Deltas are still buffered in memory.
    assert_eq!(EventLog::read_events(&p.events_file()).unwrap().len(), 1);

    // A lifecycle append drains the buffer FIRST, preserving append order.
    log.append(lifecycle("L2")).unwrap();
    let events = EventLog::read_events(&p.events_file()).unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.type_name()).collect();
    assert_eq!(
        kinds,
        vec!["user.message", "worker.message", "worker.message", "user.message"]
    );
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
    let contents: Vec<String> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::WorkerMessage { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(contents, vec!["d1", "d2"]);
}

#[test]
fn throttle_flushes_buffer_by_age() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = EventLog::acquire(&p, MISSION, Duration::from_millis(30), false).unwrap();

    log.append(delta("d1")).unwrap();
    assert_eq!(
        EventLog::read_events(&p.events_file()).unwrap().len(),
        0,
        "young delta stays buffered"
    );

    std::thread::sleep(Duration::from_millis(60));
    // The oldest buffered delta is now older than the throttle, so this
    // append drains the whole buffer.
    log.append(delta("d2")).unwrap();
    assert_eq!(EventLog::read_events(&p.events_file()).unwrap().len(), 2);
}

#[test]
fn explicit_flush_drains_buffer() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();

    log.append(delta("d1")).unwrap();
    assert_eq!(EventLog::read_events(&p.events_file()).unwrap().len(), 0);
    log.flush().unwrap();
    assert_eq!(EventLog::read_events(&p.events_file()).unwrap().len(), 1);
}

#[test]
fn drop_flushes_buffered_deltas() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    {
        let mut log = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();
        log.append(delta("d1")).unwrap();
        log.append(delta("d2")).unwrap();
    } // dropped without flush()

    let events = EventLog::read_events(&p.events_file()).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].seq, 2);
}

#[test]
fn reacquire_resumes_seq_from_existing_log() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    {
        let mut log = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();
        log.append(lifecycle("one")).unwrap();
        log.append(lifecycle("two")).unwrap();
    }
    let mut log = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();
    assert_eq!(log.last_seq(), 2);
    let e = log.append(lifecycle("three")).unwrap();
    assert_eq!(e.seq, 3);
    drop(log);
    assert_eq!(EventLog::read_events(&p.events_file()).unwrap().len(), 3);
}

// ---------------------------------------------------------------------------
// Reader validation
// ---------------------------------------------------------------------------

#[test]
fn read_events_refuses_seq_gap() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    write_raw_log(
        &p.events_file(),
        &[raw_event(1, lifecycle("a")), raw_event(3, lifecycle("b"))],
    );
    let err = EventLog::read_events(&p.events_file()).unwrap_err();
    assert!(matches!(err, EngineError::LogCorruption(_)), "got {err:?}");
}

#[test]
fn read_events_refuses_duplicate_seq() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    write_raw_log(
        &p.events_file(),
        &[raw_event(1, lifecycle("a")), raw_event(1, lifecycle("b"))],
    );
    let err = EventLog::read_events(&p.events_file()).unwrap_err();
    assert!(matches!(err, EngineError::LogCorruption(_)), "got {err:?}");
}

#[test]
fn read_events_requires_seq_starting_at_one() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    write_raw_log(&p.events_file(), &[raw_event(2, lifecycle("a"))]);
    let err = EventLog::read_events(&p.events_file()).unwrap_err();
    assert!(matches!(err, EngineError::LogCorruption(_)), "got {err:?}");
}

#[test]
fn torn_final_line_is_dropped_silently() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    write_raw_log(
        &p.events_file(),
        &[
            raw_event(1, lifecycle("a")),
            raw_event(2, lifecycle("b")),
            r#"{"seq":3,"ts":"2026-01-01T00:0"#.to_string(), // torn write
        ],
    );
    let events = EventLog::read_events(&p.events_file()).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events.last().unwrap().seq, 2);
}

#[test]
fn torn_middle_line_is_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    write_raw_log(
        &p.events_file(),
        &[
            raw_event(1, lifecycle("a")),
            "{not json".to_string(),
            raw_event(2, lifecycle("b")),
        ],
    );
    let err = EventLog::read_events(&p.events_file()).unwrap_err();
    assert!(matches!(err, EngineError::LogCorruption(_)), "got {err:?}");
}

#[test]
fn empty_log_reads_as_no_events() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    write_raw_log(&p.events_file(), &[]);
    assert!(EventLog::read_events(&p.events_file()).unwrap().is_empty());
}

#[test]
fn read_events_after_returns_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    {
        let mut log = EventLog::acquire(&p, MISSION, NEVER, false).unwrap();
        for i in 1..=5 {
            log.append(lifecycle(&format!("e{i}"))).unwrap();
        }
    }
    let tail = EventLog::read_events_after(&p.events_file(), 2).unwrap();
    assert_eq!(tail.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4, 5]);

    let all = EventLog::read_events_after(&p.events_file(), 0).unwrap();
    assert_eq!(all.len(), 5);
    let none = EventLog::read_events_after(&p.events_file(), 5).unwrap();
    assert!(none.is_empty());
}
