//! Copy-on-write immutable validator snapshot — the definitive fix for the
//! gap ticket `validator-immutability-proof` left open (and named in its
//! "Remaining gap" section): "read-only" validator sessions ran in the
//! mission's REAL session checkout, so the tamper fingerprint
//! ([`crate::validator_integrity`]) could only catch writes at session
//! boundaries and never saw write-then-revert inside the window.
//!
//! With this module the validator never sees the real checkout. Before each
//! validator session (the primary and the single retry — the same sites the
//! fingerprint wraps) the orchestrator builds a THROWAWAY snapshot of the
//! session checkout under the mission's gitignored `runs/` scratch:
//!
//! 1. `git worktree add --detach <path> <head>` — the tree at HEAD, sharing
//!    the object store (no clone cost).
//! 2. The worker's uncommitted state is replayed into it: `git diff
//!    --binary HEAD` captured in the real checkout and `git apply`ed in the
//!    snapshot (tracked edits, staged or not), plus a byte copy of every
//!    untracked non-ignored file. Validators judge exactly the tree the
//!    worker left.
//! 3. The snapshot's `target/` is a COPY of the real checkout's — the
//!    warm-target constraint. Contract commands (`cargo test …`) must not
//!    pay a cold full-workspace rebuild (~50 min in this repo): APFS
//!    clonefile (`cp -c`) when the filesystem supports it (instant,
//!    copy-on-write blocks), Linux reflink (`cp --reflink=always`)
//!    otherwise, a plain byte copy when the target is small enough, and —
//!    logged, naming the cost — a fresh empty target when no acceleration
//!    exists and the copy would be prohibitive. The real `target/` is NEVER
//!    shared or symlinked: a validator poisoning shared build artifacts
//!    would inject into the real deliverable build.
//!
//! The session's cwd, the validator's contract-command cwd, and the
//! sandbox profile's `session_cwd` all point at the snapshot, so where the
//! sandbox can express it the real checkout is not even in the writable
//! set. Only the verdict (PASS/FAIL/findings) crosses back; the snapshot is
//! discarded after the round regardless of outcome (RAII, mirroring
//! `preflight.rs`'s `DisposableWorktree` idiom). The deliverable gates and
//! the out-of-contract sweep keep running against the REAL checkout.
//!
//! The snapshot is physical separation, NOT containment (13th-pass review,
//! P1 — ticket `validator-mandatory-containment`): the worktree sits
//! underneath the real repository hierarchy and references the shared git
//! directory, so an UNWRAPPED validator (`enforce: off`, the old default)
//! could still walk to the real checkout, modify tests, run them, and
//! restore the bytes — modify → use → restore leaves no drift for the
//! tripwire to catch. The mandatory validator wrap
//! (`crate::sandbox::resolve_validator_containment`) closes that: every
//! validator session runs under a Seatbelt/bwrap profile regardless of
//! `sandbox.enforce`, with the snapshot as the sole writable root, the
//! real checkout's source tree read-denied, and the shared `.git` readable
//! but write-denied. Where the platform or backend cannot contain, the
//! resolution FAILS CLOSED by default (ticket
//! `validator-containment-degrade-fail-closed`); only the explicit
//! `validatorAllowUncontainedDegrade` opt-in runs the round anyway, with
//! the loud degradation decision recorded and the snapshot plus the
//! tripwire as the remaining layers.
//!
//! Two honest limits, both covered by the fingerprint-turned-tripwire on
//! the real checkout:
//!
//! - The file tree is isolated; git REFS are not. Worktrees share the
//!   common `.git`, so a validator `git branch -f` in the snapshot still
//!   moves shared refs where no containment applies — which the
//!   tripwire's `for-each-ref` half catches at session end. (Under the
//!   mandatory wrap the ref write is hard-denied by deny-default; the
//!   tripwire is the defense-in-depth for the degraded platforms.) A
//!   drift event now means the isolation itself failed.
//! - Under an enforced sandbox the snapshot's gitdir (`.git/worktrees/<n>`)
//!   lives outside the writable `session_cwd`, so validator git commands
//!   that try to refresh the index degrade (read-only git still works;
//!   commits fail — which validators should never need).

