//! Environment preflight (roadmap M2) — extracted from `orchestrator.rs` in
//! the monolith split (pure code motion, no behavior change). Advisory only:
//! preflight never blocks a mission (the final contract gate stays
//! authoritative); it surfaces missing prerequisites as a single
//! `orchestrator.decision` at run start, and [`PREFLIGHT_CLEAR_SUMMARY`]
//! durably supersedes an earlier warning once a run's probes come back clean.

use crate::command_exec::{is_git_repo, tail_chars};
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
    ///
    /// Synchronous by design: `run_loop` futures are spawned (`tokio::spawn`,
    /// so `Send`-bound), and an `async fn(&self)` here would hold
    /// `&MissionEngine` — not `Sync`, via `Box<dyn AgentSession>` — across an
    /// await, poisoning the whole `run()` future's `Send`. The sandbox
    /// command probes still use the shared ASYNC bounded runner: they run it
    /// on a dedicated thread owning a current-thread runtime (the
    /// [`crate::command_exec::run_bounded_gate_command`] pattern), which also
    /// keeps this callable from inside the ambient runtime without a nested
    /// `block_on` panic.
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
        // block. The warn-only resolve below never executes anything, so a
        // stand-in `session_cwd` is fine there; the command probes in
        // `sandbox_command_preflight` resolve and run against a DISPOSABLE
        // detached worktree, never the primary checkout (AGENTS.md rule 7).
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
    ///
    /// Probes run in a DISPOSABLE detached worktree at the mission's pinned
    /// base (under the mission's gitignored `runs/` scratch), resolved as the
    /// profile's `session_cwd` and used as the probe cwd — never the primary
    /// checkout, which must stay byte-untouched across a run (AGENTS.md rule
    /// 7). A worktree-creation failure is the one new failure mode here and
    /// surfaces as a `warn` (the probes are then skipped).
    ///
    /// Execution goes through [`crate::command_exec::run_bounded_argv`], the
    /// shared bounded runner (concurrent pipe drain, process-tree kill on
    /// timeout), driven on a DEDICATED thread that owns a current-thread
    /// runtime — the [`crate::command_exec::run_bounded_gate_command`]
    /// pattern. `preflight()` is sync and called on the ambient tokio
    /// runtime, where a nested `block_on` would panic; a raw
    /// `std::thread::spawn` carries no runtime context, so the runner's
    /// runtime is safe there. The disposable worktree outlives the thread
    /// (joined before the guard drops).
    fn sandbox_command_preflight(&self) -> Vec<PreflightIssue> {
        let mission_dir = self.paths.mission_dir();
        let worktree_path = self.paths.runs_dir().join("preflight-worktree");
        let (resolved, _warn) = crate::sandbox::resolve_for_session(
            &self.state.config.worker.sandbox,
            &worktree_path,
            &mission_dir,
        );
        let Some(resolved) = resolved else {
            return Vec::new();
        };
        if resolved.backend != crate::sandbox::SandboxBackend::Seatbelt {
            return Vec::new();
        }

        // The throwaway probe tree: detached at the pinned base (approval
        // base_sha, falling back to the base branch for pre-pin missions),
        // removed on guard drop however the probes end.
        let base = self
            .state
            .mission
            .base_sha
            .clone()
            .unwrap_or_else(|| self.state.mission.base_branch.clone());
        let _worktree = match DisposableWorktree::create(&self.repo, &worktree_path, &base) {
            Ok(guard) => guard,
            Err(err) => {
                return vec![PreflightIssue {
                    severity: "warn",
                    message: format!(
                        "sandbox command preflight skipped: could not create disposable \
                         worktree at {base}: {err}"
                    ),
                }];
            }
        };

        let profile = crate::sandbox::generate_profile(&resolved.inputs);
        // The profile file is runtime scratch: keep it under the gitignored
        // `runs/` dir (never the mission dir, whose unignored files would
        // show up as untracked in the primary checkout's `git status`).
        let profile_path =
            match crate::sandbox::write_profile_file(&self.paths.runs_dir(), &profile)
                .or_else(|_| crate::sandbox::write_profile_file(&resolved.inputs.tmpdir, &profile))
            {
                Ok(path) => path,
                Err(_) => return Vec::new(),
            };

        // The same COMPLETE environment the final contract gate gives command
        // assertions (minimal allowlist + scratch HOME + toolchain caches +
        // any contractEnvPassthrough names): the probe measures what the gate
        // will see, and ambient secrets never reach a contract command.
        let env = crate::agent_env::contract_command_env(
            &self.paths.runs_dir().join("contract-home"),
            self.state.mission.base_sha.as_deref(),
            &self.state.config.contract_env_passthrough,
        );

        // Probe selection (dedup, capped) happens here; execution moves to
        // the probe thread below.
        const MAX_PROBES: usize = 20;
        const TIMEOUT: Duration = Duration::from_secs(5);
        let mut probes: Vec<(String, std::path::PathBuf, Vec<String>)> = Vec::new();
        let mut probed: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for assertion in &self.state.mission.validation_contract {
            if probes.len() >= MAX_PROBES {
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
            probes.push((assertion.id.clone(), program, args));
        }
        if probes.is_empty() {
            return Vec::new();
        }

        let probe_cwd = worktree_path.clone();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let Ok(runtime) = runtime else {
                return Vec::new(); // best-effort: no runtime, no probes
            };
            runtime.block_on(async move {
                let mut issues = Vec::new();
                for (id, program, args) in probes {
                    match crate::command_exec::run_bounded_argv(
                        &probe_cwd, &program, &args, TIMEOUT, &env,
                    )
                    .await
                    {
                        // Only a real non-zero exit warns; a timeout/spawn
                        // failure (`None`) stays silent — the probe is
                        // advisory and a slow command is not a sandbox problem.
                        (Some(0), _) | (None, _) => {}
                        (Some(_), output) => {
                            let tail = tail_chars(&output, 200);
                            issues.push(PreflightIssue {
                                severity: "warn",
                                message: format!(
                                    "command assertion [{id}] fails under the fs sandbox \
                                     profile: {tail}"
                                ),
                            });
                        }
                    }
                }
                issues
            })
        });
        // A panicked probe thread must never take preflight down with it.
        worker.join().unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Preflight helpers (roadmap M2) — all pure/best-effort, no engine state
