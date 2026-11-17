//! Test support: directories that live as long as the test process.
//!
//! Nothing outside a test calls this module. It is public only because the
//! other crates' tests need it, as with [`crate::test_capability`].
//!
//! A directory one test owns belongs in a `tempfile::TempDir` binding, which
//! removes it when the test returns or panics. The directories here have no
//! such owner: a home directory every test in a binary shares, the scratch a
//! worker session writes under a root the test never names, or the empty git
//! config the engine keeps for the life of the process. A `static` has no
//! `Drop`, so nothing removed them, and each run of each test binary left
//! its set behind in the system temp directory.
//!
//! They are removed by a C `atexit` hook. libtest returns from `main` when
//! every test passed and calls `std::process::exit` when one failed, and on
//! Unix both run the hook, so a failing run cleans up too. On Windows
//! `std::process::exit` ends the process without running C exit hooks, so
//! there only a passing run cleans up. A process that is killed or aborts
//! runs no hook on any platform.
//!
//! Removal is one best-effort attempt and never panics. A directory that a
//! child process is still writing into, or on Windows still holds a handle
//! in, can be left behind, as before.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, Once};

/// Prefix of the per-process scratch root. Short, because socket paths and
/// Windows paths under a worker's scratch home have little room to spare.
const SCRATCH_PREFIX: &str = "kranz-ts-";

static DIRS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn registered_dirs() -> MutexGuard<'static, Vec<PathBuf>> {
    DIRS.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

extern "C" {
    /// The C standard library's `atexit`, present on every supported target.
    fn atexit(hook: extern "C" fn()) -> std::os::raw::c_int;
}

extern "C" fn remove_registered_dirs() {
    for dir in registered_dirs().iter() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Whether `dir` is specific enough to hand to a recursive delete: absolute,
/// free of `..`, and at least one level below a filesystem root.
fn is_removable(dir: &Path) -> bool {
    dir.is_absolute()
        && dir.parent().is_some_and(|parent| parent.parent().is_some())
        && !dir
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
}

/// Remove `dir` and everything in it when this process exits.
///
/// For a directory the calling test binary created and shares between its
/// tests. Registering a path that does not exist, or the same path twice, is
/// harmless. A relative path, a path through `..`, a filesystem root and a
/// direct child of one are ignored: a relative path would be resolved at
/// exit against whatever the working directory is by then.
///
/// The first call also registers the engine's per-process empty git config;
/// see [`remove_git_config_at_exit`].
pub fn remove_dir_at_exit(dir: &Path) {
    register(dir);
    remove_git_config_at_exit();
}

/// Record `dir` for removal at exit and install the hook on first use.
pub(crate) fn register(dir: &Path) {
    static HOOK: Once = Once::new();
    if !is_removable(dir) {
        return;
    }
    {
        let mut dirs = registered_dirs();
        if !dirs.iter().any(|known| known == dir) {
            dirs.push(dir.to_path_buf());
        }
    }
    HOOK.call_once(|| {
        // SAFETY: `atexit` only records the function pointer. The hook takes
        // no arguments, touches only the registry above, and cannot unwind:
        // a poisoned lock is recovered and every removal error is ignored.
        unsafe {
            atexit(remove_registered_dirs);
        }
    });
}

/// Remove the engine's per-process empty git config when this process exits.
///
/// Every hardened git call points git's user scope at one empty file in a
/// randomly named directory that the engine keeps until the process ends and
/// never removes. A test binary whose tests run engine git calls this once,
/// from the helper that builds its repository. It creates the file if no git
/// call has yet, so the order does not matter.
///
/// Test binaries only. A real process must keep that file for as long as any
/// git child it started can still read the path.
pub fn remove_git_config_at_exit() {
    static CONFIG: Once = Once::new();
    CONFIG.call_once(|| {
        if let Ok(config) = crate::git_ops::empty_global_config_path() {
            if let Some(dir) = config.parent() {
                register(dir);
            }
        }
    });
}

/// A new, empty scratch root inside `base`, registered for removal at exit.
fn create_scratch_root(base: &Path) -> Option<PathBuf> {
    // An operator's override may name a directory nothing has created yet;
    // the engine creates a session's scratch home with its parents.
    std::fs::create_dir_all(base).ok()?;
    let root = tempfile::Builder::new()
        .prefix(SCRATCH_PREFIX)
        .tempdir_in(base)
        .ok()?
        .keep();
    register(&root);
    Some(root)
}

/// Give this process a scratch root of its own, removed when it exits.
///
/// Worker sessions write a scratch home under
/// [`crate::backend_claude::scratch_root_base`], named for a session id the
/// engine often generates itself, and the engine does not remove it. A test
/// binary that runs sessions calls this before the first one, so those homes
/// land in a directory this module removes.
///
/// The root is created inside the scratch base in force at the first call,
/// so an operator's [`crate::backend_claude::SCRATCH_ROOT_ENV`] override
/// still decides where it lives. If it cannot be created the process keeps
/// the scratch base it had.
///
/// Test binaries only. This sets that variable for the whole process, which
/// moves every scratch path computed afterwards. Every test that starts a
/// session or computes a scratch path must call it first, before either:
/// one that does not can have another test's call move the root between a
/// session's write and its own read. Do not call it from a binary that
/// asserts the default root.
pub fn isolate_worker_scratch() {
    static ROOT: Once = Once::new();
    ROOT.call_once(|| {
        if let Some(root) = create_scratch_root(&crate::backend_claude::scratch_root_base()) {
            std::env::set_var(crate::backend_claude::SCRATCH_ROOT_ENV, &root);
        }
    });
    remove_git_config_at_exit();
}

/// The scratch base this crate's own unit tests use in place of the system
/// temp directory: one directory per test process, removed at exit.
///
/// The unit tests start sessions from many modules with no shared setup, and
/// many name their session with a fixed id, so in the system temp directory
/// two test processes would share (and neither could safely remove) the same
/// scratch homes. `None` in the child process that asserts the real default,
/// and when the directory cannot be created.
#[cfg(test)]
pub(crate) fn unit_test_scratch_base() -> Option<PathBuf> {
    static ROOT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        if std::env::var_os("KRANZ_SCRATCH_TEST_CASE").is_some() {
            return None;
        }
        create_scratch_root(&std::env::temp_dir())
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_specific_absolute_directory_is_removable() {
        let temp = std::env::temp_dir();
        assert!(is_removable(&temp.join("kranz-test-home-1")));
        assert!(!is_removable(Path::new("")));
        assert!(!is_removable(Path::new("relative/dir")));
        assert!(!is_removable(&temp.join("..").join("elsewhere")));
        let root = temp.ancestors().last().unwrap();
        assert!(!is_removable(root));
        assert!(!is_removable(&root.join("top-level")));
    }

    #[test]
    fn a_refused_path_is_not_registered() {
        register(Path::new("relative/dir"));
        assert!(!registered_dirs()
            .iter()
            .any(|dir| dir == Path::new("relative/dir")));
    }
}
