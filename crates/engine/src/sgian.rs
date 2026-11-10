//! Per-run Sgian client credential (optional coordination lane).
//!
//! When the operator runs a mission inside a repository that a Sgian daemon
//! also serves, an explicitly opted-in, uncontained worker identifies itself to
//! that daemon as its own principal: the engine asks the daemon for a
//! credential held by `kranz:<run-id>` with the `write` scope, hands the
//! token to the worker through `SGIAN_CLIENT_TOKEN`, and revokes the
//! credential when the run ends. Sgian then attributes every pane the worker
//! drives, every lease it takes and every ledger record it produces to the
//! run rather than to the operator. Revocation is best-effort, including on
//! future cancellation; engine death still needs operator reconciliation.
//!
//! The lane is best-effort and never a reason to fail a spawn:
//! - `KRANZ_SGIAN_BIN` must name an absolute helper outside the repository.
//!   Unset, empty and relative values disable the lane; PATH is never searched.
//! - Enforced sandbox sessions never receive this host-control capability.
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
use std::process::Command;
use std::time::Duration;

/// Environment variable the worker reads (Sgian's own client convention).
pub const TOKEN_ENV: &str = "SGIAN_CLIENT_TOKEN";
/// Operator override for the `sgian` binary; empty disables the lane.
pub const BIN_ENV: &str = "KRANZ_SGIAN_BIN";
/// Sibling of `.kranz/serve.read.token` (`0600`) shared with Sgian for HMAC-SHA256 execution grants.
pub const GRANT_KEY_DIR: &str = ".kranz";
pub const GRANT_KEY_FILE: &str = "serve.grant.key";
pub const GRANT_KEY_MIN_BYTES: usize = 16;
pub const GRANT_KEY_MAX_BYTES: usize = 4096;
/// Upper bound on one `sgian ctl` call. The daemon answers on a local socket
/// in milliseconds; anything longer is a wedged client and the run must not
/// wait on it.
pub const DEADLINE: Duration = Duration::from_secs(5);

pub use crate::sgian_terminal::{
    Authority as SgianAuthority, Budgets as SgianTerminalBudgets, Config as SgianTerminalConfig,
    ExecutionGrant, Provider as SgianTerminalProvider, GRANT_ENV_ALLOW,
};

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

/// Revokes an issued worker credential even if its running future is dropped.
pub(crate) struct RevocationGuard(Option<SgianCredential>);

impl RevocationGuard {
    pub(crate) async fn close(mut self) {
        if let Some(credential) = self.0.take() {
            if let Err(error) = tokio::task::spawn_blocking(move || credential.revoke()).await {
                tracing::warn!(%error, "sgian revocation task failed; operator reconciliation required");
            }
        }
    }
}

impl Drop for RevocationGuard {
    fn drop(&mut self) {
        if let Some(credential) = self.0.take() {
            // Cancellation is best-effort but must not block an async executor.
            // The helper itself retains its deadline and process-tree cleanup.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(move || credential.revoke());
            } else if let Err(error) = std::thread::Builder::new()
                .name("kranz-sgian-revoke".into())
                .spawn(move || credential.revoke())
            {
                tracing::warn!(%error, "sgian revocation could not start; operator reconciliation required");
            }
        }
    }
}

/// Cancellation during issuance drops the returned guard on the blocking pool,
/// so even a credential minted after the caller stops gets a revocation attempt.
pub(crate) async fn issue_worker(
    workspace: &Path,
    session_cwd: &Path,
    run_id: &str,
    enforce: crate::types::SandboxEnforce,
) -> Option<(RevocationGuard, String)> {
    if enforce != crate::types::SandboxEnforce::Off {
        return None;
    }
    let bin = trusted_bin(std::env::var_os(BIN_ENV), workspace)?;
    if bin.starts_with(session_cwd.canonicalize().ok()?) {
        return None;
    }
    let workspace = workspace.to_path_buf();
    let run_id = run_id.to_string();
    tokio::task::spawn_blocking(move || {
        issue_with(&bin, &workspace, &run_id, DEADLINE)
            .map(|(credential, token)| (RevocationGuard(Some(credential)), token))
    })
    .await
    .ok()
    .flatten()
}

/// The holder name Sgian records for a run.
pub fn holder_for(run_id: &str) -> String {
    format!("kranz:{run_id}")
}

