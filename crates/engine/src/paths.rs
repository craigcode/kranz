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
        Self { repo_root: repo_root.into(), mission_id: mission_id.into() }
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