use crate::error::{EngineError, Result};
use crate::git_ops::GitRepo;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Above this size a PLAIN byte copy of `target/` costs more in wall clock
/// and doubled disk than the cold rebuild it exists to avoid — with no
/// clonefile/reflink acceleration the snapshot starts with an empty target
/// instead, and the cost is named in the `validation.snapshot` event.
const PLAIN_COPY_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Which tier warmed the snapshot's `target/` — recorded on the
/// `validation.snapshot` event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetCopyTier {
    /// APFS clonefile (`cp -c`): instant, copy-on-write blocks.
    Clonefile,
    /// Linux reflink (`cp --reflink=always` succeeded).
    Reflink,
    /// Plain byte copy — no acceleration, but the target was small enough.
    Copy,
    /// No acceleration and the target was too big to copy: the snapshot
    /// starts with an EMPTY target (cold rebuild cost, logged).
    Fresh,
    /// The session checkout has no `target/` dir at all.
    Absent,
}

impl TargetCopyTier {
    /// Wire form for the `validation.snapshot` event payload.
    pub fn as_str(&self) -> &'static str {
        match self {
            TargetCopyTier::Clonefile => "clonefile",
            TargetCopyTier::Reflink => "reflink",
            TargetCopyTier::Copy => "copy",
            TargetCopyTier::Fresh => "fresh",
            TargetCopyTier::Absent => "absent",
        }
    }
}

/// The plain-copy-or-fresh decision, split out so each tier's fallback is
/// testable without a >2 GiB fixture (and because on clone-capable hosts the
/// size check never runs — the fast tiers win first).
fn pick_plain_or_fresh(target_bytes: u64) -> TargetCopyTier {
    if target_bytes <= PLAIN_COPY_MAX_BYTES {
        TargetCopyTier::Copy
    } else {
        TargetCopyTier::Fresh
    }
}

/// RAII guard for one validator session's throwaway checkout. Drop removes
/// it best-effort — `git worktree remove --force` (which also deletes the
/// directory), a dir sweep for anything git declined, and a prune of stale
/// administrative entries — so pass, fail, tamper-block, or error return
/// can never leak it.
pub struct ValidatorSnapshot {
    /// Handle the worktree was added from (the real session checkout's
    /// repo) — removal/prune run through it.
    repo: GitRepo,
    path: PathBuf,
    target_tier: TargetCopyTier,
    detail: Option<String>,
    creation: Duration,
}

impl ValidatorSnapshot {
    /// `git ls-files -v` inside the snapshot, filtered to the flag tags
    /// (`S` = skip-worktree, lowercase = assume-unchanged). The snapshot
    /// starts with a fresh, flag-free index, so any flag present after the
    /// session was set BY THE VALIDATOR — hidden modifications that would
    /// corrupt the verdict while leaving no trace once the snapshot is
    /// discarded (4th-pass review). Returns the offending lines (capped)
    /// when any flag is set.
    pub fn validator_set_index_flags(&self) -> Result<Vec<String>> {
        let repo = GitRepo::open(&self.path)?;
        let flags = repo.ls_files_v()?;
        Ok(flags
            .lines()
            .filter(|line| {
                let Some(tag) = line.chars().next() else {
                    return false;
                };
                tag == 'S' || tag.is_ascii_lowercase()
            })
            .take(5)
            .map(str::to_string)
            .collect())
    }

    /// Snapshot `repo`'s checkout (HEAD + uncommitted diff + untracked
    /// files + warmed `target/`) into a detached worktree at `path`.
    /// Idempotent against a stale leftover from a crashed round: any prior
    /// worktree/dir at `path` is cleared first (mirroring
    /// `DisposableWorktree::create`'s crash sweep).
    pub fn create(repo: &GitRepo, path: &Path) -> Result<Self> {
        let started = Instant::now();
        let _ = repo.remove_worktree(path);
        let _ = std::fs::remove_dir_all(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| EngineError::Git(format!("create {}: {e}", parent.display())))?;
        }

