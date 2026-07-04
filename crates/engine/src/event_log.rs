//! Append-only JSONL event log (plan §4.3) — the single source of truth.
//!
//! One engine process owns `events.jsonl` at a time, guarded by a lock file
//! (`events.jsonl.lock`) rather than POSIX advisory locks so Windows stays
//! first-class. Lifecycle events are flushed + fsynced per append; stream
//! deltas (`worker.message`) are buffered in memory and drained by age
//! (throttle), by the next lifecycle append, by explicit [`EventLog::flush`],
//! or on drop. Losing buffered deltas on a crash is recoverable; losing a
//! lifecycle event is not, hence the asymmetry.

use crate::error::{EngineError, Result};
use crate::events::{Event, EventKind};
use crate::paths::MissionPaths;
use chrono::Utc;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How aggressively [`EventLog::acquire`] may steal an existing lock.
///
/// A holder that is provably DEAD is always stolen (a stale lock from a
/// crashed engine), regardless of tier. The tiers only govern holders that
/// are alive or of indeterminate liveness:
///
/// | holder liveness | `No`       | `IfNotLive` | `EvenIfLive` |
/// |-----------------|------------|-------------|--------------|
/// | Dead            | steal      | steal       | steal        |
/// | Unknown         | `LockHeld` | steal       | steal        |
/// | Alive           | `LockHeld` | `LockHeld`  | steal (loud) |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockForce {
    /// Honor any lock whose holder is not provably dead.
    No,
    /// `--force-lock`: steal unless the holder is provably ALIVE. This is the
    /// historical force behavior on platforms/lockfiles where liveness cannot
    /// be probed (Unknown), but it refuses to rip the lock from a running
    /// engine.
    IfNotLive,
    /// `--dangerously-steal-live-lock`: steal even from a live holder. Only
    /// correct when the operator has verified the holder is a zombie or an
    /// unrelated (pid-reused) process — stealing from a live kranz engine
    /// means two engines write one log.
    EvenIfLive,
}

/// A serialized delta line waiting in the write buffer.
#[derive(Debug)]
struct BufferedLine {
    buffered_at: Instant,
    line: String,
}

/// Result of validating a log file, including how many leading bytes hold
/// successfully parsed lines (so `acquire` can truncate a torn tail).
#[derive(Debug)]
struct ParsedLog {
    events: Vec<Event>,
    /// Byte length of the valid prefix: the end of the last successfully
    /// parsed line, including its trailing newline when present.
    valid_len: u64,
    /// False only when the last parsed line lacked a trailing newline (a torn
    /// write that cut exactly at the terminator).
    terminated: bool,
}

/// Single-writer, append-only handle on a mission's `events.jsonl`.
///
/// Constructed via [`EventLog::acquire`]; the lock file is released on drop.
#[derive(Debug)]
pub struct EventLog {
    mission_id: String,
    events_path: PathBuf,
    lock_path: PathBuf,
    file: File,
    /// Seq to assign to the next appended event.
    next_seq: u64,
    throttle: Duration,
    buffer: Vec<BufferedLine>,
}

