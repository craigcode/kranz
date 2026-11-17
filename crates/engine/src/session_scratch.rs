//! Removal of a session's scratch home once the session is over.
//!
//! Every agent session gets a private scratch root under
//! [`crate::backend_claude::scratch_root_base`], named
//! `kranz-worker-home-<session id>`. It holds the session's `HOME`, which can
//! contain a copy of the CLI credential file and, on macOS, a symbolic link to
//! the operator's real keychain directory, plus a temp directory, a copy of
//! the Cargo registry cache and the hook spec and record files. The engine
//! reads nothing from it after the session's hook records are folded into the
//! event log, and no later session is pointed at it, so it is removed when
//! the session ends.
//!
//! [`SessionScratch`] is the owner. Create it before anything is written to
//! the scratch root and keep it until the session object is gone. On the
//! ordinary path call [`SessionScratch::remove`] and await it. If the owner
//! is dropped instead, which is what happens on an early error return and
//! when the future holding it is cancelled, the removal still happens.
//!
//! # Where the work runs
//!
//! The scratch root can be large, so removing it can take a while. Inside a
//! tokio runtime the removal runs on the runtime's blocking pool and never on
//! the thread that polls futures. [`SessionScratch::remove`] waits for it;
//! a drop does not. If the runtime shuts down before a queued removal has
//! started, the removal runs on the thread that shuts the runtime down, so
//! it is never skipped. Outside a runtime the removal runs where the owner
//! is dropped.
//!
//! # What is removed
//!
//! Unix only. On other platforms nothing is removed and the scratch root
//! stays, as it did before this module existed: there is no walk there that
//! is safe against a directory being swapped for a link while it runs.
//!
//! The scratch root is opened once without following a link, and its type
//! and owner are read from that open directory; everything under it is then
//! removed relative to it (see
//! [`crate::sandbox_container::remove_own_directory_nofollow`]). A symbolic
//! link or another user's directory at the path is refused. No link inside
//! is followed (the keychain link is unlinked, its target is untouched), and
//! a directory the session made unreadable is repaired first. The session's
//! credential and keychain entries are removed before anything else, and an
//! entry that cannot be removed does not stop the rest from going. It is one
//! best-effort attempt: a failure is logged and never fails the run.
//!
//! The directory itself is removed only when its parent is
//! [private to this user](dir_is_private_to_this_user). In a parent that
//! another user can write, such as a default Linux `/tmp` or a directory with
//! group write permission, any such user could re-create the freed name, and
//! a background process the session left behind would then find another
//! user's files at its `HOME`. There the contents are removed and the empty
//! directory, which only this user can write, is left in place.

use std::path::{Path, PathBuf};

/// The file-name prefix of every session scratch root.
const SCRATCH_HOME_PREFIX: &str = "kranz-worker-home-";

/// Whether this platform removes scratch homes at all. Tests that assert a
/// scratch home is gone check this first.
pub const REMOVES_SCRATCH_HOMES: bool = cfg!(unix);

/// Entries removed before the rest of a scratch root, relative to it: the
/// places a backend seeds a credential copy or a keychain link.
#[cfg(unix)]
const SENSITIVE_FIRST: &[&[&str]] = &[
    &["home", ".claude"],
    &["home", ".codex"],
    &["home", ".cursor"],
    &["home", ".kimi-code"],
    &["home", "Library"],
];

/// Owner of one session's scratch root; removes it when the session is over.
#[derive(Debug)]
pub struct SessionScratch {
    path: PathBuf,
    /// Taken by [`SessionScratch::remove`] or by the drop, whichever is first.
    removal: Option<Removal>,
}

/// Removes the scratch root at its path when dropped. Being a value with a
/// `Drop`, it does its work even when the blocking task that was meant to
/// run it is discarded unrun at runtime shutdown.
#[derive(Debug)]
struct Removal(PathBuf);

impl Drop for Removal {
    fn drop(&mut self) {
        if let Err(error) = remove_scratch_home(&self.0) {
            tracing::warn!(
                path = %self.0.display(),
                %error,
                "could not remove a finished session's scratch home"
            );
        }
    }
}

impl Removal {
    /// Whether there is nothing to remove, so no thread need be found to do
    /// it: nothing is at the path (one `lstat`), or this platform removes
    /// nothing.
    fn has_nothing_to_do(&self) -> bool {
        !REMOVES_SCRATCH_HOMES
            || matches!(
                std::fs::symlink_metadata(&self.0),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            )
    }
}

impl SessionScratch {
    /// The scratch root of session `session_id`.
    ///
    /// The path is resolved once, here. The scratch base is read from the
    /// environment on every call, so resolving it again at removal time
    /// could name a different directory.
    pub fn for_session(session_id: &str) -> Self {
        let path = crate::backend_claude::scratch_home_root(session_id);
        Self {
            removal: Some(Removal(path.clone())),
            path,
        }
    }