/// Issue a `write` credential for `run_id` against the daemon serving
/// `workspace`, returning the credential and its one-time token, or `None`
/// when the lane is disabled or unavailable.
pub fn issue(workspace: &Path, run_id: &str) -> Option<(SgianCredential, String)> {
    let bin = trusted_bin(std::env::var_os(BIN_ENV), workspace)?;
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
    let issued = record.get("token").and_then(serde_json::Value::as_str);
    let (Some(id), Some(token)) = (id, issued) else {
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
        id,
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
    /// Attempt revocation. Failure is logged; daemon state must be reconciled
    /// by the operator if cleanup could not be confirmed.
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
                id = %self.id,
                holder = %self.holder,
                "sgian credential revoked"
            ),
            Err(reason) => tracing::warn!(
                id = %self.id,
                holder = %self.holder,
                reason,
                "sgian credential could not be revoked; revoke it by hand with \
                 `sgian ctl identity revoke` if the daemon is still running"
            ),
        }
    }
}

/// Only an explicit absolute helper opts in. The legacy PATH argument is
/// retained for API compatibility and is deliberately ignored.
pub fn resolve_bin(override_var: Option<OsString>, _path_var: Option<OsString>) -> Option<PathBuf> {
    let path = PathBuf::from(override_var?);
    path.is_absolute().then_some(path)
}

pub(crate) fn trusted_bin(override_var: Option<OsString>, workspace: &Path) -> Option<PathBuf> {
    let bin = resolve_bin(override_var, None)?.canonicalize().ok()?;
    let workspace = workspace.canonicalize().ok()?;
    // Resolve symlinks before checking ownership: a repository-provided helper
    // must never run with the operator's host authority.
    (bin.is_file() && !bin.starts_with(workspace)).then_some(bin)
}

#[derive(Debug)]
pub(crate) enum CtlError {
    TimedOut(String),
    Io(String),
    Exited {
        status: std::process::ExitStatus,
        stdout: Vec<u8>,
    },
}

impl std::fmt::Display for CtlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut(msg) | Self::Io(msg) => f.write_str(msg),
            Self::Exited { status, .. } => write!(f, "exited with {status}"),
        }
    }
}

fn ctl_extra_env() -> Vec<(String, String)> {
    let mut extra = Vec::new();
    for key in ["XDG_DATA_HOME", "XDG_RUNTIME_DIR"] {
        if let Some(val) = std::env::var_os(key) {
            extra.push((key.to_string(), val.to_string_lossy().into_owned()));
        }
    }
    extra
}

pub(crate) fn run_ctl_detailed(
    bin: &Path,
    args: &[&str],
    limits: crate::git_ops::process::Limits,
) -> Result<Vec<u8>, CtlError> {
    let mut command = Command::new(bin);
    let extra = ctl_extra_env();
    command
        .args(args)
        .env_clear()
        .envs(crate::agent_env::probe_child_env(&extra));
    let output = crate::git_ops::process::output(command, limits).map_err(|error| {
        let msg = format!("{} control call failed: {error}", bin.display());
        if error.kind() == std::io::ErrorKind::TimedOut {
            CtlError::TimedOut(msg)
        } else {
            CtlError::Io(msg)
        }
    })?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(CtlError::Exited {
            status: output.status,
            stdout: output.stdout,
        })
    }
}

/// Run the operator helper with bounded pipes and process-tree cleanup. The
/// discovery allowlist keeps HOME for daemon lookup, without forwarding provider
/// credentials or a worker's client token. Never log the helper's reply bytes.
fn run_ctl(bin: &Path, args: &[&str], deadline: Duration) -> Result<Vec<u8>, String> {
    run_ctl_detailed(
        bin,
        args,
        crate::git_ops::process::Limits::for_control(deadline),
    )
    .map_err(|error| format!("{} {error}", bin.display()))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Mint a fresh 256-bit execution-grant key (`sgk_<64 lowercase hex digits>`).
pub fn generate_grant_key() -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"kranz.sgian.grant-key.v1\0");
    hasher.update(uuid::Uuid::new_v4().as_bytes());
    hasher.update(uuid::Uuid::new_v4().as_bytes());
    hasher.update(uuid::Uuid::new_v4().as_bytes());
    format!("sgk_{}", hex_encode(&hasher.finalize()))
}

/// Atomically write a fresh `0600` `<repo>/.kranz/serve.grant.key`, refusing
/// symlinked `.kranz` directories and replacing any existing inode via rename.
pub fn rotate_grant_key(repo_root: &Path) -> std::io::Result<PathBuf> {
    write_grant_key(repo_root, &generate_grant_key())
}