impl EventLog {
    /// Acquire the single-writer lock for a mission and open its event log.
    ///
    /// Creates the mission directory tree (mission dir, `runs/`, `control/`)
    /// if missing. If the lock file already exists, the holder's liveness
    /// decides against the [`LockForce`] tier (see its matrix): a provably
    /// dead holder is always stolen; a live or indeterminate one fails with
    /// [`EngineError::LockHeld`] naming the holder's pid unless the tier
    /// permits the steal. Existing events are loaded to resume the seq
    /// counter and to verify `mission_id` matches the log. Any torn final
    /// line left by a crash is repaired (truncated, or newline-terminated if
    /// the line itself is intact) before the append handle opens, so new
    /// events never glue onto a partial line.
    ///
    /// The lock file records two lines — `<pid>` and the acquire time as unix
    /// epoch seconds — so a later acquire can detect pid reuse (a holder
    /// process that STARTED after the lock was acquired cannot be the engine
    /// that wrote it). The legacy one-line pid-only format is still accepted;
    /// it just forgoes reuse detection.
    pub fn acquire(
        paths: &MissionPaths,
        mission_id: &str,
        throttle: Duration,
        force: LockForce,
    ) -> Result<EventLog> {
        std::fs::create_dir_all(paths.mission_dir())?;
        std::fs::create_dir_all(paths.runs_dir())?;
        std::fs::create_dir_all(paths.control_dir())?;

        let lock_path = paths.lock_file();
        let mut lock_file = match OpenOptions::new().write(true).create_new(true).open(&lock_path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let info = read_lock_info(&lock_path);
                match (probe_liveness(&info), force) {
                    // A provably-dead holder is stale (e.g. the engine was
                    // Ctrl-C'd — SIGINT skips destructors — or its pid was
                    // provably reused): steal it at every tier without
                    // demanding --force-lock.
                    (LockLiveness::Dead, _) => {
                        tracing::warn!(
                            lock = %lock_path.display(),
                            holder = %info.holder,
                            "stale engine lock (holder is dead); taking over"
                        );
                    }
                    // Not provably dead and no force: refuse. Same message
                    // whether the holder is alive or indeterminate — without
                    // force the distinction changes nothing for the operator.
                    (LockLiveness::Unknown | LockLiveness::Alive, LockForce::No) => {
                        return Err(EngineError::LockHeld(format!(
                            "lock file {} exists (held by pid {}); if that process \
                             is truly gone, re-run with --force-lock",
                            lock_path.display(),
                            info.holder
                        )));
                    }
                    // Indeterminate liveness (unparseable pid, non-unix
                    // platform): --force-lock keeps its historical meaning
                    // and steals.
                    (LockLiveness::Unknown, LockForce::IfNotLive | LockForce::EvenIfLive) => {
                        tracing::warn!(
                            lock = %lock_path.display(),
                            holder = %info.holder,
                            "forced takeover of a lock whose holder's liveness \
                             cannot be determined"
                        );
                    }
                    // A provably ALIVE holder survives --force-lock: this is
                    // exactly how an operator who believes a long run is
                    // "stuck" would otherwise corrupt it.
                    (LockLiveness::Alive, LockForce::IfNotLive) => {
                        return Err(EngineError::LockHeld(format!(
                            "lock file {} is held by pid {}, and that process is \
                             ALIVE — refusing --force-lock. Identify it with \
                             `ps -p {}`; pass --dangerously-steal-live-lock ONLY \
                             if you are certain it is a zombie or foreign process \
                             and not a running kranz engine (two engines on one \
                             mission corrupt its event log)",
                            lock_path.display(),
                            info.holder,
                            info.holder
                        )));
                    }
                    (LockLiveness::Alive, LockForce::EvenIfLive) => {
                        tracing::warn!(
                            lock = %lock_path.display(),
                            holder = %info.holder,
                            "DANGEROUS: stealing the mission lock from a LIVE \
                             process at operator request \
                             (--dangerously-steal-live-lock); if that process is \
                             a kranz engine, two engines now write one event log"
                        );
                    }
                }
                std::fs::remove_file(&lock_path)?;
                OpenOptions::new().write(true).create_new(true).open(&lock_path)?
            }
            Err(e) => return Err(e.into()),
        };

        // From here on we hold the lock; release it if the rest of the
        // acquisition fails so a failed open doesn't strand the mission.
        let mut open = || -> Result<EventLog> {
            let acquired_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            lock_file
                .write_all(format!("{}\n{}\n", std::process::id(), acquired_secs).as_bytes())?;
            lock_file.flush()?;

            let events_path = paths.events_file();
            let last_seq = if events_path.exists() {
                let parsed = Self::parse_log(&events_path)?;
                if let Some(first) = parsed.events.first() {
                    if first.mission_id != mission_id {
                        return Err(EngineError::InvalidState(format!(
                            "event log {} belongs to mission '{}', not '{}'",
                            events_path.display(),
                            first.mission_id,
                            mission_id
                        )));
                    }
                }
                // Repair torn writes from a crashed predecessor BEFORE opening
                // the append handle. A torn final line is tolerated on read,
                // but if left in place the next append glues onto it; once a
                // further event lands the spliced garbage is no longer final
                // and every read fails with LogCorruption forever.
                let file_len = std::fs::metadata(&events_path)?.len();
                if parsed.valid_len < file_len {
                    // Unparseable garbage past the last good line: cut it off.
                    let repair = OpenOptions::new().write(true).open(&events_path)?;
                    repair.set_len(parsed.valid_len)?;
                    repair.sync_data()?;
                } else if !parsed.terminated {
                    // The final line parsed but the tear ate its trailing
                    // newline; terminate it so the next append starts fresh.
                    let mut repair = OpenOptions::new().append(true).open(&events_path)?;
                    repair.write_all(b"\n")?;
                    repair.sync_data()?;
                }
                parsed.events.last().map(|e| e.seq).unwrap_or(0)
            } else {
                0
            };

            let file = OpenOptions::new().append(true).create(true).open(&events_path)?;
            Ok(EventLog {
                mission_id: mission_id.to_string(),
                events_path,
                lock_path: lock_path.clone(),
                file,
                next_seq: last_seq + 1,
                throttle,
                buffer: Vec::new(),
            })
        };

