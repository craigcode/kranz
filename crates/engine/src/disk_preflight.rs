//! Pre-drain disk-footprint check (ticket `mission-build-footprint`).
//!
//! A mission builds a full `target/debug` in its integration worktree, the
//! validation snapshot copies a warmed subset on top of that, and the merge
//! gate may build again in a scratch worktree. On a disk with less free space
//! than that ladder needs, the run dies mid-command with `os error 28`
//! (ENOSPC) — burning feature respawn budgets on `Partial` runs (observed on
//! m-a5a8fd and m-eee81f). This module refuses to START a mission when the
//! free space under the repo is below the estimated footprint, naming the
//! number, instead of dying mid-feature. It is deliberately a drain-time
//! refusal, not an in-mission guard: the honest failure is up front.

use cap_fs_ext::DirExt as _;
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use std::path::Path;

/// The decision a pre-drain disk check reaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskPreflight {
    /// Free space meets the estimate (or could not be measured — proceed and
    /// let the build surface any real ENOSPC, exactly as before this check;
    /// never fabricate a "0 bytes free" refusal).
    Sufficient,
    /// Free space is below the estimated build footprint: refuse to start.
    Insufficient {
        free_bytes: u64,
        estimate_bytes: u64,
    },
}

/// Free bytes available to unprivileged users on the volume containing
/// `path` (`statvfs` `f_bavail` × `f_frsize`). `None` when the call fails —
/// the check then degrades to "proceed" rather than inventing a number.
#[cfg(unix)]
pub fn available_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }
    Some(u64::from(stat.f_bavail).saturating_mul(stat.f_frsize))
}

/// No statvfs equivalent is wired up on non-Unix targets; report unmeasurable
/// so the check degrades to proceed.
#[cfg(not(unix))]
pub fn available_bytes(_path: &Path) -> Option<u64> {
    None
}

/// Estimate multiplier on the primary checkout's `target/` size: one fresh
/// worktree build (~1×) plus the snapshot copy and merge-gate rebuild
/// headroom (~1×). Conservative on purpose — a premature refusal costs an
/// operator re-check, a mid-mission ENOSPC costs respawn budget.
const FOOTPRINT_MULTIPLIER: u64 = 2;

/// Floor for the estimate when the repo has no measurable `target/` yet (a
/// fresh checkout still builds a full debug workspace).
const FOOTPRINT_FLOOR_BYTES: u64 = 4 * 1024 * 1024 * 1024; // 4 GiB

/// Estimated bytes a mission's build ladder needs: the primary checkout's
/// current `target/` size × [`FOOTPRINT_MULTIPLIER`], floored so a
/// target-less repo still gets a sane bound. The walk is capped at `cap`
/// bytes — once the running total passes it the estimate is already "too
/// big", so a multi-GiB tree is not priced to the last file.
pub fn estimate_build_footprint_bytes(repo_root: &Path) -> u64 {
    // Cap the walk at the point where the answer is already "insufficient"
    // for any realistic disk: past the cap the exact size is irrelevant.
    const WALK_CAP: u64 = 256 * 1024 * 1024 * 1024; // 256 GiB
    let measured = dir_size_capped(&repo_root.join("target"), WALK_CAP);
    measured
        .saturating_mul(FOOTPRINT_MULTIPLIER)
        .max(FOOTPRINT_FLOOR_BYTES)
}

fn dir_size_capped(dir: &Path, cap: u64) -> u64 {
    // A repository may contain an arbitrarily wide `target/` tree. Bound
    // both bytes and directory entries so a tree of millions of empty files
    // cannot turn the preflight itself into an unbounded denial of service.
    const MAX_WALK_ENTRIES: usize = 250_000;
    dir_size_capped_with_entry_limit(dir, cap, MAX_WALK_ENTRIES)
}

fn dir_size_capped_with_entry_limit(dir: &Path, cap: u64, max_entries: usize) -> u64 {
    let Some(parent) = dir.parent() else {
        return 0;
    };
    let Some(name) = dir.file_name() else {
        return 0;
    };
    // Pin the parent and open the target leaf no-follow. Descendants are then
    // opened relative to retained directory capabilities, so a repo-authored
    // symlink is never traversed or priced as its external target.
    let Ok(parent) = Dir::open_ambient_dir(parent, ambient_authority()) else {
        return 0;
    };
    let Ok(root) = parent.open_dir_nofollow(name) else {
        return 0;
    };

    let mut total = 0u64;
    let mut visited = 0usize;
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        let Ok(entries) = d.entries() else {
            continue;
        };
        for entry in entries.flatten() {
            visited = visited.saturating_add(1);
            if visited > max_entries {
                return cap;
            }
            let name = entry.file_name();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if let Ok(child) = d.open_dir_nofollow(&name) {
                    stack.push(child);
                }
            } else if file_type.is_file() {
                // Re-check the entry no-follow before using its size. If it
                // was swapped after `file_type`, a symlink/special entry is
                // skipped instead of followed.
                if let Ok(meta) = d.symlink_metadata(&name) {
                    if meta.file_type().is_file() {
                        total = total.saturating_add(meta.len());
                        if total >= cap {
                            return total;
                        }
                    }
                }
            }
        }
    }
    total
}

