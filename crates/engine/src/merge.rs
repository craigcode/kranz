//! Gated merge orchestration (roadmap M6): a human-triggered Merge action
//! that refuses on a dirty tracked tree, runs the repo's tracked gate suite,
//! and advances the base to an exact, gate-tested integration commit only on
//! green — never pushing.
//!
//! `merge_mission` ties together three primitives that each already carry
//! their own safety contract: [`GitRepo::is_clean_tracked`] (refuse dirty),
//! [`crate::merge_gate::MergeSuiteGate`] (refuse on a failing gate, base
//! untouched — the suite runs through the [`crate::gate`] interface), and
//! [`GitRepo::merge_no_ff`] (clean-abort on conflict). The
//! merge and gates run in a detached scratch worktree; the primary base only
//! fast-forwards to that exact tested commit. Every git command on this path
//! runs via [`GitRepo::with_hooks_disabled`], so mission-planted
//! `.git/hooks/*` never execute with the server's environment. This module
//! never calls [`GitRepo::push_mission_branch`] or any other push.

use crate::error::Result;
use crate::gate::{Gate, GateVerdict};
use crate::git_ops::{with_kranz_trailers, GitRepo, KranzCommitMetadata, MergeOutcome};
use crate::merge_gate::{parse_gate_suite, MergeSuiteGate, MERGE_GATES_PATH};
use crate::scrub::{self, SecretFinding};
use std::path::Path;

/// Number of merge commits on the live base after the mission's pinned base
/// before the merge response flags likely sibling-merge semantic drift.
pub const STALE_BASE_MERGE_THRESHOLD: usize = 1;

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
    /// The live base branch has no valid tracked merge-gate suite. Nothing
    /// ran and base is untouched; this fails closed instead of silently
    /// treating an empty suite as green.
    GateConfigInvalid { detail: String },
    /// The mission branch diff contains an unwaived secret finding. Base is
    /// untouched and no other gates ran.
    SecretScanFailed { findings: Vec<SecretFinding> },
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
        /// Informational warning when the mission's pinned base trails merge
        /// commits already landed on the live base branch.
        stale_base: Option<StaleBaseWarning>,
    },
}

/// Non-blocking merge-time warning for missions drafted from a stale base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleBaseWarning {
    pub base_sha: String,
    pub live_base: String,
    pub merge_commits_since_base: usize,
}