        match open() {
            Ok(log) => Ok(log),
            Err(e) => {
                let _ = std::fs::remove_file(&lock_path);
                Err(e)
            }
        }
    }

    /// Mission id this log was acquired for.
    pub fn mission_id(&self) -> &str {
        &self.mission_id
    }

    /// Seq of the last appended (or loaded) event; 0 for a fresh log.
    pub fn last_seq(&self) -> u64 {
        self.next_seq - 1
    }

    /// Path of the underlying `events.jsonl`.
    pub fn events_path(&self) -> &Path {
        &self.events_path
    }

    /// Append one event: assigns the next seq and the current timestamp,
    /// serializes to a single JSON line, and returns a clone of the stored
    /// event so the caller can broadcast it.
    ///
    /// Durability: lifecycle events drain any buffered deltas first (file
    /// order == append order), then write + flush + fsync. Stream deltas are
    /// buffered and drained once the oldest buffered delta exceeds the
    /// throttle age.
    pub fn append(&mut self, kind: EventKind) -> Result<Event> {
        let event = Event {
            seq: self.next_seq,
            ts: Utc::now(),
            mission_id: self.mission_id.clone(),
            kind,
        };
        let mut line = serde_json::to_string(&event)?;
        line.push('\n');

        if event.kind.is_stream_delta() {
            self.buffer.push(BufferedLine { buffered_at: Instant::now(), line });
            let oldest = self.buffer.first().expect("just pushed").buffered_at;
            if oldest.elapsed() >= self.throttle {
                self.drain_buffer()?;
            }
        } else {
            self.drain_buffer()?;
            self.file.write_all(line.as_bytes())?;
            self.file.flush()?;
            self.file.sync_data()?;
        }

        self.next_seq += 1;
        Ok(event)
    }

    /// Write any buffered deltas out to the file (no fsync — deltas are
    /// recoverable).
    pub fn flush(&mut self) -> Result<()> {
        self.drain_buffer()?;
        self.file.flush()?;
        Ok(())
    }

    fn drain_buffer(&mut self) -> Result<()> {
        for buffered in self.buffer.drain(..) {
            self.file.write_all(buffered.line.as_bytes())?;
        }
        Ok(())
    }

    // -- readers (no lock required; used by the server/CLI to tail) ---------

    /// Read and validate the full event log at `path`.
    ///
    /// Seq must start at 1 and increase by exactly 1; any gap or duplicate is
    /// [`EngineError::LogCorruption`]. An unparseable FINAL line is a torn
    /// write from a crash and is dropped with a warning; an unparseable line
    /// anywhere else is corruption.
    pub fn read_events(path: &Path) -> Result<Vec<Event>> {
        Ok(Self::parse_log(path)?.events)
    }

    /// Parse and validate the log at `path`, tracking how many leading bytes
    /// form the valid prefix so [`EventLog::acquire`] can truncate torn tails.
    ///
    /// Works on raw bytes (decoding each line lossily) because a torn write
    /// can split a multi-byte UTF-8 character, which must not render the
    /// whole log unreadable.
    fn parse_log(path: &Path) -> Result<ParsedLog> {
        let bytes = std::fs::read(path)?;

        let mut events = Vec::new();
        let mut valid_len: usize = 0;
        let mut terminated = true;
        let mut offset: usize = 0;
        let mut line_no: usize = 0;
        while offset < bytes.len() {
            line_no += 1;
            let rest = &bytes[offset..];
            let (line_end, step) = match rest.iter().position(|&b| b == b'\n') {
                Some(nl) => (nl, nl + 1),
                None => (rest.len(), rest.len()),
            };
            let is_final = offset + step == bytes.len();
            let line = String::from_utf8_lossy(&rest[..line_end]);
            let event: Event = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(err) => {
                    if is_final {
                        tracing::warn!(
                            path = %path.display(),
                            line = line_no,
                            error = %err,
                            "dropping unparseable final event line (torn write)"
                        );
                        break;
                    }
                    return Err(EngineError::LogCorruption(format!(
                        "unparseable event at {}:{}: {err}",
                        path.display(),
                        line_no
                    )));
                }
            };
            let expected = events.len() as u64 + 1;
            if event.seq != expected {
                return Err(EngineError::LogCorruption(format!(
                    "seq discontinuity at {}:{}: expected {expected}, found {}",
                    path.display(),
                    line_no,
                    event.seq
                )));
            }
            events.push(event);
            offset += step;
            valid_len = offset;
            terminated = step > line_end;
        }
        Ok(ParsedLog { events, valid_len: valid_len as u64, terminated })
    }

    /// Read events with `seq > after_seq` (WS reconnect / tailing). The whole
    /// log is still validated — a corrupt prefix must not go unnoticed.
    pub fn read_events_after(path: &Path, after_seq: u64) -> Result<Vec<Event>> {
        let mut events = Self::read_events(path)?;
        events.retain(|e| e.seq > after_seq);
        Ok(events)
    }
}

