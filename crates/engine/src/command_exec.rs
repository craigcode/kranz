//! Bounded, process-tree-killed shell execution for contract and merge-gate
//! commands — extracted from `orchestrator.rs` in the monolith split (pure
//! code motion, no behavior change). These are the ONLY places the engine
//! runs user-authored shell: contract commands need real shell semantics
//! (`sh -c` / `cmd /C`), so argument handling, timeout kill discipline
//! (process group on unix, Job Object on Windows), output tailing, and
//! environment sanitization live here as one unit.

use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Tail kept from a failed contract command's output.
const COMMAND_OUTPUT_TAIL: usize = 1500;

/// Hard cap on one contract `command` assertion at the final gate.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

/// Run `program args` to completion, polling with a bounded wall-clock
/// (`timeout`) rather than blocking forever — the sandbox preflight probe
/// runs operator-authored contract commands and must never hang a mission
/// start. Returns `None` on spawn failure or on timeout (the child is killed).
pub(crate) fn run_with_timeout(
    program: &std::path::Path,
    args: &[String],
    timeout: Duration,
) -> Option<std::process::Output> {
    let mut child = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    }
}

/// Last `max` characters of `text` (for stderr tails in sandbox preflight
/// messages — never splits a code point).
pub(crate) fn last_chars_local(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(max);
    chars[start..].iter().collect()
}

/// Whether `root` looks like a git repository — a `.git` entry exists (a dir
/// for a normal repo, a file for a worktree/submodule gitlink). Best-effort:
/// only a plainly-absent `.git` produces the preflight error.
pub(crate) fn is_git_repo(root: &std::path::Path) -> bool {
    root.join(".git").exists()
}

/// Run one user-authored contract command line at the final gate.
///
/// DELIBERATE shell usage (the one place in the engine): contract commands
/// are user-authored shell lines ("npm test -- --grep auth") that need real
/// shell semantics — argument splitting here would corrupt them. `cmd /C` on
/// Windows, `sh -c` elsewhere; cwd = repo root; 10-minute cap.
pub(crate) async fn run_shell_command(
    cwd: &std::path::Path,
    command: &str,
    env: &HashMap<String, String>,
) -> (bool, String) {
    run_shell_command_with_timeout(cwd, command, COMMAND_TIMEOUT, env).await
}

/// [`run_shell_command`] plus the process exit code: `Some(0)` is success,
/// `Some(n)` a real failure code, and `None` when the command never produced
/// one (spawn failure, the timeout/group-kill path, or signal termination —
/// in those cases the output string says which). The workspace bootstrap/
/// readiness gate names the code in its block reasons so a blocked mission
/// reads "exit code 3", not just "failed".
pub(crate) async fn run_shell_command_with_code(
    cwd: &std::path::Path,
    command: &str,
    env: &HashMap<String, String>,
) -> (Option<i32>, String) {
    run_shell_command_with_timeout_env(cwd, command, COMMAND_TIMEOUT, env, false).await
}

/// [`run_shell_command`] with an explicit timeout (separated so tests can
/// exercise the timeout path without waiting ten minutes).
///
/// Timeout kill semantics: on unix the shell is started as the leader of a
/// new process group and the WHOLE group gets SIGKILL — killing only the
/// wrapper (kill_on_drop) would leave `sleep 300 &`-style descendants running
/// (and holding the output pipes) long after the gate gave up. The killed
/// shell itself is reaped by tokio's background orphan reaper (kill_on_drop);
/// group members are re-parented to init and reaped there.
///
/// Windows has no process groups; the equivalent is a Job Object with
/// `KILL_ON_JOB_CLOSE` (see [`crate::backend_claude::win_job`]). The `cmd /C`
/// wrapper is assigned to such a job right after spawn, so on timeout
/// `TerminateJobObject` takes the whole `cmd` tree down — not just the
/// wrapper. That path compiles and is validated only on windows-latest CI,
/// never on the dev host.
async fn run_shell_command_with_timeout(
    cwd: &std::path::Path,
    command: &str,
    timeout: Duration,
    env: &HashMap<String, String>,
) -> (bool, String) {
    let (code, output) =
        run_shell_command_with_timeout_env(cwd, command, timeout, env, false).await;
    (code == Some(0), output)
}

