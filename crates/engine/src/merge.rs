//! Gated merge orchestration (roadmap M6): a human-triggered Merge action
//! that refuses on a dirty tracked tree, runs the CI gate suite, and merges
//! `--no-ff` into the base branch only on green — never pushing.
//!
//! `merge_mission` ties together three primitives that each already carry
//! their own safety contract: [`GitRepo::is_clean_tracked`] (refuse dirty),
//! [`crate::merge_gate::run_gate_suite`] (refuse on a failing gate, base
//! untouched), and [`GitRepo::merge_no_ff`] (clean-abort on conflict). This
//! module only sequences them and never calls
//! [`GitRepo::push_mission_branch`] or any other push.

use crate::error::Result;
use crate::git_ops::{GitRepo, MergeOutcome};
use crate::merge_gate::{run_gate_suite, GateSuiteResult};
use std::path::Path;

/// Outcome of a gated merge attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeReport {
    /// The tracked working tree was dirty; no gates ran and base is untouched.
    RefusedDirtyTree,
    /// A gate failed; later gates and the merge itself never ran. Base is
    /// untouched.
    GateFailed {
        /// The failing gate's command string.
        gate: String,
        /// That gate's verbatim captured output.
        output: String,
    },
    /// The merge conflicted and was rolled back (working tree left clean).
    Conflict {
        /// Conflicting paths git named (best-effort; may be empty).
        files: Vec<String>,
    },
    /// Git refused the merge before it ever started (no `MERGE_HEAD`), e.g. a
    /// divergent untracked file at a path the merge would overwrite. Base is
    /// untouched and nothing was aborted (there was no merge in progress).
    RefusedPreMerge {
        /// Git's verbatim refusal text.
        detail: String,
    },
    /// The mission branch merged cleanly into base with a `--no-ff` commit.
    Merged {
        /// The new merge commit sha, now the tip of `base_branch`.
        commit: String,
    },
}

/// Runs the gated merge: refuse-if-dirty, then gates, then `--no-ff` merge.
///
/// `executor` is forwarded to [`crate::merge_gate::run_gate_suite`] as-is
/// (production callers wrap the orchestrator's shell runner, tests inject a
/// scripted fake). This function never calls
/// [`GitRepo::push_mission_branch`] or any push — the base branch is only
/// ever advanced locally.
pub fn merge_mission<F>(
    repo: &GitRepo,
    base_branch: &str,
    base_sha: &str,
    mission_branch: &str,
    executor: F,
) -> Result<MergeReport>
where
    F: Fn(&str, &Path) -> (bool, String),
{
    if !repo.is_clean_tracked()? {
        return Ok(MergeReport::RefusedDirtyTree);
    }

    let dashboard_touched = repo.dashboard_touched(base_sha, mission_branch)?;

    match run_gate_suite(repo.root(), dashboard_touched, executor) {
        GateSuiteResult::Failed { gate, output } => {
            return Ok(MergeReport::GateFailed { gate, output });
        }
        GateSuiteResult::Passed => {}
    }

    repo.checkout(base_branch)?;
    strip_identical_untracked_twins(repo, base_sha, mission_branch)?;
    match repo.merge_no_ff(mission_branch)? {
        MergeOutcome::Conflict { files } => Ok(MergeReport::Conflict { files }),
        MergeOutcome::RefusedPreMerge { detail } => Ok(MergeReport::RefusedPreMerge { detail }),
        MergeOutcome::Clean => {
            let commit = repo.head_sha()?;
            Ok(MergeReport::Merged { commit })
        }
    }
}

/// Removes untracked working-tree files the incoming merge would touch, but
/// ONLY when their bytes are byte-identical to the version already committed
/// on `mission_branch` — the operator-visibility "preview twins" written
/// straight into the primary tree at canonical mission paths (plan.md,
/// report.md, revised-plan.md) alongside the same paths tracked on the
/// mission branch. Removing an identical file is lossless (the merge would
/// write back the exact same bytes); a divergent or tracked file is left
/// alone so git's own conflict/refusal machinery handles it.
fn strip_identical_untracked_twins(
    repo: &GitRepo,
    base_sha: &str,
    mission_branch: &str,
) -> Result<()> {
    for path in repo.changed_paths(base_sha, mission_branch)? {
        if !repo.is_untracked(&path)? {
            continue;
        }
        let incoming = match repo.show_file(mission_branch, &path)? {
            Some(bytes) => bytes,
            None => continue,
        };
        let full_path = repo.root().join(&path);
        let current = match std::fs::read(&full_path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        if current == incoming {
            std::fs::remove_file(&full_path)?;
        }
    }
    Ok(())
}
