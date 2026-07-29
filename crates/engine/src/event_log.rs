//! Append-only JSONL event log (plan §4.3) — the single source of truth.
//!
//! One engine process owns `events.jsonl` at a time, guarded by a lock file
//! (`events.jsonl.lock`) rather than POSIX advisory locks so Windows stays
//! first-class. Lifecycle events are flushed + fsynced per append; stream
//! deltas (`worker.message`) are buffered in memory and drained by age
//! (throttle) — checked on the next append, or on demand via
//! [`EventLog::flush_if_due`] — by the next lifecycle append, by explicit
//! [`EventLog::flush`], or on drop. Losing buffered deltas on a crash is
//! recoverable; losing a lifecycle event is not, hence the asymmetry.

use crate::error::{EngineError, Result};
use crate::events::{Event, EventKind};
use crate::paths::MissionPaths;
use crate::scrub::SecretFinding;
use chrono::Utc;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How aggressively [`EventLog::acquire`] may steal an existing lock.
///
/// A holder that is provably DEAD is always stolen (a stale lock from a
/// crashed engine), regardless of tier. The tiers only govern holders that
/// are alive or of indeterminate liveness. Automatic Dead detection is
/// unix-only (`kill(pid, 0)` returning `ESRCH`, plus the own-pid
/// token-reuse screen on any platform): on non-unix targets only the
/// own-pid token-reuse screen can ever yield Dead, so recovering the lock
/// from a foreign crashed holder there always requires an explicit force
/// tier (`--force-lock` / `--dangerously-steal-live-lock`).
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
    /// Generation written into the lock file at acquire. Re-checked on every
    /// append so a stolen-from process fails closed instead of dual-writing.
    lock_generation: u64,
    /// Identity token written into the lock file at acquire (when the
    /// platform can produce one). Drop deletes the lock file only when the
    /// on-disk generation (and token, when present) still match — so a
    /// stolen-from teardown cannot wipe the stealer's lock.
    lock_token: Option<String>,
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
    /// if missing — no-follow: a symlinked `.kranz`/`missions`/mission dir or
    /// runtime file is refused (P1 mission-path-no-follow), never followed
    /// into another repository's tree. If the lock file already exists, the
    /// holder's liveness decides against the [`LockForce`] tier (see its
    /// matrix): a provably
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
        // Resolve the mission tree no-follow (P1 mission-path-no-follow): a
        // symlinked component or runtime file is refused before any create or
        // open — an append through a symlink would write another repo's tree.
        let mission_dir = paths.open_mission_dir_nofollow(true)?;
        crate::paths::create_real_subdir(&mission_dir, "runs", &paths.runs_dir())?;
        crate::paths::create_real_subdir(&mission_dir, "control", &paths.control_dir())?;
        crate::paths::ensure_absent_or_regular_file(&paths.lock_file())?;
        crate::paths::ensure_absent_or_regular_file(&paths.events_file())?;

        let lock_path = paths.lock_file();
        let (mut lock_file, lock_generation) = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(f) => (f, 0u64),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let (f, prev_gen) = steal_lock(&lock_path, force)?;
                (f, prev_gen.saturating_add(1))
            }
            Err(e) => return Err(e.into()),
        };

        // From here on we hold the lock; release it if the rest of the
        // acquisition fails so a failed open doesn't strand the mission.
        let mut open = || -> Result<EventLog> {
            // Line 1: pid. Line 2: acquire time (diagnostics only — reuse
            // detection is the token's job). Line 3: our own identity token,
            // where this platform can produce one; a probe that finds it
            // missing degrades to plain pid liveness, never to Dead.
            // Line 4: generation — increments on every steal so a stolen-from
            // process fails closed on its next append.
            lock_file.write_all(
                current_lock_holder_record_with_generation(lock_generation).as_bytes(),
            )?;
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

            let file = OpenOptions::new()
                .append(true)
                .create(true)
                .open(&events_path)?;
            Ok(EventLog {
                mission_id: mission_id.to_string(),
                events_path,
                lock_path: lock_path.clone(),
                lock_generation,
                lock_token: process_identity_token(std::process::id() as i32),
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
    /// event so the caller can broadcast it. If this boundary redacts any
    /// string payload, `secret.redacted` audit events are appended immediately
    /// after the sanitized event.
    ///
    /// Durability: lifecycle events drain any buffered deltas first (file
    /// order == append order), then write + flush + fsync. Stream deltas are
    /// buffered and drained once the oldest buffered delta exceeds the
    /// throttle age — checked here on each append, or on demand (without
    /// waiting for another append) via [`EventLog::flush_if_due`].
    pub fn append(&mut self, kind: EventKind) -> Result<Event> {
        Ok(self.append_with_redaction_audits(kind)?.0)
    }

    /// Append one event and any required `secret.redacted` audit events.
    /// Returns the sanitized primary event plus the audit events that followed
    /// it, so callers that maintain snapshots can fold the same sequence.
    pub fn append_with_redaction_audits(&mut self, kind: EventKind) -> Result<(Event, Vec<Event>)> {
        let (event, redactions) = self.append_redacting(kind)?;
        let mut audits = Vec::new();
        for finding in redactions {
            let (audit, _) = self.append_redacting(EventKind::SecretRedacted {
                rule_id: finding.rule_id,
                fingerprint: finding.fingerprint,
                location: finding.location,
            })?;
            audits.push(audit);
        }
        Ok((event, audits))
    }

    /// Append one event after scanning/redacting string payloads. Returns the
    /// sanitized event plus secret findings (fingerprints only, never values).
    pub fn append_redacting(&mut self, kind: EventKind) -> Result<(Event, Vec<SecretFinding>)> {
        // Fail closed if another process stole the lock out from under us —
        // otherwise two engines dual-write one log (seq gaps / corruption).
        let current_gen = read_lock_info(&self.lock_path).generation.unwrap_or(0);
        if current_gen != self.lock_generation {
            return Err(EngineError::LockHeld(format!(
                "event log lock for '{}' was stolen (generation {} → {}); refusing append",
                self.mission_id, self.lock_generation, current_gen
            )));
        }

        let event = Event {
            seq: self.next_seq,
            ts: Utc::now(),
            mission_id: self.mission_id.clone(),
            kind,
        };
        let mut value = serde_json::to_value(&event)?;
        let findings = crate::scrub::scrub_json_value(&mut value, "event");
        let event: Event = serde_json::from_value(value)?;
        let mut line = serde_json::to_string(&event)?;
        line.push('\n');

        if event.kind.is_stream_delta() {
            self.buffer.push(BufferedLine {
                buffered_at: Instant::now(),
                line,
            });
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
        Ok((event, findings))
    }

    /// Write any buffered deltas out to the file (no fsync — deltas are
    /// recoverable).
    pub fn flush(&mut self) -> Result<()> {
        self.drain_buffer()?;
        self.file.flush()?;
        Ok(())
    }

    /// Elapsed time since the OLDEST buffered delta, or `None` when the
    /// buffer is empty.
    pub fn buffer_age(&self) -> Option<Duration> {
        self.buffer.first().map(|b| b.buffered_at.elapsed())
    }

    /// Drain the buffer to the file, WITHOUT waiting for another [`append`]
    /// call, if it is non-empty and has aged past `throttle`. Gives idle
    /// missions (waiting on an approval gate, worker stopped) a wall-clock-
    /// driven flush instead of leaving deltas buffered indefinitely.
    ///
    /// [`append`]: EventLog::append
    pub fn flush_if_due(&mut self) -> Result<bool> {
        match self.buffer_age() {
            Some(age) if age >= self.throttle => {
                self.drain_buffer()?;
                self.file.flush()?;
                Ok(true)
            }
            _ => Ok(false),
        }
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
        // A symlinked log file is refused (never read through into another
        // tree); an absent one errors NotFound from the read below, as
        // before. `O_NOFOLLOW` on unix — no check-then-open window.
        use std::io::Read;
        let mut bytes = Vec::new();
        crate::paths::open_read_nofollow(path)?.read_to_end(&mut bytes)?;

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
            if let Some(first_mission_id) = events.first().map(|e: &Event| &e.mission_id) {
                if event.mission_id != *first_mission_id {
                    return Err(EngineError::LogCorruption(format!(
                        "mission_id mismatch at {}:{}: expected '{}' (from first event), found '{}'",
                        path.display(),
                        line_no,
                        first_mission_id,
                        event.mission_id
                    )));
                }
            }
            events.push(event);
            offset += step;
            valid_len = offset;
            terminated = step > line_end;
        }
        Ok(ParsedLog {
            events,
            valid_len: valid_len as u64,
            terminated,
        })
    }

    /// Read events with `seq > after_seq` (WS reconnect / tailing). The whole
    /// log is still validated — a corrupt prefix must not go unnoticed.
    pub fn read_events_after(path: &Path, after_seq: u64) -> Result<Vec<Event>> {
        let mut events = Self::read_events(path)?;
        events.retain(|e| e.seq > after_seq);
        Ok(events)
    }

    /// Read the events on the last `max_bytes` of the log WITHOUT reading or
    /// validating the full file — O(tail) I/O for hot callers that only need
    /// trailing facts (e.g. "has a terminal lifecycle event been appended?").
    ///
    /// The window is aligned to the first complete line inside it, and
    /// unparseable lines (a torn final write) are skipped rather than treated
    /// as corruption — callers that need validation use
    /// [`EventLog::read_events`]. Returns the whole log when the file fits
    /// inside the window.
    pub fn read_tail_events(path: &Path, max_bytes: u64) -> Result<Vec<Event>> {
        use std::io::{Read, Seek, SeekFrom};
        // Same no-follow refusal as `parse_log`: never tail through a
        // symlink, `O_NOFOLLOW` on unix so there is no check-then-open window.
        let mut file = crate::paths::open_read_nofollow(path)?;
        let len = file.metadata()?.len();
        let window_start = len.saturating_sub(max_bytes);
        // Read from one byte BEFORE the window: when the window happens to
        // start exactly on a line boundary, that extra byte is the previous
        // line's '\n', so the drop-through-first-'\n' below discards zero
        // content bytes instead of eating one complete in-window line.
        let start = window_start.saturating_sub(1);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::with_capacity((len - start) as usize);
        file.read_to_end(&mut bytes)?;
        let mut slice = bytes.as_slice();
        if window_start > 0 {
            // Drop the line the window cut into; its head is outside.
            match slice.iter().position(|&b| b == b'\n') {
                Some(nl) => slice = &slice[nl + 1..],
                None => return Ok(Vec::new()),
            }
        }
        Ok(slice
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_str(&String::from_utf8_lossy(line)).ok())
            .collect())
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
        // Only remove the lock if we still own it. After a --force-lock steal
        // the stolen-from process's teardown would otherwise delete the
        // stealer's lock file; the stealer would then see generation 0 and
        // fail closed on its next append (MutationLock in queue.rs uses the
        // same still-ours check).
        let info = read_lock_info(&self.lock_path);
        let generation_matches = info.generation.unwrap_or(0) == self.lock_generation;
        let token_matches = match (&self.lock_token, &info.token) {
            (Some(ours), Some(theirs)) => ours == theirs,
            // Platforms/legacy files without a token: generation alone is
            // the ownership fence.
            _ => true,
        };
        if generation_matches && token_matches {
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
fn steal_lock(lock_path: &Path, force: LockForce) -> Result<(File, u64)> {
    #[cfg(unix)]
    let _guard = StealGuard::acquire(lock_path)?;

    loop {
        // The lock may have been RELEASED while we waited for the guard:
        // retry the clean create before probing anything.
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(f) => return Ok((f, 0)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }

        authorize_steal(lock_path, force)?;
        let prev_gen = read_lock_info(lock_path).generation.unwrap_or(0);

        // Guarded steals never interleave here, but a rival acquire's FIRST
        // (unguarded) create attempt can still slip into the remove→create
        // window and win the freed slot. If it does, loop back and judge
        // THAT holder like any other — never surface the raw io collision.
        match std::fs::remove_file(lock_path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        }
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(f) => return Ok((f, prev_gen)),
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
            let file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)?;
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

/// Lock-file contents for a lock held by the current process, in the same
/// format parsed by [`lock_holder_is_alive`].
///
/// Format (four lines):
/// `<pid>\n<acquired_unix_secs>\n<identity_token>\n<generation>\n`
///
/// `generation` increments on every steal so a stolen-from process can detect
/// that its append handle is no longer authoritative.
pub fn current_lock_holder_record() -> String {
    current_lock_holder_record_with_generation(0)
}

fn current_lock_holder_record_with_generation(generation: u64) -> String {
    let acquired_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut contents = format!("{}\n{}\n", std::process::id(), acquired_secs);
    if let Some(token) = process_identity_token(std::process::id() as i32) {
        contents.push_str(&token);
        contents.push('\n');
    } else {
        contents.push('\n');
    }
    contents.push_str(&format!("{generation}\n"));
    contents
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
    /// Steal generation (fourth line). `None` for legacy lock files — treated
    /// as generation 0 by append ownership checks.
    generation: Option<u64>,
}

/// Best-effort parse of a lock file:
/// `<pid>\n<acquired_unix_epoch_secs>\n<identity_token>\n<generation>`,
/// tolerating the legacy one-/two-/three-line formats and arbitrary garbage.
fn read_lock_info(lock_path: &Path) -> LockInfo {
    let contents = std::fs::read_to_string(lock_path).unwrap_or_default();
    let mut lines = contents.lines();
    let first = lines.next().unwrap_or("").trim();
    let holder = if first.is_empty() {
        "unknown".to_string()
    } else {
        first.to_string()
    };
    let pid = first.parse::<i32>().ok().filter(|p| *p > 0);
    let acquired_secs = lines.next().and_then(|l| l.trim().parse::<u64>().ok());
    let token = lines
        .next()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from);
    let generation = lines.next().and_then(|l| l.trim().parse::<u64>().ok());
    LockInfo {
        holder,
        pid,
        acquired_secs,
        token,
        generation,
    }
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
        // No non-unix equivalent of `kill(pid, 0)` is wired up here, and
        // `process_identity_token`'s non-unix stub always returns `None`, so
        // `alive_or_reused` can never reach `Dead` for a foreign pid on this
        // platform. Liveness is therefore unprovable for a foreign holder:
        // report Unknown rather than guessing, and never Dead.
        tracing::debug!(
            pid,
            "liveness cannot be proven for a foreign pid on this platform; \
             reporting Unknown (Dead is unreachable here)"
        );
        non_unix_liveness_fallback()
    }
}

/// The verdict `probe_liveness` reports for a foreign pid on non-unix
/// targets, where liveness cannot be proven. Factored out (and compiled on
/// every platform) so a cross-platform test can pin that it is `Unknown` and
/// never `Dead` — uncertainty must never demote to Dead.
#[cfg_attr(unix, allow(dead_code))]
fn non_unix_liveness_fallback() -> LockLiveness {
    LockLiveness::Unknown
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
pub(crate) fn process_identity_token(pid: i32) -> Option<String> {
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
///
/// This is the actual `ps` spawn — the seam [`process_identity_token`] caches
/// in front of. Kept as a separate function (rather than inlining the
/// `Command` call) so a test can observe how many times it actually ran, via
/// [`PS_SPAWN_COUNT`].
#[cfg(target_os = "macos")]
fn ps_identity_token(pid: i32) -> Option<String> {
    #[cfg(test)]
    {
        *PS_SPAWN_COUNTS
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
            .lock()
            .unwrap()
            .entry(pid)
            .or_insert(0) += 1;
    }
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

/// Counts real `ps` spawns from [`ps_identity_token`], keyed by pid, so a
/// test can prove the cache in [`process_identity_token`] collapses repeated
/// checks for one pid into a single spawn — without being confused by other
/// tests in this file concurrently spawning `ps` for a DIFFERENT pid.
/// Test-only: it exists purely to observe the seam.
#[cfg(all(test, target_os = "macos"))]
static PS_SPAWN_COUNTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<i32, usize>>,
> = std::sync::OnceLock::new();

/// How long a cached macOS identity token may be served before a fresh `ps`
/// spawn is required. This window only needs to be long enough to collapse
/// the handful of `alive_or_reused` calls a single steal decision or hygiene
/// sweep makes for the SAME pid (microseconds to low milliseconds apart in
/// practice); it must stay far shorter than any realistic pid-reuse
/// turnaround (the OS has to fully tear down the old process and allocate a
/// new one, which takes at least tens of milliseconds, typically much more).
/// A cached token therefore can never span an actual reuse: by the time a
/// pid is recycled, the cache entry for it has long since expired and the
/// next probe spawns fresh `ps`.
#[cfg(target_os = "macos")]
const IDENTITY_TOKEN_CACHE_TTL: Duration = Duration::from_millis(50);

/// Per-pid cache of [`ps_identity_token`] results, so a burst of liveness
/// checks against the same pid (queue-busy checks, hygiene sweeps, a single
/// steal decision) spawns at most one `ps`. Keyed by pid so a lookup for one
/// pid can never return another pid's token. Guarded by a `Mutex` for safe
/// concurrent access.
#[cfg(target_os = "macos")]
type IdentityTokenCache =
    std::sync::Mutex<std::collections::HashMap<i32, (Option<String>, Instant)>>;

#[cfg(target_os = "macos")]
static IDENTITY_TOKEN_CACHE: std::sync::OnceLock<IdentityTokenCache> = std::sync::OnceLock::new();

/// Cache layer in front of [`ps_identity_token`]: the real `ps` spawn seam.
/// The VERDICT (Alive/Dead) is never cached or short-circuited here — only
/// the raw token lookup is memoized; [`alive_or_reused`] still compares
/// `recorded == current` on every call, using whatever token this returns.
#[cfg(target_os = "macos")]
pub(crate) fn process_identity_token(pid: i32) -> Option<String> {
    let cache = IDENTITY_TOKEN_CACHE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let now = Instant::now();
    if let Some((token, captured)) = cache.lock().unwrap().get(&pid) {
        if now.duration_since(*captured) < IDENTITY_TOKEN_CACHE_TTL {
            return token.clone();
        }
    }
    let token = ps_identity_token(pid);
    cache.lock().unwrap().insert(pid, (token.clone(), now));
    token
}

/// Everywhere else (windows, exotic unix): no identity token, so pid reuse
/// cannot be proven and an alive holder stays Alive.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn process_identity_token(_pid: i32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // `probe_liveness`'s non-unix arm (the actual code path this pins) only
    // compiles under `#[cfg(not(unix))]`, and our CI runs macOS/Linux, so it
    // cannot be exercised directly here. `non_unix_liveness_fallback` is
    // factored out and compiled on ALL platforms so this cross-platform test
    // can still pin its invariant: uncertainty must never demote to Dead.
    #[test]
    fn non_unix_liveness_fallback_is_never_dead() {
        assert_eq!(non_unix_liveness_fallback(), LockLiveness::Unknown);
        assert_ne!(non_unix_liveness_fallback(), LockLiveness::Dead);
    }

    /// A valid one-event log body for a not-yet-acquired mission.
    // Only the unix symlink tests use this; silence dead_code off-unix
    // without masking it on unix (windows-latest clippy gates -D warnings).
    #[cfg_attr(not(unix), allow(dead_code))]
    fn one_event_line() -> String {
        let event = Event {
            seq: 1,
            ts: Utc::now(),
            mission_id: "m-1".to_string(),
            kind: EventKind::MissionCreated {
                goal: "goal".into(),
                base_branch: "main".into(),
                mission_branch: "kranz/mission-m-1".into(),
                config: crate::types::MissionConfig::default(),
            },
        };
        let mut line = serde_json::to_string(&event).unwrap();
        line.push('\n');
        line
    }

    // Symlink-creating tests are unix-only, exactly like the lessons guard's
    // tests; Windows needs privileges to create symlinks.

    #[cfg(unix)]
    #[test]
    fn read_events_refuses_a_symlinked_log() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        // A valid log at the symlink TARGET: the read must refuse, not
        // return the target's events.
        let target = dir.path().join("target.jsonl");
        std::fs::write(&target, one_event_line()).unwrap();
        let link = dir.path().join("events.jsonl");
        symlink(&target, &link).unwrap();
        let err = EventLog::read_events(&link).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn acquire_refuses_symlinked_runtime_files_without_writing_through() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let paths = MissionPaths::new(dir.path(), "m-1");
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let target = elsewhere.path().join("elsewhere.jsonl");
        std::fs::write(&target, b"").unwrap();

        // A symlinked events.jsonl is refused before the lock is taken.
        symlink(&target, paths.events_file()).unwrap();
        let err = EventLog::acquire(&paths, "m-1", Duration::ZERO, LockForce::No).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
        assert_eq!(std::fs::read(&target).unwrap(), b"");
        assert!(!paths.lock_file().exists(), "no lock taken on refusal");

        // A symlinked lock file is refused before any steal logic.
        std::fs::remove_file(paths.events_file()).unwrap();
        symlink(&target, paths.lock_file()).unwrap();
        let err = EventLog::acquire(&paths, "m-1", Duration::ZERO, LockForce::No).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
        assert_eq!(std::fs::read(&target).unwrap(), b"");
    }

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
        assert!(
            !a.is_empty() && !a.contains('\n'),
            "token must be a single non-empty line: {a:?}"
        );
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
            generation: None,
        };
        assert_eq!(probe_liveness(&info), LockLiveness::Alive);

        let info = LockInfo {
            token: Some(format!("{own}-not")),
            ..info
        };
        assert_eq!(probe_liveness(&info), LockLiveness::Dead);
    }

    /// Two `process_identity_token` calls for the SAME pid in quick
    /// succession must spawn `ps` only once — the cache should serve the
    /// second call from memory, and both returned tokens must still match.
    ///
    /// Uses pid 1 (launchd — always alive on macOS) rather than our own pid,
    /// so this test's spawn-count delta is not polluted by other tests in
    /// this file that concurrently probe `process_identity_token` for the
    /// test process's own pid.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_identity_token_caches_one_ps_per_pid() {
        let pid = 1;
        let count_for_pid = |p: i32| {
            *PS_SPAWN_COUNTS
                .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
                .lock()
                .unwrap()
                .get(&p)
                .unwrap_or(&0)
        };
        let before = count_for_pid(pid);

        let a = process_identity_token(pid).expect("own token must be obtainable");
        let b = process_identity_token(pid).expect("own token must be obtainable");

        let after = count_for_pid(pid);
        assert_eq!(
            after - before,
            1,
            "second call within the cache window must not spawn ps again"
        );
        assert_eq!(a, b, "cached token must match the freshly spawned one");
    }

    /// The cache must never mask pid reuse: it only ever memoizes the
    /// CURRENT token lookup, never the recorded-vs-current comparison. Even
    /// though `current` is served from cache here, a differing `recorded`
    /// token must still yield Dead.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cache_never_masks_pid_reuse() {
        let pid = std::process::id() as i32;
        // Prime the cache for this pid.
        let own = process_identity_token(pid).expect("own token must be obtainable");

        let info = LockInfo {
            holder: pid.to_string(),
            pid: Some(pid),
            acquired_secs: Some(0),
            token: Some(format!("{own}-not")),
            generation: None,
        };
        // `current` comes from the cache primed above, but the differing
        // `recorded` token must still be judged Dead — the comparison is
        // never skipped just because `current` was cached.
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
            .map(|i| BufferedLine {
                buffered_at: Instant::now(),
                line: format!("line-{i}\n"),
            })
            .collect()
    }

    #[test]
    fn drain_retains_unwritten_deltas_on_write_failure() {
        let k = 3;
        let n = 7;
        let mut writer = FlakyWriter {
            fail_at: k,
            writes: Vec::new(),
        };
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
        let mut retry_writer = FlakyWriter {
            fail_at: usize::MAX,
            writes: Vec::new(),
        };
        let retry_result = drain_lines(&mut retry_writer, &mut buffer);
        assert!(retry_result.is_ok());
        assert!(buffer.is_empty());
        assert_eq!(
            retry_writer.writes,
            (k..n).map(|i| format!("line-{i}\n")).collect::<Vec<_>>()
        );
    }

    /// Build a raw JSONL line for a `mission.paused` event with the given
    /// `seq`/`mission_id` — enough to exercise seq continuity and mission-id
    /// consistency without pulling in the full Event field set.
    fn event_line(seq: u64, mission_id: &str) -> String {
        let event = Event {
            seq,
            ts: Utc::now(),
            mission_id: mission_id.to_string(),
            kind: EventKind::MissionPaused {},
        };
        let mut line = serde_json::to_string(&event).unwrap();
        line.push('\n');
        line
    }

    #[test]
    fn acquire_rejects_foreign_mission_id_in_later_event() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MissionPaths::new(dir.path(), "m-a");
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        let events_path = paths.events_file();

        // Only defect: seq 2's mission_id differs from seq 1's.
        std::fs::write(
            &events_path,
            format!("{}{}", event_line(1, "m-a"), event_line(2, "m-b")),
        )
        .unwrap();

        let err = EventLog::acquire(&paths, "m-a", Duration::from_secs(1), LockForce::No)
            .expect_err("mixed mission_id log must be rejected");
        assert!(
            matches!(err, EngineError::LogCorruption(_)),
            "expected LogCorruption, got {err:?}"
        );

        // Control: identical seqs, single consistent mission_id, acquires cleanly.
        let dir2 = tempfile::tempdir().unwrap();
        let paths2 = MissionPaths::new(dir2.path(), "m-a");
        std::fs::create_dir_all(paths2.mission_dir()).unwrap();
        std::fs::write(
            paths2.events_file(),
            format!("{}{}", event_line(1, "m-a"), event_line(2, "m-a")),
        )
        .unwrap();
        let log = EventLog::acquire(&paths2, "m-a", Duration::from_secs(1), LockForce::No)
            .expect("consistent-mission log must acquire cleanly");
        assert_eq!(log.last_seq(), 2);
    }

    #[test]
    fn parse_log_rejects_mixed_mission_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(
            &path,
            format!("{}{}", event_line(1, "m-a"), event_line(2, "m-b")),
        )
        .unwrap();

        let err = EventLog::read_events(&path).expect_err("mixed mission_id must be rejected");
        assert!(
            matches!(err, EngineError::LogCorruption(_)),
            "expected LogCorruption, got {err:?}"
        );
    }

    #[test]
    fn acquire_adopts_empty_preexisting_log_as_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MissionPaths::new(dir.path(), "m-a");
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        std::fs::write(paths.events_file(), "").unwrap();

        let log = EventLog::acquire(&paths, "m-a", Duration::from_secs(1), LockForce::No)
            .expect("empty pre-existing log must be adopted as fresh");
        assert_eq!(log.last_seq(), 0);
        let appended = {
            let mut log = log;
            log.append(EventKind::MissionPaused {}).unwrap()
        };
        assert_eq!(appended.seq, 1);
    }
}