/// Runs the gated merge: refuse-if-dirty, then gates, then `--no-ff` merge.
///
/// `executor` is forwarded to the [`crate::merge_gate::MergeSuiteGate`]
/// adapter as-is (production callers wrap the orchestrator's shell runner,
/// tests inject a scripted fake). This function never calls
/// [`GitRepo::push_mission_branch`] or any push — the base branch is only
/// ever advanced locally.
pub fn merge_mission<F>(
    repo: &GitRepo,
    base_branch: &str,
    base_sha: &str,
    mission_branch: &str,
    metadata: Option<KranzCommitMetadata>,
    executor: F,
) -> Result<MergeReport>
where
    F: Fn(&str, &Path) -> (bool, String),
{
    // Every git command this merge issues — primary tree and scratch worktree
    // alike — runs with hooks disabled: the scratch worktree shares the
    // primary `.git`, so mission-authored gate code could plant
    // `.git/hooks/*` and have the merge's own checkout/merge/worktree
    // commands execute it with the server's full environment (exactly what
    // the sanitized gate executor withholds). Worker-side git is untouched.
    let repo = &repo.with_hooks_disabled();

    if !repo.is_clean_tracked_strict()? {
        return Ok(MergeReport::RefusedDirtyTree);
    }
    let primary_head_before_gates = repo.head_sha()?;
    let primary_branch_before_gates = repo.current_branch()?;

    // Resolve moving refs once. Every read, merge, and final advance below
    // uses these SHAs so a late branch update cannot bypass validation.
    let live_base_sha = repo.rev_parse(base_branch)?;
    let mission_tip_sha = repo.rev_parse(mission_branch)?;

    let diff = repo.diff_full(base_sha, &mission_tip_sha)?;
    let allowlist = repo
        .show_file(&live_base_sha, scrub::SECRET_ALLOWLIST_PATH)?
        .map(|bytes| scrub::read_allowlist_text(&String::from_utf8_lossy(&bytes)))
        .unwrap_or_default();
    let findings = scrub::filter_allowed(scrub::scan_unified_diff(&diff), &allowlist);
    if !findings.is_empty() {
        return Ok(MergeReport::SecretScanFailed { findings });
    }

    let changed_paths = repo.changed_paths(base_sha, &mission_tip_sha)?;
    let gate_bytes = match repo.show_file(&live_base_sha, MERGE_GATES_PATH)? {
        Some(bytes) => bytes,
        None => {
            return Ok(MergeReport::GateConfigInvalid {
                detail: format!(
                    "live base branch {base_branch:?} has no tracked {MERGE_GATES_PATH}; add an explicit repo gate suite before merging"
                ),
            })
        }
    };
    let gate_suite = match parse_gate_suite(&gate_bytes) {
        Ok(suite) => suite,
        Err(detail) => return Ok(MergeReport::GateConfigInvalid { detail }),
    };

    let stale_base = stale_base_warning(repo, base_branch, base_sha)?;
    let merge_message = metadata
        .as_ref()
        .map(|metadata| with_kranz_trailers(&format!("Merge {mission_branch}"), metadata));

    // Sweep scratch leftovers from any earlier merge that died between add
    // and remove (a panic, kill -9, power loss): stale kranz-merge-* temp
    // dirs and their .git/worktrees registrations would otherwise pile up
    // forever. Best-effort — this merge proceeds either way.
    remove_stale_scratch_worktrees(repo);

    let scratch_path =
        std::env::temp_dir().join(format!("kranz-merge-{}", uuid::Uuid::new_v4().simple()));
    repo.add_detached_worktree(&scratch_path, &live_base_sha)?;
    // RAII cleanup, created immediately after the worktree so a panic or an
    // overlooked error path cannot leak it. Drop is best-effort (warn, never
    // propagate): a merge that already landed must be reported as Merged,
    // not turned into an error by a failed cleanup — the sweep above retries
    // the removal on the next merge anyway.
    let _scratch_cleanup = ScratchWorktree {
        repo,
        path: scratch_path.clone(),
    };

    let scratch = GitRepo::open(&scratch_path)?.with_hooks_disabled();
    match scratch.merge_no_ff_with_message(&mission_tip_sha, merge_message.as_deref())? {
        MergeOutcome::Conflict { files } => return Ok(MergeReport::Conflict { files }),
        MergeOutcome::RefusedPreMerge { detail } => {
            return Ok(MergeReport::RefusedPreMerge { detail })
        }
        MergeOutcome::Clean => {}
    }
    let tested_commit = scratch.head_sha()?;

    // The suite runs through the first-class gate interface (gate.rs) —
    // behavior is unchanged: same commands, same declared order, stop at
    // first failure, suite bytes read from the live base branch above.
    let outcome =
        MergeSuiteGate::new(scratch.root(), &changed_paths, gate_suite, executor).evaluate();
    if outcome.verdict == GateVerdict::Fail {
        return Ok(MergeReport::GateFailed {
            gate: outcome.artefact.reference,
            output: outcome.artefact.detail.unwrap_or_default(),
        });
    }

    let post_gate_head = scratch.head_sha()?;
    let tracked_tree_clean = scratch.is_clean_tracked_strict()?;
    if post_gate_head != tested_commit || !tracked_tree_clean {
        return Ok(MergeReport::RefusedPreMerge {
            detail: format!(
                "merge gates mutated the integrated tree: expected HEAD {tested_commit}, \
                 found {post_gate_head}, tracked files clean={tracked_tree_clean}; \
                 refusing to land bytes other than the tested commit"
            ),
        });
    }

    // Production holds the repo-wide busy guard throughout this call.
    // Re-check anyway so external/manual movement fails closed.
    let current_base = repo.rev_parse(base_branch)?;
    if current_base != live_base_sha {
        return Ok(MergeReport::RefusedPreMerge {
            detail: format!(
                "base branch {base_branch:?} moved from {live_base_sha} to {current_base} while gates ran; retry the merge"
            ),
        });
    }

    let primary_head_after_gates = repo.head_sha()?;
    let primary_branch_after_gates = repo.current_branch()?;
    let primary_tree_clean = repo.is_clean_tracked_strict()?;
    if primary_head_after_gates != primary_head_before_gates
        || primary_branch_after_gates != primary_branch_before_gates
        || !primary_tree_clean
    {
        return Ok(MergeReport::RefusedPreMerge {
            detail: format!(
                "merge gates mutated the primary checkout: expected \
                 {primary_branch_before_gates}@{primary_head_before_gates}, found \
                 {primary_branch_after_gates}@{primary_head_after_gates}, tracked files \
                 clean={primary_tree_clean}; refusing to overwrite operator work"
            ),
        });
    }

    repo.checkout(base_branch)?;
    strip_identical_untracked_twins(repo, base_sha, &mission_tip_sha)?;
    match repo.fast_forward_to(&tested_commit)? {
        MergeOutcome::Clean => Ok(MergeReport::Merged {
            commit: tested_commit,
            stale_base,
        }),
        MergeOutcome::RefusedPreMerge { detail } => Ok(MergeReport::RefusedPreMerge { detail }),
        MergeOutcome::Conflict { .. } => unreachable!("--ff-only cannot create conflicts"),
    }
}