impl Drop for EventLog {
    fn drop(&mut self) {
        if let Err(e) = self.flush() {
            tracing::warn!(error = %e, "failed to flush event buffer on drop");
        }
        if let Err(e) = std::fs::remove_file(&self.lock_path) {
            if e.kind() != ErrorKind::NotFound {
                tracing::warn!(
                    path = %self.lock_path.display(),
                    error = %e,
                    "failed to remove lock file on drop"
                );
            }
        }
    }
}

/// Liveness verdict for the process recorded in a lock file.
///
/// INVARIANT: anything uncertain must NEVER report `Dead` — a false `Dead`
/// lets two engines write one log. `Dead` requires positive proof: ESRCH
/// from `kill(pid, 0)`, or a detected pid reuse (the holder process started
/// after the lock was acquired).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockLiveness {
    /// The holder is provably gone (or the pid provably belongs to a
    /// different, younger process): the lock is stale.
    Dead,
    /// The holder pid maps to a running process.
    Alive,
    /// Cannot tell (unparseable pid, non-unix platform, unexpected probe
    /// errno): honor the lock unless forced.
    Unknown,
}

/// What a lock file records about its holder.
#[derive(Debug)]
struct LockInfo {
    /// First line of the lock file, for error messages (`"unknown"` when the
    /// file is empty or unreadable).
    holder: String,
    /// The holder pid, when the first line parses as a positive i32.
    pid: Option<i32>,
    /// Unix epoch seconds at acquire time (second line). `None` for the
    /// legacy one-line pid-only format — reuse detection is then impossible
    /// and an alive pid is simply Alive, exactly as before the format grew
    /// the timestamp.
    acquired_secs: Option<u64>,
}

/// Best-effort parse of a lock file: `<pid>\n<acquired_unix_epoch_secs>`,
/// tolerating the legacy one-line pid-only format and arbitrary garbage.
fn read_lock_info(lock_path: &Path) -> LockInfo {
    let contents = std::fs::read_to_string(lock_path).unwrap_or_default();
    let mut lines = contents.lines();
    let first = lines.next().unwrap_or("").trim();
    let holder = if first.is_empty() { "unknown".to_string() } else { first.to_string() };
    let pid = first.parse::<i32>().ok().filter(|p| *p > 0);
    let acquired_secs = lines.next().and_then(|l| l.trim().parse::<u64>().ok());
    LockInfo { holder, pid, acquired_secs }
}

/// Probe the liveness of a lock file's recorded holder.
///
/// unix: `kill(pid, 0)` == 0 → Alive; EPERM → Alive (exists, not ours);
/// ESRCH → Dead; any other errno → Unknown. Unparseable or non-positive pid →
/// Unknown. Our OWN pid → Alive (we hold it). Non-unix → Unknown. An Alive
/// verdict is then screened for pid reuse (see [`alive_or_reused`]).
fn probe_liveness(info: &LockInfo) -> LockLiveness {
    let Some(pid) = info.pid else {
        return LockLiveness::Unknown;
    };
    if pid as u32 == std::process::id() {
        // We recorded this pid ourselves (double acquire) — or a dead engine
        // did and the OS recycled its pid onto us, which the reuse screen
        // can prove from the timestamps.
        return alive_or_reused(pid, info);
    }
    #[cfg(unix)]
    {
        // kill(pid, 0): 0 = alive; EPERM = alive but not ours; ESRCH = dead.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return alive_or_reused(pid, info);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM) => alive_or_reused(pid, info),
            Some(libc::ESRCH) => LockLiveness::Dead,
            _ => LockLiveness::Unknown,
        }
    }
    #[cfg(not(unix))]
    {
        LockLiveness::Unknown
    }
}