    /// The scratch root this value removes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Remove the scratch root now and return once it is gone (or once the
    /// one attempt has failed and been logged).
    pub async fn remove(mut self) {
        let Some(removal) = self.removal.take() else {
            return;
        };
        if removal.has_nothing_to_do() {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            // If the task is cancelled unrun, dropping its closure has
            // already done the removal, so the join result carries nothing.
            Ok(handle) => {
                let _ = handle.spawn_blocking(move || drop(removal)).await;
            }
            Err(_) => drop(removal),
        }
    }
}

impl Drop for SessionScratch {
    fn drop(&mut self) {
        let Some(removal) = self.removal.take() else {
            return;
        };
        if removal.has_nothing_to_do() {
            return;
        }
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(move || drop(removal));
            }
            Err(_) => drop(removal),
        }
    }
}

/// Whether `dir` is a directory only the calling user can create names in.
///
/// On Unix: `dir` (after following symbolic links) is a directory owned by
/// the effective user with no group or other write permission. The per-user
/// temp directory on macOS passes; a world-writable `/tmp` does not, and
/// neither does a directory of mode `0775`, even when the group is the
/// user's own. On other platforms this is always `false`, because the
/// standard library exposes no owner to check.
pub(crate) fn dir_is_private_to_this_user(dir: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: `geteuid` has no preconditions and cannot fail.
        let euid = unsafe { libc::geteuid() };
        match std::fs::metadata(dir) {
            Ok(meta) => meta.is_dir() && meta.uid() == euid && meta.mode() & 0o022 == 0,
            Err(_) => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        false
    }
}

/// Remove the scratch root at `path`, or empty it when its parent is shared.
/// A path that is absent is a success. A path that is not a scratch root of
/// this user is refused with an error and left alone.
#[cfg(unix)]
fn remove_scratch_home(path: &Path) -> std::io::Result<()> {
    let refuse = |why: &str| Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, why));
    let named_as_scratch = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(SCRATCH_HOME_PREFIX));
    if !named_as_scratch {
        return refuse("the path is not named as a session scratch home");
    }
    let Some(parent) = path.parent() else {
        return refuse("the path has no parent directory");
    };
    let keep_directory = !dir_is_private_to_this_user(parent);
    if keep_directory {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| {
            tracing::info!(
                parent = %parent.display(),
                "the scratch directory can be written by other users or by a group, so \
                 finished session homes are emptied and their empty directories are kept; \
                 make it mode 0700 or 0755 to have them removed"
            );
        });
    }
    // The type and owner checks are made by the remover on the directory it
    // has open, not here on the path.
    crate::sandbox_container::remove_own_directory_nofollow(path, SENSITIVE_FIRST, keep_directory)
}

