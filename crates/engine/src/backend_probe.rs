use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle as _;

const OUTPUT_TAIL_BYTES: usize = 64 * 1024;

pub(crate) fn probe_version(
    binary: &Path,
    timeout: Duration,
) -> std::result::Result<String, String> {
    let mut command = Command::new(binary);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command
        .spawn()
        .map_err(|e| format!("could not run --version: {e}"))?;
    let process_tree = match ProbeProcessTree::new(&child) {
        Ok(process_tree) => process_tree,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let stdout = child.stdout.take().expect("stdout was configured as piped");
    let stderr = child.stderr.take().expect("stderr was configured as piped");
    let stdout_rx = drain_bounded(stdout);
    let stderr_rx = drain_bounded(stderr);

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                process_tree.kill(&mut child);
                return Err(format!(
                    "--version did not exit within {}s (killed)",
                    timeout.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                process_tree.kill(&mut child);
                return Err(format!("could not wait for --version: {error}"));
            }
        }
    };

    let stdout = match receive_output(stdout_rx, "stdout", deadline) {
        Ok(stdout) => stdout,
        Err(error) => {
            process_tree.kill(&mut child);
            return Err(error);
        }
    };
    let stderr = match receive_output(stderr_rx, "stderr", deadline) {
        Ok(stderr) => stderr,
        Err(error) => {
            process_tree.kill(&mut child);
            return Err(error);
        }
    };
    if status.success() {
        Ok(String::from_utf8_lossy(&stdout).trim().to_string())
    } else {
        let detail = String::from_utf8_lossy(&stderr);
        let detail = detail.trim();
        if detail.is_empty() {
            Err(format!("--version exited with {status}"))
        } else {
            Err(format!("--version exited with {status}: {detail}"))
        }
    }
}

fn drain_bounded<R>(mut reader: R) -> mpsc::Receiver<std::io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut tail = Vec::with_capacity(OUTPUT_TAIL_BYTES);
        let mut chunk = [0u8; 8192];
        let result = loop {
            match reader.read(&mut chunk) {
                Ok(0) => break Ok(tail),
                Ok(read) => {
                    let excess = tail
                        .len()
                        .saturating_add(read)
                        .saturating_sub(OUTPUT_TAIL_BYTES);
                    if excess > 0 {
                        tail.drain(..excess);
                    }
                    tail.extend_from_slice(&chunk[..read]);
                }
                Err(error) => break Err(error),
            }
        };
        let _ = tx.send(result);
    });
    rx
}

fn receive_output(
    receiver: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    stream: &str,
    deadline: Instant,
) -> std::result::Result<Vec<u8>, String> {
    receiver
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => {
                format!("--version {stream} pipe did not close before the probe deadline")
            }
            mpsc::RecvTimeoutError::Disconnected => {
                format!("could not read --version {stream}: reader stopped")
            }
        })?
        .map_err(|error| format!("could not read --version {stream}: {error}"))
}

struct ProbeProcessTree {
    #[cfg(unix)]
    group_pid: u32,
    #[cfg(windows)]
    job: crate::backend_claude::win_job::JobHandle,
}

impl ProbeProcessTree {
    fn new(child: &Child) -> std::result::Result<Self, String> {
        #[cfg(windows)]
        let job =
            crate::backend_claude::win_job::JobHandle::create_and_assign(child.as_raw_handle())
                .map_err(|error| format!("could not isolate --version process tree: {error}"))?;
        Ok(Self {
            #[cfg(unix)]
            group_pid: child.id(),
            #[cfg(windows)]
            job,
        })
    }

    fn kill(&self, child: &mut Child) {
        #[cfg(unix)]
        if let Ok(group_pid) = i32::try_from(self.group_pid) {
            unsafe {
                libc::kill(-group_pid, libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        self.job.kill();
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn chatty_healthy_probe_drains_pipes_before_waiting_for_exit() {
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("chatty-version");
        std::fs::write(
            &stub,
            "#!/bin/sh\n\
             i=0\n\
             while [ \"$i\" -lt 20000 ]; do\n\
               printf 'stdout-padding-0123456789abcdef\\n'\n\
               printf 'stderr-padding-0123456789abcdef\\n' >&2\n\
               i=$((i + 1))\n\
             done\n\
             printf 'healthy-version-1.2.3\\n'\n",
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let version = probe_version(&stub, Duration::from_secs(3)).unwrap();

        assert!(version.ends_with("healthy-version-1.2.3"), "{version}");
    }

    #[test]
    fn output_receive_cannot_wait_past_the_probe_deadline() {
        let (_sender, receiver) = mpsc::channel();
        let start = Instant::now();
        let error =
            receive_output(receiver, "stdout", start + Duration::from_millis(100)).unwrap_err();

        assert!(error.contains("pipe did not close"), "{error}");
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
