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
    EventKind::UserMessage {
        text: text.to_string(),
        interrupt: false,
    }
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

#[test]
fn append_redacting_scrubs_secret_before_persisting_event() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths(tmp.path());
    let mut log = EventLog::acquire(&paths, MISSION, NEVER, LockForce::No).unwrap();
    let secret = "sk-ant-api03-AbCdEf_123-xyz";

    let (event, findings) = log
        .append_redacting(EventKind::UserMessage {
            text: format!("use {secret}"),
            interrupt: false,
        })
        .unwrap();
    drop(log);

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].rule_id, "anthropic-api-key");
    match event.kind {
        EventKind::UserMessage { text, .. } => assert_eq!(text, "use [REDACTED]"),
        other => panic!("wrong event: {other:?}"),
    }
    let raw = std::fs::read_to_string(paths.events_file()).unwrap();
    assert!(!raw.contains(secret), "event log leaked secret: {raw}");
    assert!(raw.contains("[REDACTED]"));
}

#[test]
fn append_emits_secret_redacted_audit_event() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths(tmp.path());
    let mut log = EventLog::acquire(&paths, MISSION, NEVER, LockForce::No).unwrap();
    let secret = "sk-ant-api03-AbCdEf_123-xyz";

    log.append(EventKind::UserMessage {
        text: format!("use {secret}"),
        interrupt: false,
    })
    .unwrap();
    drop(log);

    let events = EventLog::read_events(&paths.events_file()).unwrap();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0].kind,
        EventKind::UserMessage { text, .. } if text == "use [REDACTED]"
    ));
    assert!(matches!(
        &events[1].kind,
        EventKind::SecretRedacted {
            rule_id,
            location,
            ..
        } if rule_id == "anthropic-api-key" && location.contains("/payload/text")
    ));
}

/// Independent computation of the process identity token the engine records
/// as lock line 3, using the same platform recipe. Duplicated here on purpose:
/// it pins the on-disk token FORMAT, so an accidental format change (which
/// would misread every lock written by an older engine) fails these tests.
#[cfg(target_os = "linux")]
fn identity_token_for(pid: u32) -> String {
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let rest = &stat[stat.rfind(')').unwrap() + 1..];
    let ticks = rest.split_whitespace().nth(19).unwrap();
    format!("{}:{}", boot_id.trim(), ticks)
}