        // Capture the real checkout's state BEFORE the add, so the snapshot
        // replays exactly what the worker left.
        let repo = &repo.with_hooks_disabled()?;
        let head = repo.head_sha()?;
        let diff = repo.diff_head()?;
        let untracked = repo.untracked_files()?;
        // Fail closed on skip-worktree/assume-unchanged index flags
        // (4th-pass review): `git diff HEAD` and `git status` are BLIND to
        // flagged files, so a flagged modification can neither be replayed
        // here nor seen by the validator — a worker could hide source or
        // test edits from validation entirely. The snapshot cannot
        // faithfully represent a flagged checkout, so refuse to build one.
        let flags = repo.ls_files_v()?;
        let flagged: Vec<&str> = flags
            .lines()
            .filter(|line| {
                let Some(tag) = line.chars().next() else {
                    return false;
                };
                // 'S' = skip-worktree; any lowercase tag = assume-unchanged
                // (ls-files -v lowercases the tag for flagged entries).
                tag == 'S' || tag.is_ascii_lowercase()
            })
            .collect();
        if !flagged.is_empty() {
            return Err(EngineError::InvalidState(format!(
                "refusing validator snapshot over skip-worktree/assume-unchanged \
                 index flags (modifications hidden from git): {}",
                flagged
                    .iter()
                    .take(5)
                    .copied()
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        repo.add_detached_worktree(path, &head)?;

        // From here a failure must not leak the worktree: build the guard
        // first and let an `?` drop it.
        let mut snapshot = ValidatorSnapshot {
            repo: repo.clone(),
            path: path.to_path_buf(),
            target_tier: TargetCopyTier::Absent,
            detail: None,
            creation: Duration::default(),
        };
        let (tier, detail) = snapshot.populate(repo.root(), &diff, &untracked)?;
        snapshot.target_tier = tier;
        snapshot.detail = detail;
        snapshot.creation = started.elapsed();
        Ok(snapshot)
    }

    /// Replay the worker's uncommitted state into the fresh worktree and
    /// warm its `target/`. Separated from [`Self::create`] so the guard
    /// exists (and cleans up) across every fallible step.
    fn populate(
        &self,
        session_root: &Path,
        diff: &str,
        untracked: &[std::ffi::OsString],
    ) -> Result<(TargetCopyTier, Option<String>)> {
        let snap_repo = GitRepo::open(&self.path)?;
        if !diff.trim().is_empty() {
            // The patch is runtime scratch: a sibling of the snapshot under
            // the same gitignored runs/ dir, never inside either checkout
            // (it would show up as an untracked file in both).
            let patch = self.path.with_extension("patch");
            std::fs::write(&patch, diff).map_err(|e| {
                EngineError::Git(format!("write snapshot patch {}: {e}", patch.display()))
            })?;
            let applied = snap_repo.apply_patch(&patch);
            let _ = std::fs::remove_file(&patch);
            applied?;
        }
        let skip_note = copy_untracked(session_root, &self.path, untracked)?;
        let (tier, mut detail) = warm_target(session_root, &self.path);
        if let (Some(note), Some(existing)) = (&skip_note, &mut detail) {
            existing.push_str("; ");
            existing.push_str(note);
        } else if let Some(note) = skip_note {
            detail = Some(note);
        }
        Ok((tier, detail))
    }

    /// Root of the throwaway checkout — the validator session's cwd.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Which tier warmed the snapshot's `target/`.
    pub fn target_tier(&self) -> TargetCopyTier {
        self.target_tier
    }

    /// Extra context for the `validation.snapshot` event (notably the named
    /// cost of a `fresh` tier).
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// Wall-clock cost of building the snapshot.
    pub fn creation(&self) -> Duration {
        self.creation
    }
}

impl Drop for ValidatorSnapshot {
    fn drop(&mut self) {
        let _ = self.repo.remove_worktree(&self.path);
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = self.repo.prune_worktrees();
    }
}

/// Byte-copy the worker's untracked non-ignored files into the snapshot,
/// preserving repo-relative paths. A file that vanishes between the
/// `ls-files` listing and the copy is skipped (a racing external process
/// must not fail the round); real I/O errors propagate.
/// Copy the untracked files into the snapshot WITHOUT following any
/// link: only regular files cross (5th-pass review — an untracked
/// symlink could point a denied authority file, e.g. `.kranz/serve.token`,
/// into the snapshot where the validator reads it freely, and a FIFO or
/// device link would block the build forever). Non-regular entries are
/// skipped and recorded in the returned note (never opened), so the build
/// neither follows nor hangs; the CoW design means the real checkout is
/// untouched regardless.
fn copy_untracked(
    session_root: &Path,
    snapshot_root: &Path,
    untracked: &[std::ffi::OsString],
) -> Result<Option<String>> {
    let mut skipped: Vec<String> = Vec::new();
    for rel in untracked {
        let src = session_root.join(rel);
        let dst = snapshot_root.join(rel);
        let metadata = match std::fs::symlink_metadata(&src) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(EngineError::Git(format!(
                    "snapshot untracked stat {}: {e}",
                    src.display()
                )))
            }
        };
        if !metadata.file_type().is_file() {
            let kind = if metadata.file_type().is_symlink() {
                "symlink"
            } else if metadata.file_type().is_dir() {
                "dir"
            } else {
                "special"
            };
            skipped.push(format!("{} ({kind})", rel.to_string_lossy()));
            continue;
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                EngineError::Git(format!("snapshot untracked {}: {e}", parent.display()))
            })?;
        }
        match std::fs::copy(&src, &dst) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(EngineError::Git(format!(
                    "snapshot untracked copy {} -> {}: {e}",
                    src.display(),
                    dst.display()
                )))
            }
        }
    }
    Ok((!skipped.is_empty()).then(|| {
        format!(
            "skipped {} non-regular untracked entr{}: {}",
            skipped.len(),
            if skipped.len() == 1 { "y" } else { "ies" },
            skipped
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        )
    }))
}

