//! Environment preflight (roadmap M2) — extracted from `orchestrator.rs` in
//! the monolith split (pure code motion, no behavior change). Advisory only:
//! preflight never blocks a mission (the final contract gate stays
//! authoritative); it surfaces missing prerequisites as a single
//! `orchestrator.decision` at run start, and [`PREFLIGHT_CLEAR_SUMMARY`]
//! durably supersedes an earlier warning once a run's probes come back clean.

use crate::command_exec::{is_git_repo, last_chars_local, run_with_timeout};
use crate::contract_sweep;
use crate::orchestrator::MissionEngine;
use crate::paths::MissionPaths;
use crate::types::*;
use std::net::ToSocketAddrs;
use std::time::Duration;

/// One environment-preflight issue surfaced at run start (roadmap M2).
///
/// Preflight is advisory only: it never blocks a mission (the final contract
/// gate stays authoritative). `severity` is `"warn"` for a probably-missing
/// prerequisite and `"error"` for a hard environment defect (not a git repo,
/// `.kranz` not writable) that will almost certainly break the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightIssue {
    /// `"warn"` | `"error"`.
    pub severity: &'static str,
    pub message: String,
}

/// Durable marker emitted when a run's environment preflight is clean. A
/// clean event is necessary to supersede an issue recorded by an earlier run.
pub const PREFLIGHT_CLEAR_SUMMARY: &str = "preflight: clear — no advisory issues recorded";

impl MissionEngine {
    // -----------------------------------------------------------------------
    // Environment preflight (roadmap M2)
    // -----------------------------------------------------------------------