/// Write an explicit grant key to `<repo>/.kranz/serve.grant.key` (`0600`).
pub fn write_grant_key(repo_root: &Path, key_text: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let trimmed = key_text.trim();
    if trimmed.len() < GRANT_KEY_MIN_BYTES || trimmed.len() > GRANT_KEY_MAX_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "grant key length is outside [16, 4096] bytes",
        ));
    }
    let kranz_dir = repo_root.join(GRANT_KEY_DIR);
    match std::fs::symlink_metadata(&kranz_dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    ".kranz must be a real directory (not a symlink)",
                ));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(&kranz_dir)?;
        }
        Err(err) => return Err(err),
    }

    let dest = kranz_dir.join(GRANT_KEY_FILE);
    let tmp = kranz_dir.join(format!(
        ".{GRANT_KEY_FILE}.tmp-{}",
        uuid::Uuid::new_v4().as_simple()
    ));
    let write_tmp = || -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(trimmed.as_bytes())?;
        file.flush()?;
        Ok(())
    };
    if let Err(err) = write_tmp() {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(err);
    }
    Ok(dest)
}

/// Read and validate `<repo_canon>/.kranz/serve.grant.key`: refuses symlinks on
/// `.kranz` and the key file, enforces `0600` owner-only permissions on Unix,
/// and validates `[16, 4096]` trimmed key bytes (matching Sgian's verifier).
pub fn load_grant_key(repo_canon: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let kranz_dir = repo_canon.join(GRANT_KEY_DIR);
    let dir_meta = std::fs::symlink_metadata(&kranz_dir)
        .map_err(|error| format!("grant key unavailable ({}): {error}", kranz_dir.display()))?;
    if dir_meta.file_type().is_symlink() || !dir_meta.is_dir() {
        return Err(format!(
            "grant key directory {} must be a real directory (not a symlink)",
            kranz_dir.display()
        ));
    }
    let key_path = kranz_dir.join(GRANT_KEY_FILE);
    let meta = std::fs::symlink_metadata(&key_path)
        .map_err(|error| format!("grant key unavailable ({}): {error}", key_path.display()))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(format!(
            "grant key {} must be a regular file (not a symlink)",
            key_path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!(
                "grant key {} has insecure permissions {:04o}; expected owner-only (0600)",
                key_path.display(),
                mode
            ));
        }
    }
    if meta.len() > GRANT_KEY_MAX_BYTES as u64 {
        return Err(format!(
            "grant key {} exceeds {} bytes",
            key_path.display(),
            GRANT_KEY_MAX_BYTES
        ));
    }
    let mut raw = Vec::new();
    std::fs::File::open(&key_path)
        .and_then(|file| {
            file.take(GRANT_KEY_MAX_BYTES as u64 + 1)
                .read_to_end(&mut raw)
        })
        .map_err(|error| format!("failed to read grant key {}: {error}", key_path.display()))?;
    if raw.len() > GRANT_KEY_MAX_BYTES {
        return Err(format!(
            "grant key {} exceeds {} bytes",
            key_path.display(),
            GRANT_KEY_MAX_BYTES
        ));
    }
    let start = raw
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(raw.len());
    let end = raw
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map(|idx| idx + 1)
        .unwrap_or(start);
    let trimmed = &raw[start..end];
    if trimmed.len() < GRANT_KEY_MIN_BYTES {
        return Err(format!(
            "grant key {} is shorter than {} bytes",
            key_path.display(),
            GRANT_KEY_MIN_BYTES
        ));
    }
    Ok(trimmed.to_vec())
}

/// Load `<repo_canon>/.kranz/serve.grant.key` if present (failing closed on
/// symlinks or insecure permissions), or provision a fresh `0600` key when absent.
pub fn ensure_grant_key(repo_canon: &Path) -> Result<Vec<u8>, String> {
    let key_path = repo_canon.join(GRANT_KEY_DIR).join(GRANT_KEY_FILE);
    match std::fs::symlink_metadata(&key_path) {
        Ok(_) => load_grant_key(repo_canon),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            rotate_grant_key(repo_canon)
                .map_err(|e| format!("failed to provision grant key: {e}"))?;
            load_grant_key(repo_canon)
        }
        Err(err) => Err(format!(
            "grant key unavailable ({}): {err}",
            key_path.display()
        )),
    }
}

