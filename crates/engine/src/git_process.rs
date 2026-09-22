//! Bounded, exact child output for Git and operator coordination helpers.
//! Reuses the agent process-group / Job Object
//! primitives, without the gate runner's combined, lossy tail representation.

use std::ffi::OsString;
use std::io;
use std::process::{Command, Output, Stdio};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    timeout: Duration,
    stdout: usize,
    stderr: usize,
}

impl Limits {
    pub(crate) fn for_control(timeout: Duration) -> Self {
        Self {
            timeout,
            stdout: 64 * 1024,
            stderr: 4 * 1024,
        }
    }

    pub(super) fn for_command(args: &[OsString], network: bool) -> Self {
        let config = args.first().is_some_and(|arg| arg == "config");
        Self {
            timeout: Duration::from_secs(if config {
                10
            } else if network {
                600
            } else {
                120
            }),
            stdout: if config {
                1024 * 1024
            } else {
                64 * 1024 * 1024
            },
            stderr: 2 * 1024 * 1024,
        }
    }
}

pub(crate) fn output(command: Command, limits: Limits) -> io::Result<Output> {
    let run = move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(capture(command.into(), limits))
    };
    // Git's public API is synchronous, including some callers already inside
    // Tokio. A scoped thread avoids nesting runtimes without changing callers.
    if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|scope| {
            scope
                .spawn(run)
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("process supervisor panicked")))
        })
    } else {
        run()
    }
}

async fn capture(mut command: tokio::process::Command, limits: Limits) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(windows::Win32::System::Threading::CREATE_SUSPENDED.0);
    let mut child = command.spawn()?;
    // No Git code runs before Windows Job assignment. A running child could
    // otherwise spawn a descendant in the assignment gap, outside the Job.
    #[cfg(windows)]
    let job = match child.raw_handle() {
        Some(handle) => {
            match crate::backend_claude::win_job::JobHandle::create_and_assign(handle) {
                Ok(job) => job,
                Err(error) => {
                    let _ = child.kill().await;
                    return Err(io::Error::other(format!(
                        "cannot supervise Git process tree: {error}"
                    )));
                }
            }
        }
        None => {
            let _ = child.kill().await;
            return Err(io::Error::other("Git child has no process handle"));
        }
    };
    #[cfg(windows)]
    if let Err(error) = resume_primary_thread(child.id().expect("unreaped child has an id")) {
        job.kill();
        let _ = child.kill().await;
        return Err(error);
    }
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let execution = async {
        // Drain concurrently, but retain the unreaped leader until both pipes
        // close. Its PID then cannot be recycled during descendant cleanup.
        let (stdout, stderr) = tokio::try_join!(
            read_exact_bounded(stdout, limits.stdout, "stdout"),
            read_exact_bounded(stderr, limits.stderr, "stderr"),
        )?;
        Ok(Output {
            status: child.wait().await?,
            stdout,
            stderr,
        })
    };
    let result = match tokio::time::timeout(limits.timeout, execution).await {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("process exceeded its {:#?} deadline", limits.timeout),
        )),
    };
    if result.is_err() {
        #[cfg(unix)]
        crate::backend_claude::kill_unreaped_group(&child);
        #[cfg(windows)]
        job.kill();
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}

/// std/Tokio retains the process handle but not the primary thread handle.
/// Locate that thread while the child is still suspended, then resume only
/// after the owning Job has been assigned. Every acquired handle is RAII-owned.
#[cfg(windows)]
fn resume_primary_thread(pid: u32) -> io::Result<()> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    // SAFETY: ToolHelp returns an owned kernel handle; its checked wrapper
    // rejects INVALID_HANDLE_VALUE. Ownership transfers exactly once.
    let snapshot =
        unsafe { OwnedHandle::from_raw_handle(CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)?.0) };
    let handle = HANDLE(snapshot.as_raw_handle());
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut thread = None;
    // SAFETY: the snapshot handle and writable THREADENTRY32 remain live for
    // every call; dwSize carries the layout expected by ToolHelp.
    unsafe {
        Thread32First(handle, &mut entry)?;
        loop {
            if entry.th32OwnerProcessID == pid && thread.replace(entry.th32ThreadID).is_some() {
                return Err(io::Error::other("suspended Git child has multiple threads"));
            }
            if Thread32Next(handle, &mut entry).is_err() {
                break;
            }
        }
    }
    let id = thread.ok_or_else(|| io::Error::other("suspended Git primary thread not found"))?;
    // SAFETY: the child is still alive and suspended, so its thread identity
    // cannot be recycled. Request only the resume right; own and close once.
    let thread =
        unsafe { OwnedHandle::from_raw_handle(OpenThread(THREAD_SUSPEND_RESUME, false, id)?.0) };
    // SAFETY: the owned handle has THREAD_SUSPEND_RESUME, and Job assignment
    // completed before this function was called.
    let count = unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) };
    if count == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    if count != 1 {
        return Err(io::Error::other("unexpected Git child suspend count"));
    }
    Ok(())
}