/// Nothing is removed off Unix; see the module documentation.
#[cfg(not(unix))]
fn remove_scratch_home(path: &Path) -> std::io::Result<()> {
    let _ = (path, SCRATCH_HOME_PREFIX);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn seeded_home(base: &Path, name: &str) -> PathBuf {
        let root = base.join(name);
        std::fs::create_dir_all(root.join("home").join(".claude")).unwrap();
        std::fs::write(
            root.join("home").join(".claude").join(".credentials.json"),
            "{}",
        )
        .unwrap();
        std::fs::create_dir_all(root.join("hook-gate")).unwrap();
        std::fs::write(root.join("hook-gate").join("spec.json"), "{}").unwrap();
        root
    }

    #[cfg(unix)]
    fn owner_of(root: &Path) -> SessionScratch {
        SessionScratch {
            path: root.to_path_buf(),
            removal: Some(Removal(root.to_path_buf())),
        }
    }

    #[cfg(unix)]
    #[test]
    fn dropping_the_owner_removes_the_scratch_home() {
        let base = tempfile::tempdir().unwrap();
        let root = seeded_home(base.path(), "kranz-worker-home-s1");
        drop(owner_of(&root));
        assert!(!root.exists(), "the scratch home is gone");
    }

    /// The ordinary path: the caller awaits the removal, and it is complete
    /// when the call returns.
    #[cfg(unix)]
    #[tokio::test]
    async fn awaited_removal_is_complete_on_return() {
        let base = tempfile::tempdir().unwrap();
        let root = seeded_home(base.path(), "kranz-worker-home-s5");
        owner_of(&root).remove().await;
        assert!(!root.exists());
    }

    /// An owner dropped inside a runtime hands the removal to the blocking
    /// pool. Shutting the runtime down waits for it, or runs it if it never
    /// started, so the home is gone once the runtime is.
    #[cfg(unix)]
    #[test]
    fn an_owner_dropped_inside_a_runtime_still_removes_the_home() {
        let base = tempfile::tempdir().unwrap();
        let root = seeded_home(base.path(), "kranz-worker-home-s6");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            drop(owner_of(&root));
        });
        drop(runtime);
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_scratch_home_is_not_an_error() {
        let base = tempfile::tempdir().unwrap();
        remove_scratch_home(&base.path().join("kranz-worker-home-never-made")).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_path_without_the_scratch_name_is_refused() {
        let base = tempfile::tempdir().unwrap();
        let other = base.path().join("somebody-elses-directory");
        std::fs::create_dir_all(other.join("kept")).unwrap();
        assert!(remove_scratch_home(&other).is_err());
        assert!(other.join("kept").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_named_as_a_scratch_home_is_refused_and_its_target_kept() {
        let base = tempfile::tempdir().unwrap();
        let target = base.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("kept.txt"), "kept").unwrap();
        let link = base.path().join("kranz-worker-home-link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(remove_scratch_home(&link).is_err());
        assert!(link.symlink_metadata().is_ok(), "the link is left alone");
        assert_eq!(
            std::fs::read_to_string(target.join("kept.txt")).unwrap(),
            "kept"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_keychain_style_symlink_is_unlinked_and_its_target_kept() {
        let base = tempfile::tempdir().unwrap();
        let real_keychains = base.path().join("real-keychains");
        std::fs::create_dir_all(&real_keychains).unwrap();
        std::fs::write(real_keychains.join("login.keychain-db"), "secret").unwrap();
        let root = seeded_home(base.path(), "kranz-worker-home-s2");
        let library = root.join("home").join("Library");
        std::fs::create_dir_all(&library).unwrap();
        std::os::unix::fs::symlink(&real_keychains, library.join("Keychains")).unwrap();

        remove_scratch_home(&root).unwrap();

        assert!(!root.exists());
        assert_eq!(
            std::fs::read_to_string(real_keychains.join("login.keychain-db")).unwrap(),
            "secret",
            "removal must not follow the link into the real keychain directory"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_the_session_locked_is_still_removed() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = tempfile::tempdir().unwrap();
        let root = seeded_home(base.path(), "kranz-worker-home-s3");
        let locked = root.join("home").join("locked");
        std::fs::create_dir_all(locked.join("inner")).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result = remove_scratch_home(&root);
        if root.exists() {
            // Let the temp directory clean up if the assertion below fails.
            let _ = std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700));
        }
        result.unwrap();
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn in_a_shared_parent_the_home_is_emptied_and_the_directory_kept() {
        use std::os::unix::fs::PermissionsExt as _;
        let outer = tempfile::tempdir().unwrap();
        let shared = outer.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
        assert!(!dir_is_private_to_this_user(&shared));
        let root = seeded_home(&shared, "kranz-worker-home-s4");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();

        remove_scratch_home(&root).unwrap();

        assert!(root.is_dir(), "the name stays taken in a shared parent");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
        let mode = std::fs::metadata(&root).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    /// The shared-parent case is where another party can put a link at the
    /// scratch name. It must be refused there too, with the target untouched
    /// and its mode unchanged.
    #[cfg(unix)]
    #[test]
    fn in_a_shared_parent_a_symlink_at_the_scratch_name_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let outer = tempfile::tempdir().unwrap();
        let shared = outer.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).unwrap();
        let target = outer.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(target.join("kept.txt"), "kept").unwrap();
        let link = shared.join("kranz-worker-home-swapped");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(remove_scratch_home(&link).is_err());

        assert_eq!(
            std::fs::read_to_string(target.join("kept.txt")).unwrap(),
            "kept"
        );
        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "the link target's mode is not changed");
    }

    /// The scratch root itself may have been made unreadable by its session.
    #[cfg(unix)]
    #[test]
    fn a_scratch_root_the_session_locked_is_still_removed() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = tempfile::tempdir().unwrap();
        let root = seeded_home(base.path(), "kranz-worker-home-s7");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result = remove_scratch_home(&root);
        if root.exists() {
            let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
        }
        result.unwrap();
        assert!(!root.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_private_directory_is_recognised() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = tempfile::tempdir().unwrap();
        let private = base.path().join("private");
        std::fs::create_dir(&private).unwrap();
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(dir_is_private_to_this_user(&private));
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o775)).unwrap();
        assert!(!dir_is_private_to_this_user(&private));
        assert!(!dir_is_private_to_this_user(&private.join("absent")));
    }

    #[test]
    fn the_path_is_resolved_when_the_owner_is_created() {
        let owner = SessionScratch::for_session("abc");
        assert_eq!(
            owner.path(),
            crate::backend_claude::scratch_home_root("abc").as_path()
        );
        // Nothing was created, so dropping it removes nothing.
    }
}
