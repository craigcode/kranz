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
//!     ├── runs/<runId>.jsonl      # full per-run transcripts
//!     ├── runs/egress-denials.jsonl # fs+net proxy denial records (gitignored runtime)
//!     └── workspace/              # container-provider compose files (gitignored runtime)
//! ```
//!
//! All paths built with std::path so Windows stays first-class (§9).

use std::path::{Path, PathBuf};

/// The canonical `.kranz/.gitignore` rules the engine materializes on init
/// (`orchestrator::write_kranz_gitignore`). Single source for the engine's
/// writer and for ready.rs's engine-materialized exception: the file ignores
/// ITSELF (first rule), so it can never be a committed artifact — its rules
/// travel with the tool, not the repo.
pub const KRANZ_GITIGNORE_RULES: &[&str] = &[
    ".gitignore",
    "config.json",
    "missions/*/events.jsonl",
    "missions/*/events.jsonl.lock",
    "missions/*/state.json",
    "missions/*/state.json.tmp",
    "missions/*/estimate.json",
    "missions/*/control/",
    "missions/*/runs/",
    "missions/*/workspace/",
    "slack-threads.json",
    "queue/",
    "tickets/*.status",
    "serve.token",
];

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

    /// Mission ids that come from untrusted user input are joined into
    /// filesystem paths. Reject separators, `..`, and drive designators before
    /// constructing paths from those ids.
    pub fn is_safe_id(id: &str) -> bool {
        !id.is_empty() && !id.contains(['/', '\\', ':']) && !id.contains("..")
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

    /// Approval-time cost estimate (gitignored runtime bookkeeping, like
    /// `state.json`): persisted at plan approval / revision so the completion
    /// report can compare actual cost against the exact estimate the operator
    /// approved, rather than one recomputed against a later corpus or config.
    pub fn estimate_file(&self) -> PathBuf {
        self.mission_dir().join("estimate.json")
    }

    /// Per-mission research evidence artifact (repo-knowledge-store slice 1),
    /// committed beside `plan.md` on the mission branch when a plan is approved.
    pub fn research_file(&self) -> PathBuf {
        self.mission_dir().join("research.md")
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

    /// Mission-shared egress-denial JSONL the per-run egress proxy appends to
    /// (`fs+net` sessions; see `crate::egress_proxy`). Runtime path under the
    /// gitignored `runs/` dir; v1 correlation is mission-level.
    pub fn egress_denials_file(&self) -> PathBuf {
        self.runs_dir().join("egress-denials.jsonl")
    }

    /// Relative transcript path recorded in events/state (stable across hosts).
    pub fn transcript_rel(run_id: &str) -> String {
        format!("runs/{run_id}.jsonl")
    }

    /// List mission ids present under a repo (sorted), lenient: an unreadable
    /// missions dir lists as empty, and an erroring directory entry (fd
    /// exhaustion, mid-deletion races, permission flaps) is skipped while the
    /// REST are kept — one bad entry must not collapse the whole listing to
    /// "no missions". Use [`Self::try_list_missions`] when the caller must
    /// distinguish "no missions" from "could not list missions" (e.g. before
    /// pruning per-mission bookkeeping keyed on this listing).
    pub fn list_missions(repo_root: &Path) -> Vec<String> {
        let dir = repo_root.join(".kranz").join("missions");
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out: Vec<String> = rd
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .collect();
        out.sort();
        out
    }

    /// List mission ids present under a repo (sorted), distinguishing
    /// filesystem errors from a genuinely empty listing.
    ///
    /// A missing missions dir is `Ok(vec![])` — the repo simply has no
    /// missions yet. Any other `read_dir` failure, or an erroring directory
    /// entry (fd exhaustion, mid-deletion races, permission flaps), is `Err`:
    /// a transient error must not masquerade as "every mission was deleted".
    pub fn try_list_missions(repo_root: &Path) -> std::io::Result<Vec<String>> {
        let dir = repo_root.join(".kranz").join("missions");
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for entry in rd {
            let entry = entry?;
            if entry.path().is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
        }
        out.sort();
        Ok(out)
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

    #[test]
    fn try_list_missions_distinguishes_missing_dir_from_real_listing() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No .kranz/missions dir at all: genuinely empty, not an error.
        assert_eq!(
            MissionPaths::try_list_missions(tmp.path()).unwrap(),
            Vec::<String>::new()
        );
        // With mission subdirs (plus a stray file that must be skipped):
        // listed and sorted.
        let missions = tmp.path().join(".kranz").join("missions");
        std::fs::create_dir_all(missions.join("m-bbb222")).unwrap();
        std::fs::create_dir_all(missions.join("m-aaa111")).unwrap();
        std::fs::write(missions.join("stray.txt"), b"x").unwrap();
        let listed = MissionPaths::try_list_missions(tmp.path()).unwrap();
        assert_eq!(listed, vec!["m-aaa111".to_string(), "m-bbb222".to_string()]);
        // The lenient wrapper agrees on the happy path.
        assert_eq!(MissionPaths::list_missions(tmp.path()), listed);
    }

    #[cfg(unix)]
    #[test]
    fn try_list_missions_reports_errors_instead_of_swallowing_them() {
        // A missions *file* (not dir) makes read_dir fail with a non-NotFound
        // error; the fallible listing must surface it, not return "empty".
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".kranz")).unwrap();
        std::fs::write(tmp.path().join(".kranz").join("missions"), b"not a dir").unwrap();
        assert!(MissionPaths::try_list_missions(tmp.path()).is_err());
        // The lenient listing stays lenient on the same failure: empty, not a
        // panic or an error. (A single erroring dir ENTRY — skipped while the
        // rest are kept — is not portably constructible in a test; the walk
        // uses `.flatten()` to encode that contract.)
        assert_eq!(
            MissionPaths::list_missions(tmp.path()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn safe_id_rejects_path_traversal_shapes() {
        for id in ["", "../m-x", "m-x/../../y", "m-x\\..\\y", "c:m-x", "m-.."] {
            assert!(!MissionPaths::is_safe_id(id), "{id:?} should be unsafe");
        }
        for id in ["m-abc123", "m-2026-07-08", "m_ticket.linked"] {
            assert!(MissionPaths::is_safe_id(id), "{id:?} should be safe");
        }
    }
}