/// Warm the snapshot's `target/` from the session checkout's, trying each
/// tier in cost order and falling through on failure: clonefile → reflink →
/// plain copy (size-capped) → fresh empty target. Returns the tier and, for
/// `fresh`, the detail naming the cost.
fn warm_target(session_root: &Path, snapshot_root: &Path) -> (TargetCopyTier, Option<String>) {
    let src = session_root.join("target");
    if !src.is_dir() {
        return (TargetCopyTier::Absent, None);
    }
    let dst = snapshot_root.join("target");
    if copy_dir_clonefile(&src, &dst) {
        return (TargetCopyTier::Clonefile, None);
    }
    if copy_dir_reflink(&src, &dst) {
        return (TargetCopyTier::Reflink, None);
    }
    let bytes = dir_size_bytes(&src);
    match pick_plain_or_fresh(bytes) {
        TargetCopyTier::Copy => match copy_dir_plain(&src, &dst) {
            Ok(()) => (TargetCopyTier::Copy, None),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dst);
                let detail = format!(
                    "target/ plain copy failed ({e}); snapshot starts with an empty target \
                     and pays a cold rebuild"
                );
                tracing::warn!("{detail}");
                (TargetCopyTier::Fresh, Some(detail))
            }
        },
        _ => {
            let detail = format!(
                "no clonefile/reflink acceleration and target/ is {} GiB (plain-copy cap {} GiB); \
                 snapshot starts with an empty target and pays a cold rebuild rather than copy",
                bytes / (1024 * 1024 * 1024),
                PLAIN_COPY_MAX_BYTES / (1024 * 1024 * 1024),
            );
            tracing::warn!("{detail}");
            (TargetCopyTier::Fresh, Some(detail))
        }
    }
}

/// `cp -c -R src dst` (APFS clonefile). `false` on any failure — non-macOS
/// `cp` has no `-c`, and a non-APFS volume makes clonefile itself fail — so
/// the caller falls through to the next tier. A partial copy is swept
/// before returning `false`. Never shares or links the source: the clone is
/// copy-on-write, owned by the destination. Shared with the contract Cargo
/// cache seeding ([`crate::agent_env`]), which seeds per-env copies of the
/// operator's registry/git caches through the same tier order.
pub(crate) fn copy_dir_clonefile(src: &Path, dst: &Path) -> bool {
    run_cp(&["-c", "-R"], src, dst)
}

/// `cp --reflink=always -R src dst` (GNU coreutils). `always` (not `auto`)
/// so a non-reflink filesystem FAILS LOUDLY here and the caller falls
/// through to the size-capped plain copy instead of silently paying one.
pub(crate) fn copy_dir_reflink(src: &Path, dst: &Path) -> bool {
    run_cp(&["--reflink=always", "-R"], src, dst)
}

