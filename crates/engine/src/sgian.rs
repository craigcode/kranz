//! Per-run Sgian client credential (optional coordination lane).
//!
//! When the operator runs a mission inside a repository that a Sgian daemon
//! also serves, every worker session the engine spawns identifies itself to
//! that daemon as its own principal: the engine asks the daemon for a
//! credential held by `kranz:<run-id>` with the `write` scope, hands the
//! token to the worker through `SGIAN_CLIENT_TOKEN`, and revokes the
//! credential when the run ends. Sgian then attributes every pane the worker
//! drives, every lease it takes and every ledger record it produces to the
//! run rather than to the operator, and the token stops working the moment
//! the run is over.
//!
//! The lane is best-effort and never a reason to fail a spawn:
//! - `KRANZ_SGIAN_BIN` set to a path uses that `sgian` binary; set but empty
//!   disables the lane; unset searches `PATH` for `sgian`.
//! - No binary, no daemon serving the repository root, a refused request, a
//!   malformed reply or a call that outlasts [`DEADLINE`] all degrade to a
//!   session without the variable, logged at `debug` (absent daemon is the
//!   common case) or `warn` (the daemon answered but the reply was unusable).
//!
//! Only the credential id and holder are logged; the token crosses into the
//! session env and nowhere else.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Environment variable the worker reads (Sgian's own client convention).
pub const TOKEN_ENV: &str = "SGIAN_CLIENT_TOKEN";
/// Operator override for the `sgian` binary; empty disables the lane.
pub const BIN_ENV: &str = "KRANZ_SGIAN_BIN";
/// Upper bound on one `sgian ctl` call. The daemon answers on a local socket
/// in milliseconds; anything longer is a wedged client and the run must not
/// wait on it.
pub const DEADLINE: Duration = Duration::from_secs(5);

/// A credential the engine issued for one run and owes a revocation for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SgianCredential {
    /// Daemon-assigned credential id (`identity revoke` takes this).
    pub id: String,
    /// `kranz:<run-id>`.
    pub holder: String,
    bin: PathBuf,
    workspace: PathBuf,
}

/// The holder name Sgian records for a run.
pub fn holder_for(run_id: &str) -> String {
    format!("kranz:{run_id}")
}

/// Issue a `write` credential for `run_id` against the daemon serving
/// `workspace`, returning the credential and its one-time token, or `None`
/// when the lane is disabled or unavailable.
pub fn issue(workspace: &Path, run_id: &str) -> Option<(SgianCredential, String)> {
    let bin = resolve_bin(std::env::var_os(BIN_ENV), std::env::var_os("PATH"))?;
    issue_with(&bin, workspace, run_id, DEADLINE)
}

/// [`issue`] against an explicit binary and deadline (the testable core).
pub fn issue_with(
    bin: &Path,
    workspace: &Path,
    run_id: &str,
    deadline: Duration,
) -> Option<(SgianCredential, String)> {
    let holder = holder_for(run_id);
    let args = [
        "ctl",
        "--workspace",
        &workspace.display().to_string(),
        "--json",
        "identity",
        "issue",
        "--holder",
        &holder,
        "--scope",
        "write",
    ];
    let output = match run_ctl(bin, &args, deadline) {
        Ok(output) => output,
        Err(reason) => {
            tracing::debug!(
                run_id,
                workspace = %workspace.display(),
                reason,
                "sgian credential not issued; the session runs without one"
            );
            return None;
        }
    };
    let record: serde_json::Value = match serde_json::from_slice(&output) {
        Ok(record) => record,
        Err(error) => {
            tracing::warn!(
                run_id,
                error = %error,
                "sgian identity issue replied with something other than a credential record"
            );
            return None;
        }
    };
    let id = record.get("id").and_then(serde_json::Value::as_str);
    let token = record.get("token").and_then(serde_json::Value::as_str);
    let (Some(id), Some(token)) = (id, token) else {
        tracing::warn!(
            run_id,
            "sgian identity issue record lacks an id or token; ignoring it"
        );
        return None;
    };
    if id.is_empty() || token.is_empty() {
        return None;
    }
    tracing::info!(
        run_id,
        credential = id,
        holder = %holder,
        "sgian credential issued for the run (revoked when the run ends)"
    );
    Some((
        SgianCredential {
            id: id.to_string(),
            holder,
            bin: bin.to_path_buf(),
            workspace: workspace.to_path_buf(),
        },
        token.to_string(),
    ))
}

impl SgianCredential {
    /// Revoke the credential. Failure is logged and otherwise ignored: the
    /// daemon may have gone away, in which case nothing holds the token.
    pub fn revoke(&self) {
        self.revoke_with(DEADLINE);
    }

    /// [`revoke`](Self::revoke) with an explicit deadline.
    pub fn revoke_with(&self, deadline: Duration) {
        let args = [
            "ctl",
            "--workspace",
            &self.workspace.display().to_string(),
            "--json",
            "identity",
            "revoke",
            &self.id,
        ];
        match run_ctl(&self.bin, &args, deadline) {
            Ok(_) => tracing::info!(
                credential = %self.id,
                holder = %self.holder,
                "sgian credential revoked"
            ),
            Err(reason) => tracing::warn!(
                credential = %self.id,
                holder = %self.holder,
                reason,
                "sgian credential could not be revoked; revoke it by hand with \
                 `sgian ctl identity revoke` if the daemon is still running"
            ),
        }
    }
}

