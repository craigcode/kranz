//! Mission data layout under the target repo (plan §4):
//!
//! ```text
//! <repo>/.kranz/
//! ├── config.json                 # project config (merged over ~/.kranz/config.json)
//! └── missions/<id>/
//!     ├── plan.json               # approved plan (also committed to mission branch)
//!     ├── events.jsonl            # append-only event log (single writer)
//!     ├── events.jsonl.lock       # exclusive engine lock
//!     ├── state.json              # derived snapshot (cache; rebuildable)
//!     ├── control/                # inbox: CLI/server -> engine ControlCommand files
//!     └── runs/<runId>.jsonl      # full per-run transcripts
//! ```
//!
//! All paths built with std::path so Windows stays first-class (§9).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct MissionPaths {
    pub repo_root: PathBuf,
    pub mission_id: String,
}

impl MissionPaths {
    pub fn new(repo_root: impl Into<PathBuf>, mission_id: impl Into<String>) -> Self {
        Self {
            repo_root: repo_root.into(),
            mission_id: mission_id.into(),
        }
    }

    pub fn kranz_dir(&self) -> PathBuf {
        self.repo_root.join(".kranz")
    }

    pub fn missions_dir(&self) -> PathBuf {
        self.kranz_dir().join("missions")
    }

    pub fn mission_dir(&self) -> PathBuf {
        self.missions_dir().join(&self.mission_id)
    }

    pub fn plan_file(&self) -> PathBuf {
        self.mission_dir().join("plan.json")
    }

    /// Rendered plan markdown (distinct from `plan_file`'s plan.json).
    pub fn plan_md_file(&self) -> PathBuf {
        self.mission_dir().join("plan.md")
    }

    /// Mission report markdown, written on completion.
    pub fn report_file(&self) -> PathBuf {
        self.mission_dir().join("report.md")
    }

    pub fn events_file(&self) -> PathBuf {
        self.mission_dir().join("events.jsonl")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.mission_dir().join("events.jsonl.lock")
    }

    pub fn state_file(&self) -> PathBuf {
        self.mission_dir().join("state.json")
    }

    pub fn control_dir(&self) -> PathBuf {
        self.mission_dir().join("control")
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.mission_dir().join("runs")
    }

    /// Repo-level (not per-mission) directory of captured cross-mission
    /// lessons; deliberately survives `kranz clean` and mission deletion.
    pub fn lessons_dir(&self) -> PathBuf {
        self.kranz_dir().join("lessons")
    }

    /// Append-only manifest of captured lessons in capture order.
    pub fn lessons_index(&self) -> PathBuf {
        self.lessons_dir().join("index.md")
    }

    pub fn transcript_file(&self, run_id: &str) -> PathBuf {
        self.runs_dir().join(format!("{run_id}.jsonl"))
    }

    /// Relative transcript path recorded in events/state (stable across hosts).
    pub fn transcript_rel(run_id: &str) -> String {
        format!("runs/{run_id}.jsonl")
    }

    /// List mission ids present under a repo (sorted).
    pub fn list_missions(repo_root: &Path) -> Vec<String> {
        let dir = repo_root.join(".kranz").join("missions");
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        out.push(name.to_string());
                    }
                }
            }
        }
        out.sort();
        out
    }
}

/// Project config file path.
pub fn project_config(repo_root: &Path) -> PathBuf {
    repo_root.join(".kranz").join("config.json")
}

/// Global config file path (~/.kranz/config.json), None if no home dir.
pub fn global_config() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(|h| PathBuf::from(h).join(".kranz").join("config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lessons_paths_are_repo_level_not_per_mission() {
        let paths = MissionPaths::new("/repo", "m-abc123");
        assert_eq!(paths.lessons_dir(), PathBuf::from("/repo/.kranz/lessons"));
        assert_eq!(
            paths.lessons_index(),
            PathBuf::from("/repo/.kranz/lessons/index.md")
        );
    }

    #[test]
    fn plan_md_and_report_paths_are_per_mission() {
        let paths = MissionPaths::new("/repo", "m-abc123");
        assert_eq!(
            paths.plan_md_file(),
            PathBuf::from("/repo/.kranz/missions/m-abc123/plan.md")
        );
        assert_eq!(
            paths.report_file(),
            PathBuf::from("/repo/.kranz/missions/m-abc123/report.md")
        );
    }
}