fn run_cp(extra_flags: &[&str], src: &Path, dst: &Path) -> bool {
    let status = std::process::Command::new("cp")
        .args(extra_flags)
        .arg(src)
        .arg(dst)
        .status();
    match status {
        Ok(s) if s.success() => true,
        _ => {
            let _ = std::fs::remove_dir_all(dst);
            false
        }
    }
}

/// Total byte size of `dir` (metadata walk, best-effort: unreadable entries
/// count as zero). Cheap even on a large `target/` or Cargo cache — it reads
/// no file contents.
pub(crate) fn dir_size_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if meta.is_dir() {
                        stack.push(entry.path());
                    } else {
                        total += meta.len();
                    }
                }
            }
        }
    }
    total
}

/// Whether `dir`'s total byte size exceeds `limit`, stopping the metadata
/// walk the moment the answer is known. The cheap pre-check the contract
/// Cargo cache seeding ([`crate::agent_env`]) runs on EVERY generated child
/// env, where a full walk of a multi-GiB cache would itself be the cost the
/// copy ceiling exists to avoid.
pub(crate) fn dir_size_exceeds(dir: &Path, limit: u64) -> bool {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Ok(meta) = entry.metadata() {
                    if meta.is_dir() {
                        stack.push(entry.path());
                    } else {
                        total += meta.len();
                        if total > limit {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Recursive plain byte copy of regular files and directories only. Pin
/// each directory and open files no-follow: worker-controlled cache links
/// must never make the engine copy denied authority into a readable snapshot.
pub(crate) fn copy_dir_plain(src: &Path, dst: &Path) -> std::io::Result<()> {
    use cap_fs_ext::DirExt as _;
    let (src_parent, src_name) =
        crate::paths::open_parent_nofollow(src).map_err(std::io::Error::other)?;
    let source = src_parent.open_dir_nofollow(src_name)?;
    let (dst_parent, dst_name) =
        crate::paths::open_parent_nofollow(dst).map_err(std::io::Error::other)?;
    match dst_parent.create_dir(&dst_name) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let destination = dst_parent.open_dir_nofollow(dst_name)?;
    copy_dir_contents(&source, &destination)
}

fn copy_dir_contents(src: &cap_std::fs::Dir, dst: &cap_std::fs::Dir) -> std::io::Result<()> {
    use cap_fs_ext::{DirExt as _, OpenOptionsFollowExt as _};
    use cap_primitives::fs::FollowSymlinks;
    for entry in src.entries()? {
        let entry = entry?;
        let name = entry.file_name();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let source = src.open_dir_nofollow(&name)?;
            dst.create_dir(&name)?;
            let destination = dst.open_dir_nofollow(&name)?;
            copy_dir_contents(&source, &destination)?;
        } else if kind.is_file() {
            let mut options = cap_std::fs::OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No);
            #[cfg(unix)]
            {
                use cap_std::fs::OpenOptionsExt as _;
                // A racing replacement with a FIFO must not block the engine.
                options.custom_flags(libc::O_NONBLOCK);
            }
            let mut source = src.open_with(&name, &options)?.into_std();
            let metadata = source.metadata()?;
            if !metadata.is_file() {
                return Err(std::io::Error::other("cache entry is not a regular file"));
            }
            let mut options = cap_std::fs::OpenOptions::new();
            options
                .write(true)
                .create_new(true)
                .follow(FollowSymlinks::No);
            let mut destination = dst.open_with(&name, &options)?.into_std();
            std::io::copy(&mut source, &mut destination)?;
            destination.set_permissions(metadata.permissions())?;
        } else {
            return Err(std::io::Error::other(
                "refusing a symlink or special cache entry",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn snapshot_plain_copy_refuses_authority_links_and_preserves_executables() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("target");
        std::fs::create_dir(&source).unwrap();
        let authority = dir.path().join("serve.token");
        std::fs::write(&authority, "fake-authority").unwrap();
        symlink(&authority, source.join("leak")).unwrap();
        let destination = dir.path().join("copy");
        assert!(copy_dir_plain(&source, &destination).is_err());
        assert!(!destination.join("leak").exists());
        assert_eq!(
            std::fs::read_to_string(&authority).unwrap(),
            "fake-authority"
        );

        std::fs::remove_file(source.join("leak")).unwrap();
        std::fs::write(source.join("test-bin"), "executable").unwrap();
        std::fs::set_permissions(
            source.join("test-bin"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        copy_dir_plain(&source, &destination).unwrap();
        assert_eq!(
            std::fs::read(destination.join("test-bin")).unwrap(),
            b"executable"
        );
        assert_ne!(
            std::fs::metadata(destination.join("test-bin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );

        let linked_destination = dir.path().join("linked-copy");
        symlink(&source, &linked_destination).unwrap();
        assert!(copy_dir_plain(&source, &linked_destination).is_err());
    }

    /// A temp git repo with one committed file, or None when git is not on
    /// PATH (mirrors the orchestrator tests' `lessons_test_repo` skip).
    fn test_repo() -> Option<(tempfile::TempDir, PathBuf)> {
        let git_ok = std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !git_ok {
            crate::test_capability::skip(
                crate::test_capability::capability::GIT,
                "git is not on PATH",
            );
            return None;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        if !std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            run(&["init"]);
            run(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        }
        run(&["config", "user.name", "test"]);
        run(&["config", "user.email", "test@example.com"]);
        std::fs::write(dir.path().join("README.md"), "hello\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-m", "init"]);
        let root = dir.path().to_path_buf();
        Some((dir, root))
    }

    /// The tier-selection fallback: plain copy up to the cap, fresh (with
    /// the cost named) beyond it — no clone-capable host reaches this.
    #[test]
    fn pick_plain_or_fresh_caps_plain_copy() {
        assert_eq!(pick_plain_or_fresh(0), TargetCopyTier::Copy);
        assert_eq!(
            pick_plain_or_fresh(PLAIN_COPY_MAX_BYTES),
            TargetCopyTier::Copy
        );
        assert_eq!(
            pick_plain_or_fresh(PLAIN_COPY_MAX_BYTES + 1),
            TargetCopyTier::Fresh
        );
    }

    /// 4th-pass review: a checkout with skip-worktree/assume-unchanged
    /// flags cannot be faithfully snapshotted (`git diff` and `git status`
    /// are blind to flagged files), so creation refuses fail-closed.
    #[test]
    fn create_refuses_skip_worktree_flags() {
        let Some((dir, root)) = test_repo() else {
            return;
        };
        let repo = GitRepo::open(&root).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        // A flagged modification is invisible to git diff/status: the
        // snapshot would silently validate the WRONG content.
        run(&["update-index", "--skip-worktree", "README.md"]);
        std::fs::write(root.join("README.md"), "hidden modification\n").unwrap();

        let err = match ValidatorSnapshot::create(&repo, &root.join("snap")) {
            Ok(_) => panic!("a flagged checkout must not build a snapshot"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("skip-worktree"),
            "error must name the flag class: {err}"
        );
        drop(dir);
    }

    /// The snapshot starts flag-free, so any flag afterwards was set by the
    /// session: validator_set_index_flags detects exactly that.
    #[test]
    fn validator_set_index_flags_detects_flags_set_inside_the_snapshot() {
        let Some((_dir, root)) = test_repo() else {
            return;
        };
        let repo = GitRepo::open(&root).unwrap();
        let snapshot =
            ValidatorSnapshot::create(&repo, &root.join("snap")).expect("clean checkout builds");
        assert!(
            snapshot.validator_set_index_flags().unwrap().is_empty(),
            "a fresh snapshot has no flags"
        );

        // Simulate the validator's move INSIDE the snapshot.
        let out = std::process::Command::new("git")
            .args(["update-index", "--skip-worktree", "README.md"])
            .current_dir(snapshot.path())
            .output()
            .expect("spawn git");
        assert!(out.status.success(), "git update-index failed: {out:?}");

        let flags = snapshot.validator_set_index_flags().unwrap();
        assert_eq!(flags.len(), 1, "{flags:?}");
        assert!(flags[0].starts_with('S'), "{flags:?}");
        assert!(flags[0].contains("README.md"), "{flags:?}");
    }

    /// The snapshot sees exactly what the worker left: the committed tree,
    /// unstaged tracked edits, staged new files, and untracked non-ignored
    /// files — and drop removes the worktree.
    #[test]
    fn snapshot_replays_uncommitted_state_and_cleans_up() {
        let Some((_dir, root)) = test_repo() else {
            return;
        };
        let repo = GitRepo::open(&root).unwrap();
        let head = repo.head_sha().unwrap();

        // The worker's leavings: an unstaged edit, a staged new file, an
        // untracked file in a new dir, and an ignored artifact (must NOT
        // cross into the snapshot as an untracked copy).
        std::fs::write(root.join("README.md"), "hello\nworker edit\n").unwrap();
        std::fs::write(root.join("staged.rs"), "fn staged() {}\n").unwrap();
        let staged = std::process::Command::new("git")
            .args(["add", "staged.rs"])
            .current_dir(&root)
            .output()
            .expect("git add");
        assert!(staged.status.success(), "git add: {staged:?}");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(root.join("notes/todo.txt"), "uncommitted\n").unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join("target/debug/obj.o"), "obj").unwrap();

        let snap_path = root.parent().unwrap().join("snap-shot-under-test");
        let snapshot = ValidatorSnapshot::create(&repo, &snap_path).unwrap();

        // Content comparisons normalize EOL: windows-latest runs with
        // core.autocrlf=true, so the worktree checkout + git apply write
        // CRLF — the replay is about content identity, not EOL convention.
        let read_normalized =
            |path: &Path| std::fs::read_to_string(path).unwrap().replace("\r\n", "\n");
        assert_eq!(
            GitRepo::open(&snap_path).unwrap().head_sha().unwrap(),
            head,
            "snapshot is detached at the real checkout's HEAD"
        );
        assert_eq!(
            read_normalized(&snap_path.join("README.md")),
            "hello\nworker edit\n",
            "unstaged tracked edit must be visible in the snapshot"
        );
        assert_eq!(
            read_normalized(&snap_path.join("staged.rs")),
            "fn staged() {}\n",
            "staged new file must be visible in the snapshot"
        );
        assert_eq!(
            read_normalized(&snap_path.join("notes/todo.txt")),
            "uncommitted\n",
            "untracked files must be visible in the snapshot"
        );
        // The warmed target copy carries the content but shares nothing:
        // writing through the snapshot's copy must not touch the real one.
        assert_eq!(
            std::fs::read_to_string(snap_path.join("target/debug/obj.o")).unwrap(),
            "obj"
        );
        std::fs::write(snap_path.join("target/debug/obj.o"), "poisoned").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("target/debug/obj.o")).unwrap(),
            "obj",
            "the warmed target is a copy, never a share of the real target/"
        );

        // The real checkout is untouched by the snapshot build itself.
        assert_eq!(
            repo.head_sha().unwrap(),
            head,
            "snapshot creation must not move the real HEAD"
        );

        let snap_path_copy = snap_path.clone();
        drop(snapshot);
        assert!(
            !snap_path_copy.exists(),
            "drop discards the snapshot worktree"
        );
        // The administrative entry is pruned too.
        let listed = std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&root)
            .output()
            .expect("git worktree list");
        let listed = String::from_utf8_lossy(&listed.stdout);
        assert!(!listed.contains("snap-shot-under-test"), "{listed}");
    }

    /// A stale leftover at the snapshot path (crashed round) is cleared and
    /// replaced, mirroring `DisposableWorktree::create`'s crash sweep.
    #[test]
    fn create_clears_stale_leftover() {
        let Some((_dir, root)) = test_repo() else {
            return;
        };
        let repo = GitRepo::open(&root).unwrap();
        let snap_path = root.parent().unwrap().join("snap-stale");
        std::fs::create_dir_all(&snap_path).unwrap();
        std::fs::write(snap_path.join("leftover.txt"), "stale").unwrap();

        let snapshot = ValidatorSnapshot::create(&repo, &snap_path).unwrap();
        assert!(snap_path.join("README.md").exists());
        assert!(
            !snap_path.join("leftover.txt").exists(),
            "the stale dir is swept, not merged into"
        );
        drop(snapshot);
    }

    /// No `target/` in the session checkout: tier `absent`, nothing copied.
    #[test]
    fn warm_target_absent_without_target_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (tier, detail) = warm_target(dir.path(), &dir.path().join("snap"));
        assert_eq!(tier, TargetCopyTier::Absent);
        assert_eq!(detail, None);
    }

    /// Every host reaches SOME warm tier with the content intact; the fast
    /// tiers (clonefile/reflink) and the plain copy are indistinguishable by
    /// bytes, so the content is the assertion and the tier is host-dependent.
    #[test]
    fn warm_target_copies_content_on_any_tier() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session");
        let snap = dir.path().join("snap");
        std::fs::create_dir_all(session.join("target/debug/deps")).unwrap();
        std::fs::write(session.join("target/debug/deps/lib.rlib"), "rlib-bytes").unwrap();
        std::fs::create_dir_all(&snap).unwrap();

        let (tier, detail) = warm_target(&session, &snap);
        assert!(
            matches!(
                tier,
                TargetCopyTier::Clonefile | TargetCopyTier::Reflink | TargetCopyTier::Copy
            ),
            "a small target warms via some copy tier, got {tier:?} ({detail:?})"
        );
        assert_eq!(
            std::fs::read_to_string(snap.join("target/debug/deps/lib.rlib")).unwrap(),
            "rlib-bytes",
            "the warm copy carries the bytes regardless of tier"
        );
    }

    /// APFS (this host, every modern macOS CI runner): the clonefile tier
    /// wins and is effectively instant. Probes clonefile support first so a
    /// hypothetical non-APFS mac skips rather than fails.
    #[cfg(target_os = "macos")]
    #[test]
    fn warm_target_prefers_clonefile_on_apfs() {
        let dir = tempfile::tempdir().unwrap();
        let probe_src = dir.path().join("probe");
        std::fs::write(&probe_src, "probe").unwrap();
        if !copy_dir_clonefile(&probe_src, &dir.path().join("probe-clone")) {
            eprintln!("skipping test: clonefile unsupported on this volume");
            return;
        }
        let session = dir.path().join("session");
        let snap = dir.path().join("snap");
        std::fs::create_dir_all(session.join("target")).unwrap();
        std::fs::write(session.join("target/artifact.o"), "bytes").unwrap();
        std::fs::create_dir_all(&snap).unwrap();

        let (tier, _) = warm_target(&session, &snap);
        assert_eq!(tier, TargetCopyTier::Clonefile);
        assert_eq!(
            std::fs::read_to_string(snap.join("target/artifact.o")).unwrap(),
            "bytes"
        );
    }

    /// No clone acceleration + a target over the plain-copy cap: tier
    /// `fresh` with the cost named, and NO copy attempted (dst stays absent).
    /// The over-cap size comes from a sparse file — no real blocks.
    #[cfg(unix)]
    #[test]
    fn warm_target_fresh_when_copy_prohibitive() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session");
        let snap = dir.path().join("snap");
        std::fs::create_dir_all(session.join("target")).unwrap();
        std::fs::write(session.join("target/big.bin"), "").unwrap();
        std::fs::File::options()
            .write(true)
            .open(session.join("target/big.bin"))
            .unwrap()
            .set_len(PLAIN_COPY_MAX_BYTES + 1)
            .unwrap();
        std::fs::create_dir_all(&snap).unwrap();

        // The size gate must select fresh for the over-cap sparse target…
        assert_eq!(
            pick_plain_or_fresh(dir_size_bytes(&session.join("target"))),
            TargetCopyTier::Fresh,
            "over-cap sparse target must select fresh"
        );
        // …and the full cascade honours it wherever no fast tier exists. On
        // a clone-capable host (this one) a fast tier legitimately wins
        // first — clonefile handles a sparse 2 GiB instantly, the size gate
        // never runs — so there assert the copy happened instead.
        let (tier, detail) = warm_target(&session, &snap);
        if tier == TargetCopyTier::Fresh {
            let detail = detail.expect("fresh tier names the cost");
            assert!(detail.contains("GiB"), "{detail}");
            assert!(detail.contains("cold rebuild"), "{detail}");
            assert!(
                !snap.join("target/big.bin").exists(),
                "fresh tier does not attempt the copy"
            );
        } else {
            assert!(
                snap.join("target/big.bin").exists(),
                "a fast tier won on this clone-capable host: the copy exists"
            );
        }
    }
}