    /// Best-effort check of obvious prerequisites of the validation contract's
    /// `command` assertions, run once at the start of [`Self::run`] before the
    /// first worker spawns (roadmap M2). Advisory only: the returned issues are
    /// surfaced as a single `orchestrator.decision`, never as a block — the
    /// contract gate at mission completion is still the authoritative check.
    ///
    /// For each `command` assertion the leading program token is extracted (the
    /// interpreter for `sh -c` / `python3 -c` shapes, else the first word) and
    /// probed on PATH; a clearly-missing program is a `warn`. Two hard
    /// environment defects are `error`s: the repo not being a git repo, and
    /// `.kranz` not being writable. The probe is intentionally lenient — only
    /// programs that plainly do not resolve are flagged, so a shell builtin or
    /// an odd-but-valid command never produces a false warning.
    pub fn preflight(&self) -> Vec<PreflightIssue> {
        let mut issues = Vec::new();

        // Hard defects first (an "error" severity): a run against a non-repo or
        // a read-only .kranz is almost certainly doomed.
        if !is_git_repo(self.paths.repo_root.as_path()) {
            issues.push(PreflightIssue {
                severity: "error",
                message: format!("{} is not a git repository", self.paths.repo_root.display()),
            });
        }
        if !kranz_dir_is_writable(&self.paths) {
            issues.push(PreflightIssue {
                severity: "error",
                message: ".kranz directory is not writable".to_string(),
            });
        }

        for role in [
            Role::Orchestrator,
            Role::Worker,
            Role::ValidatorScrutiny,
            Role::ValidatorFunctional,
        ] {
            let role_key = role_config_key(role);
            match self.state.config.backend_kind(role) {
                BackendKind::Codex => {
                    if let Err(err) = crate::backend_codex::discover_codex_binary(None) {
                        issues.push(PreflightIssue {
                            severity: "warn",
                            message: format!(
                                "{role_key}.backend is \"codex\" but no codex binary was found \
                                 ({err}); that role will fall back to the claude backend"
                            ),
                        });
                    }
                }
                BackendKind::Droid => {
                    if let Err(err) = crate::backend_droid::discover_droid_binary(None) {
                        issues.push(PreflightIssue {
                            severity: "warn",
                            message: format!(
                                "{role_key}.backend is \"droid\" but no droid binary was found \
                                 ({err}); that role will fall back to the claude backend"
                            ),
                        });
                    }
                }
                BackendKind::Kimi => {
                    if let Err(err) = crate::backend_kimi::discover_kimi_binary(None) {
                        issues.push(PreflightIssue {
                            severity: "warn",
                            message: format!(
                                "{role_key}.backend is \"kimi\" but no kimi binary was found \
                                 ({err}); that role will fall back to the claude backend"
                            ),
                        });
                    }
                }
                BackendKind::Claude => {}
                BackendKind::Local => {
                    if let Some(base_url) = self.state.config.role(role).base_url.as_deref() {
                        if !probe_local_endpoint_reachable(base_url) {
                            issues.push(PreflightIssue {
                                severity: "warn",
                                message: format!(
                                    "{role_key}.backend is \"local\" but {base_url} did not \
                                     respond to a reachability probe; that role's HTTP calls \
                                     may fail"
                                ),
                            });
                        }
                    }
                }
            }
        }

        // Contract command programs: probe the leading token of each distinct
        // command, flagging only ones that clearly do not resolve on PATH.
        let mut probed: std::collections::HashSet<String> = std::collections::HashSet::new();
        for assertion in &self.state.mission.validation_contract {
            if assertion.check != AssertionCheck::Command {
                continue;
            }
            let Some(command) = assertion.command.as_deref() else {
                continue;
            };
            let Some(program) = leading_program(command) else {
                continue;
            };
            if !probed.insert(program.clone()) {
                continue; // already reported/checked this program
            }
            if !program_resolves(&program) {
                issues.push(PreflightIssue {
                    severity: "warn",
                    message: format!(
                        "command assertion [{}] uses '{program}', which was not found on PATH",
                        assertion.id
                    ),
                });
            }
            if !contract_sweep::cargo_test_has_anti_vacuity(command) {
                issues.push(PreflightIssue {
                    severity: "warn",
                    message: format!(
                        "command assertion [{}] runs `cargo test` without anti-vacuity \
                         (`ok. [1-9]`); a zero-test filter would pass vacuously",
                        assertion.id
                    ),
                });
            }
        }

        // Sandbox preflight (f-2-3/f-2-4): surface unsupported/missing
        // sandbox tooling as a warning, and on macOS run each distinct
        // contract `command` assertion under the generated worker Seatbelt
        // profile. Best-effort and advisory only: never an `error`, never a
        // block. `session_cwd` uses `self.paths.repo_root` (the primary
        // checkout) as a cheap stand-in for the actual per-session worktree
        // root, which does not exist yet at preflight time.
        if self.state.config.worker.sandbox.enforce != crate::types::SandboxEnforce::Off {
            let mission_dir = self.paths.mission_dir();
            let (_resolved, warn) = crate::sandbox::resolve_for_session(
                &self.state.config.worker.sandbox,
                self.paths.repo_root.as_path(),
                &mission_dir,
            );
            if let Some(warn) = warn {
                issues.push(PreflightIssue {
                    severity: "warn",
                    message: warn,
                });
            }
        }
        if matches!(
            self.state.config.worker.sandbox.enforce,
            crate::types::SandboxEnforce::Fs | crate::types::SandboxEnforce::FsNet
        ) && cfg!(target_os = "macos")
        {
            issues.extend(self.sandbox_command_preflight());
        }

        issues
    }

