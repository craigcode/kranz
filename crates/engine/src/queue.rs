//! Per-repo, priority-ordered execution queue (design: docs/backlog-and-slack.md §3).
//!
//! Approval enqueues a mission rather than starting it. Entries live as one
//! JSON file each under `.kranz/queue/`, named
//! `<priority>-<paddedSeq>-<missionId>.json` so plain lexicographic filename
//! order equals `(priority, insertion order)`. A monotonic counter file
//! (`.seq`) assigns the sequence number.
//!
//! Per-repo serialization is mandatory: missions share the working tree, so at
//! most one may run at a time in a repo. [`is_repo_busy`] detects the currently
//! RUNNING mission by finding any live `events.jsonl.lock` (a lock held by a
//! process that is still alive), delegating to the event-log module's
//! canonical liveness probe.

use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Width of the zero-padded sequence field in a queue filename. u64 max is 20
/// digits, so this keeps lexicographic order == numeric order for any seq.
const SEQ_WIDTH: usize = 20;

/// One queued mission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub mission_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket_slug: Option<String>,
    pub priority: u8,
    pub seq: u64,
}

impl QueueEntry {
    /// Filename encoding `(priority, seq)` into lexicographic order.
    fn file_name(&self) -> String {
        format!(
            "{:03}-{:0width$}-{}.json",
            self.priority,
            self.seq,
            self.mission_id,
            width = SEQ_WIDTH
        )
    }
}

/// The `.kranz/queue/` directory for a repo.
pub fn queue_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".kranz").join("queue")
}

/// Path of the monotonic sequence counter file.
fn seq_file(repo_root: &Path) -> PathBuf {
    queue_dir(repo_root).join(".seq")
}

/// Reserve the next sequence number: read the counter file, increment, write
/// it back. Falls back to `max(existing entry seq) + 1` if the counter file is
/// missing or corrupt, so a wiped counter can never hand out a duplicate that
/// would reorder existing entries.
fn next_seq(repo_root: &Path) -> Result<u64> {
    let path = seq_file(repo_root);
    let from_file = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());

    let next = match from_file {
        Some(current) => current.saturating_add(1),
        None => {
            let max_existing = list(repo_root).iter().map(|e| e.seq).max();
            max_existing.map(|m| m.saturating_add(1)).unwrap_or(0)
        }
    };

    atomic_write(&path, next.to_string().as_bytes())?;
    Ok(next)
}

/// Enqueue a mission. The entry's `seq` is assigned here from the monotonic
/// counter (any incoming `seq` is overwritten). Write is atomic (temp + rename).
/// A no-op if the mission is already queued.
pub fn enqueue(repo_root: &Path, entry: QueueEntry) -> Result<QueueEntry> {
    let dir = queue_dir(repo_root);
    std::fs::create_dir_all(&dir)?;

    if contains(repo_root, &entry.mission_id) {
        // Already queued: return the existing entry unchanged.
        if let Some(existing) = list(repo_root)
            .into_iter()
            .find(|e| e.mission_id == entry.mission_id)
        {
            return Ok(existing);
        }
    }

    let seq = next_seq(repo_root)?;
    let entry = QueueEntry { seq, ..entry };
    let json = serde_json::to_string_pretty(&entry)?;
    atomic_write(&dir.join(entry.file_name()), json.as_bytes())?;
    Ok(entry)
}

/// All queued entries, sorted by `(priority, seq)` (== filename order).
pub fn list(repo_root: &Path) -> Vec<QueueEntry> {
    let dir = queue_dir(repo_root);
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<QueueEntry>(&text) {
                Ok(qe) => out.push(qe),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping unparseable queue entry");
                }
            },
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "unreadable queue entry, skipping");
            }
        }
    }
    out.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.seq.cmp(&b.seq)));
    out
}

/// The front of the queue (highest priority, then earliest insertion), if any.
pub fn peek(repo_root: &Path) -> Option<QueueEntry> {
    list(repo_root).into_iter().next()
}

/// Remove the entry for `mission_id`. Returns true if something was removed.
pub fn remove(repo_root: &Path, mission_id: &str) -> bool {
    let dir = queue_dir(repo_root);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return false;
    };
    let mut removed = false;
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let is_match = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<QueueEntry>(&text).ok())
            .is_some_and(|qe| qe.mission_id == mission_id);
        if is_match && std::fs::remove_file(&path).is_ok() {
            removed = true;
        }
    }
    removed
}

/// Whether `mission_id` is currently queued.
pub fn contains(repo_root: &Path, mission_id: &str) -> bool {
    list(repo_root).iter().any(|e| e.mission_id == mission_id)
}

/// The mission id currently RUNNING in this repo, if any: detected by any
/// `.kranz/missions/*/events.jsonl.lock` whose recorded pid is still alive.
/// This enforces one-mission-at-a-time-per-repo.
pub fn is_repo_busy(repo_root: &Path) -> Option<String> {
    let missions = repo_root.join(".kranz").join("missions");
    let rd = std::fs::read_dir(&missions).ok()?;
    for entry in rd.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let lock = dir.join("events.jsonl.lock");
        if !lock.exists() {
            continue;
        }
        if lock_pid_is_alive(&lock) {
            if let Some(id) = dir.file_name().and_then(|n| n.to_str()) {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// True when a lock file records a holder that is still alive.
///
/// Delegates to the event-log module's canonical probe
/// ([`crate::event_log::lock_holder_is_alive`]) — ONE source of truth for
/// lock-file format and liveness semantics, so the queue can never diverge
/// from what `EventLog::acquire` itself would decide. (A divergent local
/// parser once read the entire file as a single integer, so any multi-line
/// lock was "unparseable ⇒ busy" — a SIGKILL'd engine livelocked `kranz
/// work` forever.) Genuinely unparseable-but-present locks still read as
/// busy, conservatively: a false "busy" only delays a queued mission, while
/// a false "free" could run two missions on one working tree.
fn lock_pid_is_alive(lock_path: &Path) -> bool {
    crate::event_log::lock_holder_is_alive(lock_path)
}

/// Atomic write via a sibling temp file + rename.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("entry");
    let tmp = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) if cfg!(windows) => {
            let _ = std::fs::remove_file(path);
            std::fs::rename(&tmp, path)?;
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}