/// Canonical JSON serialization (recursively sorted object keys, no whitespace),
/// matching Sgian's `canonical_json` byte-for-byte.
pub fn canonical_json(value: &serde_json::Value) -> String {
    fn sort_value(v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Object(map) => {
                let mut sorted = serde_json::Map::new();
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for k in keys {
                    sorted.insert(k.clone(), sort_value(&map[k]));
                }
                serde_json::Value::Object(sorted)
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(sort_value).collect())
            }
            other => other.clone(),
        }
    }
    sort_value(value).to_string()
}

/// Pure-Rust RFC 2104 HMAC-SHA256 encoded as 64 lowercase hex digits.
pub fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    const BLOCK_LEN: usize = 64;
    let mut key_block = [0u8; BLOCK_LEN];
    if key.len() > BLOCK_LEN {
        let digest = Sha256::digest(key);
        key_block[..digest.len()].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0x36u8; BLOCK_LEN];
    let mut opad = [0x5cu8; BLOCK_LEN];
    for i in 0..BLOCK_LEN {
        ipad[i] ^= key_block[i];
        opad[i] ^= key_block[i];
    }

    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_digest);
    hex_encode(&outer.finalize())
}

/// Apply an issued credential to a session env; a convenience for callers
/// that already hold the token.
pub fn seed_env(env: &mut HashMap<String, String>, token: String) {
    env.insert(TOKEN_ENV.to_string(), token);
}

