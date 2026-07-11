//! Git operations for mission branches (plan §4.4 — git is the source of truth).
//!
//! Every operation shells out to the `git` binary with an explicit argument
//! vector (never a shell string, §9) and runs synchronously with the repo
//! root as the working directory. Callers on async paths wrap calls in
//! `tokio::task::spawn_blocking`.
//!
//! All failures surface as [`EngineError::Git`] with the command context and
//! whatever git printed, so mission logs show *why* a git step failed.

use crate::error::{EngineError, Result};
use crate::scrub;
use crate::types::TokenUsage;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// One commit in a [`GitRepo::commits_between`] listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// Full commit sha.
    pub sha: String,
    /// First line of the commit message.
    pub subject: String,
}

/// Mission facts attached to kranz-authored durable commits as git trailers.
#[derive(Debug, Clone, PartialEq)]
pub struct KranzCommitMetadata {
    pub mission_id: String,
    pub cost_usd: f64,
    pub tokens: TokenUsage,
}

/// Append-only git trailers for mission attribution and actual cost.
pub fn kranz_commit_trailers(metadata: &KranzCommitMetadata) -> String {
    format!(
        "Kranz-Mission: {}\nKranz-Cost-USD: {:.4}\nKranz-Tokens-Input: {}\nKranz-Tokens-Output: {}\nKranz-Tokens-Cache-Read: {}\nKranz-Tokens-Cache-Write: {}",
        metadata.mission_id,
        metadata.cost_usd,
        metadata.tokens.input,
        metadata.tokens.output,
        metadata.tokens.cache_read,
        metadata.tokens.cache_write,
    )
}

/// Commit message with kranz trailers separated in the standard trailer block.
pub fn with_kranz_trailers(subject: &str, metadata: &KranzCommitMetadata) -> String {
    format!("{subject}\n\n{}", kranz_commit_trailers(metadata))
}

/// Outcome of a [`GitRepo::merge_no_ff`] into the current branch (roadmap M3).
///
/// A `Conflict` merge is always rolled back with `git merge --abort` before it
/// is returned, so the working tree is left clean either way — the caller never
/// has to clean up a half-merged tree. A `RefusedPreMerge` failure never had a
/// merge in progress (no `MERGE_HEAD`), so no abort is attempted — there is
/// nothing to roll back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The branch merged cleanly; the merge commit is on the current branch.
    Clean,
    /// The merge hit conflicts and was aborted. `files` lists the conflicting
    /// paths git reported (best-effort; empty when git named none).
    Conflict { files: Vec<String> },
    /// Git refused the merge before it started (no `MERGE_HEAD` was ever
    /// created) — e.g. an untracked file at a path the merge would bring in.
    /// `detail` is git's verbatim stderr/stdout for the failed merge command.
    /// No `git merge --abort` is attempted, since there is no merge in
    /// progress to abort.
    RefusedPreMerge { detail: String },
}

/// Outcome of a scoped engine checkpoint commit ([`GitRepo::commit_dirty_paths`]).
///
/// The pre-commit secret scan refusing a checkpoint is a POLICY decision, not
/// a git failure, so it is an outcome (mirroring [`MergeOutcome`]) rather than
/// an [`EngineError::Git`]: callers on the mission loop must be able to record
/// the refusal and keep the mission moving — a dirty tree survives resume, so
/// a propagated refusal would wedge the mission re-hitting the same error
/// forever. Real git failures still surface as `Err`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckpointOutcome {
    /// The checkpoint landed (or the tree was already clean); carries the
    /// resulting head sha.
    Committed(String),
    /// The secret scan refused the checkpoint. `detail` names the findings
    /// (rule ids + fingerprints, never raw secret bytes) and the allowlist
    /// path for a reviewed waiver. Nothing was staged or committed.
    RefusedBySecretScan { detail: String },
}

/// Handle to a local git repository rooted at a working-tree directory.
#[derive(Debug, Clone)]
pub struct GitRepo {
    root: PathBuf,
    /// When true, every git invocation from this handle runs with hooks
    /// disabled via `-c core.hooksPath=` (see [`GitRepo::with_hooks_disabled`]).
    /// Default false: worker-side git behavior keeps the repo's hooks.
    hooks_disabled: bool,
}

