//! Integration tests for the append-only JSONL event log (plan §4.3).

use chrono::Utc;
use kranz_engine::error::EngineError;
use kranz_engine::event_log::{EventLog, LockForce};
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

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A real, live child process (`sleep 300`) whose pid can be planted in a
/// lock file. Killed and reaped on drop so no test leaks a sleeper.
#[cfg(unix)]
struct LiveHolder(std::process::Child);

#[cfg(unix)]
impl LiveHolder {
    fn spawn() -> Self {
        LiveHolder(
            std::process::Command::new("sleep")
                .arg("300")
                .spawn()
                .expect("spawn sleep child"),
        )
    }

    fn pid(&self) -> u32 {
        self.0.id()
    }
}

#[cfg(unix)]
impl Drop for LiveHolder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// ---------------------------------------------------------------------------
// Locking
// ---------------------------------------------------------------------------

#[test]
fn acquire_creates_dirs_and_lock() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let before = now_epoch_secs();
    let log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();

    assert!(p.mission_dir().is_dir());
    assert!(p.runs_dir().is_dir());
    assert!(p.control_dir().is_dir());
    assert!(p.lock_file().is_file());
    // Two-line lock format: pid, then acquire time (unix epoch secs).
    let contents = std::fs::read_to_string(p.lock_file()).unwrap();
    let mut lines = contents.lines();
    assert_eq!(lines.next().unwrap(), std::process::id().to_string());
    let acquired: u64 = lines
        .next()
        .expect("second lock line: acquire epoch secs")
        .parse()
        .expect("acquire time must be an integer");
    assert!(
        acquired >= before && acquired <= now_epoch_secs(),
        "acquire time {acquired} outside [{before}, now]"
    );
    assert_eq!(log.last_seq(), 0);
}

#[test]
fn second_acquire_fails_with_lock_held_naming_pid() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let _held = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();

    let err = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap_err();
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

/// The holder here is OUR OWN (live) pid, so only the strongest tier may
/// steal: `IfNotLive` (the old --force-lock) must now refuse a live holder.
#[test]
fn even_if_live_steals_lock_from_live_holder_if_not_live_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let first = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();

    let err = EventLog::acquire(&p, MISSION, NEVER, LockForce::IfNotLive).unwrap_err();
    match err {
        EngineError::LockHeld(msg) => assert!(
            msg.contains("dangerously-steal-live-lock"),
            "live-holder refusal must point at the stronger flag: {msg}"
        ),
        other => panic!("expected LockHeld, got {other:?}"),
    }

    let mut stolen = EventLog::acquire(&p, MISSION, NEVER, LockForce::EvenIfLive).unwrap();
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
    let log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
    assert!(p.lock_file().exists());
    drop(log);
    assert!(!p.lock_file().exists());
    // Reacquire works after release.
    let _again = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
}

#[test]
fn failed_acquire_releases_lock() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    {
        let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
        log.append(lifecycle("hello")).unwrap();
    }
    // Wrong mission id: acquire must fail AND must not leave the lock behind.
    let err = EventLog::acquire(&p, "m-other", NEVER, LockForce::No).unwrap_err();
    assert!(matches!(err, EngineError::InvalidState(_)), "got {err:?}");
    assert!(!p.lock_file().exists());
    let _ok = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
}

// ---------------------------------------------------------------------------
// Appending
// ---------------------------------------------------------------------------

#[test]
fn append_assigns_contiguous_seq_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();

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
    let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();

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
    let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();

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
    let mut log = EventLog::acquire(&p, MISSION, Duration::from_millis(30), LockForce::No).unwrap();

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
    let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();

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
        let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
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
        let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
        log.append(lifecycle("one")).unwrap();
        log.append(lifecycle("two")).unwrap();
    }
    let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
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

// ---------------------------------------------------------------------------
// Torn-write repair on acquire
// ---------------------------------------------------------------------------

/// Append raw bytes to an existing log, simulating a torn (partial) write
/// from a crashed engine process.
fn append_raw_bytes(path: &std::path::Path, bytes: &[u8]) {
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(bytes).unwrap();
}

#[test]
fn reacquire_truncates_torn_final_line_without_newline() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    {
        let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
        log.append(lifecycle("one")).unwrap();
        log.append(lifecycle("two")).unwrap();
    }
    // Crash mid-append: a partial line with no trailing newline.
    append_raw_bytes(&p.events_file(), br#"{"seq":3,"ts":"2026-01-01T00:0"#);

    let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
    assert_eq!(log.last_seq(), 2, "torn line must not count toward seq");
    log.append(lifecycle("three")).unwrap();
    log.append(lifecycle("four")).unwrap();
    drop(log);

    // Without truncation the first append would glue onto the torn line,
    // making it a non-final garbage line and poisoning every future read.
    let events = EventLog::read_events(&p.events_file()).unwrap();
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
}

