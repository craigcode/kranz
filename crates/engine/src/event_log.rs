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

/// Write buffered lines to `sink` from the front, removing each line from
/// `buffer` only after it is successfully written. On the first write error
/// the failing line and everything after it stay in `buffer`, in original
/// order, so a later retry can pick up exactly where this call left off —
/// `Vec::drain` cannot do this since its guard discards not-yet-yielded
/// items if the iteration is cut short by `?`.
fn drain_lines<W: std::io::Write>(
    sink: &mut W,
    buffer: &mut Vec<BufferedLine>,
) -> std::io::Result<()> {
    while !buffer.is_empty() {
        sink.write_all(buffer[0].line.as_bytes())?;
        buffer.remove(0);
    }
    Ok(())
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
    /// The lock file records up to three lines — `<pid>`, the acquire time as
    /// unix epoch seconds (diagnostics only), and the holder's own process
    /// identity token — so a later acquire can detect pid reuse: a holder
    /// whose CURRENT token differs from the recorded one is not the process
    /// that wrote the lock. Tokens are compared for raw equality (see
    /// [`process_identity_token`]) — never via clock arithmetic, which
    /// wall-clock steps would poison. The legacy one- and two-line formats
    /// are still accepted; they just forgo reuse detection.
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
            Err(e) if e.kind() == ErrorKind::AlreadyExists => steal_lock(&lock_path, force)?,
            Err(e) => return Err(e.into()),
        };

        // From here on we hold the lock; release it if the rest of the
        // acquisition fails so a failed open doesn't strand the mission.
        let mut open = || -> Result<EventLog> {
            let acquired_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            // Line 1: pid. Line 2: acquire time (diagnostics only — reuse
            // detection is the token's job). Line 3: our own identity token,
            // where this platform can produce one; a probe that finds it
            // missing degrades to plain pid liveness, never to Dead.
            let mut lock_contents = format!("{}\n{}\n", std::process::id(), acquired_secs);
            if let Some(token) = process_identity_token(std::process::id() as i32) {
                lock_contents.push_str(&token);
                lock_contents.push('\n');
            }
            lock_file.write_all(lock_contents.as_bytes())?;
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
        drain_lines(&mut self.file, &mut self.buffer)?;
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
            tracing::warn!(
                error = %e,
                retained = self.buffer.len(),
                "failed to flush event buffer on drop; buffered deltas retained for a future drain"
            );
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

/// Contended-path acquire: decide whether the existing lock at `lock_path`
/// may be stolen under `force` and, if so, atomically replace it with a fresh
/// lock file owned by this process.
///
/// unix: the whole probe→remove→create sequence is serialized under an
/// exclusive `flock` on a sibling guard file (`<lock>.steal`). Unserialized,
/// two concurrent acquires can both judge the same stale holder Dead; the
/// slower one's `remove_file` then deletes the FASTER one's freshly created
/// lock and both end up holding. After winning the flock the lock is
/// re-attempted and re-probed from scratch — it may have been released, or
/// stolen and rewritten, while we waited.
///
/// non-unix: no guard, and that is acceptable — liveness is never probed
/// there (every verdict is Unknown, see [`probe_liveness`]), so the Dead
/// auto-steal path cannot trigger; steals happen only under explicit operator
/// force flags, which are deliberate one-off actions rather than the
/// concurrent-by-accident crash-recovery restarts the guard defends against.
fn steal_lock(lock_path: &Path, force: LockForce) -> Result<File> {
    #[cfg(unix)]
    let _guard = StealGuard::acquire(lock_path)?;

    loop {
        // The lock may have been RELEASED while we waited for the guard:
        // retry the clean create before probing anything.
        match OpenOptions::new().write(true).create_new(true).open(lock_path) {
            Ok(f) => return Ok(f),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }

        authorize_steal(lock_path, force)?;

        // Guarded steals never interleave here, but a rival acquire's FIRST
        // (unguarded) create attempt can still slip into the remove→create
        // window and win the freed slot. If it does, loop back and judge
        // THAT holder like any other — never surface the raw io collision.
        match std::fs::remove_file(lock_path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        }
        match OpenOptions::new().write(true).create_new(true).open(lock_path) {
            Ok(f) => return Ok(f),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

/// Probe the CURRENT holder recorded at `lock_path` and decide, against the
/// [`LockForce`] matrix, whether stealing is permitted: `Ok(())` authorizes
/// the steal, `Err(LockHeld)` refuses with operator guidance.
fn authorize_steal(lock_path: &Path, force: LockForce) -> Result<()> {
    let info = read_lock_info(lock_path);
    match (probe_liveness(&info), force) {
        // A provably-dead holder is stale (e.g. the engine was Ctrl-C'd —
        // SIGINT skips destructors — or its pid was provably reused): steal
        // it at every tier without demanding --force-lock.
        (LockLiveness::Dead, _) => {
            tracing::warn!(
                lock = %lock_path.display(),
                holder = %info.holder,
                "stale engine lock (holder is dead); taking over"
            );
            Ok(())
        }
        // Not provably dead and no force: refuse. Same message whether the
        // holder is alive or indeterminate — without force the distinction
        // changes nothing for the operator.
        (LockLiveness::Unknown | LockLiveness::Alive, LockForce::No) => {
            Err(EngineError::LockHeld(format!(
                "lock file {} exists (held by pid {}); if that process \
                 is truly gone, re-run with --force-lock",
                lock_path.display(),
                info.holder
            )))
        }
        // Indeterminate liveness (unparseable pid, non-unix platform):
        // --force-lock keeps its historical meaning and steals.
        (LockLiveness::Unknown, LockForce::IfNotLive | LockForce::EvenIfLive) => {
            tracing::warn!(
                lock = %lock_path.display(),
                holder = %info.holder,
                "forced takeover of a lock whose holder's liveness \
                 cannot be determined"
            );
            Ok(())
        }
        // A provably ALIVE holder survives --force-lock: this is exactly how
        // an operator who believes a long run is "stuck" would otherwise
        // corrupt it.
        (LockLiveness::Alive, LockForce::IfNotLive) => Err(EngineError::LockHeld(format!(
            "lock file {} is held by pid {}, and that process is \
             ALIVE — refusing --force-lock. Identify it with \
             `ps -p {}`; pass --dangerously-steal-live-lock ONLY \
             if you are certain it is a zombie or foreign process \
             and not a running kranz engine (two engines on one \
             mission corrupt its event log)",
            lock_path.display(),
            info.holder,
            info.holder
        ))),
        (LockLiveness::Alive, LockForce::EvenIfLive) => {
            tracing::warn!(
                lock = %lock_path.display(),
                holder = %info.holder,
                "DANGEROUS: stealing the mission lock from a LIVE \
                 process at operator request \
                 (--dangerously-steal-live-lock); if that process is \
                 a kranz engine, two engines now write one event log"
            );
            Ok(())
        }
    }
}

/// RAII serialization of the lock-steal sequence (unix): an exclusive
/// `flock` on a sibling guard file (`<lock>.steal`).
///
/// The guard file is best-effort removed on drop, so acquisition must defend
/// against the unlink race: a waiter can win the flock on an inode that was
/// unlinked (and possibly recreated) while it slept, which would serialize
/// nothing. After each flock win the held fd's identity is compared to
/// whatever the path names NOW; a mismatch retries on the fresh file.
///
/// `flock` contends across separate fds within one process too, so the guard
/// serializes racing threads exactly like racing processes.
#[cfg(unix)]
struct StealGuard {
    /// Held only for the flock; dropping (closing) it releases the lock.
    _file: File,
    path: PathBuf,
}

#[cfg(unix)]
impl StealGuard {
    fn acquire(lock_path: &Path) -> Result<StealGuard> {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::io::AsRawFd;
        let mut name = lock_path.file_name().unwrap_or_default().to_os_string();
        name.push(".steal");
        let path = lock_path.with_file_name(name);
        loop {
            // Contents are irrelevant (the file exists only to be flock'd),
            // but be explicit that nothing is truncated.
            let file =
                OpenOptions::new().write(true).create(true).truncate(false).open(&path)?;
            loop {
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                    break;
                }
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::EINTR) {
                    return Err(err.into());
                }
            }
            let held = file.metadata()?;
            match std::fs::metadata(&path) {
                Ok(m) if m.dev() == held.dev() && m.ino() == held.ino() => {
                    return Ok(StealGuard { _file: file, path });
                }
                // The guard file was unlinked (and possibly recreated) while
                // we waited: this flock guards a dead inode and serializes
                // nothing. Retry on whatever the path names now.
                _ => continue,
            }
        }
    }
}

#[cfg(unix)]
impl Drop for StealGuard {
    fn drop(&mut self) {
        // Best-effort cleanup; the identity re-check in acquire() keeps this
        // safe against waiters still blocked on the removed inode.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Whether the holder recorded in the lock file at `lock_path` is still
/// alive — the ONE liveness query shared by every subsystem that asks "is
/// this mission's engine running?" (the event-log acquire path itself, queue
/// busy checks, hygiene sweeps), so no caller can diverge from the canonical
/// lock-file format.
///
/// Missing lock file → `false` (nothing holds it). A provably-dead holder
/// (ESRCH, or a token-proven pid reuse) → `false`. Everything else — alive
/// holder, unparseable/garbage lock file, unprobeable platform — → `true`:
/// conservative, because a false "alive" merely delays a queued mission or
/// spares a directory from cleaning, while a false "dead" runs two engines
/// on one working tree.
pub fn lock_holder_is_alive(lock_path: &Path) -> bool {
    if !lock_path.exists() {
        return false;
    }
    let info = read_lock_info(lock_path);
    match probe_liveness(&info) {
        LockLiveness::Dead => false,
        LockLiveness::Alive | LockLiveness::Unknown => true,
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
    /// Unix epoch seconds at acquire time (second line): diagnostics only.
    /// `None` for the legacy one-line pid-only format.
    acquired_secs: Option<u64>,
    /// Process identity token the holder recorded for ITSELF at acquire
    /// (third line, see [`process_identity_token`]). `None` for the older
    /// one-/two-line formats and on platforms that cannot produce one —
    /// reuse detection is then impossible and an alive pid is simply Alive.
    token: Option<String>,
}

/// Best-effort parse of a lock file:
/// `<pid>\n<acquired_unix_epoch_secs>\n<identity_token>`, tolerating the
/// legacy one- and two-line formats and arbitrary garbage.
fn read_lock_info(lock_path: &Path) -> LockInfo {
    let contents = std::fs::read_to_string(lock_path).unwrap_or_default();
    let mut lines = contents.lines();
    let first = lines.next().unwrap_or("").trim();
    let holder = if first.is_empty() { "unknown".to_string() } else { first.to_string() };
    let pid = first.parse::<i32>().ok().filter(|p| *p > 0);
    let acquired_secs = lines.next().and_then(|l| l.trim().parse::<u64>().ok());
    let token = lines.next().map(str::trim).filter(|t| !t.is_empty()).map(String::from);
    LockInfo { holder, pid, acquired_secs, token }
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

/// Screen an Alive pid for reuse by comparing process identity tokens: the
/// token the holder recorded for ITSELF at acquire (lock line 3) against the
/// token of whatever occupies that pid NOW. Tokens are compared for raw
/// equality — no clocks, no slack — so wall-clock steps (NTP corrections,
/// manual resets) can never misclassify a live holder as Dead the way the
/// old start-time-vs-acquire-time arithmetic could. Different tokens prove
/// the pid was recycled within this boot, or the machine rebooted; the
/// process that wrote the lock is dead either way, so the verdict is `Dead`.
/// Any inability to obtain a token (legacy one-/two-line lock file,
/// unsupported platform, probe error) keeps the verdict `Alive` — uncertainty
/// must never demote Alive to Dead.
fn alive_or_reused(pid: i32, info: &LockInfo) -> LockLiveness {
    let Some(recorded) = info.token.as_deref() else {
        return LockLiveness::Alive;
    };
    let Some(current) = process_identity_token(pid) else {
        return LockLiveness::Alive;
    };
    if current == recorded {
        LockLiveness::Alive
    } else {
        tracing::warn!(
            pid,
            recorded_token = recorded,
            current_token = %current,
            lock_acquired_epoch_secs = ?info.acquired_secs,
            "lock holder pid was REUSED: the process now at this pid is not \
             the one that recorded the lock, so the engine that wrote the \
             lock is dead"
        );
        LockLiveness::Dead
    }
}

/// Platform-opaque identity token for process `pid`, or `None` when this
/// platform cannot produce one. Two readings of a LIVE process's token are
/// byte-identical because both derive from the same immutable kernel state
/// (boot + spawn identity), never from wall-clock arithmetic — so tokens are
/// compared for raw equality only.
///
/// linux: `"<boot_id>:<starttime_ticks>"`. `/proc/sys/kernel/random/boot_id`
/// is fixed per boot; `/proc/<pid>/stat` field 22 (starttime) is a
/// CLOCK_MONOTONIC-based tick count fixed for the life of the process. comm
/// (field 2) is parenthesized and may itself contain spaces or ')', so
/// fields 3.. are indexed from after the LAST ')'.
#[cfg(target_os = "linux")]
fn process_identity_token(pid: i32) -> Option<String> {
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let boot_id = boot_id.trim();
    if boot_id.is_empty() {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    let start_ticks = rest.split_whitespace().nth(19)?;
    start_ticks.parse::<u64>().ok()?; // reject garbage rather than record it
    Some(format!("{boot_id}:{start_ticks}"))
}

/// macOS: `ps -p <pid> -o lstart=` prints an absolute spawn timestamp
/// rendered from the kernel's stored `p_starttime`. Locale and timezone are
/// pinned so the acquire-time and probe-time renderings of the SAME stored
/// value are byte-identical; the strings are compared for equality, never
/// parsed back into clock arithmetic.
#[cfg(target_os = "macos")]
fn process_identity_token(pid: i32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// Everywhere else (windows, exotic unix): no identity token, so pid reuse
/// cannot be proven and an alive holder stays Alive.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_identity_token(_pid: i32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_info_parses_all_formats() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("l");

        // Current three-line format: pid, acquire time, identity token.
        std::fs::write(&lock, "1234\n1700000000\nabcd-boot-id:5678\n").unwrap();
        let info = read_lock_info(&lock);
        assert_eq!(info.holder, "1234");
        assert_eq!(info.pid, Some(1234));
        assert_eq!(info.acquired_secs, Some(1_700_000_000));
        assert_eq!(info.token.as_deref(), Some("abcd-boot-id:5678"));

        // Two-line format: token unknown, reuse screen impossible.
        std::fs::write(&lock, "1234\n1700000000\n").unwrap();
        let info = read_lock_info(&lock);
        assert_eq!(info.pid, Some(1234));
        assert_eq!(info.acquired_secs, Some(1_700_000_000));
        assert_eq!(info.token, None);

        // An EMPTY third line (a platform with no token) is the same as none.
        std::fs::write(&lock, "1234\n1700000000\n\n").unwrap();
        assert_eq!(read_lock_info(&lock).token, None);

        // Legacy one-line format: pid known, everything else unknown.
        std::fs::write(&lock, "1234").unwrap();
        let info = read_lock_info(&lock);
        assert_eq!(info.pid, Some(1234));
        assert_eq!(info.acquired_secs, None);
        assert_eq!(info.token, None);

        // Garbage: nothing parseable, holder preserved for the message.
        std::fs::write(&lock, "not-a-pid\nnot-a-time").unwrap();
        let info = read_lock_info(&lock);
        assert_eq!(info.holder, "not-a-pid");
        assert_eq!(info.pid, None);
        assert_eq!(info.acquired_secs, None);
        assert_eq!(info.token, None);

        // Non-positive pids are never probeable.
        std::fs::write(&lock, "-4\n1700000000").unwrap();
        assert_eq!(read_lock_info(&lock).pid, None);
    }

    /// Two readings of a live process's identity token must be byte-identical
    /// — the entire reuse screen rests on this. (The old design reconstructed
    /// a wall-clock start time on every probe, which clock steps shifted.)
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn identity_token_is_stable_for_a_live_process() {
        let pid = std::process::id() as i32;
        let a = process_identity_token(pid).expect("own token must be obtainable");
        let b = process_identity_token(pid).expect("own token must be obtainable");
        assert_eq!(a, b, "token readings of the same live process must match");
        assert!(!a.is_empty() && !a.contains('\n'), "token must be a single non-empty line: {a:?}");
    }

    /// A pid that provably has no process yields no token (dead pids have no
    /// /proc entry on linux and no ps row on macOS) — never a fabricated one.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn identity_token_of_a_dead_pid_is_none() {
        assert_eq!(process_identity_token(i32::MAX), None);
    }

    /// Token mismatch on a lock that records OUR pid: the recorder was a
    /// different process whose pid the OS recycled onto us — provably Dead.
    /// Matching token: an ordinary double acquire — Alive.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn own_pid_reuse_is_decided_by_token_equality() {
        let pid = std::process::id() as i32;
        let own = process_identity_token(pid).expect("own token must be obtainable");

        let info = LockInfo {
            holder: pid.to_string(),
            pid: Some(pid),
            acquired_secs: Some(0),
            token: Some(own.clone()),
        };
        assert_eq!(probe_liveness(&info), LockLiveness::Alive);

        let info = LockInfo { token: Some(format!("{own}-not")), ..info };
        assert_eq!(probe_liveness(&info), LockLiveness::Dead);
    }

    /// A writer that succeeds for the first `fail_at` writes and then always
    /// errors, recording every line it actually wrote.
    struct FlakyWriter {
        fail_at: usize,
        writes: Vec<String>,
    }

    impl std::io::Write for FlakyWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.writes.len() >= self.fail_at {
                return Err(std::io::Error::other("simulated write failure"));
            }
            self.writes.push(String::from_utf8_lossy(buf).into_owned());
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn buffered(n: usize) -> Vec<BufferedLine> {
        (0..n)
            .map(|i| BufferedLine { buffered_at: Instant::now(), line: format!("line-{i}\n") })
            .collect()
    }

    #[test]
    fn drain_retains_unwritten_deltas_on_write_failure() {
        let k = 3;
        let n = 7;
        let mut writer = FlakyWriter { fail_at: k, writes: Vec::new() };
        let mut buffer = buffered(n);

        let result = drain_lines(&mut writer, &mut buffer);

        assert!(result.is_err(), "drain must surface the write error");
        assert_eq!(
            writer.writes,
            (0..k).map(|i| format!("line-{i}\n")).collect::<Vec<_>>(),
            "exactly the first k lines must have been written, in order"
        );
        assert_eq!(
            buffer.iter().map(|b| b.line.clone()).collect::<Vec<_>>(),
            (k..n).map(|i| format!("line-{i}\n")).collect::<Vec<_>>(),
            "the remaining lines, including the one that failed, must stay buffered in order"
        );

        // A subsequent drain with a working writer must recover the retained
        // lines successfully — nothing is permanently lost.
        let mut retry_writer = FlakyWriter { fail_at: usize::MAX, writes: Vec::new() };
        let retry_result = drain_lines(&mut retry_writer, &mut buffer);
        assert!(retry_result.is_ok());
        assert!(buffer.is_empty());
        assert_eq!(
            retry_writer.writes,
            (k..n).map(|i| format!("line-{i}\n")).collect::<Vec<_>>()
        );
    }
}