    /// Run each distinct contract `command` assertion under the worker's
    /// generated Seatbelt profile; a non-zero exit under the sandbox becomes a
    /// `warn` `PreflightIssue` naming the assertion. Best-effort: any failure
    /// to resolve the sandbox or write the profile file is silently skipped
    /// (never escalated) rather than reported, since this probe must never
    /// block or mislabel an environment problem as a sandbox problem.
    fn sandbox_command_preflight(&self) -> Vec<PreflightIssue> {
        let mission_dir = self.paths.mission_dir();
        let (resolved, _warn) = crate::sandbox::resolve_for_session(
            &self.state.config.worker.sandbox,
            self.paths.repo_root.as_path(),
            &mission_dir,
        );
        let Some(resolved) = resolved else {
            return Vec::new();
        };
        if resolved.backend != crate::sandbox::SandboxBackend::Seatbelt {
            return Vec::new();
        }
        let profile = crate::sandbox::generate_profile(&resolved.inputs);
        let profile_path = match crate::sandbox::write_profile_file(&mission_dir, &profile)
            .or_else(|_| crate::sandbox::write_profile_file(&resolved.inputs.tmpdir, &profile))
        {
            Ok(path) => path,
            Err(_) => return Vec::new(),
        };

        let mut issues = Vec::new();
        let mut probed: std::collections::HashSet<&str> = std::collections::HashSet::new();
        const MAX_PROBES: usize = 20;
        const TIMEOUT: Duration = Duration::from_secs(5);
        for assertion in &self.state.mission.validation_contract {
            if issues.len() >= MAX_PROBES || probed.len() >= MAX_PROBES {
                break;
            }
            if assertion.check != AssertionCheck::Command {
                continue;
            }
            let Some(command) = assertion.command.as_deref() else {
                continue;
            };
            if !probed.insert(command) {
                continue; // already probed this exact command
            }
            let (program, args) = crate::backend_claude::sandbox_command(
                &profile_path,
                std::path::Path::new("/bin/sh"),
                &["-c".to_string(), command.to_string()],
            );
            match run_with_timeout(&program, &args, TIMEOUT) {
                Some(output) if !output.status.success() => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let tail = last_chars_local(&stderr, 200);
                    issues.push(PreflightIssue {
                        severity: "warn",
                        message: format!(
                            "command assertion [{}] fails under the fs sandbox profile: {tail}",
                            assertion.id
                        ),
                    });
                }
                _ => {}
            }
        }
        issues
    }
}

// ---------------------------------------------------------------------------
// Preflight helpers (roadmap M2) — all pure/best-effort, no engine state
// ---------------------------------------------------------------------------

fn role_config_key(role: Role) -> &'static str {
    match role {
        Role::Orchestrator => "orchestrator",
        Role::Worker => "worker",
        Role::ValidatorScrutiny => "validatorScrutiny",
        Role::ValidatorFunctional => "validatorFunctional",
    }
}

/// Extract the leading program token of a contract `command` line for a PATH
/// probe (roadmap M2 preflight). Best-effort by design:
///
/// - `sh -c '…'` / `bash -c '…'` / `python3 -c '…'`-style forms name the
///   INTERPRETER as the program (the thing that must exist), so the first
///   token is returned rather than trying to parse the embedded script.
/// - Otherwise the first whitespace-delimited token is returned, with common
///   leading `VAR=value` environment assignments skipped and a leading path
///   (`./scripts/check.sh`) reduced to its final component only for the
///   presence check semantics of [`program_resolves`].
///
/// Returns `None` when no plausible program token can be found (empty command,
/// or a line that is only environment assignments) — the caller then skips the
/// probe rather than emit a spurious warning.
fn leading_program(command: &str) -> Option<String> {
    // Skip leading `VAR=value` assignments ("FOO=bar cmd …" is common in
    // contract lines); the program is the first token that is not an
    // assignment.
    let mut token = None;
    for tok in command.split_whitespace() {
        if is_env_assignment(tok) {
            continue;
        }
        token = Some(tok);
        break;
    }
    let token = token?;
    if token.is_empty() {
        return None;
    }
    Some(token.to_string())
}