/// Projects a read-only Sgian pane observation over a contained ACP terminal
/// handle without transferring execution or consent authority.
///
/// The returned [`kranz_acp::terminal::PaneObservation`] is verified against
/// `expected_binding` (`run_id`, `engine_session_id`, `peer_session_id`,
/// `generation`, `terminal_id`, `action_digest`). Any generation mismatch,
/// daemon loss, or unconfirmed cleanup fails closed.
pub fn project_terminal_pane(
    expected_binding: &kranz_acp::terminal::PaneBinding,
    observed_binding: kranz_acp::terminal::PaneBinding,
    snapshot: kranz_acp::terminal::OutputSnapshot,
    cleanup: Option<kranz_acp::terminal::CleanupReceipt>,
    daemon_alive: bool,
    cleanup_confirmed: bool,
) -> Result<kranz_acp::terminal::PaneObservation, kranz_acp::terminal::Error> {
    use kranz_acp::terminal::{PaneLifecycleState, PaneObservation};
    let state = if !cleanup_confirmed {
        PaneLifecycleState::CleanupUnconfirmed
    } else if !daemon_alive {
        PaneLifecycleState::DaemonLost
    } else if cleanup.is_some() {
        PaneLifecycleState::Released
    } else if snapshot.exit_status.is_some() {
        PaneLifecycleState::Exited
    } else {
        PaneLifecycleState::Active
    };
    let observation = PaneObservation {
        binding: observed_binding,
        snapshot,
        state,
        cleanup,
    };
    observation.verify_against(expected_binding)?;
    Ok(observation)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

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
            r#"case "$*" in *"identity issue"*) printf '{"id":"cred-7","holder":"kranz:run-1","scopes":["write"],"token":"sgc_t"}\n';; *) printf '{"id":"cred-7","revoked":true}\n';; esac"#,
        );
        // The fixture value stays under eight characters so the repository's own
        // secret scanner, which runs with the base branch's allowlist, does not
        // read it as a credential assignment.
        let (cred, token) = issue_with(&bin, &ws, "run-1", DEADLINE).expect("credential issued");
        assert_eq!(cred.id, "cred-7");
        assert_eq!(cred.holder, "kranz:run-1");
        assert_eq!(token, "sgc_t");
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
    fn sgian_control_deadline_covers_pipes_after_leader_exit() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("descendant-ran");
        let bin = fake_sgian(
            dir.path(),
            &format!("(sleep 1; touch '{}') & exit 0", marker.display()),
        );
        // Prove the same descendant would act without cancellation.
        assert!(run_ctl(&bin, &[], Duration::from_secs(3)).is_ok());
        assert!(marker.exists());
        std::fs::remove_file(&marker).unwrap();
        let started = Instant::now();
        assert!(run_ctl(&bin, &[], Duration::from_millis(100)).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(1200));
        assert!(!marker.exists(), "timed-out descendants must not continue");
    }

    #[test]
    fn sgian_control_refuses_oversize_output_and_does_not_log_reply_bytes() {
        let dir = tempfile::tempdir().unwrap();
        for stream in ["", " >&2"] {
            let bin = fake_sgian(dir.path(), &format!("head -c 65537 /dev/zero{stream}"));
            assert!(run_ctl(&bin, &[], DEADLINE).is_err());
        }
        let bin = fake_sgian(dir.path(), "echo 'private-reply' >&2; exit 1");
        let error = run_ctl(&bin, &[], DEADLINE).unwrap_err();
        assert!(!error.contains("private-reply"));
    }

    #[test]
    fn sgian_control_keeps_discovery_environment_without_ambient_secrets() {
        let name =
            "sgian::tests::sgian_control_keeps_discovery_environment_without_ambient_secrets";
        if crate::agent_env::isolated_global_home_test(name) {
            return;
        }
        std::env::set_var("KRANZ_SGIAN_PRIVATE_FIXTURE", "private-value");
        std::env::set_var(TOKEN_ENV, "private-value");
        let dir = tempfile::tempdir().unwrap();
        let bin = fake_sgian(dir.path(), "env");
        let output = run_ctl(&bin, &[], DEADLINE).unwrap();
        let env = String::from_utf8(output).unwrap();
        assert!(env.lines().any(|line| line.starts_with("HOME=")));
        assert!(env.lines().any(|line| line.starts_with("PATH=")));
        assert!(!env.contains("KRANZ_SGIAN_PRIVATE_FIXTURE="));
        assert!(!env.contains("SGIAN_CLIENT_TOKEN="));
    }

    #[test]
    fn issue_is_none_when_the_binary_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(issue_with(&dir.path().join("absent"), dir.path(), "run-4", DEADLINE).is_none());
    }

    #[test]
    fn resolve_bin_requires_explicit_absolute_opt_in() {
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
        assert_eq!(resolve_bin(None, Some(path)), None);
        assert_eq!(resolve_bin(Some(OsString::from("./sgian")), None), None);
        assert_eq!(trusted_bin(Some(bin.clone().into()), dir.path()), None);
        let workspace = tempfile::tempdir().unwrap();
        assert_eq!(
            trusted_bin(Some(bin.clone().into()), workspace.path()),
            Some(bin.canonicalize().unwrap())
        );
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

    #[test]
    fn sgian_pane_projection_refuses_generation_drift_daemon_loss_and_unconfirmed_cleanup() {
        use kranz_acp::terminal::{
            CleanupReceipt, Error, ExitStatus, OutputSnapshot, PaneBinding, PaneLifecycleState,
            Scope,
        };
        let binding = PaneBinding {
            scope: Scope {
                run_id: "run-sgian".into(),
                engine_session_id: "engine-sgian".into(),
                peer_session_id: "peer-sgian".into(),
                generation: "gen-live".into(),
            },
            terminal_id: "terminal-7".into(),
            action_digest: "sha256:action".into(),
        };
        let active_snap = OutputSnapshot {
            output: "compiling...\n".into(),
            truncated: false,
            exit_status: None,
        };
        let active = project_terminal_pane(
            &binding,
            binding.clone(),
            active_snap.clone(),
            None,
            true,
            true,
        )
        .unwrap();
        assert_eq!(active.state, PaneLifecycleState::Active);

        // Stale generation after daemon/provider restart is refused.
        let mut stale = binding.clone();
        stale.scope.generation = "gen-previous".into();
        assert_eq!(
            project_terminal_pane(&binding, stale, active_snap.clone(), None, true, true),
            Err(Error::InvalidHandle)
        );

        // Daemon death surfaces Unavailable; unconfirmed cleanup surfaces CleanupUnconfirmed.
        assert_eq!(
            project_terminal_pane(
                &binding,
                binding.clone(),
                active_snap.clone(),
                None,
                false,
                true
            ),
            Err(Error::Unavailable)
        );
        assert_eq!(
            project_terminal_pane(&binding, binding.clone(), active_snap, None, true, false),
            Err(Error::CleanupUnconfirmed)
        );

        // Clean release requires matching exitStatus and confirmed descendant/drain flags.
        let exit = ExitStatus {
            exit_code: Some(0),
            signal: None,
        };
        let released = project_terminal_pane(
            &binding,
            binding.clone(),
            OutputSnapshot {
                output: "done\n".into(),
                truncated: false,
                exit_status: Some(exit.clone()),
            },
            Some(CleanupReceipt {
                terminal_id: "terminal-7".into(),
                descendants_reaped: true,
                output_drained: true,
                released: true,
                exit_status: exit,
            }),
            true,
            true,
        )
        .unwrap();
        assert_eq!(released.state, PaneLifecycleState::Released);
    }
}