/// RAII cleanup for the merge's detached scratch worktree.
///
/// Removal lives in `Drop` so a panic mid-merge (or an error path this module
/// missed) cannot leak the temp directory and its
/// `.git/worktrees/kranz-merge-*` registration. Cleanup is best-effort: a
/// failure is logged at warn and never propagated, so a merge whose report is
/// already decided keeps that report — [`remove_stale_scratch_worktrees`]
/// retries the removal at the start of the next merge.
struct ScratchWorktree<'a> {
    repo: &'a GitRepo,
    path: std::path::PathBuf,
}

impl Drop for ScratchWorktree<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.repo.remove_worktree(&self.path) {
            tracing::warn!(
                path = %self.path.display(),
                %error,
                "failed to remove merge scratch worktree; the next merge will retry"
            );
        }
    }
}

/// Best-effort removal of `kranz-merge-*` scratch worktrees leaked by an
/// earlier merge that died between add and remove, plus a
/// `git worktree prune` for registrations whose directories are already gone
/// (invisible to `worktree remove`). Failures are logged and swallowed — a
/// stale leftover must never block a fresh merge.
fn remove_stale_scratch_worktrees(repo: &GitRepo) {
    match repo.list_worktrees() {
        Ok(worktrees) => {
            for worktree in worktrees {
                let path = Path::new(&worktree);
                let is_scratch = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("kranz-merge-"));
                if !is_scratch {
                    continue;
                }
                if let Err(error) = repo.remove_worktree(path) {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "failed to remove stale merge scratch worktree"
                    );
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "failed to list worktrees for stale merge-scratch cleanup")
        }
    }
    if let Err(error) = repo.prune_worktrees() {
        tracing::warn!(%error, "failed to prune stale worktree registrations");
    }
}

fn stale_base_warning(
    repo: &GitRepo,
    base_branch: &str,
    base_sha: &str,
) -> Result<Option<StaleBaseWarning>> {
    let merge_commits_since_base = repo.merge_commit_count(base_sha, base_branch)?;
    if merge_commits_since_base < STALE_BASE_MERGE_THRESHOLD {
        return Ok(None);
    }
    Ok(Some(StaleBaseWarning {
        base_sha: base_sha.to_string(),
        live_base: base_branch.to_string(),
        merge_commits_since_base,
    }))
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