impl GitRepo {
    /// Open `root` as a git repository.
    ///
    /// Verifies `git rev-parse --git-dir` succeeds inside `root`; returns
    /// [`EngineError::Git`] when `root` is not a repository (or git itself
    /// cannot be invoked).
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let repo = GitRepo {
            root: root.into(),
            hooks_disabled: false,
        };
        let out = repo.probe(&["rev-parse", "--git-dir"])?;
        if out.status.success() {
            Ok(repo)
        } else {
            Err(EngineError::Git(format!(
                "not a git repository: {} ({})",
                repo.root.display(),
                failure_detail(&out)
            )))
        }
    }

    /// The working-tree root this handle operates on.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// A handle to the same repository whose every git invocation runs with
    /// hooks disabled (`git -c core.hooksPath=` — the empty value resolves
    /// every hook lookup to nothing, so no hook file is ever executed).
    ///
    /// The gated merge path uses this: its scratch worktree's gitdir points
    /// into the primary `.git`, so mission-authored gate/test code can plant
    /// `.git/hooks/*` — which the merge's own checkout / merge / worktree
    /// commands would then execute with the server's full inherited
    /// environment, exactly the tokens the sanitized gate executor withholds.
    /// Opt-in per handle: worker-side git behavior is deliberately unchanged.
    pub fn with_hooks_disabled(&self) -> GitRepo {
        GitRepo {
            root: self.root.clone(),
            hooks_disabled: true,
        }
    }

    /// Sha of `HEAD` (`git rev-parse HEAD`).
    pub fn head_sha(&self) -> Result<String> {
        Ok(self.run(&["rev-parse", "HEAD"])?.trim().to_string())
    }

    /// Name of the currently checked-out branch (`"HEAD"` when detached).
    pub fn current_branch(&self) -> Result<String> {
        Ok(self
            .run(&["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_string())
    }

    /// Sha of an arbitrary ref (`git rev-parse <refname>`).
    ///
    /// Rejects a flag-shaped `refname` (leading `-`) with an
    /// [`EngineError::Git`] before invoking git, mirroring the guard on
    /// [`GitRepo::add_worktree`]/[`GitRepo::merge_no_ff`]/
    /// [`GitRepo::push_mission_branch`].
    pub fn rev_parse(&self, refname: &str) -> Result<String> {
        if refname.starts_with('-') {
            return Err(EngineError::Git(format!(
                "refusing rev-parse of flag-shaped ref {refname:?}"
            )));
        }
        Ok(self.run(&["rev-parse", refname])?.trim().to_string())
    }

    /// Whether `ancestor` is an ancestor of (or equal to) `descendant`
    /// (`git merge-base --is-ancestor <ancestor> <descendant>`).
    ///
    /// git's contract: exit 0 => `Ok(true)`; exit 1 => `Ok(false)`; any other
    /// exit code is a real git failure, surfaced as [`EngineError::Git`].
    /// Rejects a flag-shaped `ancestor`/`descendant` (leading `-`) before
    /// invoking git, mirroring [`GitRepo::rev_parse`]/[`GitRepo::merge_no_ff`].
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool> {
        for slot in [ancestor, descendant] {
            if slot.starts_with('-') {
                return Err(EngineError::Git(format!(
                    "refusing is_ancestor with flag-shaped ref {slot:?}"
                )));
            }
        }
        let out = self.probe(&["merge-base", "--is-ancestor", ancestor, descendant])?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(EngineError::Git(format!(
                "git merge-base --is-ancestor {ancestor} {descendant} failed ({}): {}",
                out.status,
                failure_detail(&out)
            ))),
        }
    }

    /// Whether a local branch of this name exists.
    pub fn branch_exists(&self, name: &str) -> Result<bool> {
        let git_ref = format!("refs/heads/{name}");
        let out = self.probe(&["rev-parse", "--verify", "--quiet", &git_ref])?;
        Ok(out.status.success())
    }

    /// Create branch `name` at `from` (a sha or ref), or at `HEAD` when
    /// `from` is `None`. Does not check the branch out.
    pub fn create_branch(&self, name: &str, from: Option<&str>) -> Result<()> {
        let mut args = vec!["branch", name];
        if let Some(start) = from {
            args.push(start);
        }
        self.run(&args)?;
        Ok(())
    }

    /// Check out an existing branch (or any committish).
    pub fn checkout(&self, name: &str) -> Result<()> {
        self.run(&["checkout", name])?;
        Ok(())
    }

    /// True when the working tree has no changes at all. `--porcelain`
    /// output includes untracked files, so those count as dirty too.
    pub fn is_clean(&self) -> Result<bool> {
        Ok(self.run(&["status", "--porcelain"])?.trim().is_empty())
    }

    /// Like [`Self::is_clean`] but ignoring untracked files: `true` when no
    /// TRACKED file is modified, staged, or deleted. Untracked files never
    /// block a branch switch (git carries them across), so restore-checkout
    /// paths use this rather than full cleanliness.
    pub fn is_clean_tracked(&self) -> Result<bool> {
        Ok(self
            .run(&["status", "--porcelain", "--untracked-files=no"])?
            .trim()
            .is_empty())
    }

    /// Like [`Self::is_clean_tracked`], but also rejects index flags that can
    /// hide working-tree changes (`assume-unchanged` / `skip-worktree`).
    ///
    /// Scratch merge worktrees are never sparse and never need either flag,
    /// so every tracked entry must have git's normal `H` tag.
    pub fn is_clean_tracked_strict(&self) -> Result<bool> {
        if !self.is_clean_tracked()? {
            return Ok(false);
        }
        Ok(self
            .run(&["ls-files", "-v"])?
            .lines()
            .all(|line| line.starts_with("H ")))
    }

    /// Whether one tracked path has Git's normal index tag. Lowercase tags
    /// (`assume-unchanged`) and `S` (`skip-worktree`) can hide worktree bytes
    /// from ordinary diff/status commands and must not guard a trust decision.
    pub fn has_normal_index_entry(&self, path: &str) -> Result<bool> {
        let output = self.run(&["ls-files", "-v", "--", path])?;
        let mut lines = output.lines();
        Ok(lines.next() == Some(format!("H {path}").as_str()) && lines.next().is_none())
    }

    /// `git add -A` then `git commit -m <message>`; returns the new head sha.
    ///
    /// A no-change commit attempt exits non-zero, so it surfaces as an
    /// [`EngineError::Git`] carrying git's own "nothing to commit" output.
    pub fn add_all_and_commit(&self, message: &str) -> Result<String> {
        self.run(&["add", "-A"])?;
        self.run(&["commit", "-m", message])?;
        self.head_sha()
    }

    /// Paths currently dirty in the working tree (`git status --porcelain`),
    /// relative to the repo root. Empty when clean.
    pub fn dirty_paths(&self) -> Result<Vec<PathBuf>> {
        let out = self.run(&["status", "--porcelain", "-z"])?;
        let mut paths = Vec::new();
        // Porcelain -z records: XY<space>path\0, or for rename/copy
        // XY<space>newpath\0oldpath\0. Walk byte-wise so a bare oldpath
        // record is not mistaken for a status line.
        let bytes = out.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0 {
                i += 1;
                continue;
            }
            let start = i;
            while i < bytes.len() && bytes[i] != 0 {
                i += 1;
            }
            let entry = std::str::from_utf8(&bytes[start..i]).unwrap_or("");
            i += 1; // skip NUL
            if entry.len() < 4 {
                continue;
            }
            let status = &entry[..2];
            let path = if entry.as_bytes().get(2) == Some(&b' ') {
                &entry[3..]
            } else {
                entry.trim()
            };
            if path.is_empty() {
                continue;
            }
            paths.push(PathBuf::from(path));
            // Rename/copy: the record continues as `\0oldpath\0`. The source
            // path is part of the same change — a staged `git mv a b` must
            // report BOTH `b` and `a`, or a checkpoint commit scoped to the
            // dirty set commits only `b` and leaves the staged `D a` behind —
            // so it joins the dirty set rather than being skipped.
            if status.contains('R') || status.contains('C') {
                let old_start = i;
                while i < bytes.len() && bytes[i] != 0 {
                    i += 1;
                }
                let old = std::str::from_utf8(&bytes[old_start..i]).unwrap_or("");
                if i < bytes.len() {
                    i += 1; // skip NUL after oldpath
                }
                if !old.is_empty() {
                    paths.push(PathBuf::from(old));
                }
            }
        }
        Ok(paths)
    }

    /// Stage and commit only currently-dirty paths (scoped checkpoint).
    /// Prefer this over [`Self::add_all_and_commit`] for engine checkpoints so
    /// a concurrent operator edit outside the worker's tree is not scooped in
    /// via `git add -A`. No-op (returns current HEAD) when the tree is clean.
    ///
    /// A secret-scan refusal is reported as
    /// [`CheckpointOutcome::RefusedBySecretScan`], never as an `Err` —
    /// checkpoint callers sit on the mission loop and must record the refusal
    /// instead of erroring the run (see [`CheckpointOutcome`]). Real git
    /// failures still propagate.
    pub fn commit_dirty_paths(&self, message: &str) -> Result<CheckpointOutcome> {
        let paths = self.dirty_paths()?;
        if paths.is_empty() {
            return Ok(CheckpointOutcome::Committed(self.head_sha()?));
        }
        let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        if let Some(detail) = self.secret_scan_refusal(&refs) {
            return Ok(CheckpointOutcome::RefusedBySecretScan { detail });
        }
        Ok(CheckpointOutcome::Committed(
            self.commit_paths_unscanned(&refs, message)?,
        ))
    }

    /// Stage and commit only the given paths; returns the new head sha.
    ///
    /// Paths may be absolute or relative to the repo root. Content staged
    /// for *other* paths is left staged and untouched (`git commit -- <paths>`
    /// commits just the named pathspecs).
    ///
    /// Idempotent: if staging the named paths yields no change (e.g. a
    /// crash-replayed re-commit of byte-identical files), this is a no-op that
    /// returns the current head rather than an empty-commit error. An empty
    /// `paths` slice is still rejected up front.
    pub fn commit_paths(&self, paths: &[&Path], message: &str) -> Result<String> {
        if paths.is_empty() {
            return Err(EngineError::Git("commit_paths: no paths given".into()));
        }
        // Durable-record commits (plans, reports) treat a scan refusal as a
        // hard error: the engine authored those files itself, so a finding
        // there is a bug, not a worker leftover to route around. Checkpoint
        // callers go through commit_dirty_paths, which surfaces the same
        // refusal as a CheckpointOutcome instead.
        if let Some(detail) = self.secret_scan_refusal(paths) {
            return Err(EngineError::Git(detail));
        }
        self.commit_paths_unscanned(paths, message)
    }

    /// The formatted refusal message when the engine secret scan (minus
    /// allowlisted fingerprints) finds anything in `paths`, or `None` when
    /// the commit may proceed. The message names the findings via
    /// [`scrub::format_findings`] (rule ids + fingerprints, never raw secret
    /// bytes) and the allowlist path for a reviewed waiver.
    fn secret_scan_refusal(&self, paths: &[&Path]) -> Option<String> {
        let allowed = std::fs::read_to_string(self.root.join(scrub::SECRET_ALLOWLIST_PATH))
            .ok()
            .map(|text| scrub::read_allowlist_text(&text))
            .unwrap_or_default();
        let findings = scrub::filter_allowed(scrub::scan_paths(&self.root, paths), &allowed);
        if findings.is_empty() {
            None
        } else {
            Some(format!(
                "secret scan blocked engine commit; add a fingerprint to {} only for a reviewed false positive:\n{}",
                scrub::SECRET_ALLOWLIST_PATH,
                scrub::format_findings(&findings)
            ))
        }
    }

    /// [`Self::commit_paths`] minus the secret scan. Private on purpose:
    /// every public commit path must either run the scan (commit_paths) or
    /// surface its refusal as a [`CheckpointOutcome`] (commit_dirty_paths).
    fn commit_paths_unscanned(&self, paths: &[&Path], message: &str) -> Result<String> {
        let path_args = paths.iter().map(|p| p.as_os_str().to_os_string());

        // `git add` fatals ("pathspec ... did not match any files") on a path
        // that is gone from BOTH the working tree and the index — exactly a
        // rename/copy source whose deletion `git mv` already staged. Such a
        // path needs no staging (the commit pathspec below still carries the
        // staged deletion into the commit), so it is left out of the add. A
        // path merely deleted from the working tree but still in the index
        // stays in: `git add` stages that removal.
        let add_paths = self.addable_paths(paths)?;
        if !add_paths.is_empty() {
            let mut add: Vec<OsString> = vec!["add".into(), "--".into()];
            add.extend(add_paths.iter().map(|p| p.as_os_str().to_os_string()));
            self.run_os(&add)?;
        }

        // Idempotent: if staging these pathspecs produced nothing (e.g. a
        // crash-replayed re-approval that rewrites byte-identical files), skip
        // the commit and return the unchanged head. `git commit` errors on an
        // empty commit, which would otherwise wedge the caller on replay.
        let mut staged: Vec<OsString> = vec![
            "diff".into(),
            "--cached".into(),
            "--name-only".into(),
            "--".into(),
        ];
        staged.extend(path_args.clone());
        if self.run_os(&staged)?.trim().is_empty() {
            return self.head_sha();
        }

        let mut commit: Vec<OsString> =
            vec!["commit".into(), "-m".into(), message.into(), "--".into()];
        commit.extend(path_args);
        self.run_os(&commit)?;

        self.head_sha()
    }

    /// The subset of `paths` that `git add` can act on: present in the
    /// working tree (`symlink_metadata`, so a dangling symlink still counts)
    /// or still known to the index (a working-tree deletion whose removal
    /// `git add` stages). A path in NEITHER — e.g. the source of an
    /// already-staged rename — would make `git add` fail with "pathspec did
    /// not match any files", and has nothing left to stage anyway.
    fn addable_paths<'a>(&self, paths: &[&'a Path]) -> Result<Vec<&'a Path>> {
        let missing: Vec<&Path> = paths
            .iter()
            .copied()
            .filter(|p| {
                let full = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    self.root.join(p)
                };
                std::fs::symlink_metadata(full).is_err()
            })
            .collect();
        if missing.is_empty() {
            return Ok(paths.to_vec());
        }
        // One batched index probe for the disk-missing subset. `git ls-files`
        // exits 0 with empty output for pathspecs that match nothing, and
        // prints matches relative to the repo root.
        let mut ls: Vec<OsString> = vec!["ls-files".into(), "-z".into(), "--".into()];
        ls.extend(missing.iter().map(|p| p.as_os_str().to_os_string()));
        let in_index: std::collections::HashSet<PathBuf> = self
            .run_os(&ls)?
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        Ok(paths
            .iter()
            .copied()
            .filter(|p| {
                let rel = p.strip_prefix(&self.root).unwrap_or(p);
                std::fs::symlink_metadata(self.root.join(rel)).is_ok() || in_index.contains(rel)
            })
            .collect())
    }

    /// Commits reachable from `to` but not `from` (`from..to`), oldest first.
    pub fn commits_between(&self, from: &str, to: &str) -> Result<Vec<CommitInfo>> {
        let range = format!("{from}..{to}");
        // %x09 = tab separator; a subject can contain anything but a newline.
        let out = self.run(&["log", "--reverse", "--format=%H%x09%s", &range])?;
        let mut commits = Vec::new();
        for line in out.lines() {
            // `lines()` strips \n; strip a stray \r for CRLF robustness (§9).
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            let (sha, subject) = line.split_once('\t').unwrap_or((line, ""));
            commits.push(CommitInfo {
                sha: sha.to_string(),
                subject: subject.to_string(),
            });
        }
        Ok(commits)
    }

    /// Count merge commits reachable from `to` but not `from`.
    pub fn merge_commit_count(&self, from: &str, to: &str) -> Result<usize> {
        for slot in [from, to] {
            if slot.starts_with('-') {
                return Err(EngineError::Git(format!(
                    "refusing merge_commit_count with flag-shaped ref {slot:?}"
                )));
            }
        }
        let range = format!("{from}..{to}");
        let out = self.run(&["rev-list", "--merges", "--count", &range])?;
        out.trim().parse::<usize>().map_err(|e| {
            EngineError::Git(format!(
                "git rev-list --merges --count {range} returned non-numeric output {out:?}: {e}"
            ))
        })
    }

    /// `git diff --stat <from>..<to>` output, verbatim.
    pub fn diff_stat(&self, from: &str, to: &str) -> Result<String> {
        let range = format!("{from}..{to}");
        self.run(&["diff", "--stat", &range])
    }

    /// Full `git diff <from>..<to>` output, verbatim.
    pub fn diff_full(&self, from: &str, to: &str) -> Result<String> {
        let range = format!("{from}..{to}");
        self.run(&["diff", &range])
    }

    /// Full `git diff <range>` output for a caller-supplied range.
    pub fn diff_range(&self, range: &str) -> Result<String> {
        if range.starts_with('-') || range.chars().any(char::is_whitespace) {
            return Err(EngineError::Git(format!(
                "refusing diff of malformed range {range:?}"
            )));
        }
        self.run(&["diff", range])
    }

    /// Full staged diff (`git diff --cached`) output.
    pub fn diff_staged(&self) -> Result<String> {
        self.run(&["diff", "--cached"])
    }

    /// Paths changed in `from..to` (`git diff --name-only <from>..<to>`),
    /// one per line as git reports them.
    ///
    /// Rejects a flag-shaped `from`/`to` (leading `-`) before invoking git,
    /// mirroring the guard on [`GitRepo::is_ancestor`]/[`GitRepo::rev_parse`].
    pub fn changed_paths(&self, from: &str, to: &str) -> Result<Vec<String>> {
        for slot in [from, to] {
            if slot.starts_with('-') {
                return Err(EngineError::Git(format!(
                    "refusing changed_paths with flag-shaped ref {slot:?}"
                )));
            }
        }
        let range = format!("{from}..{to}");
        let out = self.run(&["diff", "--name-only", &range])?;
        Ok(out
            .lines()
            .map(|l| l.trim_end_matches('\r').trim())
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Whether `from..to` touches anything under `apps/dashboard/` — the
    /// signal the gate suite uses to decide whether to run the dashboard
    /// gates (roadmap M6 gated merge).
    pub fn dashboard_touched(&self, from: &str, to: &str) -> Result<bool> {
        Ok(self
            .changed_paths(from, to)?
            .iter()
            .any(|p| p.starts_with("apps/dashboard/")))
    }

    /// Create an annotated tag at `HEAD` (`git tag -a <name> -m <message>`).
    pub fn tag(&self, name: &str, message: &str) -> Result<()> {
        self.run(&["tag", "-a", name, "-m", message])?;
        Ok(())
    }

    // -- worktrees (roadmap M3 parallel workers) ---------------------------
    //
    // Parallel-within-milestone execution runs each independent feature's
    // worker in its own git worktree checked out to a per-feature branch off
    // the milestone-start sha, then merges those branches back into the mission
    // branch in declared order. The worktrees share this repo's object store
    // but have their own working directories, so concurrent workers never step
    // on each other's files. All operations shell out with explicit arg vectors
    // and std::path, so they stay Windows-safe like the rest of GitRepo.

    /// Create a new worktree at `path`, checked out to a NEW branch `branch`
    /// created at `from_sha` (`git worktree add -b <branch> <path> <from_sha>`).
    ///
    /// `path` may be absolute or relative to the repo root; git records the
    /// absolute path either way. The branch must not already exist (git's `-b`
    /// fails otherwise) — callers use a fresh per-feature branch name.
    pub fn add_worktree(&self, path: &Path, branch: &str, from_sha: &str) -> Result<()> {
        // Guard against a caller sneaking a flag through the branch/sha slots.
        for slot in [branch, from_sha] {
            if slot.starts_with('-') {
                return Err(EngineError::Git(format!(
                    "refusing worktree add with flag-shaped argument {slot:?}"
                )));
            }
        }
        let args: Vec<OsString> = vec![
            "worktree".into(),
            "add".into(),
            "-b".into(),
            branch.into(),
            path.as_os_str().to_os_string(),
            from_sha.into(),
        ];
        self.run_os(&args)?;
        Ok(())
    }

    /// Create a new worktree at `path`, checked out to the EXISTING branch
    /// `branch` (`git worktree add <path> <branch>`, no `-b`).
    ///
    /// `path` may be absolute or relative to the repo root; git records the
    /// absolute path either way. `branch` must already exist and must NOT
    /// already be checked out in another worktree — git refuses to check the
    /// same branch out twice and that failure surfaces as [`EngineError::Git`].
    pub fn add_worktree_checkout(&self, path: &Path, branch: &str) -> Result<()> {
        // Guard against a caller sneaking a flag through the branch slot.
        if branch.starts_with('-') {
            return Err(EngineError::Git(format!(
                "refusing worktree add with flag-shaped argument {branch:?}"
            )));
        }
        let args: Vec<OsString> = vec![
            "worktree".into(),
            "add".into(),
            path.as_os_str().to_os_string(),
            branch.into(),
        ];
        self.run_os(&args)?;
        Ok(())
    }

    /// Create a detached worktree at `path` pinned to `commit`.
    ///
    /// Gated merge uses this to build and validate an integration commit
    /// without checking out either moving branch in the primary tree.
    pub fn add_detached_worktree(&self, path: &Path, commit: &str) -> Result<()> {
        if commit.starts_with('-') {
            return Err(EngineError::Git(format!(
                "refusing detached worktree add with flag-shaped commit {commit:?}"
            )));
        }
        let args: Vec<OsString> = vec![
            "worktree".into(),
            "add".into(),
            "--detach".into(),
            path.as_os_str().to_os_string(),
            commit.into(),
        ];
        self.run_os(&args)?;
        Ok(())
    }

    /// Remove a worktree at `path` (`git worktree remove --force <path>`),
    /// tolerating a worktree that is already gone.
    ///
    /// `--force` is used so a worktree with a dirty tree (a worker that left
    /// uncommitted changes, or a merge that has already consumed its commits)
    /// is still removed — leaked worktrees are the failure mode this guards
    /// against. When git reports the worktree is not registered / does not
    /// exist, that is treated as success (idempotent cleanup). Any OTHER git
    /// failure surfaces as [`EngineError::Git`].
    pub fn remove_worktree(&self, path: &Path) -> Result<()> {
        let args: Vec<OsString> = vec![
            "worktree".into(),
            "remove".into(),
            "--force".into(),
            path.as_os_str().to_os_string(),
        ];
        let out = self.probe_os(&args)?;
        if out.status.success() {
            return Ok(());
        }
        // Already-gone worktrees are fine: git says "is not a working tree" or
        // "No such file or directory" / "not a valid path". Match leniently on
        // the combined output so cleanup is idempotent across git versions.
        let detail = failure_detail(&out).to_lowercase();
        let already_gone = detail.contains("is not a working tree")
            || detail.contains("not a working tree")
            || detail.contains("no such file")
            || detail.contains("is not a valid path")
            || detail.contains("not a valid path");
        if already_gone {
            Ok(())
        } else {
            Err(EngineError::Git(format!(
                "git worktree remove {} failed ({}): {}",
                path.display(),
                out.status,
                failure_detail(&out)
            )))
        }
    }

    /// Merge `branch` into the current branch with an explicit merge commit
    /// (`git merge --no-ff --no-edit <branch>`), reporting clean vs conflict.
    ///
    /// A clean merge returns [`MergeOutcome::Clean`] with the merge commit on
    /// the current branch. On conflict the merge is rolled back with
    /// `git merge --abort` (so the working tree is left CLEAN — the porcelain
    /// status is empty afterwards) and [`MergeOutcome::Conflict`] is returned,
    /// carrying the conflicting paths git named. When git refuses the merge
    /// before it ever starts (no `MERGE_HEAD`, e.g. an untracked file in the
    /// way) [`MergeOutcome::RefusedPreMerge`] is returned instead, carrying
    /// git's verbatim refusal — no abort is attempted, since there is nothing
    /// to abort. Only a genuine git failure (git could not be spawned, or the
    /// abort itself failed on a real conflict) is an `Err`.
    pub fn merge_no_ff(&self, branch: &str) -> Result<MergeOutcome> {
        self.merge_no_ff_with_message(branch, None)
    }

    /// Like [`Self::merge_no_ff`] but supplies an explicit merge commit
    /// message, used for kranz-authored trailer metadata.
    pub fn merge_no_ff_with_message(
        &self,
        branch: &str,
        message: Option<&str>,
    ) -> Result<MergeOutcome> {
        if branch.starts_with('-') {
            return Err(EngineError::Git(format!(
                "refusing to merge flag-shaped ref {branch:?}"
            )));
        }
        let out = match message {
            Some(message) => self.probe(&["merge", "--no-ff", "-m", message, branch])?,
            None => self.probe(&["merge", "--no-ff", "--no-edit", branch])?,
        };
        if out.status.success() {
            return Ok(MergeOutcome::Clean);
        }
        // Distinguish a genuine content conflict (MERGE_HEAD exists — a merge
        // is actually in progress) from a pre-merge refusal (e.g. an
        // untracked file the merge would overwrite), which never creates
        // MERGE_HEAD and so has nothing for `git merge --abort` to roll back.
        let merge_in_progress = self
            .probe(&["rev-parse", "-q", "--verify", "MERGE_HEAD"])?
            .status
            .success();
        if !merge_in_progress {
            return Ok(MergeOutcome::RefusedPreMerge {
                detail: failure_detail(&out),
            });
        }
        // A conflicting merge leaves the tree mid-merge; collect the unmerged
        // paths (best-effort) BEFORE aborting, then abort to restore a clean
        // tree so the caller never inherits a half-merged working directory.
        let files = self.unmerged_paths().unwrap_or_default();
        // `git merge --abort` must succeed to honour the clean-tree contract;
        // a failure here is a real error (the tree is left mid-merge).
        self.run(&["merge", "--abort"]).map_err(|e| {
            EngineError::Git(format!(
                "merge of {branch:?} conflicted and `git merge --abort` also failed: {e}"
            ))
        })?;
        Ok(MergeOutcome::Conflict { files })
    }

    /// Move the current branch to an already-created descendant commit with
    /// `git merge --ff-only`. Gated merge uses this after validating the exact
    /// integration commit in a scratch worktree.
    pub fn fast_forward_to(&self, commit: &str) -> Result<MergeOutcome> {
        if commit.starts_with('-') {
            return Err(EngineError::Git(format!(
                "refusing fast-forward to flag-shaped commit {commit:?}"
            )));
        }
        let out = self.probe(&["merge", "--ff-only", commit])?;
        if out.status.success() {
            Ok(MergeOutcome::Clean)
        } else {
            Ok(MergeOutcome::RefusedPreMerge {
                detail: failure_detail(&out),
            })
        }
    }

    /// Bytes of `path` as it exists on `branch` (`git show <branch>:<path>`),
    /// or `None` when the path does not exist on that branch. Used to compare
    /// an untracked working-tree file byte-for-byte against the version a
    /// merge would bring in, so it can be safely removed when identical.
    pub fn show_file(&self, branch: &str, path: &str) -> Result<Option<Vec<u8>>> {
        if branch.starts_with('-') {
            return Err(EngineError::Git(format!(
                "refusing show_file with flag-shaped ref {branch:?}"
            )));
        }
        let spec = format!("{branch}:{path}");
        let out = self.probe(&["show", &spec])?;
        if out.status.success() {
            Ok(Some(out.stdout))
        } else {
            let detail = failure_detail(&out).to_lowercase();
            if detail.contains("does not exist") || detail.contains("exists on disk, but not") {
                Ok(None)
            } else {
                Err(EngineError::Git(format!(
                    "git show {spec} failed ({}): {}",
                    out.status,
                    failure_detail(&out)
                )))
            }
        }
    }

    /// Whether `path` is currently untracked in the working tree
    /// (`git status --porcelain -- <path>` reports a `??` entry). `false`
    /// when the path is tracked, ignored-and-absent, or simply not present.
    pub fn is_untracked(&self, path: &str) -> Result<bool> {
        let out = self.run(&["status", "--porcelain", "--", path])?;
        Ok(out.lines().any(|l| l.starts_with("??")))
    }

    /// Paths with unmerged (conflicted) entries in the index
    /// (`git diff --name-only --diff-filter=U`). Empty when there are none.
    fn unmerged_paths(&self) -> Result<Vec<String>> {
        let out = self.run(&["diff", "--name-only", "--diff-filter=U"])?;
        Ok(out
            .lines()
            .map(|l| l.trim_end_matches('\r').trim())
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Absolute paths of every registered worktree (`git worktree list`),
    /// including the primary working tree. Used by cleanup to detect leaks.
    pub fn list_worktrees(&self) -> Result<Vec<String>> {
        // `--porcelain` emits `worktree <abs-path>` lines (plus HEAD/branch
        // detail we ignore); parse just the paths for a stable, quoting-free
        // listing across git versions.
        let out = self.run(&["worktree", "list", "--porcelain"])?;
        let mut paths = Vec::new();
        for line in out.lines() {
            let line = line.trim_end_matches('\r');
            if let Some(rest) = line.strip_prefix("worktree ") {
                paths.push(rest.trim().to_string());
            }
        }
        Ok(paths)
    }

    /// Prune administrative records of worktrees whose directories are gone
    /// (`git worktree prune`). Safe to call unconditionally after cleanup.
    pub fn prune_worktrees(&self) -> Result<()> {
        self.run(&["worktree", "prune"])?;
        Ok(())
    }

    /// Delete a local branch, force (`git branch -D <name>`), tolerating a
    /// branch that is already gone. Used to tidy per-feature worktree branches
    /// after their worktrees are removed (roadmap M3 cleanup).
    pub fn delete_branch_force(&self, name: &str) -> Result<()> {
        if name.starts_with('-') {
            return Err(EngineError::Git(format!(
                "refusing to delete flag-shaped branch {name:?}"
            )));
        }
        let out = self.probe(&["branch", "-D", name])?;
        if out.status.success() {
            return Ok(());
        }
        let detail = failure_detail(&out).to_lowercase();
        if detail.contains("not found") || detail.contains("no branch") {
            Ok(())
        } else {
            Err(EngineError::Git(format!(
                "git branch -D {name} failed ({}): {}",
                out.status,
                failure_detail(&out)
            )))
        }
    }

    /// Whether a remote named `name` is configured (`git remote get-url`).
    ///
    /// A probe, not an assertion: returns `Ok(false)` when the remote is
    /// absent and only errors when git itself cannot be spawned. Callers use
    /// this to decide whether a cloud mission has anywhere to push to before
    /// calling [`GitRepo::push_mission_branch`].
    pub fn has_remote(&self, name: &str) -> Result<bool> {
        let out = self.probe(&["remote", "get-url", name])?;
        Ok(out.status.success())
    }

    /// Push a single `kranz/*` mission ref to `remote` — **the one and only
    /// push path in Kranz, and it is cloud-opt-in.**
    ///
    /// ## Local default: Kranz never pushes (plan §4.4)
    ///
    /// Git is the source of truth, but on a local host Kranz writes only to the
    /// working tree and local refs — it never contacts a remote. No mission
    /// loop, no CLI verb, and nothing in the server calls this method today; it
    /// exists as the primitive that M6 cloud-mission wiring will call
    /// explicitly. Nothing about the local default changes by this method
    /// merely existing (roadmap M6, "Scoped push").
    ///
    /// ## Guard rails (why this is safe to expose)
    ///
    /// - The branch **must** begin with `kranz/` — mission branches are
    ///   `kranz/mission-<id>` and mission tags live under `kranz/<id>/…`.
    ///   Anything else (`main`, `master`, `HEAD`, a bare sha, `--force`, or a
    ///   refspec smuggling a second ref) is rejected with
    ///   [`EngineError::Git`] **before any git process runs** — no network.
    /// - The push is a plain `git push <remote> <branch>`: never `--force`,
    ///   never a `src:dst` refspec, never `main`, never a merge. The human
    ///   still reviews the `kranz/*` branch and opens the PR (roadmap M6).
    /// - On failure git's stderr is surfaced verbatim via [`EngineError::Git`],
    ///   so a bad deploy key or a rejected non-fast-forward shows up in the
    ///   mission log with git's own words.
    ///
    /// The deploy key / GitHub App backing `remote` should itself be scoped to
    /// `kranz/*` refs (see docs/deploy.md); this guard is defence in depth, not
    /// the only line of defence.
    pub fn push_mission_branch(&self, remote: &str, branch: &str) -> Result<()> {
        // Defence in depth: refuse anything that is not a mission ref *before*
        // spawning git, so a mis-wired caller can never push main or a merge.
        // `kranz/` (with the slash) is required so a branch literally named
        // "kranz" or "kranzfoo" cannot slip through.
        if !branch.starts_with("kranz/") {
            return Err(EngineError::Git(format!(
                "refusing to push non-kranz ref {branch:?}: push_mission_branch \
                 only pushes kranz/* mission refs, never main or merges"
            )));
        }
        // Reject characters that could turn a single branch name into extra
        // arguments or a src:dst refspec. A legitimate mission ref never
        // contains whitespace, a colon, or a leading dash.
        if branch.contains(':')
            || branch.starts_with('-')
            || branch.chars().any(char::is_whitespace)
        {
            return Err(EngineError::Git(format!(
                "refusing to push malformed ref {branch:?}: a mission branch is \
                 a plain kranz/* name with no refspec, flags, or whitespace"
            )));
        }
        // Plain push of one local branch to the same-named remote branch.
        // Never --force; never a refspec; never main.
        self.run(&["push", remote, branch])?;
        Ok(())
    }

    /// Guarantee commits can be made: when `user.name` / `user.email` resolve
    /// to nothing for this repo (any config scope), set a local identity of
    /// `kranz <kranz@localhost>`. Existing identities are never overwritten,
    /// and missions never fail on hosts without a global git identity.
    pub fn ensure_identity(&self) -> Result<()> {
        for (key, value) in [("user.name", "kranz"), ("user.email", "kranz@localhost")] {
            let probe = self.probe(&["config", "--get", key])?;
            let already_set =
                probe.status.success() && !String::from_utf8_lossy(&probe.stdout).trim().is_empty();
            if !already_set {
                // `git config <key> <value>` writes to the local repo config.
                self.run(&["config", key, value])?;
            }
        }
        Ok(())
    }

    /// The git identity this repo resolves to right now: `(user.name,
    /// user.email)` from any config scope (local/global/system) visible to
    /// the calling process's environment, falling back to the same
    /// `kranz`/`kranz@localhost` pair [`ensure_identity`] would write when
    /// neither key resolves.
    ///
    /// Used to carry the *engine's* resolved identity into a worker session
    /// whose relocated `HOME` can no longer see the operator's global
    /// `~/.gitconfig` (see `GIT_AUTHOR_NAME` etc. injection in
    /// `runner::seed_worker_env`).
    pub fn resolved_identity(&self) -> Result<(String, String)> {
        let resolve = |key: &str, fallback: &str| -> Result<String> {
            let probe = self.probe(&["config", "--get", key])?;
            let value = String::from_utf8_lossy(&probe.stdout).trim().to_string();
            if probe.status.success() && !value.is_empty() {
                Ok(value)
            } else {
                Ok(fallback.to_string())
            }
        };
        let name = resolve("user.name", "kranz")?;
        let email = resolve("user.email", "kranz@localhost")?;
        Ok((name, email))
    }

    // -- plumbing ----------------------------------------------------------

    /// Run git and return the raw `Output` without checking the exit status
    /// (for existence/is-set probes). Errors only when git cannot be spawned.
    fn probe(&self, args: &[&str]) -> Result<Output> {
        let os: Vec<OsString> = args.iter().map(OsString::from).collect();
        self.probe_os(&os)
    }

    fn probe_os(&self, args: &[OsString]) -> Result<Output> {
        let mut cmd = Command::new("git");
        if self.hooks_disabled {
            // `-c` must precede the subcommand; the empty value makes every
            // hook lookup resolve to nothing (see with_hooks_disabled).
            cmd.args(["-c", "core.hooksPath="]);
        }
        cmd.args(args)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| {
                EngineError::Git(format!("failed to invoke git {}: {e}", render_args(args)))
            })
    }

    /// Run git, demanding success; returns raw stdout (callers trim as needed).
    fn run(&self, args: &[&str]) -> Result<String> {
        let os: Vec<OsString> = args.iter().map(OsString::from).collect();
        self.run_os(&os)
    }

    fn run_os(&self, args: &[OsString]) -> Result<String> {
        let out = self.probe_os(args)?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(EngineError::Git(format!(
                "git {} failed ({}): {}",
                render_args(args),
                out.status,
                failure_detail(&out)
            )))
        }
    }
}

/// Human-readable rendering of an argument vector for error context.
fn render_args(args: &[OsString]) -> String {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Best error detail available: stderr, falling back to stdout (git prints
/// e.g. "nothing to commit" on stdout).
fn failure_detail(out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    match (stderr.is_empty(), stdout.is_empty()) {
        (false, true) => stderr,
        (true, false) => stdout,
        (false, false) => format!("{stderr} | {stdout}"),
        (true, true) => "no output".to_string(),
    }
}