#[test]
fn reacquire_truncates_torn_final_line_with_newline() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    {
        let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
        log.append(lifecycle("one")).unwrap();
        log.append(lifecycle("two")).unwrap();
    }
    // Garbage final line that did get its newline out before the crash.
    append_raw_bytes(&p.events_file(), b"{\"seq\":3,\"ts\":\"2026-01-01T00:0\n");

    let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
    assert_eq!(log.last_seq(), 2);
    log.append(lifecycle("three")).unwrap();
    log.append(lifecycle("four")).unwrap();
    drop(log);

    let events = EventLog::read_events(&p.events_file()).unwrap();
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
}

#[test]
fn reacquire_repairs_valid_final_line_missing_its_newline() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    // A tear can cut exactly at the terminator: the final line is complete,
    // valid JSON but has no trailing newline. It must be kept (not truncated)
    // and terminated so the next append does not glue onto it.
    std::fs::create_dir_all(p.events_file().parent().unwrap()).unwrap();
    let mut f = std::fs::File::create(p.events_file()).unwrap();
    writeln!(f, "{}", raw_event(1, lifecycle("a"))).unwrap();
    write!(f, "{}", raw_event(2, lifecycle("b"))).unwrap(); // no '\n'
    drop(f);

    let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
    assert_eq!(log.last_seq(), 2, "unterminated valid line must survive");
    log.append(lifecycle("c")).unwrap();
    drop(log);

    let events = EventLog::read_events(&p.events_file()).unwrap();
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
}

#[test]
fn reacquire_truncates_log_that_is_only_a_torn_line() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    std::fs::create_dir_all(p.events_file().parent().unwrap()).unwrap();
    std::fs::write(p.events_file(), b"{\"seq\":1,\"ts").unwrap();

    let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
    assert_eq!(log.last_seq(), 0);
    log.append(lifecycle("first")).unwrap();
    drop(log);

    let events = EventLog::read_events(&p.events_file()).unwrap();
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1]);
}

// ---------------------------------------------------------------------------
// Torn writes splitting multi-byte UTF-8
// ---------------------------------------------------------------------------

#[test]
fn torn_final_line_splitting_multibyte_char_is_dropped_on_read() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    {
        let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
        log.append(lifecycle("one")).unwrap();
    }
    // Tear mid multi-byte character: 0xE2 is the first byte of a 3-byte UTF-8
    // sequence. The file is now invalid UTF-8; the reader must still return
    // the valid events instead of failing wholesale.
    append_raw_bytes(&p.events_file(), b"{\"seq\":2,\"ts\":\"2026\xE2");

    let events = EventLog::read_events(&p.events_file()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].seq, 1);
}

#[test]
fn reacquire_truncates_torn_line_splitting_multibyte_char() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    {
        let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
        log.append(lifecycle("one")).unwrap();
    }
    append_raw_bytes(&p.events_file(), b"{\"seq\":2,\"ts\":\"2026\xE2");

    let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
    assert_eq!(log.last_seq(), 1);
    log.append(lifecycle("two")).unwrap();
    drop(log);

    let events = EventLog::read_events(&p.events_file()).unwrap();
    assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2]);
}

#[test]
fn read_events_after_returns_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    {
        let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
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

/// A lock whose recorded holder is provably dead is stale: EVERY tier steals
/// it, force flags not required. (Legacy one-line pid-only lock format —
/// compat is exercised at the same time.)
#[cfg(unix)]
#[test]
fn dead_holder_lock_is_stolen_at_every_tier() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(tmp.path(), "m-lock");
    std::fs::create_dir_all(paths.mission_dir()).unwrap();

    for force in [LockForce::No, LockForce::IfNotLive, LockForce::EvenIfLive] {
        // i32::MAX exceeds every real pid space: kill(_, 0) -> ESRCH -> dead.
        std::fs::write(paths.lock_file(), i32::MAX.to_string()).unwrap();
        let log = EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), force)
            .unwrap_or_else(|e| panic!("dead holder must be stolen at tier {force:?}: {e}"));
        drop(log); // releases the lock for the next tier's fixture
    }
}

/// A lock held by a provably ALIVE process (a real spawned child): `No` and
/// `IfNotLive` refuse — with tier-specific guidance — and only
/// `EvenIfLive` steals.
#[cfg(unix)]
#[test]
fn alive_holder_lock_needs_the_dangerous_tier() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(tmp.path(), "m-lock");
    std::fs::create_dir_all(paths.mission_dir()).unwrap();

    let holder = LiveHolder::spawn();
    // Legacy one-line format: liveness still probes, reuse screen is skipped.
    std::fs::write(paths.lock_file(), holder.pid().to_string()).unwrap();

    let err =
        EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), LockForce::No).unwrap_err();
    match err {
        EngineError::LockHeld(msg) => assert!(
            msg.contains("--force-lock") && msg.contains(&holder.pid().to_string()),
            "no-force refusal keeps today's message shape: {msg}"
        ),
        other => panic!("expected LockHeld, got {other:?}"),
    }

    let err = EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), LockForce::IfNotLive)
        .unwrap_err();
    match err {
        EngineError::LockHeld(msg) => {
            assert!(msg.contains("ALIVE"), "must say the holder is alive: {msg}");
            assert!(
                msg.contains(&format!("ps -p {}", holder.pid())),
                "must suggest identifying the holder: {msg}"
            );
            assert!(
                msg.contains("--dangerously-steal-live-lock"),
                "must name the stronger flag: {msg}"
            );
        }
        other => panic!("expected LockHeld, got {other:?}"),
    }

    let log = EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), LockForce::EvenIfLive)
        .expect("EvenIfLive must steal even from a live holder");
    drop(log);
    drop(holder);
}

