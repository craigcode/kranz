//! Kill a child's whole descendant tree, not only its process group.
//!
//! Every child spawns as the leader of its own process group and cleanup
//! signals that group. A descendant that calls `setsid` (or `setpgid`) leaves
//! the group and used to outlive the mission with whatever access its
//! sandbox gave it (audit M11). Leaving a group does not change who a
//! process's parent is, so this module walks parent links instead.
//!
//! The walk starts from the leader's pid, which is only safe while the leader
//! is UNREAPED: a reaped pid can be reused by an unrelated process. Callers
//! hold the `Child` and call this before `wait`.
//!
//! Known limit: a descendant whose parent has already exited is reparented to
//! the init process and no longer has a parent link to follow. A process that
//! double-forks and lets the middle process exit before cleanup runs is not
//! found here. An OS-level boundary (a pid namespace, a job object) is what
//! closes that; on macOS there is none to use.

/// Descendants found per pass are stopped before the next pass, so a tree
/// that keeps forking converges; this bounds the passes regardless.
#[cfg(unix)]
const MAX_PASSES: usize = 8;

/// SIGKILL the process group led by `leader` and every descendant of
/// `leader` found by parent links, whatever group or session it moved to.
/// Returns whether the group signal reached at least one process, matching
/// [`crate::backend_claude::kill_group`].
///
/// `leader` must be a live or zombie child the caller has not reaped.
#[cfg(unix)]
pub(crate) fn kill_tree(leader: i32) -> bool {
    debug_assert!(leader > 0, "kill_tree needs a positive leader pid");
    let mut found: Vec<i32> = Vec::new();
    for _ in 0..MAX_PASSES {
        let fresh: Vec<i32> = descendants(leader)
            .into_iter()
            .filter(|pid| !found.contains(pid))
            .collect();
        if fresh.is_empty() {
            break;
        }
        for pid in &fresh {
            // A stopped process cannot fork, so what the next pass finds is
            // only what was created before this signal landed.
            signal(*pid, libc::SIGSTOP);
        }
        found.extend(fresh);
    }
    let delivered = crate::backend_claude::kill_group(leader);
    for pid in found {
        signal(pid, libc::SIGKILL);
    }
    delivered
}

#[cfg(unix)]
fn signal(pid: i32, signal: libc::c_int) {
    if pid > 1 {
        // SAFETY: kill(2) takes a pid and a signal number; no pointers or
        // shared state are involved.
        unsafe {
            libc::kill(pid, signal);
        }
    }
}

/// Every live descendant of `root` (not `root` itself), by parent links.
#[cfg(unix)]
fn descendants(root: i32) -> Vec<i32> {
    let parents = parent_links();
    let mut out: Vec<i32> = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for (pid, ppid) in &parents {
            if *ppid == parent && *pid != root && !out.contains(pid) {
                out.push(*pid);
                frontier.push(*pid);
            }
        }
    }
    out
}

/// `(pid, parent pid)` for every process this user can see.
#[cfg(target_os = "macos")]
fn parent_links() -> Vec<(i32, i32)> {
    // SAFETY: a null buffer asks libproc for the number of pids only.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }
    // Headroom for processes created between the two calls.
    let mut pids = vec![0 as libc::pid_t; count as usize + 64];
    let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
    // SAFETY: the buffer is valid for `bytes` bytes of pid_t.
    let filled = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    if filled <= 0 {
        return Vec::new();
    }
    pids.truncate((filled as usize).min(pids.len()));
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    pids.into_iter()
        .filter(|pid| *pid > 0)
        .filter_map(|pid| {
            // SAFETY: zeroed is a valid proc_bsdinfo (plain integers and
            // byte arrays); the kernel fills at most `size` bytes of it.
            let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
            let read = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTBSDINFO,
                    0,
                    (&mut info as *mut libc::proc_bsdinfo).cast(),
                    size,
                )
            };
            (read == size).then_some((pid, info.pbi_ppid as i32))
        })
        .collect()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn parent_links() -> Vec<(i32, i32)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid: i32 = entry.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            // `pid (comm) state ppid ...`; comm may contain spaces and
            // parentheses, so split after the LAST closing one.
            let rest = stat.rsplit_once(')')?.1;
            let ppid: i32 = rest.split_whitespace().nth(1)?.parse().ok()?;
            Some((pid, ppid))
        })
        .collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    fn alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks that the pid can be signalled.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    fn gone_within(pid: i32, limit: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + limit;
        while std::time::Instant::now() < deadline {
            if !alive(pid) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    /// Audit M11: a descendant that starts its own session leaves the
    /// leader's process group. The group signal alone does not reach it; the
    /// tree kill does.
    #[test]
    fn a_descendant_in_its_own_session_is_killed_with_the_tree() {
        if Command::new("perl").arg("-e").arg("1").status().is_err() {
            eprintln!("SKIP: perl is not available to call setsid");
            return;
        }
        let spawn = || {
            let mut child = Command::new("sh")
                .arg("-c")
                .arg("perl -MPOSIX -e 'POSIX::setsid(); print \"$$\\n\"; $|=1; close(STDOUT); sleep 300' & wait")
                .stdout(Stdio::piped())
                .process_group(0)
                .spawn()
                .expect("spawn leader");
            let mut line = String::new();
            BufReader::new(child.stdout.take().expect("stdout"))
                .read_line(&mut line)
                .expect("read escapee pid");
            let escapee: i32 = line.trim().parse().expect("escapee pid");
            (child, escapee)
        };

        // The gap being closed: the group signal leaves the escapee running.
        let (mut child, escapee) = spawn();
        let leader = child.id() as i32;
        assert!(crate::backend_claude::kill_group(leader));
        let _ = child.wait();
        assert!(
            !gone_within(escapee, std::time::Duration::from_millis(300)),
            "a setsid descendant should survive a group-only kill"
        );
        signal(escapee, libc::SIGKILL);

        let (mut child, escapee) = spawn();
        let leader = child.id() as i32;
        assert!(alive(escapee));
        assert!(kill_tree(leader));
        let _ = child.wait();
        assert!(
            gone_within(escapee, std::time::Duration::from_secs(5)),
            "the tree kill must reach a descendant outside the group"
        );
    }

    #[test]
    fn descendants_exclude_the_root_and_unrelated_processes() {
        let mut child = Command::new("sleep").arg("30").spawn().expect("spawn");
        let pid = child.id() as i32;
        let own = std::process::id() as i32;
        let found = descendants(own);
        assert!(found.contains(&pid), "a direct child is found: {found:?}");
        assert!(!found.contains(&own));
        assert!(descendants(pid).is_empty());
        let _ = child.kill();
        let _ = child.wait();
    }
}
