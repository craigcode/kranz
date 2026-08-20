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

/// The commit that introduced a path, from [`GitRepo::commit_that_added`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedCommit {
    /// Full commit sha.
    pub sha: String,
    /// First line of the commit message.
    pub subject: String,
    /// The message body after the subject (carries the trailer block).
    pub body: String,
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

/// One entry of a recursive tree listing ([`GitRepo::ls_tree_recursive`]):
/// the git file mode (`100644`/`100755` regular blob, `120000` symlink,
/// `160000` submodule commit), the object kind (`blob`/`commit`), the blob
/// size in bytes (`None` for non-blobs), and the repo-relative path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: String,
    pub kind: String,
    pub size: Option<u64>,
    pub path: String,
}

/// Handle to a local git repository rooted at a working-tree directory.
#[derive(Debug, Clone)]
pub struct GitRepo {
    root: PathBuf,
    /// `Some(argv)` when every git invocation from this handle must run with
    /// executable configuration disabled (see [`GitRepo::with_hooks_disabled`]):
    /// the complete `-c key=value` argv segment, built once at
    /// handle-construction time. `None` keeps the repo's executable config —
    /// worker-side git behavior is deliberately unchanged.
    exec_disable_flags: Option<Vec<String>>,
}

/// git on Windows cannot parse VERBATIM paths (`\\?\C:\...`, which
/// `std::fs::canonicalize` returns there — and the engine canonicalizes
/// repo roots for the no-follow guards): `git worktree add //?/C:/...`
/// fails with "Invalid argument". Strip the prefix when handing a path to
/// git; a no-op off Windows and on non-verbatim paths. (`\\?\UNC\` shares
/// are not collapsed — no mission root legitimately lives on one.)
fn git_path_arg(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let rendered = path.as_os_str().to_string_lossy();
        if let Some(rest) = rendered.strip_prefix(r"\\?\") {
            if !rest.starts_with("UNC") {
                return PathBuf::from(rest);
            }
        }
    }
    path.to_path_buf()
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
            exec_disable_flags: None,
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
    /// executable configuration disabled (see [`Self::build_exec_disable_flags`]
    /// for the exact flag set and the surfaces each entry neutralizes, 13th-pass
    /// review P1 — the set previously stopped at `core.hooksPath=` +
    /// `core.fsmonitor=` while this doc claimed "every executable surface",
    /// leaving planted filter drivers and `gpg.program` executable).
    ///
    /// The gated merge path uses this: its scratch worktree's gitdir points
    /// into the primary `.git`, so mission-authored gate/test code can plant
    /// executable config — which the merge's own checkout / merge / worktree
    /// commands would then execute with the server's full inherited
    /// environment, exactly the tokens the sanitized gate executor withholds.
    /// The validator-integrity fingerprint runs on a verification handle for
    /// the same reason: a validator that poisons `core.fsmonitor` must not
    /// get its payload executed by the detection itself (4th-pass review —
    /// detection previously ran `git status` BEFORE comparing config, so the
    /// payload ran first). Opt-in per handle: worker-side git behavior is
    /// deliberately unchanged.
    ///
    /// Building the handle enumerates the repo's configured filter drivers;
    /// an enumeration failure fails CLOSED (no handle) — a verification
    /// handle that cannot name its armed drivers cannot promise the surface
    /// is disabled.
    pub fn with_hooks_disabled(&self) -> Result<GitRepo> {
        if let Some(flags) = &self.exec_disable_flags {
            // Already a verification handle: re-wrapping is a clone, never a
            // second enumeration (idempotent).
            return Ok(GitRepo {
                root: self.root.clone(),
                exec_disable_flags: Some(flags.clone()),
            });
        }
        Ok(GitRepo {
            root: self.root.clone(),
            exec_disable_flags: Some(self.build_exec_disable_flags()?),
        })
    }

    /// The complete `-c key=value` argv segment [`Self::probe_os`] prepends to
    /// every git invocation of a verification handle, and WHY each entry
    /// exists (13th-pass review, P1):
    ///
    /// - `core.hooksPath=` / `core.fsmonitor=` — the original pair: hook
    ///   lookup resolves to nothing and the fsmonitor hook `git status`
    ///   would otherwise run is off.
    /// - `core.attributesFile=/dev/null` — the per-user attributes file is
    ///   replaced with the null device. HONEST SCOPE: this does NOT touch
    ///   the repo's own attribute sources — a checkout's `.gitattributes`
    ///   and `$GIT_DIR/info/attributes` are consulted regardless (probed
    ///   2026-08-04: an armed `*.txt filter=evil` in a worktree
    ///   `.gitattributes` still fired its driver under this flag alone).
    ///   Those files are deliverable content that must keep staging
    ///   verbatim, so the armed-driver attack is closed config-side — see
    ///   the filter enumeration below.
    /// - `filter.<name>.clean=` / `.smudge=` / `.process=` plus
    ///   `filter.<name>.required=false` for EVERY filter driver named in
    ///   the repo's config (any scope): `git add` runs an armed driver's
    ///   clean/process command with the engine's privileges. The names are
    ///   enumerated with `git config --get-regexp -z '^filter\.'` (a pure
    ///   config read — include.path expansion reads files, it never
    ///   executes), then each is overridden EMPTY on the command line,
    ///   which git honors as "no driver": the add stages the raw bytes
    ///   verbatim (probed 2026-08-04, dotted subsection names included).
    /// - `commit.gpgSign=false` + `gpg.program=/bin/false` — belt and
    ///   braces: repo config can force signing on (`commit.gpgSign=true`)
    ///   and name a payload as the signer. The first flag turns signing
    ///   off; the second makes the payload inert even if a future caller
    ///   forces signing back on (`-S`). `/bin/false` is never resolved
    ///   unless signing actually runs.
    ///
    /// Documented residual (no overclaim this time): `merge.<name>.driver`
    /// and `diff.<name>.command`/`.textconv` also execute repo-configured
    /// commands when an attribute arms them — but only on merge/diff
    /// porcelain, and an EMPTY override makes those git commands fail
    /// loudly rather than fall back to the builtin behavior (probed
    /// 2026-08-04), so neutralizing them is a behavior change of its own,
    /// not a silent rider on this countermeasure.
    fn build_exec_disable_flags(&self) -> Result<Vec<String>> {
        const BASE: &[&str] = &[
            "core.hooksPath=",
            "core.fsmonitor=",
            "core.attributesFile=/dev/null",
            "commit.gpgSign=false",
            "gpg.program=/bin/false",
        ];
        let mut flags = Vec::with_capacity(BASE.len() * 2 + 8);
        for kv in BASE {
            flags.push("-c".to_string());
            flags.push((*kv).to_string());
        }
        for name in self.configured_filter_drivers()? {
            for sub in ["clean", "smudge", "process"] {
                flags.push("-c".to_string());
                flags.push(format!("filter.{name}.{sub}="));
            }
            flags.push("-c".to_string());
            flags.push(format!("filter.{name}.required=false"));
        }
        Ok(flags)
    }

    /// The filter-driver names configured for this repository (any config
    /// scope), for [`Self::build_exec_disable_flags`]' neutralizing
    /// overrides. `git config --get-regexp` exits 1 when nothing matches
    /// (the common case — no drivers configured); any other failure is an
    /// error.
    ///
    /// `-z` output is `key\nvalue\0` per entry, so the key runs to the
    /// first newline. A subsection name can legally contain spaces; such a
    /// name is SKIPPED here, which is safe rather than a hole:
    /// `.gitattributes` attribute settings are whitespace-delimited, so a
    /// whitespace-carrying driver name can never be armed. Dotted names
    /// (`filter.weird.name.clean`) re-split at the LAST dot, so they
    /// round-trip into `-c` keys exactly as git prints them.
    fn configured_filter_drivers(&self) -> Result<Vec<String>> {
        let out = self.probe(&["config", "--get-regexp", "-z", "^filter\\."])?;
        if !out.status.success() {
            // Exit 1 is "no matches" (the common case); anything else is a
            // real failure and must not silently yield an unneutralized set.
            if out.status.code() == Some(1) {
                return Ok(Vec::new());
            }
            return Err(EngineError::Git(format!(
                "git config --get-regexp filter.* failed ({}): {}",
                out.status,
                failure_detail(&out)
            )));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut names = std::collections::BTreeSet::new();
        for entry in stdout.split('\0') {
            if entry.is_empty() {
                continue;
            }
            let key = entry.split('\n').next().unwrap_or("");
            let Some(rest) = key.strip_prefix("filter.") else {
                continue;
            };
            let Some((name, _subkey)) = rest.rsplit_once('.') else {
                continue;
            };
            if name.is_empty() || name.chars().any(char::is_whitespace) {
                continue;
            }
            names.insert(name.to_string());
        }
        Ok(names.into_iter().collect())
    }

    /// Sha of `HEAD` (`git rev-parse HEAD`).
    pub fn head_sha(&self) -> Result<String> {
        Ok(self.run(&["rev-parse", "HEAD"])?.trim().to_string())
    }

    /// The shared git directory (`.git` in a plain checkout, the MAIN repo's
    /// git dir for a linked worktree) — where config, hooks, and refs live.
    /// Relative `--git-common-dir` output resolves against the repo root.
    pub fn git_common_dir(&self) -> Result<std::path::PathBuf> {
        let out = self.run(&["rev-parse", "--git-common-dir"])?;
        let path = std::path::PathBuf::from(out.trim());
        Ok(if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        })
    }

    /// Mission-significant refs for the tamper fingerprint: the CONTENT of
    /// `refs/heads/kranz/*` (mission branches — a validator force-moving one
    /// retargets the deliverable), `refs/tags/*`, AND `refs/replace/*` (a
    /// replace ref changes how EVERY later git command resolves an object —
    /// `git show <base>` renders a fake without HEAD, status, heads, or tags
    /// moving), plus the COUNT of all `refs/heads/*` (a validator-created
    /// sneaky branch shows as count+1).
    ///
    /// `refs/remotes/*` is excluded (ambient mirror state: any operator/CI
    /// fetch), and other local heads' CONTENT is excluded too — the operator
    /// committing to `main` mid-round is ambient work, not tamper (mission
    /// m-83d1ed's second tripwire fire was exactly that: the instrumented
    /// `refs` field catching the operator's own push to main).
    pub fn for_each_ref(&self) -> Result<String> {
        let scoped = self.run(&[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads/kranz",
            "refs/tags",
            "refs/replace",
        ])?;
        let all_heads = self.run(&["for-each-ref", "--format=%(refname)", "refs/heads"])?;
        let count = all_heads.lines().filter(|l| !l.trim().is_empty()).count();
        Ok(format!("{scoped}heads-count: {count}\n"))
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

    /// Full `git status --porcelain` (v1) output: index + worktree status of
    /// tracked files plus untracked non-ignored paths, respecting .gitignore
    /// (so build-artifact churn like `target/` and the gitignored `.kranz`
    /// runtime never appears). The validator immutability fingerprint
    /// ([`crate::validator_integrity`]) compares this verbatim across a
    /// session; v1's C-quoting keeps even exotic paths to one line per entry.
    pub fn porcelain_status(&self) -> Result<String> {
        // --untracked-files=all: the default collapses untracked DIRECTORIES
        // (`?? dir/`), so files added inside an already-untracked dir would
        // be invisible to the validator-integrity fingerprint (review 2 pass).
        self.run(&["status", "--porcelain", "--untracked-files=all"])
    }

    /// `git ls-files -v`: every index entry with its flag column (`S` =
    /// skip-worktree, lowercase = assume-unchanged). A `skip-worktree` flag
    /// hides worktree modifications from `git status` entirely (4th-pass
    /// review: set the flag, overwrite the file, HEAD and porcelain both
    /// unchanged), so the immutability fingerprint covers the flags too.
    pub fn ls_files_v(&self) -> Result<String> {
        self.run(&["ls-files", "-v"])
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
    /// hide working-tree changes (`assume-unchanged`, `skip-worktree`, or
    /// fsmonitor-valid).
    ///
    /// Scratch merge worktrees are never sparse and never need either flag,
    /// so every tracked entry must have git's normal `H` tag.
    pub fn is_clean_tracked_strict(&self) -> Result<bool> {
        if !self.is_clean_tracked()? {
            return Ok(false);
        }
        Ok(self
            .run(&["ls-files", "-v", "-f"])?
            .lines()
            .all(|line| line.starts_with("H ")))
    }

    /// Whether one tracked path has Git's normal index tag. Lowercase tags
    /// (`assume-unchanged` or fsmonitor-valid) and `S` (`skip-worktree`) can
    /// hide worktree bytes from ordinary diff/status commands and must not
    /// guard a trust decision.
    pub fn has_normal_index_entry(&self, path: &str) -> Result<bool> {
        let output = self.run(&["ls-files", "-v", "-f", "--", path])?;
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
        // Split the dirty paths: TRACKED files scan only the mission's added
        // lines (git diff HEAD) — a mission must not be refused for
        // pre-existing base content in a file it merely touches (m-0f1abd,
        // checkpoint-refused twice by unchanged base code). NEW (untracked)
        // files still scan full-file — `git diff HEAD` never sees them and
        // their whole content is added lines anyway.
        let (mut tracked, mut new_files) = (Vec::new(), Vec::new());
        for path in paths {
            let in_index = self
                .run_os(&[
                    "ls-files".into(),
                    "--error-unmatch".into(),
                    "--".into(),
                    path.as_os_str().to_os_string(),
                ])
                .is_ok();
            if in_index {
                tracked.push(*path);
            } else {
                new_files.push(*path);
            }
        }

        let mut findings = Vec::new();
        if !tracked.is_empty() {
            let diff = self.diff_head_paths(&tracked).unwrap_or_default();
            findings.extend(scrub::scan_unified_diff(&diff));
        }
        if !new_files.is_empty() {
            findings.extend(scrub::scan_paths(&self.root, &new_files));
        }
        let findings = scrub::filter_allowed(findings, &allowed);
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

    /// Count first-parent commits on `branch` whose committer date falls in
    /// `(since, until]` (`git rev-list --first-parent --count --since
    /// --until`) — the landed-changes denominator of the industry-comparison
    /// fold (ticket `outcomes-comparison-metrics`, KRZ-333). First-parent
    /// counts one entry per change that landed on the branch's own line of
    /// history — a direct commit or a `--no-ff` merge — never the commits a
    /// merge brought with it, so a landed mission merge and a hand-written
    /// commit each count once. git's `--since` is exclusive and `--until`
    /// inclusive; the timestamps go to git verbatim as RFC 3339.
    pub fn count_first_parent_commits(
        &self,
        branch: &str,
        since: &chrono::DateTime<chrono::Utc>,
        until: &chrono::DateTime<chrono::Utc>,
    ) -> Result<u64> {
        if branch.starts_with('-') {
            return Err(EngineError::Git(format!(
                "refusing count_first_parent_commits with flag-shaped ref {branch:?}"
            )));
        }
        let out = self.run(&[
            "rev-list",
            "--first-parent",
            "--count",
            &format!("--since={}", since.to_rfc3339()),
            &format!("--until={}", until.to_rfc3339()),
            branch,
        ])?;
        out.trim().parse::<u64>().map_err(|e| {
            EngineError::Git(format!(
                "git rev-list --first-parent --count {branch} returned non-numeric output {out:?}: {e}"
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

    /// Full `git diff --binary HEAD` output (index + working tree vs HEAD),
    /// verbatim — everything a worker left uncommitted on TRACKED files,
    /// binary-safe so it replays byte-for-byte through `git apply`
    /// ([`GitRepo::apply_patch`]). The validator snapshot
    /// ([`crate::validator_snapshot`]) captures this in the real checkout and
    /// applies it in the throwaway copy so validators judge exactly the tree
    /// the worker left.
    pub fn diff_head(&self) -> Result<String> {
        self.run(&["diff", "--binary", "HEAD"])
    }

    /// `git apply <patch_file>` against the worktree (index untouched). The
    /// validator snapshot replays the real checkout's [`GitRepo::diff_head`]
    /// this way; the patch comes from a file path so no stdin plumbing is
    /// needed.
    pub fn apply_patch(&self, patch_file: &Path) -> Result<()> {
        let args: Vec<OsString> = vec!["apply".into(), patch_file.as_os_str().to_os_string()];
        self.run_os(&args)?;
        Ok(())
    }

    /// Untracked, non-ignored files (`git ls-files --others
    /// --exclude-standard -z`), repo-relative. `-z` gives unquoted raw paths
    /// (NUL is the only byte git never allows in one), so even
    /// newline-bearing names survive the split. Ignored paths (`target/`,
    /// the `.kranz` runtime) never appear — mirroring
    /// [`GitRepo::porcelain_status`].
    /// Untracked non-ignored files, NUL-separated raw bytes preserved:
    /// `ls-files -z` output is byte-oriented, and a name that is not valid
    /// UTF-8 must NOT be lossy-mangled — the replacement character turns
    /// into a path that then fails to copy and (pre-fix) was silently
    /// swallowed as NotFound (5th-pass review). On unix the raw bytes are
    /// used verbatim; on Windows (where git emits WTF-8) the lossy form is
    /// the pragmatic fallback, documented.
    pub fn untracked_files(&self) -> Result<Vec<std::ffi::OsString>> {
        let out = self.probe(&["ls-files", "--others", "--exclude-standard", "-z"])?;
        if !out.status.success() {
            return Err(EngineError::Git(format!(
                "git ls-files --others failed ({})",
                failure_detail(&out)
            )));
        }
        Ok(out
            .stdout
            .split(|b| *b == 0)
            .filter(|seg| !seg.is_empty())
            .map(|seg| {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt as _;
                    std::ffi::OsString::from(std::ffi::OsStr::from_bytes(seg))
                }
                #[cfg(not(unix))]
                {
                    std::ffi::OsString::from(String::from_utf8_lossy(seg).into_owned())
                }
            })
            .collect())
    }

    /// Full `git diff HEAD -- <paths>` output (index + working tree vs HEAD),
    /// verbatim — the checkpoint scan's "what this mission actually changed",
    /// never the pre-existing base content of files it merely touches.
    pub fn diff_head_paths(&self, paths: &[&Path]) -> Result<String> {
        let mut args: Vec<OsString> = vec!["diff".into(), "HEAD".into(), "--".into()];
        args.extend(paths.iter().map(|p| p.as_os_str().to_os_string()));
        self.run_os(&args)
    }

    /// Full `git diff <from>..<to> -- <paths>` output, verbatim — the
    /// affected-path diff a Flight Rules waiver's digest binds (KRZ-344
    /// D-I): only changes under the named paths alter the bytes, so an
    /// unrelated-path change can never invalidate (or be covered by) the
    /// waiver. Refuses flag-shaped refs (the [`GitRepo::changed_paths`]
    /// guard) and an EMPTY path set — `git diff <range> --` with no
    /// pathspec silently means the WHOLE diff, which would bind authority
    /// the caller never scoped.
    pub fn diff_range_paths(&self, from: &str, to: &str, paths: &[String]) -> Result<String> {
        for slot in [from, to] {
            if slot.starts_with('-') {
                return Err(EngineError::Git(format!(
                    "refusing diff_range_paths with flag-shaped ref {slot:?}"
                )));
            }
        }
        if paths.is_empty() {
            return Err(EngineError::Git(
                "refusing diff_range_paths with an empty path set — `--` alone means the \
                 whole diff, not an empty one"
                    .to_string(),
            ));
        }
        let range = format!("{from}..{to}");
        let mut args: Vec<OsString> = vec!["diff".into(), range.into(), "--".into()];
        args.extend(paths.iter().map(OsString::from));
        self.run_os(&args)
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

    /// The most recent commit that ADDED `rel_path` (repo-relative,
    /// forward-slash), with its subject and full message body — or `None` if
    /// the path is untracked / was never added under version control.
    ///
    /// Used to check lesson-file provenance: a lesson only reaches a planning
    /// prompt if a `[kranz] mission report` commit carrying a matching
    /// `Kranz-Mission` trailer introduced it, so an untracked drop or a
    /// worker feature-commit fails the check (see the lesson-manifest render).
    pub fn commit_that_added(&self, rel_path: &str) -> Result<Option<AddedCommit>> {
        if rel_path.starts_with('-') {
            return Err(EngineError::Git(format!(
                "refusing commit_that_added with flag-shaped path {rel_path:?}"
            )));
        }
        // Unit-separator (\x1f) between fields; -n 1 → the newest add commit
        // (lessons are append-only and never rewritten, so there is one).
        let out = self.run(&[
            "log",
            "--diff-filter=A",
            "-n",
            "1",
            "--format=%H%x1f%s%x1f%b",
            "--",
            rel_path,
        ])?;
        let out = out.trim_end_matches('\n');
        if out.is_empty() {
            return Ok(None);
        }
        let mut parts = out.splitn(3, '\u{1f}');
        let sha = parts.next().unwrap_or_default().trim().to_string();
        if sha.is_empty() {
            return Ok(None);
        }
        let subject = parts.next().unwrap_or_default().to_string();
        let body = parts.next().unwrap_or_default().to_string();
        Ok(Some(AddedCommit { sha, subject, body }))
    }

    /// Whether `path` has a commit at or after `since_ymd` (`YYYY-MM-DD`).
    ///
    /// Used by knowledge-refresh drift checks: a note whose `verified_against`
    /// path has history after `last_verified` is check-needed. Empty history
    /// (unknown path, or no commits in the window) is `false`, not an error.
    /// Flag-shaped/non-repository paths and invalid dates are refused before
    /// git runs. A non-zero `git log` is an error, never "unchanged".
    pub fn path_changed_since(&self, path: &str, since_ymd: &str) -> Result<bool> {
        let candidate = Path::new(path);
        if path.starts_with('-')
            || path.contains('\0')
            || path.is_empty()
            || candidate.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(EngineError::Git(format!(
                "refusing path_changed_since with non-repository path {path:?}"
            )));
        }
        let since_date =
            chrono::NaiveDate::parse_from_str(since_ymd, "%Y-%m-%d").map_err(|_| {
                EngineError::Git(format!(
                    "refusing path_changed_since with non YYYY-MM-DD date {since_ymd:?}"
                ))
            })?;
        let normalized_since = since_date.format("%Y-%m-%d");
        if normalized_since.to_string() != since_ymd {
            return Err(EngineError::Git(format!(
                "refusing path_changed_since with non YYYY-MM-DD date {since_ymd:?}"
            )));
        }
        // Exclusive of the verification calendar day: `--since=YYYY-MM-DD`
        // includes that midnight, so a note verified the same day it was
        // committed would false-drift. End-of-day keeps date granularity.
        let since = format!("--since={normalized_since}T23:59:59");
        let out = self.probe(&["log", "-1", &since, "--format=%H", "--", path])?;
        if !out.status.success() {
            return Err(EngineError::Git(format!(
                "path_changed_since probe failed for {path:?}: {}",
                failure_detail(&out)
            )));
        }
        Ok(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
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
            git_path_arg(path).into_os_string(),
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
            git_path_arg(path).into_os_string(),
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
            git_path_arg(path).into_os_string(),
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
            git_path_arg(path).into_os_string(),
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

    /// Whether `path` is tracked in the index (`git ls-files --error-unmatch
    /// -- <path>`): exit 0 ⇒ tracked; exit 1 ⇒ untracked/absent (NOT an
    /// error); any other status is a real git failure. The Flight Rules
    /// trust boundary (KRZ-341, D-A/D-J) uses this to decide whether a pack
    /// may activate ENFORCED rules: only tracked, repo-relative pack bytes
    /// have provable base history.
    pub fn is_tracked(&self, path: &str) -> Result<bool> {
        if path.starts_with('-') {
            return Err(EngineError::Git(format!(
                "refusing is_tracked with flag-shaped path {path:?}"
            )));
        }
        let out = self.probe(&["ls-files", "--error-unmatch", "--", path])?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(EngineError::Git(format!(
                "git ls-files --error-unmatch -- {path} failed ({}): {}",
                out.status,
                failure_detail(&out)
            ))),
        }
    }

    /// Recursive `git ls-tree -r -l <refname> -- <prefix>`: every entry under
    /// `prefix` at `refname` with its git mode, object kind, and blob size.
    /// The Flight Rules loader (KRZ-341) reads a standards corpus from a
    /// PINNED base tree through this — never from the worktree — so a mission
    /// branch edit cannot reshape the policy judging it. A flag-shaped ref
    /// or prefix is refused before invoking git (mirroring [`Self::show_file`]).
    pub fn ls_tree_recursive(&self, refname: &str, prefix: &str) -> Result<Vec<TreeEntry>> {
        for slot in [refname, prefix] {
            if slot.starts_with('-') {
                return Err(EngineError::Git(format!(
                    "refusing ls-tree with flag-shaped argument {slot:?}"
                )));
            }
        }
        let out = self.probe(&["ls-tree", "-r", "-l", refname, "--", prefix])?;
        if !out.status.success() {
            return Err(EngineError::Git(format!(
                "git ls-tree -r -l {refname} -- {prefix} failed ({}): {}",
                out.status,
                failure_detail(&out)
            )));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut entries = Vec::new();
        for line in stdout.lines() {
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            // `<mode> SP <type> SP <oid> SP <size> TAB <path>`; size is `-`
            // for non-blobs. A path git had to C-quote (control/non-ASCII
            // bytes) keeps its leading `"` here so the consumer fails closed
            // instead of misreading an unquoted rendering.
            let Some((meta, path)) = line.split_once('\t') else {
                return Err(EngineError::Git(format!(
                    "git ls-tree emitted an unparseable line: {line:?}"
                )));
            };
            let fields: Vec<&str> = meta.split_whitespace().collect();
            let [mode, kind, _oid, size] = fields.as_slice() else {
                return Err(EngineError::Git(format!(
                    "git ls-tree emitted an unparseable line: {line:?}"
                )));
            };
            let size = match *size {
                "-" => None,
                digits => Some(digits.parse::<u64>().map_err(|_| {
                    EngineError::Git(format!("git ls-tree emitted a bad size in line: {line:?}"))
                })?),
            };
            entries.push(TreeEntry {
                mode: (*mode).to_string(),
                kind: (*kind).to_string(),
                size,
                path: path.to_string(),
            });
        }
        Ok(entries)
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

    /// URL of remote `name` (`git remote get-url`), or `Ok(None)` when absent.
    pub fn remote_url(&self, name: &str) -> Result<Option<String>> {
        if name.starts_with('-') || name.chars().any(char::is_whitespace) {
            return Err(EngineError::Git(format!(
                "refusing remote_url of malformed remote {name:?}"
            )));
        }
        let out = self.probe(&["remote", "get-url", name])?;
        if !out.status.success() {
            return Ok(None);
        }
        let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if url.is_empty() {
            Ok(None)
        } else {
            Ok(Some(url))
        }
    }

    /// Whether `remote` advertises branch `branch` (`git ls-remote --heads`).
    /// Read-only network probe — never updates local refs.
    pub fn remote_has_branch(&self, remote: &str, branch: &str) -> Result<bool> {
        for slot in [remote, branch] {
            if slot.starts_with('-') || slot.contains(':') || slot.chars().any(char::is_whitespace)
            {
                return Err(EngineError::Git(format!(
                    "refusing remote_has_branch with malformed ref {slot:?}"
                )));
            }
        }
        let out = self.probe(&["ls-remote", "--heads", remote, branch])?;
        if !out.status.success() {
            return Err(EngineError::Git(format!(
                "git ls-remote --heads {remote} {branch} failed ({}): {}",
                out.status,
                failure_detail(&out)
            )));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let needle = format!("refs/heads/{branch}");
        Ok(stdout.lines().any(|line| line.contains(&needle)))
    }

    /// Whether a remote named `name` is configured (`git remote get-url`).
    ///
    /// A probe, not an assertion: returns `Ok(false)` when the remote is
    /// absent and only errors when git itself cannot be spawned. Callers use
    /// this to decide whether a cloud mission has anywhere to push to before
    /// calling [`GitRepo::push_mission_branch`].
    pub fn has_remote(&self, name: &str) -> Result<bool> {
        Ok(self.remote_url(name)?.is_some())
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
    /// `kranz`/`kranz@localhost` pair [`Self::ensure_identity`] would write when
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
        if let Some(flags) = &self.exec_disable_flags {
            // `-c` must precede the subcommand; the segment neutralizes every
            // executable config surface this handle promises to cover (see
            // with_hooks_disabled / build_exec_disable_flags).
            cmd.args(flags);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_git(root: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("spawn git")
    }

    fn init_test_repo(root: &Path) {
        if !test_git(root, &["init", "-b", "main"]).status.success() {
            assert!(test_git(root, &["init"]).status.success());
        }
        assert!(test_git(root, &["config", "user.name", "kranz-test"])
            .status
            .success());
        assert!(
            test_git(root, &["config", "user.email", "test@kranz.local"])
                .status
                .success()
        );
    }

    fn commit_test_repo_at(root: &Path, message: &str, timestamp: &str) {
        assert!(test_git(root, &["add", "-A"]).status.success());
        let output = Command::new("git")
            .args(["-c", "commit.gpgsign=false", "commit", "-m", message])
            .current_dir(root)
            .env("GIT_AUTHOR_DATE", timestamp)
            .env("GIT_COMMITTER_DATE", timestamp)
            .output()
            .expect("spawn git commit");
        assert!(output.status.success(), "git commit failed: {output:?}");
    }

    #[test]
    fn path_changed_since_excludes_verification_day_and_detects_later_commit() {
        let dir = tempfile::tempdir().unwrap();
        init_test_repo(dir.path());
        std::fs::write(dir.path().join("evidence.md"), "v1\n").unwrap();
        commit_test_repo_at(dir.path(), "seed", "2026-07-08T12:00:00Z");
        let repo = GitRepo::open(dir.path()).unwrap();

        assert!(!repo
            .path_changed_since("evidence.md", "2026-07-08")
            .unwrap());

        std::fs::write(dir.path().join("evidence.md"), "v2\n").unwrap();
        commit_test_repo_at(dir.path(), "later", "2026-07-09T12:00:00Z");
        assert!(repo
            .path_changed_since("evidence.md", "2026-07-08")
            .unwrap());
        assert!(!repo
            .path_changed_since("evidence.md", "2026-07-09")
            .unwrap());
    }

    #[test]
    fn path_changed_since_refuses_invalid_inputs_and_propagates_git_failure() {
        let dir = tempfile::tempdir().unwrap();
        init_test_repo(dir.path());
        std::fs::write(dir.path().join("evidence.md"), "uncommitted\n").unwrap();
        let repo = GitRepo::open(dir.path()).unwrap();

        assert!(repo
            .path_changed_since("../outside.md", "2026-07-08")
            .is_err());
        assert!(repo
            .path_changed_since("evidence.md", "not-a-date")
            .is_err());
        assert!(repo
            .path_changed_since("evidence.md", "2026-07-08")
            .is_err());
    }

    /// git on Windows cannot parse verbatim (`\\?\C:\...`) paths — the
    /// prefix is stripped for git arguments (worktree add/remove). On all
    /// platforms a plain path passes through untouched; the verbatim strip
    /// itself is cfg(windows) and oracled by the windows-latest CI leg.
    #[test]
    fn git_path_arg_passes_plain_paths_through() {
        let plain = Path::new(if cfg!(windows) {
            r"C:\repo\wt"
        } else {
            "/repo/wt"
        });
        assert_eq!(git_path_arg(plain), plain);
    }

    #[cfg(windows)]
    #[test]
    fn git_path_arg_strips_the_verbatim_prefix() {
        let verbatim = Path::new(r"\\?\C:\repo\wt");
        assert_eq!(git_path_arg(verbatim), Path::new(r"C:\repo\wt"));
        // UNC shares are NOT collapsed.
        let unc = Path::new(r"\\?\UNC\share\repo");
        assert_eq!(git_path_arg(unc), unc);
    }

    // -----------------------------------------------------------------------
    // 13th-pass review (P1): the with_hooks_disabled countermeasure covers
    // the WHOLE executable git-config surface — planted filter drivers and
    // gpg.program, not just hooks/fsmonitor. Fixture idiom mirrors
    // validator_integrity's planted-hook test: prove the fixture is LIVE
    // with an ordinary handle, then prove the verification handle never
    // executes the payload. Unix-only: the payloads are /bin/sh scripts.
    // -----------------------------------------------------------------------

    /// A repo with an initial commit and a scripted payload on disk; returns
    /// the repo root (inside `dir`), the payload script path, and the
    /// invocation log path the payload appends to when it runs.
    #[cfg(unix)]
    fn git_exec_config_repo(
        dir: &tempfile::TempDir,
        payload_body: &str,
    ) -> (PathBuf, PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt as _;
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let log = dir.path().join("payload-invocations");
        let payload = dir.path().join("payload");
        std::fs::write(
            &payload,
            payload_body.replace("__LOG__", &log.display().to_string()),
        )
        .unwrap();
        std::fs::set_permissions(&payload, std::fs::Permissions::from_mode(0o755)).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(root.join("seed.txt"), "seed\n").unwrap();
        git(&["add", "seed.txt"]);
        git(&["commit", "-qm", "seed"]);
        (root, payload, log)
    }

    /// A planted `filter.<name>.clean` driver (repo config) armed by a
    /// worker-writable `.gitattributes` must never execute on the engine's
    /// checkpoint `git add`/`git commit` — and the add must still stage the
    /// bytes VERBATIM (the armed attribute is deliverable content, not
    /// something the countermeasure may strip). The driver name is DOTTED
    /// (`weird.name`) to cover the subsection round-trip in
    /// `configured_filter_drivers`.
    #[cfg(unix)]
    #[test]
    fn git_exec_config_planted_clean_filter_never_runs_on_checkpoint_add() {
        let dir = tempfile::tempdir().unwrap();
        let (root, payload, log) =
            git_exec_config_repo(&dir, "#!/bin/sh\necho clean-ran >> '__LOG__'\ncat\n");
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git")
        };
        // Plant: the driver in repo config, armed for *.txt by a
        // worker-writable attributes file.
        assert!(git(&[
            "config",
            "filter.weird.name.clean",
            payload.to_str().unwrap()
        ])
        .status
        .success());
        std::fs::write(root.join(".gitattributes"), "*.txt filter=weird.name\n").unwrap();

        // Fixture proof: an ORDINARY `git add` executes the planted driver —
        // then reset the log so any later invocation can only have come from
        // the engine's checkpoint.
        std::fs::write(root.join("probe.txt"), "probe\n").unwrap();
        assert!(git(&["add", "probe.txt"]).status.success());
        assert!(
            std::fs::read_to_string(&log)
                .map(|hits| !hits.is_empty())
                .unwrap_or(false),
            "fixture: ordinary git add runs the planted clean filter"
        );
        let _ = std::fs::remove_file(&log);

        // The engine's checkpoint path (commit_dirty_paths is what the pool
        // checkpoint and the sequential dirty-tree turn call): the driver
        // must NOT execute, and the staged bytes must be verbatim.
        let repo = GitRepo::open(&root).unwrap().with_hooks_disabled().unwrap();
        std::fs::write(root.join("deliverable.txt"), "exact bytes ✓\n").unwrap();
        match repo.commit_dirty_paths("checkpoint").unwrap() {
            CheckpointOutcome::Committed(_) => {}
            other => panic!("checkpoint must commit, got {other:?}"),
        }
        assert!(
            !log.exists(),
            "the checkpoint's git add must never execute the planted clean filter: {}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        let shown = repo.show_file("HEAD", "deliverable.txt").unwrap().unwrap();
        assert_eq!(
            shown,
            "exact bytes ✓\n".as_bytes(),
            "the add stages the raw bytes verbatim — the armed attribute is content, not a hook"
        );
        // Re-wrapping an already-verified handle is idempotent: the same
        // argv segment, never a duplicated or re-enumerated one.
        let rewrapped = repo.with_hooks_disabled().unwrap();
        assert_eq!(repo.exec_disable_flags, rewrapped.exec_disable_flags);
    }

    /// A planted `gpg.program` with signing forced on by repo config
    /// (`commit.gpgSign=true`) must never execute on the engine's commit:
    /// `commit.gpgSign=false` turns signing off and `gpg.program=/bin/false`
    /// makes the payload inert even if signing is forced back on.
    #[cfg(unix)]
    #[test]
    fn git_exec_config_planted_gpg_program_never_runs_when_signing_forced() {
        let dir = tempfile::tempdir().unwrap();
        let (root, payload, log) =
            git_exec_config_repo(&dir, "#!/bin/sh\necho gpg-ran >> '__LOG__'\nexit 1\n");
        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git")
        };
        assert!(git(&["config", "commit.gpgSign", "true"]).status.success());
        assert!(git(&["config", "gpg.program", payload.to_str().unwrap()])
            .status
            .success());

        // Fixture proof: an ORDINARY commit invokes the planted signer (and
        // fails because the payload exits 1) — the repo config really forces
        // signing. Then reset the log.
        std::fs::write(root.join("probe.txt"), "probe\n").unwrap();
        assert!(git(&["add", "probe.txt"]).status.success());
        assert!(
            !git(&["commit", "-qm", "probe"]).status.success(),
            "fixture: signing with the failing payload must fail the commit"
        );
        assert!(
            std::fs::read_to_string(&log)
                .map(|hits| !hits.is_empty())
                .unwrap_or(false),
            "fixture: ordinary git commit runs the planted gpg.program"
        );
        let _ = std::fs::remove_file(&log);

        // The engine's commit runs with the payload neutralized: it commits
        // unsigned and the signer never fires.
        let repo = GitRepo::open(&root).unwrap().with_hooks_disabled().unwrap();
        match repo.commit_dirty_paths("checkpoint").unwrap() {
            CheckpointOutcome::Committed(_) => {}
            other => panic!("checkpoint must commit, got {other:?}"),
        }
        assert!(
            !log.exists(),
            "the engine's commit must never execute the planted gpg.program: {}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        // The commit really landed (ordinary add/commit behavior unchanged).
        assert_eq!(repo.commits_between("HEAD~1", "HEAD").unwrap().len(), 1);
    }
}