async fn run_shell_command_with_timeout_env(
    cwd: &std::path::Path,
    command: &str,
    timeout: Duration,
    env: &HashMap<String, String>,
    clear_env: bool,
) -> (Option<i32>, String) {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    };
    if clear_env {
        cmd.env_clear();
    }
    cmd.current_dir(cwd)
        .envs(env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Unix: new process group with the shell as leader, so the timeout path
    // can kill the entire command tree, not just the `sh -c` wrapper.
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return (None, format!("failed to spawn shell: {e}")),
    };
    let stdout = child.stdout.take().expect("stdout was configured as piped");
    let stderr = child.stderr.take().expect("stderr was configured as piped");
    #[cfg(unix)]
    let group_pid = child.id();

    // Windows: assign the `cmd /C` wrapper to a kill-on-close Job Object so the
    // timeout path can kill the whole command tree. Held across the await; on
    // timeout it is killed explicitly and, either way, dropped at scope end
    // (CloseHandle → KILL_ON_JOB_CLOSE). Job setup failure is non-fatal — the
    // command still runs, timeout just falls back to killing the wrapper only.
    // Compiled and validated only on windows-latest CI.
    #[cfg(windows)]
    let job = match child.raw_handle() {
        Some(handle) => crate::backend_claude::win_job::JobHandle::create_and_assign(handle)
            .map_err(|e| {
                tracing::warn!(error = %e, "failed to create Job Object for shell command; \
                    timeout will kill only the cmd wrapper");
            })
            .ok(),
        None => None,
    };

    let execution = async {
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            read_stream_tail(stdout),
            read_stream_tail(stderr)
        );
        Ok::<_, String>((
            status.map_err(|e| format!("failed waiting for shell: {e}"))?,
            stdout.map_err(|e| format!("failed reading shell stdout: {e}"))?,
            stderr.map_err(|e| format!("failed reading shell stderr: {e}"))?,
        ))
    };
    match tokio::time::timeout(timeout, execution).await {
        Err(_elapsed) => {
            // The read futures were dropped with `execution`; SIGKILL the
            // whole group so descendants die too (a still-live member keeps
            // the pgid valid, and the leader zombie pins it until reaped).
            #[cfg(unix)]
            if let Some(pid) = group_pid {
                // Negative pid targets every process in the group.
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            // Windows: TerminateJobObject kills the whole `cmd` tree now
            // (dropping `job` at scope end would also do it via
            // KILL_ON_JOB_CLOSE, but the explicit kill is deterministic).
            #[cfg(windows)]
            if let Some(job) = &job {
                job.kill();
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            (None, format!("timed out after {}s", timeout.as_secs()))
        }
        Ok(Err(error)) => (None, error),
        Ok(Ok((status, stdout, stderr))) => {
            let mut combined = stdout;
            if !stderr.trim().is_empty() {
                combined.push_str("\n--- stderr ---\n");
                combined.push_str(stderr.trim_end());
            }
            // `status.code()` is None on signal termination; the bool shape
            // (`success()`) is recovered by callers as `code == Some(0)`.
            (
                status.code(),
                tail_chars(combined.trim_end(), COMMAND_OUTPUT_TAIL),
            )
        }
    }
}

async fn read_stream_tail<R>(mut reader: R) -> std::io::Result<String>
where
    R: AsyncRead + Unpin,
{
    let max_bytes = COMMAND_OUTPUT_TAIL * 4;
    let mut tail = Vec::with_capacity(max_bytes);
    let mut chunk = [0u8; 8192];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        if read >= max_bytes {
            tail.clear();
            tail.extend_from_slice(&chunk[read - max_bytes..read]);
            continue;
        }
        let excess = tail.len().saturating_add(read).saturating_sub(max_bytes);
        if excess > 0 {
            tail.drain(..excess);
        }
        tail.extend_from_slice(&chunk[..read]);
    }
    Ok(tail_chars(
        &String::from_utf8_lossy(&tail),
        COMMAND_OUTPUT_TAIL,
    ))
}

/// Execute one repository-owned merge gate with the same process-tree timeout
/// used by validation-contract commands, but with a deliberately small
/// inherited environment. This synchronous wrapper is intended for a
/// `spawn_blocking` thread; it owns a current-thread runtime so the robust
/// async timeout/kill implementation remains the single source of truth.
pub fn run_bounded_gate_command(cwd: &std::path::Path, command: &str) -> (bool, String) {
    let env = sanitized_gate_env();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return (false, format!("failed to create gate runtime: {error}")),
    };
    let (code, output) = runtime.block_on(run_shell_command_with_timeout_env(
        cwd,
        command,
        COMMAND_TIMEOUT,
        &env,
        true,
    ));
    (code == Some(0), output)
}

fn sanitized_gate_env() -> HashMap<String, String> {
    // Keep only process/toolchain location and locale values. In particular,
    // API keys, GitHub/Slack tokens, cloud credentials, SSH agent sockets and
    // arbitrary server configuration never cross into mission-authored tests.
    const SAFE: &[&str] = &[
        "PATH",
        "HOME",
        "USERPROFILE",
        "TMPDIR",
        "TMP",
        "TEMP",
        "SYSTEMROOT",
        "SystemRoot",
        "COMSPEC",
        "ComSpec",
        "PATHEXT",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "NPM_CONFIG_CACHE",
        "CI",
        "TERM",
        "LANG",
        "LC_ALL",
        "TZ",
    ];
    SAFE.iter()
        .filter_map(|key| {
            std::env::var_os(key).map(|value| ((*key).to_string(), value.to_string_lossy().into()))
        })
        .collect()
}