/// Screen an Alive pid for reuse: if the holder process STARTED after the
/// lock was acquired (+2s clock/rounding slack), the pid was recycled — the
/// process that wrote the lock is dead, so the verdict is `Dead`. Any
/// inability to determine the start time (legacy lock format, unsupported
/// platform, probe error) keeps the verdict `Alive` — a probe failure must
/// never demote Alive to Dead.
fn alive_or_reused(pid: i32, info: &LockInfo) -> LockLiveness {
    let Some(acquired) = info.acquired_secs else {
        return LockLiveness::Alive;
    };
    let Some(started) = process_start_epoch_secs(pid) else {
        return LockLiveness::Alive;
    };
    if started > acquired + 2 {
        tracing::warn!(
            pid,
            lock_acquired_epoch_secs = acquired,
            holder_started_epoch_secs = started,
            "lock holder pid was REUSED: the process started after the lock \
             was acquired, so the engine that wrote the lock is dead"
        );
        LockLiveness::Dead
    } else {
        LockLiveness::Alive
    }
}

/// Unix-epoch start time (seconds) of process `pid`, or `None` when it
/// cannot be determined on this platform. Callers treat `None` as "still
/// alive" — never as dead.
#[cfg(target_os = "linux")]
fn process_start_epoch_secs(pid: i32) -> Option<u64> {
    // /proc/<pid>/stat field 22 is starttime, in clock ticks since boot.
    // comm (field 2) is parenthesized and may itself contain spaces or ')',
    // so index from the LAST ')' — fields 3.. follow it.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    let start_ticks: u64 = rest.split_whitespace().nth(19)?.parse().ok()?;
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if hz <= 0 {
        return None;
    }
    // Boot time (unix epoch secs) from /proc/stat's btime line.
    let btime: u64 = std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("btime "))?
        .trim()
        .parse()
        .ok()?;
    Some(btime + start_ticks / hz as u64)
}

/// macOS: `ps -p <pid> -o etime=` prints the elapsed time since the process
/// started in the fixed, locale-independent `[[dd-]hh:]mm:ss` format;
/// start = now − elapsed. Second-granular, which the +2s reuse slack absorbs.
#[cfg(target_os = "macos")]
fn process_start_epoch_secs(pid: i32) -> Option<u64> {
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "etime="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let elapsed = parse_etime(String::from_utf8_lossy(&out.stdout).trim())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(now.saturating_sub(elapsed))
}

/// Everywhere else (windows, exotic unix): start time is not determinable,
/// so pid reuse cannot be proven and an alive holder stays Alive.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_start_epoch_secs(_pid: i32) -> Option<u64> {
    None
}

/// Parse `ps`'s ELAPSED (`etime`) format `[[dd-]hh:]mm:ss` into seconds.
#[cfg(any(target_os = "macos", test))]
fn parse_etime(s: &str) -> Option<u64> {
    let (days, rest) = match s.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0u64, s),
    };
    let mut parts = rest.split(':').rev();
    let secs: u64 = parts.next()?.parse().ok()?;
    let mins: u64 = parts.next()?.parse().ok()?;
    let hours: u64 = match parts.next() {
        Some(h) => h.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some(((days * 24 + hours) * 60 + mins) * 60 + secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etime_parses_all_shapes() {
        assert_eq!(parse_etime("00:03"), Some(3));
        assert_eq!(parse_etime("12:34"), Some(12 * 60 + 34));
        assert_eq!(parse_etime("01:02:03"), Some(3600 + 2 * 60 + 3));
        assert_eq!(parse_etime("2-01:02:03"), Some(2 * 86400 + 3600 + 2 * 60 + 3));
        assert_eq!(parse_etime(""), None);
        assert_eq!(parse_etime("garbage"), None);
        assert_eq!(parse_etime("1:2:3:4"), None);
    }

    #[test]
    fn lock_info_parses_both_formats() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("l");

        std::fs::write(&lock, "1234\n1700000000\n").unwrap();
        let info = read_lock_info(&lock);
        assert_eq!(info.holder, "1234");
        assert_eq!(info.pid, Some(1234));
        assert_eq!(info.acquired_secs, Some(1_700_000_000));

        // Legacy one-line format: pid known, acquire time unknown.
        std::fs::write(&lock, "1234").unwrap();
        let info = read_lock_info(&lock);
        assert_eq!(info.pid, Some(1234));
        assert_eq!(info.acquired_secs, None);

        // Garbage: nothing parseable, holder preserved for the message.
        std::fs::write(&lock, "not-a-pid\nnot-a-time").unwrap();
        let info = read_lock_info(&lock);
        assert_eq!(info.holder, "not-a-pid");
        assert_eq!(info.pid, None);
        assert_eq!(info.acquired_secs, None);

        // Non-positive pids are never probeable.
        std::fs::write(&lock, "-4\n1700000000").unwrap();
        assert_eq!(read_lock_info(&lock).pid, None);
    }
}