/// Pick the `sgian` binary: the override wins, an empty override disables,
/// otherwise the first `sgian` on `PATH`.
pub fn resolve_bin(override_var: Option<OsString>, path_var: Option<OsString>) -> Option<PathBuf> {
    match override_var {
        Some(value) if value.is_empty() => None,
        Some(value) => Some(PathBuf::from(value)),
        None => std::env::split_paths(&path_var?).find_map(|dir| {
            let candidate = dir.join(BIN_NAME);
            candidate.is_file().then_some(candidate)
        }),
    }
}

#[cfg(windows)]
const BIN_NAME: &str = "sgian.exe";
#[cfg(not(windows))]
const BIN_NAME: &str = "sgian";

/// Run `bin args…`, returning stdout on exit 0, within `deadline`. The child
/// inherits the ambient environment (the daemon's socket lives under the
/// operator's home) minus any client token of the engine's own, so the call
/// is made as the workspace owner and not under a credential that may lack
/// the `admin` scope.
fn run_ctl(bin: &Path, args: &[&str], deadline: Duration) -> Result<Vec<u8>, String> {
    let mut child = Command::new(bin)
        .args(args)
        .env_remove(TOKEN_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{} did not start: {error}", bin.display()))?;
    let started = Instant::now();
    // Drain the pipes on a helper thread so a chatty child cannot block on a
    // full pipe while we poll for exit.
    let mut stdout = child.stdout.take().expect("stdout piped");
    let mut stderr = child.stderr.take().expect("stderr piped");
    let drain = std::thread::spawn(move || {
        use std::io::Read;
        let err = std::thread::spawn(move || {
            let mut err = Vec::new();
            let _ = stderr.read_to_end(&mut err);
            err
        });
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out);
        (out, err.join().unwrap_or_default())
    });
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let (out, err) = drain.join().unwrap_or_default();
                if status.success() {
                    return Ok(out);
                }
                let message = String::from_utf8_lossy(&err).trim().to_string();
                return Err(format!(
                    "{} exited with {status}: {}",
                    bin.display(),
                    first_line(&message)
                ));
            }
            Ok(None) if started.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = drain.join();
                return Err(format!(
                    "{} did not answer within {deadline:?}",
                    bin.display()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(format!("waiting for {}: {error}", bin.display())),
        }
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// Apply an issued credential to a session env; a convenience for callers
/// that already hold the token.
pub fn seed_env(env: &mut HashMap<String, String>, token: String) {
    env.insert(TOKEN_ENV.to_string(), token);
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A fake `sgian` that appends its argv to `<dir>/calls`, then behaves
    /// per `body` (a shell snippet run with the args still in `$@`).
    fn fake_sgian(dir: &Path, body: &str) -> PathBuf {
        let bin = dir.join("sgian");
        let calls = dir.join("calls");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n{body}\n",
                calls.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    fn calls(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn issue_parses_the_record_and_revoke_uses_its_id() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().join("repo");
        std::fs::create_dir_all(&ws).unwrap();
        let bin = fake_sgian(
            dir.path(),
            r#"case "$*" in *"identity issue"*) printf '{"id":"cred-7","holder":"kranz:run-1","scopes":["write"],"token":"sgc_test_token"}\n';; *) printf '{"id":"cred-7","revoked":true}\n';; esac"#,
        );
        let (cred, token) = issue_with(&bin, &ws, "run-1", DEADLINE).expect("credential issued");
        assert_eq!(cred.id, "cred-7");
        assert_eq!(cred.holder, "kranz:run-1");
        assert_eq!(token, "sgc_test_token");
        cred.revoke();
        let calls = calls(dir.path());
        assert_eq!(
            calls,
            vec![
                format!(
                    "ctl --workspace {} --json identity issue --holder kranz:run-1 --scope write",
                    ws.display()
                ),
                format!(
                    "ctl --workspace {} --json identity revoke cred-7",
                    ws.display()
                ),
            ]
        );
    }

    #[test]
    fn issue_is_none_when_ctl_fails_or_answers_nonsense() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        let failing = fake_sgian(
            dir.path(),
            "echo 'no daemon serves this workspace' >&2; exit 1",
        );
        assert!(issue_with(&failing, &ws, "run-2", DEADLINE).is_none());
        let garbage = fake_sgian(dir.path(), "echo 'not json'");
        assert!(issue_with(&garbage, &ws, "run-2", DEADLINE).is_none());
        let partial = fake_sgian(dir.path(), r#"printf '{"id":"x"}\n'"#);
        assert!(issue_with(&partial, &ws, "run-2", DEADLINE).is_none());
    }

    #[test]
    fn issue_gives_up_on_a_wedged_client() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_sgian(dir.path(), "sleep 5");
        let started = Instant::now();
        assert!(issue_with(&bin, dir.path(), "run-3", Duration::from_millis(200)).is_none());
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the deadline must cut the wait short"
        );
    }

    #[test]
    fn issue_is_none_when_the_binary_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(issue_with(&dir.path().join("absent"), dir.path(), "run-4", DEADLINE).is_none());
    }

    #[test]
    fn resolve_bin_honours_the_override_and_searches_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_sgian(dir.path(), "true");
        assert_eq!(
            resolve_bin(Some(OsString::from("/opt/sgian")), None),
            Some(PathBuf::from("/opt/sgian"))
        );
        assert_eq!(
            resolve_bin(Some(OsString::new()), Some(dir.path().into())),
            None
        );
        let path =
            std::env::join_paths([dir.path().join("nowhere"), dir.path().to_path_buf()]).unwrap();
        assert_eq!(resolve_bin(None, Some(path)), Some(bin));
        assert_eq!(
            resolve_bin(None, Some(OsString::from(dir.path().join("nowhere")))),
            None
        );
        assert_eq!(resolve_bin(None, None), None);
    }

    #[test]
    fn holder_names_the_run() {
        assert_eq!(holder_for("abc"), "kranz:abc");
    }
}