/// Indeterminate liveness (garbage pid in the lock file): `No` refuses with
/// today's message; both force tiers steal — --force-lock keeps its
/// historical meaning where liveness cannot be probed.
#[test]
fn unknown_holder_lock_yields_to_any_force_tier() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(tmp.path(), "m-lock");
    std::fs::create_dir_all(paths.mission_dir()).unwrap();

    std::fs::write(paths.lock_file(), "not-a-pid").unwrap();
    let err =
        EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), LockForce::No).unwrap_err();
    match err {
        EngineError::LockHeld(msg) => assert!(
            msg.contains("--force-lock"),
            "unknown-holder refusal mentions --force-lock: {msg}"
        ),
        other => panic!("expected LockHeld, got {other:?}"),
    }

    for force in [LockForce::IfNotLive, LockForce::EvenIfLive] {
        std::fs::write(paths.lock_file(), "not-a-pid").unwrap();
        let log = EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), force)
            .unwrap_or_else(|e| panic!("unknown holder must yield to {force:?}: {e}"));
        drop(log);
    }

    // Non-positive pids are equally unprobeable: Unknown, not Dead.
    std::fs::write(paths.lock_file(), "-7").unwrap();
    let err =
        EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), LockForce::No).unwrap_err();
    assert!(matches!(err, EngineError::LockHeld(_)));
}

/// PID-REUSE DETECTION: a holder pid that is alive but whose process started
/// AFTER the lock's recorded acquire time cannot be the engine that wrote the
/// lock — the pid was recycled, the writer is dead, and ALL tiers steal.
/// Start-time probing is implemented on linux (/proc) and macOS (ps etime).
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn reused_pid_lock_is_stale_at_every_tier() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(tmp.path(), "m-lock");
    std::fs::create_dir_all(paths.mission_dir()).unwrap();

    let holder = LiveHolder::spawn();
    for force in [LockForce::No, LockForce::IfNotLive, LockForce::EvenIfLive] {
        // Lock "acquired" an hour before the child started: provable reuse.
        std::fs::write(
            paths.lock_file(),
            format!("{}\n{}\n", holder.pid(), now_epoch_secs() - 3600),
        )
        .unwrap();
        let log = EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), force)
            .unwrap_or_else(|e| panic!("reused pid means dead writer; {force:?} must steal: {e}"));
        drop(log);
    }
    drop(holder);
}

/// The inverse guard: an acquire time at/after the holder's start (here:
/// stamped in the future) proves nothing — the holder counts as plain ALIVE
/// and `IfNotLive` keeps refusing. A reuse probe must never demote a live
/// holder on ambiguous timestamps.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn holder_older_than_lock_stays_alive_and_refuses_force_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(tmp.path(), "m-lock");
    std::fs::create_dir_all(paths.mission_dir()).unwrap();

    let holder = LiveHolder::spawn();
    // Acquired "in the future": start < acquired + slack, so NOT a reuse.
    std::fs::write(
        paths.lock_file(),
        format!("{}\n{}\n", holder.pid(), now_epoch_secs() + 60),
    )
    .unwrap();

    let err =
        EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), LockForce::No).unwrap_err();
    assert!(matches!(err, EngineError::LockHeld(_)));
    let err = EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), LockForce::IfNotLive)
        .unwrap_err();
    match err {
        EngineError::LockHeld(msg) => {
            assert!(msg.contains("ALIVE"), "alive holder refusal: {msg}")
        }
        other => panic!("expected LockHeld, got {other:?}"),
    }
    drop(holder);
}

/// A two-line lock whose second line is garbage degrades to the legacy
/// behavior: acquire time unknown, alive holder is plain Alive.
#[cfg(unix)]
#[test]
fn garbage_acquire_time_degrades_to_plain_liveness() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(tmp.path(), "m-lock");
    std::fs::create_dir_all(paths.mission_dir()).unwrap();

    let holder = LiveHolder::spawn();
    std::fs::write(paths.lock_file(), format!("{}\nnot-a-time\n", holder.pid())).unwrap();
    let err = EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), LockForce::IfNotLive)
        .unwrap_err();
    assert!(matches!(err, EngineError::LockHeld(_)), "got {err:?}");
    drop(holder);
}