#[cfg(target_os = "macos")]
fn identity_token_for(pid: u32) -> String {
    let out = std::process::Command::new("ps")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ps -o lstart= must succeed for a live pid"
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
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
    // Lock format: pid, acquire time (unix epoch secs, diagnostics only),
    // then — where the platform supports it — the holder's identity token.
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    assert_eq!(
        lines
            .next()
            .expect("third lock line: identity token")
            .trim(),
        identity_token_for(std::process::id()),
        "the recorded token must be OUR OWN process identity"
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

/// After a force-steal, the stolen-from EventLog's Drop must NOT delete the
/// stealer's lock (otherwise the stealer sees generation 0 and fails closed).
#[test]
fn stolen_from_drop_leaves_stealers_lock() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let first = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();
    let mut stealer = EventLog::acquire(&p, MISSION, NEVER, LockForce::EvenIfLive).unwrap();
    assert!(p.lock_file().exists());
    drop(first);
    assert!(
        p.lock_file().exists(),
        "stolen-from Drop must not remove the stealer's lock"
    );
    stealer.append(lifecycle("still holding")).unwrap();
    drop(stealer);
    assert!(!p.lock_file().exists());
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
        EventKind::MilestoneStarted {
            milestone_id,
            start_sha,
        } => {
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
        vec![
            "user.message",
            "worker.message",
            "worker.message",
            "user.message"
        ]
    );
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
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
fn flush_if_due_drains_idle_buffer_by_age() {
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
    // No intervening append/flush/drop: an idle mission still ages out.
    let due = log.flush_if_due().unwrap();
    assert!(due, "flush_if_due must report that it drained the buffer");
    assert_eq!(EventLog::read_events(&p.events_file()).unwrap().len(), 1);
}

#[test]
fn buffer_age_reports_oldest_and_none_when_empty() {
    let dir = tempfile::tempdir().unwrap();
    let p = paths(dir.path());
    let mut log = EventLog::acquire(&p, MISSION, NEVER, LockForce::No).unwrap();

    assert_eq!(log.buffer_age(), None, "fresh log has no buffered deltas");

    log.append(delta("d1")).unwrap();
    assert!(
        log.buffer_age().is_some(),
        "buffer_age must report the oldest delta's age"
    );

    // Throttle is NEVER (1 hour): the buffer is far too young to flush.
    let due = log.flush_if_due().unwrap();
    assert!(
        !due,
        "flush_if_due must not drain a buffer younger than the throttle"
    );
    assert_eq!(
        EventLog::read_events(&p.events_file()).unwrap().len(),
        0,
        "delta must remain buffered"
    );
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
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
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
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
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
    assert_eq!(
        events.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
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
    assert_eq!(
        tail.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![3, 4, 5]
    );

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

    let err = EventLog::acquire(
        &paths,
        "m-lock",
        Duration::from_millis(50),
        LockForce::IfNotLive,
    )
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

    let log = EventLog::acquire(
        &paths,
        "m-lock",
        Duration::from_millis(50),
        LockForce::EvenIfLive,
    )
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

/// PID-REUSE DETECTION: a holder pid that is alive but whose CURRENT identity
/// token differs from the token recorded in the lock file cannot be the
/// engine that wrote the lock — the pid was recycled (or the machine
/// rebooted), the writer is dead, and ALL tiers steal. Identity tokens are
/// implemented on linux (boot_id + /proc starttime ticks) and macOS
/// (ps lstart).
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn reused_pid_lock_is_stale_at_every_tier() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(tmp.path(), "m-lock");
    std::fs::create_dir_all(paths.mission_dir()).unwrap();

    let holder = LiveHolder::spawn();
    for force in [LockForce::No, LockForce::IfNotLive, LockForce::EvenIfLive] {
        // A recorded token no real process can present: provable reuse.
        std::fs::write(
            paths.lock_file(),
            format!(
                "{}\n{}\nsome-other-boot-id:12345\n",
                holder.pid(),
                now_epoch_secs()
            ),
        )
        .unwrap();
        let log = EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), force)
            .unwrap_or_else(|e| panic!("reused pid means dead writer; {force:?} must steal: {e}"));
        drop(log);
    }
    drop(holder);
}

/// CLOCK-STEP REGRESSION (adversarial finding A): the reuse screen must be
/// immune to wall-clock steps. The old design compared the holder's
/// RECONSTRUCTED start time against the lock's acquire time, so an NTP step
/// (forward on linux, backward on macOS) made a LIVE holder look younger
/// than its own lock → Dead → auto-steal → two engines, one log. This
/// fixture is exactly what a >1h step produces: an acquire time an hour in
/// the "past" on a holder that just started — but the identity token
/// MATCHES, which proves the holder IS the recorder. It must read ALIVE:
/// `No` and `IfNotLive` refuse.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn live_holder_with_matching_token_survives_clock_steps() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(tmp.path(), "m-lock");
    std::fs::create_dir_all(paths.mission_dir()).unwrap();

    let holder = LiveHolder::spawn();
    let token = identity_token_for(holder.pid());
    assert!(!token.is_empty(), "live child must have an identity token");
    std::fs::write(
        paths.lock_file(),
        format!("{}\n{}\n{}\n", holder.pid(), now_epoch_secs() - 3600, token),
    )
    .unwrap();

    let err =
        EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), LockForce::No).unwrap_err();
    assert!(matches!(err, EngineError::LockHeld(_)), "got {err:?}");
    let err = EventLog::acquire(
        &paths,
        "m-lock",
        Duration::from_millis(50),
        LockForce::IfNotLive,
    )
    .unwrap_err();
    match err {
        EngineError::LockHeld(msg) => {
            assert!(
                msg.contains("ALIVE"),
                "matching token ⇒ alive holder refusal: {msg}"
            )
        }
        other => panic!("expected LockHeld, got {other:?}"),
    }
    drop(holder);
}

/// A two-line lock (pid + acquire time, no token) can no longer prove reuse:
/// timestamps are diagnostics-only in the token design (comparing them
/// against reconstructed start times is exactly the clock-step hazard). A
/// live holder therefore reads plain ALIVE even with an acquire time far in
/// the past, and `IfNotLive` refuses. Uncertainty must never produce Dead.
#[cfg(unix)]
#[test]
fn two_line_lock_without_token_degrades_to_plain_liveness() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(tmp.path(), "m-lock");
    std::fs::create_dir_all(paths.mission_dir()).unwrap();

    let holder = LiveHolder::spawn();
    std::fs::write(
        paths.lock_file(),
        format!("{}\n{}\n", holder.pid(), now_epoch_secs() - 3600),
    )
    .unwrap();
    let err = EventLog::acquire(
        &paths,
        "m-lock",
        Duration::from_millis(50),
        LockForce::IfNotLive,
    )
    .unwrap_err();
    match err {
        EngineError::LockHeld(msg) => {
            assert!(msg.contains("ALIVE"), "tokenless lock ⇒ plain alive: {msg}")
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
    let err = EventLog::acquire(
        &paths,
        "m-lock",
        Duration::from_millis(50),
        LockForce::IfNotLive,
    )
    .unwrap_err();
    assert!(matches!(err, EngineError::LockHeld(_)), "got {err:?}");
    drop(holder);
}

/// Even OUR OWN pid in a lock file is stolen without force when the token
/// proves the recorder was a different (dead) process whose pid the OS
/// recycled onto us — while a genuine double acquire (matching token, see
/// `second_acquire_fails_with_lock_held_naming_pid`) keeps refusing.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn own_pid_with_foreign_token_is_provably_reused() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(tmp.path(), "m-lock");
    std::fs::create_dir_all(paths.mission_dir()).unwrap();

    std::fs::write(
        paths.lock_file(),
        format!(
            "{}\n{}\nsome-other-boot-id:12345\n",
            std::process::id(),
            now_epoch_secs()
        ),
    )
    .unwrap();
    let log = EventLog::acquire(&paths, "m-lock", Duration::from_millis(50), LockForce::No)
        .expect("a foreign token on our own pid proves the recorder is dead");
    drop(log);
}

