//! Slack thread affinity, persisted so a restarted bridge re-threads.
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
    pub fn set(
        &mut self,
        mission_id: impl Into<String>,
        thread_ts: impl Into<String>,
    ) -> Option<String> {
        self.by_mission.insert(mission_id.into(), thread_ts.into())
    }

    /// Whether a mission already has a thread.
    pub fn contains(&self, mission_id: &str) -> bool {
        self.by_mission.contains_key(mission_id)
    }

    /// Existing entries, used once when a single-repository thread map is
    /// migrated into the operator-owned multi-repository affinity map.
    pub fn entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.by_mission
            .iter()
            .map(|(mission, thread)| (mission.as_str(), thread.as_str()))
    }
}

/// Composite mission identity at the Slack host boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoMissionId {
    pub repo_id: String,
    pub mission_id: String,
}

/// One operator-owned team/channel/thread affinity record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadAffinity {
    pub team_id: String,
    pub channel_id: String,
    pub thread_ts: String,
    pub target: RepoMissionId,
}

/// Multi-repository affinity map. A vector keeps the persisted shape readable;
/// the bridge volume is human-scale, so linear lookup is preferable to an
/// encoded composite string key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AffinityMap {
    #[serde(default)]
    entries: Vec<ThreadAffinity>,
}

impl AffinityMap {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| anyhow!("invalid thread affinity {}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(anyhow!("cannot read {}: {error}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        atomic_write(path, json.as_bytes())
    }

    pub fn target_for(
        &self,
        team_id: &str,
        channel_id: &str,
        thread_ts: &str,
    ) -> Option<&RepoMissionId> {
        self.entries
            .iter()
            .find(|entry| {
                entry.team_id == team_id
                    && entry.channel_id == channel_id
                    && entry.thread_ts == thread_ts
            })
            .map(|entry| &entry.target)
    }

    pub fn thread_for(
        &self,
        repo_id: &str,
        mission_id: &str,
        team_id: &str,
        channel_id: &str,
    ) -> Option<&str> {
        self.entries
            .iter()
            .find(|entry| {
                entry.target.repo_id == repo_id
                    && entry.target.mission_id == mission_id
                    && entry.team_id == team_id
                    && entry.channel_id == channel_id
            })
            .map(|entry| entry.thread_ts.as_str())
    }

    pub fn set(
        &mut self,
        team_id: impl Into<String>,
        channel_id: impl Into<String>,
        thread_ts: impl Into<String>,
        repo_id: impl Into<String>,
        mission_id: impl Into<String>,
    ) {
        let (team_id, channel_id, thread_ts, repo_id, mission_id) = (
            team_id.into(),
            channel_id.into(),
            thread_ts.into(),
            repo_id.into(),
            mission_id.into(),
        );
        self.entries.retain(|entry| {
            !(entry.team_id == team_id
                && entry.channel_id == channel_id
                && (entry.thread_ts == thread_ts
                    || (entry.target.repo_id == repo_id && entry.target.mission_id == mission_id)))
        });
        self.entries.push(ThreadAffinity {
            team_id,
            channel_id,
            thread_ts,
            target: RepoMissionId {
                repo_id,
                mission_id,
            },
        });
        self.entries.sort_by(|left, right| {
            (&left.team_id, &left.channel_id, &left.thread_ts).cmp(&(
                &right.team_id,
                &right.channel_id,
                &right.thread_ts,
            ))
        });
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Atomic write via a sibling temp file + rename (mirrors the engine's helpers
/// so behaviour matches on Windows, where rename-over-existing fails).
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("slack-threads.json");
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

    #[test]
    fn affinity_uses_the_complete_slack_and_mission_identity() {
        let mut map = AffinityMap::default();
        map.set("T1", "C1", "100.1", "alpha", "m-same");
        map.set("T1", "C2", "100.1", "beta", "m-same");

        assert_eq!(
            map.target_for("T1", "C1", "100.1"),
            Some(&RepoMissionId {
                repo_id: "alpha".into(),
                mission_id: "m-same".into(),
            })
        );
        assert_eq!(
            map.target_for("T1", "C2", "100.1")
                .map(|target| target.repo_id.as_str()),
            Some("beta")
        );
        assert_eq!(map.thread_for("alpha", "m-same", "T1", "C1"), Some("100.1"));
        assert_eq!(map.thread_for("alpha", "m-same", "T1", "C2"), None);
    }
}
