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

/// Outcome of a [`GitRepo::merge_no_ff`] into the current branch (roadmap M3).
///
/// A `Conflict` merge is always rolled back with `git merge --abort` before it
/// is returned, so the working tree is left clean either way — the caller never
/// has to clean up a half-merged tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The branch merged cleanly; the merge commit is on the current branch.
    Clean,
    /// The merge hit conflicts and was aborted. `files` lists the conflicting
    /// paths git reported (best-effort; empty when git named none).
    Conflict { files: Vec<String> },
}

/// Handle to a local git repository rooted at a working-tree directory.
#[derive(Debug, Clone)]
pub struct GitRepo {
    root: PathBuf,
}

impl GitRepo {
    /// Open `root` as a git repository.
    ///
    /// Verifies `git rev-parse --git-dir` succeeds inside `root`; returns
    /// [`EngineError::Git`] when `root` is not a repository (or git itself
    /// cannot be invoked).
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let repo = GitRepo { root: root.into() };
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

    /// `git add -A` then `git commit -m <message>`; returns the new head sha.
    ///
    /// A no-change commit attempt exits non-zero, so it surfaces as an
    /// [`EngineError::Git`] carrying git's own "nothing to commit" output.
    pub fn add_all_and_commit(&self, message: &str) -> Result<String> {
        self.run(&["add", "-A"])?;
        self.run(&["commit", "-m", message])?;
        self.head_sha()
    }

    /// Stage and commit only the given paths; returns the new head sha.
    ///
    /// Paths may be absolute or relative to the repo root. Content staged
    /// for *other* paths is left staged and untouched (`git commit -- <paths>`
    /// commits just the named pathspecs).
    pub fn commit_paths(&self, paths: &[&Path], message: &str) -> Result<String> {
        if paths.is_empty() {
            return Err(EngineError::Git("commit_paths: no paths given".into()));
        }
        let path_args = paths.iter().map(|p| p.as_os_str().to_os_string());

        let mut add: Vec<OsString> = vec!["add".into(), "--".into()];
        add.extend(path_args.clone());
        self.run_os(&add)?;

        let mut commit: Vec<OsString> =
            vec!["commit".into(), "-m".into(), message.into(), "--".into()];
        commit.extend(path_args);
        self.run_os(&commit)?;

        self.head_sha()
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
    /// carrying the conflicting paths git named. Only a genuine git failure
    /// (git could not be spawned, or the abort itself failed) is an `Err`.
    pub fn merge_no_ff(&self, branch: &str) -> Result<MergeOutcome> {
        if branch.starts_with('-') {
            return Err(EngineError::Git(format!(
                "refusing to merge flag-shaped ref {branch:?}"
            )));
        }
        if self
            .probe(&["merge", "--no-ff", "--no-edit", branch])?
            .status
            .success()
        {
            return Ok(MergeOutcome::Clean);
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

    // -- plumbing ----------------------------------------------------------

    /// Run git and return the raw `Output` without checking the exit status
    /// (for existence/is-set probes). Errors only when git cannot be spawned.
    fn probe(&self, args: &[&str]) -> Result<Output> {
        let os: Vec<OsString> = args.iter().map(OsString::from).collect();
        self.probe_os(&os)
    }

    fn probe_os(&self, args: &[OsString]) -> Result<Output> {
        Command::new("git")
            .args(args)
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