/// A `VAR=value` leading environment assignment (`FOO=bar`): an identifier,
/// then `=`. Used to skip past them when finding the program token.
fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) if !name.is_empty() => {
            let mut chars = name.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

/// Whether a program token plausibly resolves to something runnable
/// (roadmap M2 preflight). LENIENT: this only ever produces a warning, so it
/// errs heavily toward "resolves" to avoid false positives.
///
/// - A program containing a path separator is checked as a filesystem path
///   (it names its own location; PATH does not apply).
/// - A bare name is looked up across every `PATH` entry.
/// - Common POSIX shell builtins that have no on-disk binary (`cd`, `:`,
///   `true`, `false`, `echo`, `test`, `[`) always resolve — a contract line
///   like `cd . && …` must never warn.
///
/// On non-unix hosts the executable-bit check is skipped (mere existence in a
/// PATH dir counts), and `.exe`/`.bat`/`.cmd` variants are also accepted.
fn program_resolves(program: &str) -> bool {
    // Shell builtins with no backing binary — never a missing prerequisite.
    const BUILTINS: &[&str] = &[
        "cd", ":", "true", "false", "echo", "test", "[", "set", "export", "unset",
    ];
    if BUILTINS.contains(&program) {
        return true;
    }

    // A path-bearing program names its own location; PATH does not apply.
    if program.contains('/') || program.contains('\\') {
        return path_is_executable(std::path::Path::new(program));
    }

    let Some(path) = std::env::var_os("PATH") else {
        // No PATH to scan: cannot disprove existence, so do not warn.
        return true;
    };
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if path_is_executable(&dir.join(program)) {
            return true;
        }
        // Windows: accept the usual executable extensions.
        #[cfg(windows)]
        for ext in ["exe", "bat", "cmd", "com"] {
            if path_is_executable(&dir.join(format!("{program}.{ext}"))) {
                return true;
            }
        }
    }
    false
}

/// Whether `path` is a regular file that is executable (unix: any execute bit;
/// other platforms: mere existence as a file).
fn path_is_executable(path: &std::path::Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Best-effort, short-timeout (1.5s) reachability probe for a `backend =
/// local` role's `base_url`: a raw TCP connect to the URL's host/port, since
/// an OpenAI-compatible server's root path need not resolve to anything
/// meaningful — any successful connection counts as "reachable"; only a
/// connection-level failure (refused, timeout) does not. Never escalated past
/// a `warn` `PreflightIssue`: this must never block a mission start.
///
/// Deliberately runtime-free (no `tokio::runtime::Builder`/`block_on`):
/// `preflight()` runs synchronously inside the process's own tokio runtime
/// (see `run_loop()`), and entering a nested runtime here panics
/// unconditionally with "Cannot start a runtime from within a runtime".
/// Mirrors the proven-safe pattern in
/// `backend_readiness::probe_local_reachability`.
fn probe_local_endpoint_reachable(base_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return true; // can't probe; don't manufacture a false warning
    };
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return true;
    };
    let addr = match (host, port).to_socket_addrs() {
        Ok(mut addrs) => addrs.next(),
        Err(_) => None,
    };
    let Some(addr) = addr else {
        return true; // unresolvable; don't manufacture a false warning
    };
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(1500)).is_ok()
}

/// Whether the mission's `.kranz` directory is writable: create it if needed,
/// then probe with a temp file. Conservative — any error other than a clean
/// write is reported as "not writable".
fn kranz_dir_is_writable(paths: &MissionPaths) -> bool {
    let dir = paths.kranz_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".preflight-{}", uuid::Uuid::new_v4().simple()));
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Test support — shared with orchestrator.rs's fallback tests
// ---------------------------------------------------------------------------