/// Last `max` characters of `text` (char-safe).
pub(crate) fn tail_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    text.chars().skip(count - max).collect()
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::runner;

    #[test]
    fn tail_chars_keeps_the_end() {
        assert_eq!(tail_chars("abcdef", 3), "def");
        assert_eq!(tail_chars("ab", 3), "ab");
        assert_eq!(tail_chars("héllo", 2), "lo");
    }

    /// Timeout kill discipline: the whole process GROUP dies, not just the
    /// `sh -c` wrapper — a backgrounded child must not survive the gate
    /// giving up. Unix-only test (`kill(-pgid)`); the Windows equivalent uses
    /// a kill-on-close Job Object (see `run_shell_command_with_timeout`) and
    /// is validated by windows-latest CI, not on this host.
    #[cfg(unix)]
    #[tokio::test]
    async fn shell_command_timeout_kills_the_whole_process_tree() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("child.pid");
        // A background child that would outlive the wrapper by minutes; its
        // pid is written out before the shell parks in `wait`.
        let command = format!("sleep 300 & echo $! > '{}'; wait", pidfile.display());

        let (ok, output) = tokio::time::timeout(
            Duration::from_secs(10),
            run_shell_command_with_timeout(
                dir.path(),
                &command,
                Duration::from_millis(500),
                &std::collections::HashMap::new(),
            ),
        )
        .await
        .expect("timed-out command must return promptly");
        assert!(!ok, "command must be reported failed: {output}");
        assert!(output.contains("timed out"), "got: {output}");

        let pid: i32 = std::fs::read_to_string(&pidfile)
            .expect("shell wrote the background pid before the timeout")
            .trim()
            .parse()
            .expect("pidfile contains a pid");

        // The group SIGKILL must take the background child down: poll until
        // kill(pid, 0) no longer reports it (dead + reaped by init), bounded.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "background child {pid} survived the group kill"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_command_drains_large_output_while_running_and_keeps_only_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let command = "i=0; while [ \"$i\" -lt 20000 ]; do \
                       printf '0123456789abcdef0123456789abcdef\\n'; \
                       i=$((i + 1)); done; printf 'OUTPUT-END'";

        let (ok, output) = run_shell_command_with_timeout(
            dir.path(),
            command,
            Duration::from_secs(10),
            &std::collections::HashMap::new(),
        )
        .await;

        assert!(ok, "large-output command must complete: {output}");
        assert!(output.ends_with("OUTPUT-END"), "{output}");
        assert!(
            output.chars().count() <= COMMAND_OUTPUT_TAIL,
            "retained output exceeded the cap: {} chars",
            output.chars().count()
        );
    }

    #[test]
    fn merge_gate_environment_excludes_server_secrets() {
        let env = sanitized_gate_env();
        for secret in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "SLACK_BOT_TOKEN",
            "GITHUB_TOKEN",
            "GH_TOKEN",
            "SSH_AUTH_SOCK",
            "AWS_SECRET_ACCESS_KEY",
        ] {
            assert!(!env.contains_key(secret), "gate env leaked {secret}");
        }
        assert!(env.keys().all(|key| matches!(
            key.as_str(),
            "PATH"
                | "HOME"
                | "USERPROFILE"
                | "TMPDIR"
                | "TMP"
                | "TEMP"
                | "SYSTEMROOT"
                | "SystemRoot"
                | "COMSPEC"
                | "ComSpec"
                | "PATHEXT"
                | "CARGO_HOME"
                | "RUSTUP_HOME"
                | "NPM_CONFIG_CACHE"
                | "CI"
                | "TERM"
                | "LANG"
                | "LC_ALL"
                | "TZ"
        )));
    }
    /// The final gate's command executor must carry the same
    /// KRANZ_BASE_SHA env that worker/validator sessions get, via the one
    /// shared `runner::contract_env` constructor (mission m-d341a7's false
    /// CRITICAL came from this gate omitting it).
    #[cfg(unix)]
    #[tokio::test]
    async fn base_sha_reaches_final_gate_env() {
        let dir = tempfile::tempdir().unwrap();
        let env = runner::contract_env(Some("deadbeefcafe"));
        let (ok, output) = run_shell_command_with_timeout(
            dir.path(),
            "test \"$KRANZ_BASE_SHA\" = deadbeefcafe",
            Duration::from_secs(10),
            &env,
        )
        .await;
        assert!(ok, "expected command to succeed: {output}");
    }

    /// The exit-code variant surfaces the real failure code (`Some(n)`) and
    /// keeps `Some(0)` as the only success — the workspace gate's block
    /// reasons name it (`exit code 3`), and a nonzero code must never map to
    /// success. Commands stay `sh`/`cmd` portable (`echo`, `exit`).
    #[tokio::test]
    async fn shell_command_with_code_reports_the_real_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let env = std::collections::HashMap::new();

        let (code, output) = run_shell_command_with_code(dir.path(), "echo hi", &env).await;
        assert_eq!(code, Some(0), "{output}");
        assert!(output.contains("hi"), "{output}");

        let (code, output) = run_shell_command_with_code(dir.path(), "exit 3", &env).await;
        assert_eq!(code, Some(3), "{output}");
    }
}