async fn read_exact_bounded<R: AsyncRead + Unpin>(
    mut reader: R,
    limit: usize,
    stream: &str,
) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            return Ok(output);
        }
        if count > limit.saturating_sub(output.len()) {
            return Err(io::Error::other(format!(
                "Git {stream} exceeded its {limit}-byte limit"
            )));
        }
        output.extend_from_slice(&chunk[..count]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const FIXTURE: &str = "git_ops::process::tests::git_process_fixture";
    const MODE: &str = "KRANZ_GIT_PROCESS_FIXTURE";

    fn fixture(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--ignored", "--exact", FIXTURE, "--nocapture"]);
        command.env(MODE, mode);
        command
    }

    fn limits() -> Limits {
        Limits {
            timeout: Duration::from_secs(5),
            stdout: 65536,
            stderr: 65536,
        }
    }

    #[test]
    #[ignore = "subprocess fixture, invoked by supervisor tests"]
    // Intentionally leave the descendant for the supervisor to kill. Waiting
    // here would remove the exited-leader regression this fixture exercises.
    #[allow(clippy::zombie_processes)]
    fn git_process_fixture() {
        let Ok(mode) = std::env::var(MODE) else {
            return;
        };
        if mode == "descendant" {
            std::thread::sleep(Duration::from_millis(1000));
            std::fs::write(
                std::env::var_os("KRANZ_GIT_PROCESS_MARKER").unwrap(),
                "survived",
            )
            .unwrap();
        } else if mode == "bytes" {
            std::io::stdout().write_all(b"\0\xffraw\r\n").unwrap();
            std::io::stderr().write_all(b"\xfeerror\0").unwrap();
        } else {
            let _child = fixture("descendant").spawn().unwrap();
            if mode == "orphan" {
                std::process::exit(0);
            }
            if mode == "stdout" {
                std::io::stdout()
                    .write_all(&vec![b'x'; 128 * 1024])
                    .unwrap();
            } else if mode == "stderr" {
                std::io::stderr()
                    .write_all(&vec![b'x'; 128 * 1024])
                    .unwrap();
            }
            std::thread::sleep(Duration::from_secs(60));
        }
        std::process::exit(0);
    }

    #[test]
    fn git_capture_preserves_separate_binary_streams() {
        let out = output(fixture("bytes"), limits()).unwrap();
        assert!(out.status.success());
        assert!(out.stdout.ends_with(b"\0\xffraw\r\n"));
        assert_eq!(out.stderr, b"\xfeerror\0");
    }

    #[tokio::test]
    async fn git_capture_works_inside_an_existing_runtime() {
        assert!(output(fixture("bytes"), limits()).unwrap().status.success());
    }

    #[tokio::test]
    async fn git_capture_stream_accepts_exact_limit_and_refuses_one_extra_byte() {
        assert_eq!(
            read_exact_bounded(&b"123"[..], 3, "stdout").await.unwrap(),
            b"123"
        );
        assert!(read_exact_bounded(&b"1234"[..], 3, "stdout")
            .await
            .unwrap_err()
            .to_string()
            .contains("3-byte"));
    }

    fn assert_tree_stopped(mode: &str) {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("late-descendant");
        let mut command = fixture(mode);
        command.env("KRANZ_GIT_PROCESS_MARKER", &marker);
        let mut limits = limits();
        if mode == "timeout" || mode == "orphan" {
            limits.timeout = Duration::from_millis(300);
        }
        let started = std::time::Instant::now();
        let error = output(command, limits).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(
            error
                .to_string()
                .contains(if mode == "timeout" || mode == "orphan" {
                    "deadline"
                } else {
                    mode
                }),
            "{error}"
        );
        std::thread::sleep(Duration::from_millis(1300));
        assert!(!marker.exists(), "descendant survived {mode}");
    }

    #[test]
    fn git_capture_deadline_kills_descendants() {
        assert_tree_stopped("timeout");
    }
    #[test]
    fn git_capture_deadline_kills_descendants_after_leader_exits() {
        assert_tree_stopped("orphan");
    }

    #[test]
    fn git_capture_descendant_fixture_writes_marker_without_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("control-marker");
        let mut command = fixture("descendant");
        command.env("KRANZ_GIT_PROCESS_MARKER", &marker);
        assert!(output(command, limits()).unwrap().status.success());
        assert_eq!(std::fs::read(marker).unwrap(), b"survived");
    }

    #[test]
    fn git_capture_stdout_limit_kills_descendants() {
        assert_tree_stopped("stdout");
    }
    #[test]
    fn git_capture_stderr_limit_kills_descendants() {
        assert_tree_stopped("stderr");
    }
}