/// The pre-drain check: compare free space under `repo_root` against the
/// estimated build footprint.
pub fn check(repo_root: &Path) -> DiskPreflight {
    decide(
        available_bytes(repo_root),
        estimate_build_footprint_bytes(repo_root),
    )
}

/// The decision boundary, separated from the host-dependent statvfs/read_dir
/// probes so the refusal logic is unit-testable. `None` free space (a failed
/// measurement) degrades to proceed — never a fabricated refusal.
fn decide(free: Option<u64>, estimate: u64) -> DiskPreflight {
    match free {
        Some(free) if free < estimate => DiskPreflight::Insufficient {
            free_bytes: free,
            estimate_bytes: estimate,
        },
        _ => DiskPreflight::Sufficient,
    }
}

/// Render a byte count as `N.N GiB` for the refusal reason.
pub fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mission_build_footprint_estimate_floors_a_targetless_repo() {
        let dir = tempfile::tempdir().unwrap();
        // No target/ at all → the floor, not zero.
        let estimate = estimate_build_footprint_bytes(dir.path());
        assert_eq!(estimate, FOOTPRINT_FLOOR_BYTES);
    }

    #[test]
    fn mission_build_footprint_measurement_and_cap() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target").join("debug");
        std::fs::create_dir_all(&target).unwrap();
        // 3 MiB of real bytes across files.
        let chunk = vec![0_u8; 1024 * 1024];
        for i in 0..3 {
            std::fs::write(target.join(format!("f{i}")), &chunk).unwrap();
        }
        // The measurement sums real bytes; the walk stops at the cap.
        assert_eq!(
            dir_size_capped(&dir.path().join("target"), u64::MAX),
            3 * 1024 * 1024
        );
        assert_eq!(
            dir_size_capped(&dir.path().join("target"), 1024 * 1024),
            1024 * 1024
        );
        // A small target is dominated by the floor (the multiplier never
        // produces a sub-floor estimate).
        assert_eq!(
            estimate_build_footprint_bytes(dir.path()),
            FOOTPRINT_FLOOR_BYTES
        );
    }

    #[test]
    fn mission_build_footprint_entry_limit_is_conservative() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        for i in 0..3 {
            std::fs::write(target.join(format!("empty-{i}")), []).unwrap();
        }
        assert_eq!(
            dir_size_capped_with_entry_limit(&target, 123_456, 2),
            123_456
        );
    }

    #[cfg(unix)]
    #[test]
    fn mission_build_footprint_never_follows_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("real"), [0_u8; 7]).unwrap();
        std::fs::write(outside.path().join("large"), vec![0_u8; 1024 * 1024]).unwrap();
        symlink(outside.path(), target.join("outside-link")).unwrap();

        assert_eq!(dir_size_capped(&target, u64::MAX), 7);
    }

    #[test]
    fn mission_build_footprint_decide_refuses_only_below_the_estimate() {
        // Below the estimate: refuse, naming both figures.
        assert_eq!(
            decide(Some(1024), 2048),
            DiskPreflight::Insufficient {
                free_bytes: 1024,
                estimate_bytes: 2048
            }
        );
        // At and above: sufficient.
        assert_eq!(decide(Some(2048), 2048), DiskPreflight::Sufficient);
        assert_eq!(decide(Some(4096), 2048), DiskPreflight::Sufficient);
        // Unmeasurable: proceed (never a fabricated refusal).
        assert_eq!(decide(None, 2048), DiskPreflight::Sufficient);
    }

    #[cfg(unix)]
    #[test]
    fn mission_build_footprint_available_bytes_reports_a_real_figure() {
        let dir = tempfile::tempdir().unwrap();
        let free = available_bytes(dir.path()).expect("statvfs on a tempdir");
        assert!(free > 0, "a real volume reports non-zero free bytes");
    }
}
