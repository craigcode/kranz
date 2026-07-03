//! Mission ↔ Slack thread mapping, persisted so a restarted bridge re-threads.
//!
//! One Slack thread per mission keeps routing trivial: an inbound message event
//! carries a `thread_ts`, and this map turns it back into a mission id. The map
//! lives at `.kranz/slack-threads.json` under the repo (it is bridge state, not
//! mission state, so it stays out of `missions/`). Writes are atomic (temp +
//! rename) so a crash mid-write never leaves a half-written map that would drop
//! every thread association.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Bidirectional mission ↔ thread_ts map. `BTreeMap` for deterministic
/// serialization (stable diffs, reproducible tests).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadMap {
    /// mission id → root message `ts` of that mission's thread.
    #[serde(default)]
    by_mission: BTreeMap<String, String>,
}

impl ThreadMap {
    /// The `.kranz/slack-threads.json` path for a repo.
    pub fn path(repo_root: &Path) -> PathBuf {
        repo_root.join(".kranz").join("slack-threads.json")
    }

    /// Load the map from disk. A missing file is an empty map (first run); an
    /// unparseable file is an error so corruption is surfaced rather than
    /// silently discarding every thread association.
    pub fn load(repo_root: &Path) -> Result<ThreadMap> {
        let path = Self::path(repo_root);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| anyhow!("invalid thread map {}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ThreadMap::default()),
            Err(e) => Err(anyhow!("cannot read {}: {e}", path.display())),
        }
    }

    /// Persist the map atomically (temp file + rename).
    pub fn save(&self, repo_root: &Path) -> Result<()> {
        let path = Self::path(repo_root);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        atomic_write(&path, json.as_bytes())
    }

    /// The thread root `ts` recorded for a mission, if any.
    pub fn thread_ts(&self, mission_id: &str) -> Option<&str> {
        self.by_mission.get(mission_id).map(String::as_str)
    }

    /// The mission whose thread root is `thread_ts` (reverse lookup for routing
    /// inbound replies). Linear scan — the map is small (one entry per mission).
    pub fn mission_for_thread(&self, thread_ts: &str) -> Option<&str> {
        self.by_mission
            .iter()
            .find(|(_, ts)| ts.as_str() == thread_ts)
            .map(|(id, _)| id.as_str())
    }

    /// Record the thread root for a mission. Returns the previous value if the
    /// mission was already threaded (a re-post overwriting the old root).
    pub fn set(&mut self, mission_id: impl Into<String>, thread_ts: impl Into<String>) -> Option<String> {
        self.by_mission.insert(mission_id.into(), thread_ts.into())
    }

    /// Whether a mission already has a thread.
    pub fn contains(&self, mission_id: &str) -> bool {
        self.by_mission.contains_key(mission_id)
    }
}

/// Atomic write via a sibling temp file + rename (mirrors the engine's helpers
/// so behaviour matches on Windows, where rename-over-existing fails).
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("slack-threads.json");
    let tmp = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) if cfg!(windows) => {
            let _ = std::fs::remove_file(path);
            std::fs::rename(&tmp, path).map_err(Into::into)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_lookup_both_directions() {
        let mut map = ThreadMap::default();
        assert!(map.set("m-01", "1700000000.000100").is_none());
        assert_eq!(map.thread_ts("m-01"), Some("1700000000.000100"));
        assert_eq!(map.mission_for_thread("1700000000.000100"), Some("m-01"));
        assert!(map.contains("m-01"));
        assert!(!map.contains("m-02"));
        assert_eq!(map.mission_for_thread("nope"), None);
    }

    #[test]
    fn set_returns_previous() {
        let mut map = ThreadMap::default();
        map.set("m-01", "ts-a");
        assert_eq!(map.set("m-01", "ts-b"), Some("ts-a".to_string()));
        assert_eq!(map.thread_ts("m-01"), Some("ts-b"));
    }
}
