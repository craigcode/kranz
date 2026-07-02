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
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A serialized delta line waiting in the write buffer.
#[derive(Debug)]
struct BufferedLine {
    buffered_at: Instant,
    line: String,
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
    /// if missing. If the lock file already exists and `force` is false this
    /// fails with [`EngineError::LockHeld`] naming the holder's pid; with
    /// `force` the stale lock is replaced. Existing events are loaded to
    /// resume the seq counter and to verify `mission_id` matches the log.
    pub fn acquire(
        paths: &MissionPaths,
        mission_id: &str,
        throttle: Duration,
        force: bool,
    ) -> Result<EventLog> {
        std::fs::create_dir_all(paths.mission_dir())?;
        std::fs::create_dir_all(paths.runs_dir())?;
        std::fs::create_dir_all(paths.control_dir())?;

        let lock_path = paths.lock_file();
        let mut lock_file = match OpenOptions::new().write(true).create_new(true).open(&lock_path)
        {
            Ok(f) => f,
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                if !force {
                    return Err(EngineError::LockHeld(format!(
                        "lock file {} exists (held by pid {})",
                        lock_path.display(),
                        read_lock_pid(&lock_path)
                    )));
                }
                // Steal: replace the stale lock with our own pid.
                std::fs::remove_file(&lock_path)?;
                OpenOptions::new().write(true).create_new(true).open(&lock_path)?
            }
            Err(e) => return Err(e.into()),
        };

        // From here on we hold the lock; release it if the rest of the
        // acquisition fails so a failed open doesn't strand the mission.
        let mut open = || -> Result<EventLog> {
            lock_file.write_all(std::process::id().to_string().as_bytes())?;
            lock_file.flush()?;

            let events_path = paths.events_file();
            let last_seq = if events_path.exists() {
                let existing = Self::read_events(&events_path)?;
                if let Some(first) = existing.first() {
                    if first.mission_id != mission_id {
                        return Err(EngineError::InvalidState(format!(
                            "event log {} belongs to mission '{}', not '{}'",
                            events_path.display(),
                            first.mission_id,
                            mission_id
                        )));
                    }
                }
                existing.last().map(|e| e.seq).unwrap_or(0)
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
        let mut content = String::new();
        File::open(path)?.read_to_string(&mut content)?;
        let lines: Vec<&str> = content.lines().collect();

        let mut events = Vec::with_capacity(lines.len());
        for (i, line) in lines.iter().enumerate() {
            let event: Event = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(err) => {
                    if i + 1 == lines.len() {
                        tracing::warn!(
                            path = %path.display(),
                            line = i + 1,
                            error = %err,
                            "dropping unparseable final event line (torn write)"
                        );
                        break;
                    }
                    return Err(EngineError::LogCorruption(format!(
                        "unparseable event at {}:{}: {err}",
                        path.display(),
                        i + 1
                    )));
                }
            };
            let expected = events.len() as u64 + 1;
            if event.seq != expected {
                return Err(EngineError::LogCorruption(format!(
                    "seq discontinuity at {}:{}: expected {expected}, found {}",
                    path.display(),
                    i + 1,
                    event.seq
                )));
            }
            events.push(event);
        }
        Ok(events)
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

/// Best-effort read of the pid recorded in a lock file (for error messages).
fn read_lock_pid(lock_path: &Path) -> String {
    match std::fs::read_to_string(lock_path) {
        Ok(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => "unknown".to_string(),
    }
}