/// Serializes tests that mutate `KRANZ_DROID_BIN` so they don't race
/// concurrently with each other (mirrors `CODEX_ENV_LOCK`; kept on its
/// own dedicated mutex since it guards a different env var). Lives outside
/// `mod tests` because `orchestrator.rs`'s `KRANZ_DROID_BIN`-mutating tests
/// (`droid_absent_loud_fallback` via [`DroidEnvGuard`], and the unix
/// `DroidStubEnvGuard` directly) engage this same lock.
#[cfg(test)]
pub(crate) static DROID_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard: points `KRANZ_DROID_BIN` at a path that cannot exist, so
/// droid discovery misses deterministically. Restores the previous value
/// on drop, including on panic.
#[cfg(test)]
pub(crate) struct DroidEnvGuard {
    prev_bin: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl DroidEnvGuard {
    pub(crate) fn engage() -> Self {
        let lock = DROID_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev_bin = std::env::var_os("KRANZ_DROID_BIN");
        std::env::set_var(
            "KRANZ_DROID_BIN",
            "/nonexistent/kranz-test-droid-binary-absent",
        );
        DroidEnvGuard {
            prev_bin,
            _lock: lock,
        }
    }
}

#[cfg(test)]
impl Drop for DroidEnvGuard {
    fn drop(&mut self) {
        match self.prev_bin.take() {
            Some(v) => std::env::set_var("KRANZ_DROID_BIN", v),
            None => std::env::remove_var("KRANZ_DROID_BIN"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::AgentBackend;
    use std::sync::Arc;

    /// `validatorScrutiny.backend = "droid"` with no droid binary reachable:
    /// preflight must warn and mention "droid".
    #[test]
    fn droid_preflight_warns_when_binary_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
        let _ = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&root)
            .output();
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(&root)
            .output();
        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&root)
            .output();
        std::fs::write(root.join("README.md"), "seed\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&root)
            .output();
        let _ = std::process::Command::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(&root)
            .output();

        let mut cfg = MissionConfig::default();
        cfg.validator_scrutiny.backend = Some("droid".to_string());

        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let engine = MissionEngine::create(backend, &root, "goal", cfg).expect("create engine");

        let env_guard = DroidEnvGuard::engage();
        let issues = engine.preflight();
        drop(env_guard);

        assert!(
            issues
                .iter()
                .any(|i| i.severity == "warn" && i.message.contains("droid")),
            "expected a droid preflight warning, got {issues:?}"
        );
    }

    /// `worker.backend = "local"` with an unreachable `base_url`: preflight
    /// must surface exactly a `"warn"` issue (never `"error"`, never a
    /// block) naming the endpoint.
    #[test]
    fn local_preflight_warns_when_base_url_unreachable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
        let _ = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&root)
            .output();
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(&root)
            .output();
        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&root)
            .output();
        std::fs::write(root.join("README.md"), "seed\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&root)
            .output();
        let _ = std::process::Command::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(&root)
            .output();

        let mut cfg = MissionConfig::default();
        cfg.worker.backend = Some("local".to_string());
        // Port 0 never accepts connections; a fast, reliable "unreachable".
        cfg.worker.base_url = Some("http://127.0.0.1:0/v1".to_string());
        cfg.worker.context_budget = Some(8192);
        cfg.allow_below_default_worker_model = true;

        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let engine = MissionEngine::create(backend, &root, "goal", cfg).expect("create engine");

        let issues = engine.preflight();

        assert!(
            issues
                .iter()
                .any(|i| i.severity == "warn" && i.message.contains("127.0.0.1:0")),
            "expected a local preflight warning, got {issues:?}"
        );
        assert!(
            !issues.iter().any(|i| i.severity == "error"),
            "local reachability must never escalate to an error, got {issues:?}"
        );
    }

    /// Regression test for the nested-runtime panic: `preflight()` must be
    /// callable from *within* an already-running tokio runtime (as it is by
    /// `run_loop()`) without `probe_local_endpoint_reachable` trying to spin
    /// up its own nested `Runtime::block_on`, which panics unconditionally.
    /// This test would fail (panic) if a nested `block_on` were ever
    /// reintroduced.
    #[tokio::test]
    async fn local_preflight_warns_inside_runtime_without_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
        let _ = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&root)
            .output();
        let _ = std::process::Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(&root)
            .output();
        let _ = std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&root)
            .output();
        std::fs::write(root.join("README.md"), "seed\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&root)
            .output();
        let _ = std::process::Command::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(&root)
            .output();

        let mut cfg = MissionConfig::default();
        cfg.worker.backend = Some("local".to_string());
        // Port 0 never accepts connections; a fast, reliable "unreachable".
        cfg.worker.base_url = Some("http://127.0.0.1:0/v1".to_string());
        cfg.worker.context_budget = Some(8192);
        cfg.allow_below_default_worker_model = true;

        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let engine = MissionEngine::create(backend, &root, "goal", cfg).expect("create engine");

        // Called from within this #[tokio::test]'s active runtime, exactly
        // as it would be from the async run_loop(): must not panic.
        let issues = engine.preflight();

        assert!(
            issues
                .iter()
                .any(|i| i.severity == "warn" && i.message.contains("127.0.0.1:0")),
            "expected a local preflight warning, got {issues:?}"
        );
        assert!(
            !issues.iter().any(|i| i.severity == "error"),
            "local reachability must never escalate to an error, got {issues:?}"
        );
    }
}