// ---------------------------------------------------------------------------

/// RAII guard for the disposable preflight worktree (P1, ticket
/// preflight-in-disposable-worktree): a throwaway detached worktree the
/// sandbox command probes run in so contract commands never execute against
/// the primary checkout (AGENTS.md rule 7 — the primary must stay
/// byte-untouched across a run). The worktree lives under the mission's own
/// gitignored `runs/` scratch, not global temp.
///
/// Drop removes it best-effort — `git worktree remove --force` (which also
/// deletes the directory), a dir sweep for anything git declined, and a
/// prune of stale administrative entries — so even a probe failure or early
/// return cannot leak it.
struct DisposableWorktree {
    repo: crate::git_ops::GitRepo,
    path: std::path::PathBuf,
}

impl DisposableWorktree {
    /// Create a detached worktree at `path` pinned to `base` (`git worktree
    /// add --detach`). Idempotent against a stale leftover from a crashed
    /// run: any prior worktree/dir at `path` is cleared first, mirroring
    /// `setup_mission_worktree`'s crash sweep.
    fn create(
        repo: &crate::git_ops::GitRepo,
        path: &std::path::Path,
        base: &str,
    ) -> crate::error::Result<Self> {
        let _ = repo.remove_worktree(path);
        let _ = std::fs::remove_dir_all(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                crate::error::EngineError::Git(format!("create {}: {e}", parent.display()))
            })?;
        }
        repo.add_detached_worktree(path, base)?;
        Ok(Self {
            repo: repo.clone(),
            path: path.to_path_buf(),
        })
    }
}

