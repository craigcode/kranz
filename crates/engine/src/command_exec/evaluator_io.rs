//! Structured I/O for an owned control-plane process. In addition to this
//! host group, the caller must clean up the evaluator's container namespace.
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

pub(crate) struct Output {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    program: &Path,
    args: &[String],
    env: &HashMap<String, String>,
    input: &[u8],
    wall: Duration,
    write: Duration,
    stdout_cap: usize,
    stderr_cap: usize,
    cancelled: &AtomicBool,
) -> Result<Output, String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("evaluation cancelled before spawn".into());
    }
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).env_clear();
    let mut cmd = super::configure_bounded_child(cmd, Path::new("/"), env);
    cmd.stdin(std::process::Stdio::piped());
    let mut child = super::ControlChild(
        cmd.spawn()
            .map_err(|e| format!("control-plane spawn failed: {e}"))?,
    );
    let mut stdin = child.0.stdin.take().ok_or("missing control stdin")?;
    let stdout = child.0.stdout.take().ok_or("missing control stdout")?;
    let stderr = child.0.stderr.take().ok_or("missing control stderr")?;
    let io = async {
        let send = async {
            tokio::time::timeout(write, async {
                stdin.write_all(input).await?;
                stdin.shutdown().await
            })
            .await
            .map_err(|_| "stdin write timed out".to_string())?
            .map_err(|e| format!("stdin write failed: {e}"))?;
            drop(stdin);
            Ok::<(), String>(())
        };
        let (_, stdout, stderr) = tokio::try_join!(
            send,
            read_capped(stdout, stdout_cap),
            read_capped(stderr, stderr_cap)
        )?;
        Ok::<_, String>((stdout, stderr))
    };
    tokio::pin!(io);
    let leader = super::control_leader_exited(child.0.id().ok_or("unowned control process")?);
    tokio::pin!(leader);
    let cancellation = async {
        while !cancelled.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let mut captured = None;
    let completion = tokio::time::timeout(wall, async {
        tokio::pin!(cancellation);
        loop {
            tokio::select! {
                result = &mut leader => return result.map_err(|e| format!("control wait failed: {e}")),
                () = &mut cancellation => return Err("evaluation cancelled".into()),
                result = &mut io, if captured.is_none() => captured = Some(result?),
            }
        }
    }).await.map_err(|_| "evaluation process timed out".to_string()).and_then(|r| r);
    crate::backend_claude::kill_unreaped_group(&child.0);
    if completion.is_err() {
        let _ = child.0.start_kill();
    }
    let status = tokio::time::timeout(Duration::from_secs(2), child.0.wait())
        .await
        .map_err(|_| "control reap timed out")?
        .map_err(|e| e.to_string())?;
    completion?;
    let (stdout, stderr) = match captured {
        Some(output) => output,
        None => tokio::time::timeout(Duration::from_secs(1), &mut io)
            .await
            .map_err(|_| "control pipes remained open after exit")??,
    };
    Ok(Output {
        code: status.code(),
        stdout,
        stderr,
    })
}

async fn read_capped(mut reader: impl AsyncRead + Unpin, cap: usize) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut buffer)
            .await
            .map_err(|e| format!("stream read failed: {e}"))?;
        if n == 0 {
            return Ok(bytes);
        }
        if n > cap.saturating_sub(bytes.len()) {
            return Err("evaluator stream byte limit exceeded".into());
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn gate_subprocess_v1_io_bounds_blocked_stdin_and_parallel_streams() {
        let env = HashMap::from([("PATH".into(), "/usr/bin:/bin".into())]);
        let cancel = AtomicBool::new(false);
        let result = run(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 60".into()],
            &env,
            &vec![b'x'; 1_048_576],
            Duration::from_secs(2),
            Duration::from_millis(50),
            32,
            32,
            &cancel,
        )
        .await;
        assert!(result.err().unwrap().contains("stdin write timed out"));
        for script in ["printf '%100s' x", "printf '%100s' x >&2"] {
            let result = run(
                Path::new("/bin/sh"),
                &["-c".into(), script.into()],
                &env,
                b"",
                Duration::from_secs(2),
                Duration::from_secs(1),
                32,
                32,
                &cancel,
            )
            .await;
            assert!(result.err().unwrap().contains("byte limit"));
        }
    }
}