// ---------------------------------------------------------------------------
// The shared liveness probe (finding B: one lock parser, not three)
// ---------------------------------------------------------------------------

/// `lock_holder_is_alive` is the ONE probe every subsystem shares. Missing
/// lock → not alive; our own pid → alive; garbage → conservatively alive;
/// well-formed multi-line lock with a dead pid → NOT alive (this last case is
/// what the divergent per-module parsers used to get wrong).
#[test]
fn lock_holder_is_alive_understands_every_lock_format() {
    use kranz_engine::event_log::lock_holder_is_alive;
    let tmp = tempfile::tempdir().unwrap();
    let lock = tmp.path().join("events.jsonl.lock");

    assert!(
        !lock_holder_is_alive(&lock),
        "missing lock has no live holder"
    );

    std::fs::write(&lock, "garbage\n").unwrap();
    assert!(
        lock_holder_is_alive(&lock),
        "unparseable lock is conservatively alive"
    );

    std::fs::write(
        &lock,
        format!("{}\n{}\n", std::process::id(), now_epoch_secs()),
    )
    .unwrap();
    assert!(lock_holder_is_alive(&lock), "our own pid is alive");

    #[cfg(unix)]
    {
        std::fs::write(
            &lock,
            format!("{}\n{}\nsome-token\n", i32::MAX, now_epoch_secs()),
        )
        .unwrap();
        assert!(
            !lock_holder_is_alive(&lock),
            "a multi-line lock with a dead pid must read NOT alive"
        );
    }
}

/// `mission_lock_is_live` (hygiene sweeps, `kranz clean`) delegates to the
/// canonical probe and therefore understands the CURRENT multi-line lock
/// format, including dead holders.
#[test]
fn mission_lock_is_live_delegates_to_the_canonical_probe() {
    use kranz_engine::orchestrator::mission_lock_is_live;
    let tmp = tempfile::tempdir().unwrap();
    let p = MissionPaths::new(tmp.path(), "m-lock");
    std::fs::create_dir_all(p.mission_dir()).unwrap();

    assert!(!mission_lock_is_live(&p), "missing lock is not live");

    std::fs::write(p.lock_file(), "not-a-pid\n").unwrap();
    assert!(
        mission_lock_is_live(&p),
        "unparseable lock is conservatively live"
    );

    std::fs::write(
        p.lock_file(),
        format!("{}\n{}\n", std::process::id(), now_epoch_secs()),
    )
    .unwrap();
    assert!(mission_lock_is_live(&p), "a live holder (us) is live");

    #[cfg(unix)]
    {
        std::fs::write(
            p.lock_file(),
            format!("{}\n{}\ntok\n", i32::MAX, now_epoch_secs()),
        )
        .unwrap();
        assert!(
            !mission_lock_is_live(&p),
            "dead-holder multi-line lock is not live"
        );
    }
}

/// FINDING F REGRESSION: the dead-holder steal (probe → remove → create)
/// must be atomic under contention. Unserialized, N racing acquires can all
/// judge the stale holder Dead; a slow racer's `remove_file` then deletes a
/// fast racer's FRESH lock and both end up holding (or the losers die with
/// io errors instead of LockHeld). `flock` contends across separate fds
/// within one process, so racing threads exercise the same guard as racing
/// processes.
#[cfg(unix)]
#[test]
fn racing_acquires_on_a_dead_lock_admit_exactly_one_winner() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(tmp.path(), "m-lock");
    std::fs::create_dir_all(paths.mission_dir()).unwrap();
    // A provably dead holder: every racer's probe says Dead → steal allowed.
    std::fs::write(paths.lock_file(), i32::MAX.to_string()).unwrap();

    const RACERS: usize = 16;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(RACERS));
    let root = tmp.path().to_path_buf();
    let handles: Vec<_> = (0..RACERS)
        .map(|_| {
            let barrier = std::sync::Arc::clone(&barrier);
            let paths = MissionPaths::new(&root, "m-lock");
            std::thread::spawn(move || {
                barrier.wait();
                EventLog::acquire(&paths, "m-lock", NEVER, LockForce::No)
            })
        })
        .collect();

    // Join everything BEFORE dropping any result: the winner's EventLog must
    // stay alive for the whole race, or a loser could legitimately acquire.
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(winners, 1, "exactly one racer may steal a dead-holder lock");
    for r in &results {
        if let Err(e) = r {
            assert!(
                matches!(e, EngineError::LockHeld(_)),
                "losers must see LockHeld (the winner is alive), got: {e:?}"
            );
        }
    }
}