impl Drop for DisposableWorktree {
    fn drop(&mut self) {
        let _ = self.repo.remove_worktree(&self.path);
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = self.repo.prune_worktrees();
    }
}

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
/// unconditionally with "Cannot start a runtime from within a runtime". (The
/// sandbox command probes avoid the same trap by owning a runtime on a
/// DEDICATED thread — see `sandbox_command_preflight`.) Mirrors the
/// proven-safe pattern in `backend_readiness::probe_local_reachability`.
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

    // -----------------------------------------------------------------------
    // Disposable-worktree sandbox probes (P1, ticket
    // preflight-in-disposable-worktree). macOS-only: the Seatbelt probe path
    // is the only one that executes contract commands.
    // -----------------------------------------------------------------------

    /// Init a throwaway repo with one seed commit; returns (tempdir guard,
    /// canonical root, seed commit sha).
    #[cfg(target_os = "macos")]
    fn seeded_git_repo() -> (tempfile::TempDir, std::path::PathBuf, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "test"],
            vec!["config", "user.email", "test@example.com"],
        ] {
            let _ = std::process::Command::new("git")
                .args(&args)
                .current_dir(&root)
                .output();
        }
        std::fs::write(root.join("README.md"), "seed\n").unwrap();
        let _ = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&root)
            .output();
        let _ = std::process::Command::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(&root)
            .output();
        let sha = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&root)
            .output()
            .expect("rev-parse HEAD");
        let sha = String::from_utf8_lossy(&sha.stdout).trim().to_string();
        (dir, root, sha)
    }

    #[cfg(target_os = "macos")]
    fn command_assertion(id: &str, command: &str) -> Assertion {
        Assertion {
            id: id.to_string(),
            statement: "the check passes".to_string(),
            check: AssertionCheck::Command,
            command: Some(command.to_string()),
        }
    }

    #[cfg(target_os = "macos")]
    fn git_status_porcelain(root: &std::path::Path) -> String {
        let out = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(root)
            .output()
            .expect("git status");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[cfg(target_os = "macos")]
    fn sandbox_exec_available() -> bool {
        std::process::Command::new("which")
            .arg("sandbox-exec")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// The P1 regression test: with `worker.sandbox.enforce = fs`, the
    /// contract-command probes must run in a DISPOSABLE worktree — proven by
    /// a `pwd -P` assertion inside the probe — and the primary checkout must
    /// be byte-identical (`git status --porcelain` unchanged, no marker file)
    /// across a preflight whose contract command writes a file. The
    /// disposable worktree is removed afterwards (success AND failing probe
    /// alike; this run has both).
    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_preflight_probes_disposable_worktree_not_primary() {
        if !sandbox_exec_available() {
            eprintln!("sandbox-exec not found on this host; skipping");
            return;
        }
        let (_dir, root, sha) = seeded_git_repo();

        let mut cfg = MissionConfig::default();
        cfg.worker.sandbox.enforce = crate::types::SandboxEnforce::Fs;

        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg).expect("create engine");
        engine.state.mission.base_sha = Some(sha);

        let worktree_path = engine.paths.runs_dir().join("preflight-worktree");
        let home_marker = format!("kranz_pf_{}", uuid::Uuid::new_v4());
        // The REAL home is outside the generated allowlist (worktree /
        // mission dir / tmpdir): bake it in literally, because the probe's
        // contract env deliberately redefines $HOME to the writable scratch.
        let real_home = std::env::var("HOME").expect("HOME must be set for this test");
        engine.state.mission.validation_contract = vec![
            // cwd-relative write: lands in the probe's cwd, must NOT warn…
            command_assertion("a-rel-write", "echo x > preflight-marker.txt"),
            // …and the probe's cwd must BE the disposable worktree.
            command_assertion(
                "a-cwd",
                &format!("[ \"$(pwd -P)\" = '{}' ]", worktree_path.display()),
            ),
            // A write outside the sandbox allowlist: must fail and warn —
            // also proves the probes really executed (anti-vacuity).
            command_assertion(
                "a-outside",
                &format!("echo x > '{real_home}/{home_marker}'"),
            ),
        ];

        let status_before = git_status_porcelain(&root);
        let issues = engine.preflight();

        // The failing probe warned; the in-worktree probes did not.
        assert!(
            issues.iter().any(|i| i.severity == "warn"
                && i.message.contains("[a-outside]")
                && i.message.contains("fs sandbox profile")),
            "expected a sandbox warn for the out-of-allowlist write: {issues:?}"
        );
        for id in ["a-rel-write", "a-cwd"] {
            let needle = format!("[{id}]");
            assert!(
                !issues.iter().any(|i| i.message.contains(&needle)),
                "{id} must not warn — probes run with cwd = the disposable worktree: {issues:?}"
            );
        }
        assert!(
            !issues.iter().any(|i| i.severity == "error"),
            "sandbox preflight must never escalate to error: {issues:?}"
        );

        // AGENTS.md rule 7: the primary checkout is byte-untouched.
        assert_eq!(
            status_before,
            git_status_porcelain(&root),
            "primary checkout changed across preflight"
        );
        assert!(
            !root.join("preflight-marker.txt").exists(),
            "the probe's cwd-relative write landed in the primary checkout"
        );

        // The disposable worktree is gone after the run (both the succeeding
        // and the failing probe used it).
        assert!(
            !worktree_path.exists(),
            "disposable preflight worktree leaked at {}",
            worktree_path.display()
        );

        // Clean up in case the sandbox somehow did not block the $HOME write.
        if let Ok(home) = std::env::var("HOME") {
            let _ = std::fs::remove_file(std::path::Path::new(&home).join(&home_marker));
        }
    }

    /// The new failure mode: a disposable worktree that cannot be created
    /// (here: a pinned base that does not resolve) becomes one advisory
    /// `warn` — never an error, never a panic, and no leftover tree.
    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_preflight_worktree_creation_failure_is_advisory() {
        if !sandbox_exec_available() {
            eprintln!("sandbox-exec not found on this host; skipping");
            return;
        }
        let (_dir, root, _sha) = seeded_git_repo();

        let mut cfg = MissionConfig::default();
        cfg.worker.sandbox.enforce = crate::types::SandboxEnforce::Fs;

        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine = MissionEngine::create(backend, &root, "goal", cfg).expect("create engine");
        engine.state.mission.base_sha = Some("0".repeat(40));
        engine.state.mission.validation_contract = vec![command_assertion("a-1", "true")];

        let issues = engine.sandbox_command_preflight();

        assert_eq!(
            issues.len(),
            1,
            "exactly one advisory issue for the worktree failure: {issues:?}"
        );
        assert_eq!(issues[0].severity, "warn");
        assert!(
            issues[0]
                .message
                .contains("could not create disposable worktree"),
            "the warn names the worktree failure: {}",
            issues[0].message
        );
        assert!(
            !engine.paths.runs_dir().join("preflight-worktree").exists(),
            "a failed worktree creation must not leave a tree behind"
        );
    }
}
