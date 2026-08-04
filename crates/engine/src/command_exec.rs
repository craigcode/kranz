//! Bounded, process-tree-killed shell execution for contract and merge-gate
//! commands — extracted from `orchestrator.rs` in the monolith split (pure
//! code motion, no behavior change). These are the ONLY places the engine
//! runs user-authored shell: contract commands need real shell semantics
//! (`sh -c` / `cmd /C`), so argument handling, timeout kill discipline
//! (process group on unix, Job Object on Windows), output tailing, and
//! environment sanitization live here as one unit.
//!
//! ## Sandbox wrap (ticket engine-gates-sandbox-wrapped)
//!
//! Env-clearing alone is not isolation: engine-run gates execute
//! worker-authored build scripts and test binaries, and an env-cleared
//! process still holds the engine's filesystem and network authority (the
//! operator home is discoverable without `HOME` via pwent / `/Users/*`). When
//! the mission's `worker.sandbox.enforce` is not `off`, the gate's `sh -c` is
//! therefore wrapped in the SAME resolved profile an agent session would get
//! — [`GateSandbox::Seatbelt`] (`sandbox-exec -f`) on macOS,
//! [`GateSandbox::Bubblewrap`] on Linux — reusing `crate::sandbox`'s
//! writable-root computation, mission-metadata write denies, and authority
//! read denies. `enforce == off` (and the documented no-op postures below)
//! keeps the pre-wrap behavior byte-for-byte.
//!
//! The gate profile's writable shape is the gate's cwd (the worktree —
//! `target/` and everything else a build writes lives under it) plus a
//! private scratch (validation/final gate: the mission's `runs/contract-home`
//! the contract env already points HOME/TMPDIR/CARGO_HOME at; merge gate: a
//! per-run self-cleaning `kranz-gate-*` temp root). The gate profile also
//! appends two narrow extras the session profile lacks (see
//! [`gate_profile_extras`] for the evidence): a `/dev/null` write allow
//! (`deny default` otherwise rejects the redirects real gate scripts use
//! liberally — this repo's gascity merge-gate scripts alone carry 148 of
//! them) and, on macOS, a name-anchored `xcrun_db*` write regex over the
//! Darwin per-user temp dir (the xcrun shims behind `/usr/bin/git` et al.
//! refresh their tool-resolution cache there via confstr, IGNORING TMPDIR;
//! a refresh under parallel spawns killed a wrapped `cargo test` with EPERM
//! while the unwrapped run silently recovered). SBPL allows compose
//! order-independently and denies still take precedence, so the appends
//! cannot weaken the generated profile; the agent-session profile itself is
//! deliberately untouched. bwrap has no equivalent gaps (`--dev /dev` covers
//! device writes; Linux has no xcrun shim).
//!
//! Network posture: the profile's, mirroring sessions — `fs` keeps full
//! egress (write containment is the fs-tier promise), `fs+net` cuts outbound
//! TCP to loopback. Sessions escape loopback through the filtering egress
//! proxy (`crate::egress_proxy`); engine-run gates are NOT wired through it —
//! it is session infrastructure, and standalone merge gates run with no
//! engine alive to host one — so an `fs+net` gate is offline-by-cache: the
//! stage-1 seeded cache-only Cargo home is its registry, and
//! `CARGO_NET_OFFLINE=true` is injected so a missing crate fails with a
//! clear cargo error instead of a kernel-denied socket. Toolchains without a
//! warm seeded cache (a cold `npm ci`) need `enforce: fs`.
//!
//! Measured spawn cost (2026-08-03, macOS 15, M-series, Seatbelt; harness:
//! `gate_sandbox_wrap_measure`). Per-spawn micro (`true`, 50 reps): 23.5ms
//! unwrapped vs 26.0ms wrapped — +2.5ms/spawn (+10.8%; across four runs the
//! absolute delta held at ~1.3–5.7ms). Real gate
//! (`cargo test -p kranz-engine --lib` with the sandbox-hostile skips named
//! in the harness, 2 reps, fresh cache-only Cargo home each rep): 97.7s
//! unwrapped vs 96.2s wrapped mean — a −1.5% delta, i.e. NO measurable
//! overhead at gate scale (noise; the ~2.5ms wrap cost vanishes against a
//! ~97s gate). Nowhere near the ticket's ~20% opt-in threshold, so the wrap
//! is the DEFAULT under `enforce != off`, not an opt-in.

use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};

/// Tail kept from a failed contract command's output.
const COMMAND_OUTPUT_TAIL: usize = 1500;

/// Hard cap on one contract `command` assertion at the final gate.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

/// Run `program args` to completion, polling with a bounded wall-clock
/// (`timeout`) rather than blocking forever — the container provider's
/// runtime probes (`workspace_container::spawn_bounded`) must never hang a
/// readiness check. Returns `None` on spawn failure or on timeout (the child
/// is killed). Synchronous and runtime-free so it is callable from inside the
/// ambient tokio runtime; short-lived runtime probes only — anything that can
/// spawn a tree of children or emit large output belongs on
/// [`run_command_bounded`] (concurrent pipe drain + process-tree kill).
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
///
/// agent-env-clear: the shell spawns with a CLEARED environment — `env` is
/// the child's COMPLETE environment, built by callers via
/// [`crate::agent_env::contract_command_env`] (minimal allowlist +
/// `KRANZ_BASE_SHA` + toolchain caches + any `contractEnvPassthrough`
/// names). Ambient secrets never reach a contract command.
///
/// Test-only since `engine-gates-sandbox-wrapped`: production contract/gate
/// execution goes through [`run_shell_command_sandboxed`] (whose
/// [`GateSandbox::Disabled`] arm reproduces this path byte-for-byte — the
/// off-regression tests compare against this reference implementation), and
/// the workspace-gate trust channel uses [`run_shell_command_with_code`].
#[cfg(test)]
pub(crate) async fn run_shell_command(
    cwd: &std::path::Path,
    command: &str,
    env: &HashMap<String, String>,
) -> (bool, String) {
    run_shell_command_with_timeout(cwd, command, COMMAND_TIMEOUT, env).await
}

/// `run_shell_command` plus the process exit code: `Some(0)` is success,
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
/// `clear_env` selects the trust channel: `true` for validation-contract
/// commands (the `env` map is the child's COMPLETE environment — see
/// [`run_shell_command`]); `false` for workspace-gate/bootstrap/data-hook
/// commands, whose workspace contract declares its own secrets channel
/// (`workspace.json`'s `secrets`) fed from ambient — never a contract
/// command path.
///
/// Pipe draining and the process-tree timeout kill (unix process group,
/// Windows Job Object) live in the one shared core, [`run_command_bounded`].
#[cfg(test)]
async fn run_shell_command_with_timeout(
    cwd: &std::path::Path,
    command: &str,
    timeout: Duration,
    env: &HashMap<String, String>,
) -> (bool, String) {
    let (code, output) = run_shell_command_with_timeout_env(cwd, command, timeout, env, true).await;
    (code == Some(0), output)
}

async fn run_shell_command_with_timeout_env(
    cwd: &std::path::Path,
    command: &str,
    timeout: Duration,
    env: &HashMap<String, String>,
    clear_env: bool,
) -> (Option<i32>, String) {
    let (program, args) = shell_argv(command);
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    if clear_env {
        cmd.env_clear();
    }
    run_command_bounded(configure_bounded_child(cmd, cwd, env), timeout).await
}

/// The shell argv every contract/gate command bottoms out in (`cmd /C` on
/// Windows, `sh -c` elsewhere), factored out of
/// [`run_shell_command_with_timeout_env`] so [`GateSandbox::Disabled`]
/// reproduces the pre-wrap invocation byte-for-byte.
fn shell_argv(command: &str) -> (std::path::PathBuf, Vec<String>) {
    #[cfg(windows)]
    {
        (
            std::path::PathBuf::from("cmd"),
            vec!["/C".to_string(), command.to_string()],
        )
    }
    #[cfg(not(windows))]
    {
        (
            std::path::PathBuf::from("sh"),
            vec!["-c".to_string(), command.to_string()],
        )
    }
}

/// Bounded run of an arbitrary program argv with the same pipe-draining /
/// process-tree-kill discipline as contract shell commands — the sandbox
/// preflight probe (`sandbox-exec -f <profile> /bin/sh -c <command>`) runs
/// here rather than through a spawner of its own. `env` is the child's
/// COMPLETE environment (the process env is cleared first): probes are
/// operator-authored contract commands, so they get exactly the contract env
/// the final gate would give them — never ambient secrets.
pub(crate) async fn run_bounded_argv(
    cwd: &std::path::Path,
    program: &std::path::Path,
    args: &[String],
    timeout: Duration,
    env: &HashMap<String, String>,
) -> (Option<i32>, String) {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    cmd.env_clear();
    run_command_bounded(configure_bounded_child(cmd, cwd, env), timeout).await
}

// ---------------------------------------------------------------------------
// Gate sandbox wrap (ticket engine-gates-sandbox-wrapped) — see the module doc
// ---------------------------------------------------------------------------

/// The resolved sandbox posture for one engine-run gate execution context
/// (validation-round contract commands, the final gate, merge gates).
///
/// [`GateSandbox::Disabled`] is the byte-identical pre-wrap behavior:
/// `enforce == off` (the operator opted out; the cache-only `CARGO_HOME`
/// still applies), a platform [`crate::sandbox::platform_support`] cannot
/// honor (a documented no-op — agent sessions already REFUSE to run there,
/// so only standalone merge gates of missions run elsewhere can reach it),
/// or `provider = "container"` (tier-3 wraps agent sessions;
/// container-wrapping engine-side gates is out of scope). Tooling that is
/// requested but missing (Linux without `bwrap`) FAILS CLOSED at resolve
/// time, mirroring session resolution.
#[derive(Debug)]
pub(crate) enum GateSandbox {
    /// Run the shell exactly as before the wrap — no wrapper process.
    Disabled,
    /// macOS Seatbelt: `sandbox-exec -f <profile> /bin/sh -c <command>`.
    Seatbelt {
        enforce: crate::types::SandboxEnforce,
        profile_path: std::path::PathBuf,
    },
    /// Linux bubblewrap: `bwrap <args> -- /bin/sh -c <command>`. The inputs
    /// ride along because the argv — including its spawn-time mask-bind
    /// preparation — is built per command.
    Bubblewrap {
        inputs: Box<crate::sandbox::SandboxInputs>,
    },
}

impl GateSandbox {
    /// The enforcement level the wrap applies (`Off` when disabled) — the
    /// runner keys the fs+net offline-by-cache env adjustment on it.
    pub(crate) fn enforce(&self) -> crate::types::SandboxEnforce {
        match self {
            GateSandbox::Disabled => crate::types::SandboxEnforce::Off,
            GateSandbox::Seatbelt { enforce, .. } => *enforce,
            GateSandbox::Bubblewrap { inputs } => inputs.enforce,
        }
    }

    /// Build the `(program, args)` that runs `command` under this posture.
    /// `Disabled` reproduces [`shell_argv`] EXACTLY, so the off path is
    /// byte-identical to the pre-wrap behavior. A bubblewrap mask-prep
    /// failure FAILS CLOSED — a gate that cannot be wrapped must not run
    /// unsandboxed under enforcement.
    fn wrap_shell(&self, command: &str) -> crate::error::Result<(std::path::PathBuf, Vec<String>)> {
        match self {
            GateSandbox::Disabled => Ok(shell_argv(command)),
            GateSandbox::Seatbelt { profile_path, .. } => {
                Ok(crate::backend_claude::sandbox_command(
                    profile_path,
                    std::path::Path::new("/bin/sh"),
                    &["-c".to_string(), command.to_string()],
                ))
            }
            GateSandbox::Bubblewrap { inputs } => {
                let args = crate::sandbox::bubblewrap_args(
                    inputs,
                    std::path::Path::new("/bin/sh"),
                    &["-c".to_string(), command.to_string()],
                )?;
                Ok((std::path::PathBuf::from("bwrap"), args))
            }
        }
    }
}

/// The outcome of resolving a gate sandbox: the posture plus an optional
/// operator-facing note (surfaced as an orchestrator decision / merge
/// detail) when enforcement degraded to a documented no-op.
#[derive(Debug)]
pub(crate) struct GateSandboxResolution {
    pub sandbox: GateSandbox,
    pub note: Option<String>,
}

/// SBPL appended to the SESSION profile for gate use — never edited into
/// `crate::sandbox::generate_profile` (the agent-session profile is
/// deliberately untouched). SBPL allows compose order-independently and
/// denies still take precedence regardless of clause order (verified with
/// sandbox-exec), so appending cannot weaken the generated profile.
///
/// - `(literal "/dev/null")` write allow: `deny default` otherwise rejects
///   `/dev/null` redirects (probed 2026-08-03: "Operation not permitted"),
///   which real gate lines and scripts use liberally (this repo's gascity
///   merge-gate scripts: 148 hits in one file).
/// - an `xcrun_db*` write REGEX over the Darwin per-user temp dir (macOS
///   only): the `/usr/bin/*` xcrun shims (git, clang, …) keep their
///   tool-resolution cache there — via `confstr(_CS_DARWIN_USER_TEMP_DIR)`,
///   IGNORING `TMPDIR` — and a cache refresh fires unpredictably under
///   parallel spawns. Measured 2026-08-03: a wrapped `cargo test` died at
///   test-binary startup with `git: error: couldn't create cache file
///   '…/T/xcrun_db-kJ956XOY' (errno=Operation not permitted)` while the same
///   run unwrapped silently recovered (the temp root is writable there).
///   The regex is name-anchored (`xcrun_db` prefix only, never the whole
///   temp root — sibling missions' worktrees live beside it), and both the
///   raw and canonical temp-dir forms are emitted (the macOS
///   `/var` ↔ `/private/var` split the write allowlist already handles).
///   An operator whose confstr temp dir differs from the engine's
///   `std::env::temp_dir()` (a custom `TMPDIR` on the server) gets a loud
///   gate failure, not a silent hole — the documented edge.
fn gate_profile_extras() -> String {
    // The /dev/null literal is universal; the xcrun regex is macOS-only. The
    // shadowed rebinding keeps the mutation inside the cfg so linux clippy
    // sees no unused `mut` (windows-latest CI gates -D warnings).
    let extras = String::from("\n(allow file-write* (literal \"/dev/null\"))\n");
    #[cfg(target_os = "macos")]
    let extras = {
        let mut extras = extras;
        let temp = std::env::temp_dir();
        let mut prefixes = std::collections::BTreeSet::new();
        prefixes.insert(crate::sandbox::escape_sbpl_regex(&temp));
        prefixes.insert(crate::sandbox::escape_sbpl_regex(
            &crate::sandbox::absolutize(&temp),
        ));
        extras.push_str("(allow file-write*\n");
        for prefix in prefixes {
            extras.push_str(&format!("  (regex #\"^{prefix}/xcrun_db[^/]*$\")\n"));
        }
        extras.push_str(")\n");
        extras
    };
    extras
}

/// Resolve the sandbox posture for one engine-run gate execution context.
///
/// `gate_cwd` is the gate's working directory AND the profile's writable
/// root — it fills the session profile's
/// [`crate::sandbox::SandboxInputs::session_cwd`] slot so the writable-root
/// computation (`target/` and everything else a build writes lives under the
/// gate tree) is REUSED, never re-rolled. `scratch_home` is the gate's
/// private writable scratch: the mission's `runs/contract-home` for
/// validation/final gates (the contract env already points
/// HOME/TMPDIR/CARGO_HOME there), a per-run temp root for merge gates.
/// `profile_dir` is where the Seatbelt profile file is written (gitignored
/// scratch — `runs/` for the engine paths, the per-run scratch for merges).
/// Mission metadata write-denies and authority read-denies come from
/// `mission_dir`, exactly as sessions derive them.
pub(crate) fn resolve_gate_sandbox(
    sandbox_cfg: &crate::types::SandboxConfig,
    gate_cwd: &std::path::Path,
    mission_dir: &std::path::Path,
    scratch_home: &std::path::Path,
    profile_dir: &std::path::Path,
) -> crate::error::Result<GateSandboxResolution> {
    resolve_gate_sandbox_target(
        sandbox_cfg,
        gate_cwd,
        mission_dir,
        scratch_home,
        profile_dir,
        std::env::consts::OS,
        crate::sandbox::command_available("bwrap"),
    )
}

/// [`resolve_gate_sandbox`] parameterized on the target OS and bwrap
/// availability so the decision matrix is testable cross-platform (mirrors
/// `crate::sandbox::resolve_for_session_target`).
fn resolve_gate_sandbox_target(
    sandbox_cfg: &crate::types::SandboxConfig,
    gate_cwd: &std::path::Path,
    mission_dir: &std::path::Path,
    scratch_home: &std::path::Path,
    profile_dir: &std::path::Path,
    target_os: &str,
    bwrap_available: bool,
) -> crate::error::Result<GateSandboxResolution> {
    use crate::types::{SandboxEnforce, SandboxProvider};
    let disabled = |note: Option<String>| {
        Ok(GateSandboxResolution {
            sandbox: GateSandbox::Disabled,
            note,
        })
    };
    if sandbox_cfg.enforce == SandboxEnforce::Off {
        return disabled(None);
    }
    if sandbox_cfg.provider == SandboxProvider::Container {
        return disabled(Some(format!(
            "engine-run gates are not container-wrapped (provider:container wraps agent \
             sessions only); gates run unsandboxed despite enforce:{}",
            sandbox_cfg.enforce.as_str()
        )));
    }
    match crate::sandbox::platform_support(sandbox_cfg.enforce, target_os) {
        // Unreachable (Off returns above) — platform_support is the shared
        // vocabulary, so the match stays exhaustive anyway.
        crate::sandbox::SandboxDecision::Off => disabled(None),
        crate::sandbox::SandboxDecision::UnsupportedWarn => disabled(Some(format!(
            "sandbox enforce:{} requested but unsupported on target_os={target_os}; engine-run \
             gates execute UNSANDBOXED (agent sessions already refuse to run on this platform, \
             so only standalone merge gates can reach this posture)",
            sandbox_cfg.enforce.as_str()
        ))),
        crate::sandbox::SandboxDecision::Enforce(crate::sandbox::SandboxBackend::Bubblewrap)
            if !bwrap_available =>
        {
            Err(crate::error::EngineError::Config(format!(
                "sandbox enforce:{} requested on linux but `bwrap` was not found; refusing \
                 to run engine-run gates unsandboxed",
                sandbox_cfg.enforce.as_str()
            )))
        }
        crate::sandbox::SandboxDecision::Enforce(backend) => {
            let inputs = crate::sandbox::SandboxInputs {
                enforce: sandbox_cfg.enforce,
                session_cwd: gate_cwd.to_path_buf(),
                mission_dir: mission_dir.to_path_buf(),
                tmpdir: scratch_home.to_path_buf(),
                extra_write: sandbox_cfg
                    .extra_write
                    .iter()
                    .map(|raw| crate::sandbox::expand_tilde(raw))
                    .collect(),
                egress: sandbox_cfg.egress.clone(),
            };
            match backend {
                crate::sandbox::SandboxBackend::Seatbelt => {
                    // The session profile PLUS the gate-specific extras (see
                    // [`gate_profile_extras`]) — appended, never edited in,
                    // so the session generator stays untouched.
                    let mut profile = crate::sandbox::generate_profile(&inputs);
                    profile.push_str(&gate_profile_extras());
                    let profile_path = crate::sandbox::write_profile_file(profile_dir, &profile)?;
                    Ok(GateSandboxResolution {
                        sandbox: GateSandbox::Seatbelt {
                            enforce: sandbox_cfg.enforce,
                            profile_path,
                        },
                        note: None,
                    })
                }
                crate::sandbox::SandboxBackend::Bubblewrap => Ok(GateSandboxResolution {
                    sandbox: GateSandbox::Bubblewrap {
                        inputs: Box::new(inputs),
                    },
                    note: None,
                }),
                // platform_support never selects Container (that resolution
                // is `resolve_container_target`'s, and the provider check
                // above already returned) — the match stays exhaustive.
                crate::sandbox::SandboxBackend::Container => {
                    unreachable!("container provider returned above")
                }
            }
        }
    }
}

/// The env a sandboxed gate actually runs with: the caller's contract/gate
/// env, plus — under `fs+net` ONLY — `CARGO_NET_OFFLINE=true`. The wrapped
/// gate's network posture is the profile's (`fs`: full egress; `fs+net`:
/// loopback-only), and engine-run gates are not wired through the egress
/// proxy (session infrastructure — see the module doc), so an `fs+net` gate
/// is offline-by-cache: the explicit offline flag turns a missing crate into
/// a clear cargo error instead of a kernel-denied socket.
fn gate_env_for_sandbox(
    env: &HashMap<String, String>,
    sandbox: &GateSandbox,
) -> HashMap<String, String> {
    let mut env = env.clone();
    if sandbox.enforce() == crate::types::SandboxEnforce::FsNet {
        env.insert("CARGO_NET_OFFLINE".to_string(), "true".to_string());
    }
    env
}

/// The pre-wrap contract-command runner under a resolved gate sandbox:
/// validation-round contract commands, the final gate, and pack gates run
/// through here. [`GateSandbox::Disabled`] reproduces the pre-wrap `sh -c`
/// behavior byte-for-byte;
/// an enforced posture wraps the SAME `sh -c` in the resolved profile, and
/// the wrapper still leads the SAME new process group
/// ([`configure_bounded_child`]) — so the bounded core's timeout SIGKILL
/// reaches the whole tree, sandbox-exec/bwrap and every descendant alike.
pub(crate) async fn run_shell_command_sandboxed(
    cwd: &std::path::Path,
    command: &str,
    env: &HashMap<String, String>,
    sandbox: &GateSandbox,
) -> (bool, String) {
    let (code, output) =
        run_shell_command_sandboxed_with_code(cwd, command, COMMAND_TIMEOUT, env, sandbox).await;
    (code == Some(0), output)
}

/// [`run_shell_command_sandboxed`] with an explicit timeout and the real
/// exit code (the [`run_shell_command_with_code`] shape), so the merge-gate
/// runner and tests can drive the same path.
async fn run_shell_command_sandboxed_with_code(
    cwd: &std::path::Path,
    command: &str,
    timeout: Duration,
    env: &HashMap<String, String>,
    sandbox: &GateSandbox,
) -> (Option<i32>, String) {
    let (program, args) = match sandbox.wrap_shell(command) {
        Ok(argv) => argv,
        Err(error) => {
            return (
                None,
                format!("gate sandbox wrap failed closed (the command did not run): {error}"),
            )
        }
    };
    let env = gate_env_for_sandbox(env, sandbox);
    run_bounded_argv(cwd, &program, &args, timeout, &env).await
}

/// Child setup shared by every bounded run: piped stdout/stderr (drained
/// concurrently by [`run_command_bounded`]), stdin null, kill_on_drop, and —
/// on unix — the child as leader of a NEW process group, so the timeout path
/// can kill the entire command tree, not just the direct child.
fn configure_bounded_child(
    mut cmd: tokio::process::Command,
    cwd: &std::path::Path,
    env: &HashMap<String, String>,
) -> tokio::process::Command {
    cmd.current_dir(cwd)
        .envs(env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
}

/// Bounded-execution core: spawn an already-configured command, drain both
/// pipes CONCURRENTLY with the wait (a full pipe never deadlocks the child),
/// keep only the tailed combined output, and on timeout kill the whole
/// process tree.
///
/// Timeout kill semantics: on unix the child leads its own process group
/// ([`configure_bounded_child`]) and the WHOLE group gets SIGKILL — killing
/// only the wrapper (kill_on_drop) would leave `sleep 300 &`-style
/// descendants running (and holding the output pipes) long after the gate
/// gave up. The killed wrapper itself is reaped by tokio's background orphan
/// reaper (kill_on_drop); group members are re-parented to init and reaped
/// there.
///
/// Windows has no process groups; the equivalent is a Job Object with
/// `KILL_ON_JOB_CLOSE` (see [`crate::backend_claude::win_job`]). The spawned
/// child is assigned to such a job right after spawn, so on timeout
/// `TerminateJobObject` takes the whole tree down — not just the wrapper.
/// That path compiles and is validated only on windows-latest CI, never on
/// the dev host.
async fn run_command_bounded(
    cmd: tokio::process::Command,
    timeout: Duration,
) -> (Option<i32>, String) {
    let mut cmd = cmd;
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return (None, format!("failed to spawn shell: {e}")),
    };
    let stdout = child.stdout.take().expect("stdout was configured as piped");
    let stderr = child.stderr.take().expect("stderr was configured as piped");
    #[cfg(unix)]
    let group_pid = child.id();

    // Windows: assign the spawned child to a kill-on-close Job Object so the
    // timeout path can kill the whole command tree. Held across the await; on
    // timeout it is killed explicitly and, either way, dropped at scope end
    // (CloseHandle → KILL_ON_JOB_CLOSE). Job setup failure is non-fatal — the
    // command still runs, timeout just falls back to killing the child only.
    // Compiled and validated only on windows-latest CI.
    #[cfg(windows)]
    let job = match child.raw_handle() {
        Some(handle) => crate::backend_claude::win_job::JobHandle::create_and_assign(handle)
            .map_err(|e| {
                tracing::warn!(error = %e, "failed to create Job Object for shell command; \
                    timeout will kill only the spawned child");
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
            // Windows: TerminateJobObject kills the whole tree now (dropping
            // `job` at scope end would also do it via KILL_ON_JOB_CLOSE, but
            // the explicit kill is deterministic).
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
///
/// The gate env intentionally retains ambient `HOME`/`CI`/temp dirs (the
/// operator's toolchain shape — see `agent_env`'s module doc), but NOT the
/// ambient `CARGO_HOME`: gate commands execute worker-authored build scripts
/// and test binaries engine-side, and the real Cargo root carries registry
/// credentials and credential-provider config.
/// It is replaced with a fresh cache-only home (registry/git seeded as
/// per-env copies — clonefile/reflink/plain — never credentials;
/// [`crate::agent_env::cache_only_cargo_home`]) over a temp scratch that
/// self-cleans when the gate returns. The
/// substitution FAILS CLOSED: no scratch, no gate run — running with the
/// ambient Cargo root is the hole this exists to close.
///
/// This is the UNSANDBOXED executor — today's exact behavior, kept for the
/// `enforce == off` posture. When the merged mission's
/// `worker.sandbox.enforce` is not `off`, the server routes to
/// [`run_bounded_gate_command_sandboxed`] instead.
pub fn run_bounded_gate_command(cwd: &std::path::Path, command: &str) -> (bool, String) {
    // cache_only_cargo_home creates a fresh unpredictable dir under the
    // given base; the system temp dir keeps it out of the gated worktree
    // (an untracked `.cargo-cache-only-*` at the root would dirty every
    // gate's `git status`). The dir holds the seeded registry/git cache
    // copies plus whatever Cargo drops at its root; it is removed after the
    // run.
    let cargo_home = crate::agent_env::cache_only_cargo_home(std::env::temp_dir().as_path());
    if !cargo_home.is_dir() {
        return (
            false,
            format!(
                "could not create the gate's cache-only Cargo home at {}",
                cargo_home.display()
            ),
        );
    }
    let mut env = sanitized_gate_env();
    env.insert("CARGO_HOME".to_string(), cargo_home.display().to_string());
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
    let _ = std::fs::remove_dir_all(&cargo_home);
    (code == Some(0), output)
}

/// What the merge-gate path needs to wrap its gates (ticket
/// engine-gates-sandbox-wrapped): the MERGED mission's `worker.sandbox`
/// config (the gates execute that mission's worker-authored test/build code,
/// so the worker role's posture is the right one — the same choice the
/// sandbox preflight makes for contract-command probes) and the mission dir
/// the metadata write-denies / authority read-denies derive from. The server
/// builds one per merge from the folded event state; the gate cwd is only
/// known per command, so the profile resolution itself happens per command
/// inside [`run_bounded_gate_command_sandboxed`].
pub struct MergeGatePolicy {
    pub sandbox: crate::types::SandboxConfig,
    pub mission_dir: std::path::PathBuf,
}

impl MergeGatePolicy {
    /// The no-enforcement policy: gates run exactly as before the wrap.
    pub fn disabled() -> Self {
        MergeGatePolicy {
            sandbox: crate::types::SandboxConfig::default(),
            mission_dir: std::path::PathBuf::new(),
        }
    }

    /// Whether resolution on THIS host yields an enforced wrap — the cheap
    /// pre-check callers use to choose between the sandboxed runner and their
    /// pre-existing executor seam. `false` for `enforce: off`, for platforms
    /// [`crate::sandbox::platform_support`] cannot honor, and for
    /// `provider: container` (both documented no-ops — see
    /// [`resolve_gate_sandbox`]). Linux WITHOUT `bwrap` still returns `true`:
    /// requested-but-missing tooling fails CLOSED at resolve time, mirroring
    /// session resolution.
    pub fn enforces_on_this_host(&self) -> bool {
        self.sandbox.provider == crate::types::SandboxProvider::Process
            && matches!(
                crate::sandbox::platform_support(self.sandbox.enforce, std::env::consts::OS),
                crate::sandbox::SandboxDecision::Enforce(_)
            )
    }
}

/// [`run_bounded_gate_command`] under a [`MergeGatePolicy`] (ticket
/// engine-gates-sandbox-wrapped). A non-enforcing policy delegates to
/// [`run_bounded_gate_command`] unchanged — the byte-identical off path. An
/// enforcing policy runs the gate inside the resolved profile with:
///
/// - the SAME sanitized env, ambient `HOME` included — under the profile the
///   real home is simply outside the writable roots, i.e. a READ-ONLY home:
///   `~/.gitconfig` identity reads keep working (probed under Seatbelt; see
///   `gate_sandbox_wrap_merge_gate_reads_git_identity_from_read_only_home`),
///   while writes to `$HOME` are denied. That replaces the ticket's
///   open question — no HOME redirect is needed, so the pass-through stays
///   and the profile does the containment;
/// - `TMPDIR`/`TMP`/`TEMP` redirected into a fresh per-run scratch
///   (`kranz-gate-<uuid>/tmp`): the ambient temp dir is deliberately NOT in
///   the writable roots (sandbox-writable-scope parity — the shared temp
///   root holds every sibling mission's worktrees), and a gate that cannot
///   write temp files fails in opaque ways;
/// - the cache-only Cargo home created INSIDE that scratch (the unsandboxed
///   path places it directly under the system temp root, which the profile
///   denies);
/// - the whole scratch — Seatbelt profile file included — removed after the
///   run, and every setup failure failing CLOSED (no scratch, no profile, no
///   gate run — never a silent unsandboxed fallback under enforcement).
pub fn run_bounded_gate_command_sandboxed(
    cwd: &std::path::Path,
    command: &str,
    policy: &MergeGatePolicy,
) -> (bool, String) {
    if !policy.enforces_on_this_host() {
        return run_bounded_gate_command(cwd, command);
    }
    let scratch =
        std::env::temp_dir().join(format!("kranz-gate-{}", uuid::Uuid::new_v4().simple()));
    if std::fs::create_dir_all(scratch.join("tmp")).is_err() {
        return (
            false,
            format!(
                "could not create the gate's sandbox scratch at {}",
                scratch.display()
            ),
        );
    }
    let cargo_home = crate::agent_env::cache_only_cargo_home(scratch.as_path());
    if !cargo_home.is_dir() {
        let _ = std::fs::remove_dir_all(&scratch);
        return (
            false,
            format!(
                "could not create the gate's cache-only Cargo home at {}",
                cargo_home.display()
            ),
        );
    }
    let resolution = match resolve_gate_sandbox(
        &policy.sandbox,
        cwd,
        &policy.mission_dir,
        &scratch,
        &scratch,
    ) {
        Ok(resolution) => resolution,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&scratch);
            return (
                false,
                format!("could not resolve the gate sandbox (failing closed): {error}"),
            );
        }
    };
    if let Some(note) = &resolution.note {
        // Unreachable today (enforces_on_this_host excludes every noted
        // posture); kept so a future posture can never degrade silently.
        tracing::warn!(note = %note, "merge gate sandbox degraded to a no-op");
    }
    let mut env = sanitized_gate_env();
    env.insert("CARGO_HOME".to_string(), cargo_home.display().to_string());
    for var in ["TMPDIR", "TMP", "TEMP"] {
        env.insert(var.to_string(), scratch.join("tmp").display().to_string());
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&scratch);
            return (false, format!("failed to create gate runtime: {error}"));
        }
    };
    let (code, output) = runtime.block_on(run_shell_command_sandboxed_with_code(
        cwd,
        command,
        COMMAND_TIMEOUT,
        &env,
        &resolution.sandbox,
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    (code == Some(0), output)
}

fn sanitized_gate_env() -> HashMap<String, String> {
    // Keep only process/toolchain location and locale values. In particular,
    // API keys, GitHub/Slack tokens, cloud credentials, SSH agent sockets and
    // arbitrary server configuration never cross into mission-authored tests.
    // `CARGO_HOME` is deliberately ABSENT from this list — the caller
    // substitutes a cache-only home (see `run_bounded_gate_command`); the
    // ambient Cargo root is a credential directory.
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
        assert!(
            !env.contains_key("CARGO_HOME"),
            "the ambient Cargo root is a credential directory; \
             run_bounded_gate_command substitutes a cache-only home"
        );
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
                | "RUSTUP_HOME"
                | "NPM_CONFIG_CACHE"
                | "CI"
                | "TERM"
                | "LANG"
                | "LC_ALL"
                | "TZ"
        )));
    }

    /// contract-cargo-home-cache-only: the merge gate's `CARGO_HOME` is a
    /// fresh cache-only home — registry/git caches seeded, NO credentials —
    /// never the ambient Cargo root. Gate commands run worker-authored test
    /// code engine-side and unsandboxed, so this is the link that keeps
    /// registry tokens out of mission-authored code.
    #[cfg(unix)]
    #[test]
    fn contract_cargo_home_replaces_ambient_root_in_merge_gates() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("registry")).unwrap();
        std::fs::write(source.path().join("registry/cache-marker"), "registry").unwrap();
        std::fs::write(source.path().join("credentials.toml"), "operator-secret").unwrap();
        let _guard = crate::agent_env::EnvTestGuard::engage(&[(
            "CARGO_HOME",
            source.path().to_str().expect("utf-8 temp path"),
        )]);
        let dir = tempfile::tempdir().unwrap();

        let (ok, output) = run_bounded_gate_command(
            dir.path(),
            "printf '%s' \"$CARGO_HOME\" \
             && test -f \"$CARGO_HOME/registry/cache-marker\" \
             && test ! -e \"$CARGO_HOME/credentials.toml\"",
        );
        assert!(
            ok,
            "gate command must see a seeded, credential-free Cargo home: {output}"
        );
        assert!(
            !output.is_empty() && output != source.path().to_string_lossy().as_ref(),
            "the gate must NOT receive the ambient Cargo root: {output}"
        );
    }
    /// agent-env-clear: a contract command run through the final-gate path
    /// (`run_shell_command`, env built by `contract_command_env`) cannot see
    /// poisoned ambient secrets — but does see PATH, the per-mission scratch
    /// HOME, KRANZ_BASE_SHA, the real rustup toolchain, and an isolated
    /// cache-only Cargo home.
    #[cfg(unix)]
    #[tokio::test]
    async fn contract_command_cannot_see_ambient_secrets() {
        let _poison = crate::agent_env::EnvTestGuard::engage(&[
            ("GH_TOKEN", "hunter2"),
            ("SLACK_BOT_TOKEN", "x"),
            ("AWS_SECRET_ACCESS_KEY", "y"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let env = crate::agent_env::contract_command_env(scratch.path(), Some("deadbeef"), &[]);

        // Probed by name: the poisoned vars are really unset in the child.
        let (ok, output) = run_shell_command(
            dir.path(),
            "test -z \"$GH_TOKEN\" && test -z \"$SLACK_BOT_TOKEN\" && test -z \"$AWS_SECRET_ACCESS_KEY\"",
            &env,
        )
        .await;
        assert!(
            ok,
            "poisoned ambient vars reached the contract command: {output}"
        );

        // A full env dump shows exactly the contract boundary.
        let (ok, dump) = run_shell_command(dir.path(), "env", &env).await;
        assert!(ok, "{dump}");
        for leaked in [
            "GH_TOKEN",
            "SLACK_BOT_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "hunter2",
        ] {
            assert!(
                !dump.contains(leaked),
                "contract env leaked {leaked}:\n{dump}"
            );
        }
        assert!(dump.contains("PATH="), "PATH must cross:\n{dump}");
        assert!(
            dump.contains(&format!("HOME={}", scratch.path().display())),
            "HOME must be the per-mission scratch:\n{dump}"
        );
        assert!(
            dump.contains("KRANZ_BASE_SHA=deadbeef"),
            "base sha must reach the contract env:\n{dump}"
        );
        let cargo_home = env.get("CARGO_HOME").expect("CARGO_HOME");
        assert!(
            std::path::Path::new(cargo_home).starts_with(scratch.path()),
            "contract CARGO_HOME must live under mission scratch: {cargo_home}"
        );
        assert!(
            dump.contains(&format!("CARGO_HOME={cargo_home}")),
            "cache-only Cargo home must reach the child:\n{dump}"
        );
    }

    /// agent-env-clear design 4: `contractEnvPassthrough` admits EXACTLY the
    /// named ambient var — and only when configured.
    #[cfg(unix)]
    #[tokio::test]
    async fn contract_env_passthrough_admits_only_the_named_var() {
        let _guard = crate::agent_env::EnvTestGuard::engage(&[
            ("KRANZ_CONTRACT_TEST_CRED", "cred-value"),
            ("GH_TOKEN", "hunter2"),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();

        // Not configured: the var does NOT cross.
        let env = crate::agent_env::contract_command_env(scratch.path(), None, &[]);
        let (ok, output) =
            run_shell_command(dir.path(), "test -z \"$KRANZ_CONTRACT_TEST_CRED\"", &env).await;
        assert!(
            ok,
            "an unconfigured var must not reach the contract env: {output}"
        );

        // Configured: exactly that var crosses, with its value; GH_TOKEN
        // still does not.
        let env = crate::agent_env::contract_command_env(
            scratch.path(),
            None,
            &["KRANZ_CONTRACT_TEST_CRED".to_string()],
        );
        let (ok, output) = run_shell_command(
            dir.path(),
            "test \"$KRANZ_CONTRACT_TEST_CRED\" = cred-value && test -z \"$GH_TOKEN\"",
            &env,
        )
        .await;
        assert!(
            ok,
            "the passthrough-named var must cross, nothing else: {output}"
        );
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

    /// The preflight argv runner shares the bounded core: a timeout SIGKILLs
    /// the whole process GROUP, not just the direct child — a backgrounded
    /// grandchild must not survive. Unix-only (`kill(-pgid)`); the Windows
    /// equivalent goes through the kill-on-close Job Object in
    /// `run_command_bounded`, validated by windows-latest CI.
    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_argv_timeout_kills_the_whole_process_tree() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("child.pid");
        let script = format!("sleep 300 & echo $! > '{}'; wait", pidfile.display());
        let env = std::collections::HashMap::new();

        let (code, output) = tokio::time::timeout(
            Duration::from_secs(10),
            run_bounded_argv(
                dir.path(),
                std::path::Path::new("/bin/sh"),
                &["-c".to_string(), script],
                Duration::from_millis(500),
                &env,
            ),
        )
        .await
        .expect("timed-out command must return promptly");
        assert_eq!(code, None, "a timeout yields no exit code: {output}");
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

    /// The preflight argv runner drains both pipes CONCURRENTLY with the
    /// wait: a command emitting far more than the 64KB pipe buffer completes
    /// instead of deadlocking, and only the capped tail is retained. Real
    /// exit codes pass through (`Some(3)`), `Some(0)` stays the only success.
    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_argv_drains_large_output_and_reports_exit_codes() {
        let dir = tempfile::tempdir().unwrap();
        let env = std::collections::HashMap::new();
        let big = "i=0; while [ \"$i\" -lt 20000 ]; do \
                   printf '0123456789abcdef0123456789abcdef\\n'; \
                   i=$((i + 1)); done; printf 'OUTPUT-END'";

        let (code, output) = run_bounded_argv(
            dir.path(),
            std::path::Path::new("/bin/sh"),
            &["-c".to_string(), big.to_string()],
            Duration::from_secs(10),
            &env,
        )
        .await;

        assert_eq!(
            code,
            Some(0),
            "large-output command must complete: {output}"
        );
        assert!(output.ends_with("OUTPUT-END"), "{output}");
        assert!(
            output.chars().count() <= COMMAND_OUTPUT_TAIL,
            "retained output exceeded the cap: {} chars",
            output.chars().count()
        );

        let (code, output) = run_bounded_argv(
            dir.path(),
            std::path::Path::new("/bin/sh"),
            &["-c".to_string(), "exit 3".to_string()],
            Duration::from_secs(10),
            &env,
        )
        .await;
        assert_eq!(code, Some(3), "{output}");
    }

    // -----------------------------------------------------------------------
    // Gate sandbox wrap (ticket engine-gates-sandbox-wrapped). The
    // enforcement tests spawn the real platform sandbox (sandbox-exec /
    // bwrap) and skip cleanly where it cannot apply — the same posture as
    // crate::sandbox's own enforcement tests.
    // -----------------------------------------------------------------------

    /// Serializes the enforcement probes below (sandbox-exec/bwrap spawn
    /// contention made these flaky unguarded — mirrors
    /// `crate::sandbox`'s SANDBOX_EXEC_TEST_LOCK).
    #[cfg(unix)]
    static GATE_SANDBOX_WRAP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(target_os = "macos")]
    fn gate_wrap_sandbox_exec_can_apply() -> bool {
        let found = std::process::Command::new("which")
            .arg("sandbox-exec")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !found {
            eprintln!("sandbox-exec not found on this host; skipping");
            return false;
        }
        let smoke = std::process::Command::new("sandbox-exec")
            .arg("-p")
            .arg("(version 1)\n(allow default)\n")
            .arg("/usr/bin/true")
            .output();
        match smoke {
            Ok(output) if output.status.success() => true,
            Ok(output) => {
                eprintln!(
                    "sandbox-exec cannot apply a smoke profile on this host; skipping: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                false
            }
            Err(e) => {
                eprintln!("sandbox-exec smoke probe failed; skipping: {e}");
                false
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn gate_wrap_bwrap_can_apply() -> bool {
        if !crate::sandbox::command_available("bwrap") {
            eprintln!("bwrap not found on this host; skipping");
            return false;
        }
        let smoke = std::process::Command::new("bwrap")
            .args([
                "--die-with-parent",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--",
                "/bin/true",
            ])
            .output();
        match smoke {
            Ok(output) if output.status.success() => true,
            Ok(output) => {
                eprintln!(
                    "bwrap cannot apply a smoke sandbox on this host; skipping: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                false
            }
            Err(e) => {
                eprintln!("bwrap smoke probe failed; skipping: {e}");
                false
            }
        }
    }

    /// Whether THIS host can apply the resolved gate wrap (the enforcement
    /// tests skip where it cannot — CI linux runners may lack bwrap).
    #[cfg(unix)]
    fn gate_wrap_enforcement_available() -> bool {
        #[cfg(target_os = "macos")]
        {
            gate_wrap_sandbox_exec_can_apply()
        }
        #[cfg(target_os = "linux")]
        {
            gate_wrap_bwrap_can_apply()
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            false
        }
    }

    fn fs_sandbox_config(enforce: crate::types::SandboxEnforce) -> crate::types::SandboxConfig {
        crate::types::SandboxConfig {
            enforce,
            provider: crate::types::SandboxProvider::Process,
            image: None,
            extra_write: vec![],
            egress: vec![],
        }
    }

    /// A repo-shaped layout for the gate wrap probes: `<repo>/.kranz` with
    /// authority material, `<repo>/.kranz/missions/m-gate` with engine-owned
    /// metadata, and a public file — the checkout-mode hostile shape where
    /// the gate cwd is an ANCESTOR of the mission dir.
    #[cfg(unix)]
    fn gate_wrap_layout() -> (tempfile::TempDir, std::path::PathBuf) {
        let repo = tempfile::tempdir().unwrap();
        let kranz_dir = repo.path().join(".kranz");
        let mission = kranz_dir.join("missions").join("m-gate");
        std::fs::create_dir_all(mission.join("runs")).unwrap();
        std::fs::create_dir_all(mission.join("control")).unwrap();
        std::fs::write(mission.join("events.jsonl"), "{\"seq\":1}\n").unwrap();
        std::fs::write(mission.join("state.json"), "{}").unwrap();
        for name in ["serve.token", "serve.read.token", "config.json"] {
            std::fs::write(kranz_dir.join(name), "secret").unwrap();
        }
        std::fs::write(repo.path().join("public.txt"), "public").unwrap();
        (repo, mission)
    }

    /// The resolve matrix, pure and cross-platform: off stays disabled (no
    /// note), macOS resolves Seatbelt (profile file written, `/dev/null`
    /// allow appended, denies + writable roots in shape), linux resolves
    /// Bubblewrap and fails CLOSED without bwrap, an unsupported platform
    /// and the container provider degrade to documented no-ops.
    #[test]
    fn gate_sandbox_wrap_resolve_matrix() {
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let off = crate::types::SandboxConfig::default();
        let fs = fs_sandbox_config(crate::types::SandboxEnforce::Fs);

        // off → Disabled, no note, on every platform.
        let resolution = resolve_gate_sandbox_target(
            &off,
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "macos",
            false,
        )
        .unwrap();
        assert!(matches!(resolution.sandbox, GateSandbox::Disabled));
        assert!(resolution.note.is_none());

        // fs on macOS → Seatbelt: profile written, gate device allow
        // appended, session denies/writable roots reused.
        let resolution = resolve_gate_sandbox_target(
            &fs,
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "macos",
            false,
        )
        .unwrap();
        assert!(resolution.note.is_none());
        let GateSandbox::Seatbelt {
            enforce,
            profile_path,
        } = &resolution.sandbox
        else {
            panic!("fs on macOS must resolve to Seatbelt");
        };
        assert_eq!(*enforce, crate::types::SandboxEnforce::Fs);
        let profile = std::fs::read_to_string(profile_path).unwrap();
        assert!(profile.contains("(deny default)"), "{profile}");
        assert!(
            profile.contains("(allow file-write* (literal \"/dev/null\"))"),
            "the gate profile must add the /dev/null device write allow:\n{profile}"
        );
        #[cfg(target_os = "macos")]
        {
            let temp = std::env::temp_dir();
            let expected = format!(
                "(regex #\"^{}/xcrun_db[^/]*$\")",
                crate::sandbox::escape_sbpl_regex(&crate::sandbox::absolutize(&temp))
            );
            assert!(
                profile.contains(&expected),
                "the gate profile must add the name-anchored xcrun_db cache allow:\n{profile}"
            );
        }
        assert!(
            profile.contains("events.jsonl"),
            "mission metadata write denies must ride along:\n{profile}"
        );
        assert!(
            profile.contains("serve.token"),
            "authority read denies must ride along:\n{profile}"
        );

        // fs on linux with bwrap → Bubblewrap inputs shaped like the gate.
        let resolution = resolve_gate_sandbox_target(
            &fs,
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "linux",
            true,
        )
        .unwrap();
        let GateSandbox::Bubblewrap { inputs } = &resolution.sandbox else {
            panic!("fs on linux with bwrap must resolve to Bubblewrap");
        };
        assert_eq!(inputs.session_cwd, repo.path());
        assert_eq!(inputs.tmpdir, scratch.path());
        assert_eq!(inputs.mission_dir, mission);

        // fs on linux WITHOUT bwrap → fail closed, naming bwrap (mirrors
        // session resolution; never a silent unsandboxed gate).
        let error = resolve_gate_sandbox_target(
            &fs,
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "linux",
            false,
        )
        .expect_err("linux without bwrap must fail closed");
        assert!(error.to_string().contains("bwrap"), "{error}");

        // fs on an unsupported platform → Disabled with a documented note
        // (sessions already refuse there; only standalone merge gates reach
        // this posture).
        let resolution = resolve_gate_sandbox_target(
            &fs,
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "windows",
            false,
        )
        .unwrap();
        assert!(matches!(resolution.sandbox, GateSandbox::Disabled));
        let note = resolution.note.expect("unsupported platform must be noted");
        assert!(note.contains("UNSANDBOXED"), "{note}");

        // provider:container → Disabled with a documented note (tier-3 wraps
        // sessions; container-wrapping gates is out of scope).
        let container = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::Fs,
            provider: crate::types::SandboxProvider::Container,
            image: None,
            extra_write: vec![],
            egress: vec![],
        };
        let resolution = resolve_gate_sandbox_target(
            &container,
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "macos",
            false,
        )
        .unwrap();
        assert!(matches!(resolution.sandbox, GateSandbox::Disabled));
        let note = resolution.note.expect("container provider must be noted");
        assert!(note.contains("container"), "{note}");
    }

    /// The off regression: `enforce == off` resolves to
    /// [`GateSandbox::Disabled`], and a command through the Disabled wrap
    /// behaves BYTE-IDENTICALLY to the pre-wrap runner — including a write
    /// OUTSIDE any allowlist succeeding (today's documented posture).
    #[cfg(unix)]
    #[tokio::test]
    async fn gate_sandbox_wrap_off_keeps_byte_identical_behavior() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let env = std::collections::HashMap::new();

        let resolution = resolve_gate_sandbox(
            &crate::types::SandboxConfig::default(),
            dir.path(),
            dir.path(),
            dir.path(),
            dir.path(),
        )
        .unwrap();
        assert!(matches!(resolution.sandbox, GateSandbox::Disabled));
        assert!(resolution.note.is_none());

        let marker = outside.path().join("gate_sandbox_wrap_off_marker");
        let command = format!("echo hi > '{}' && printf MARKER", marker.display());
        let (ok_reference, out_reference) = run_shell_command(dir.path(), &command, &env).await;
        let (ok_wrapped, out_wrapped) =
            run_shell_command_sandboxed(dir.path(), &command, &env, &GateSandbox::Disabled).await;
        assert!(ok_reference, "reference run failed: {out_reference}");
        assert!(ok_wrapped, "disabled wrap run failed: {out_wrapped}");
        assert_eq!(
            out_reference, out_wrapped,
            "the Disabled wrap must reproduce the pre-wrap runner byte-for-byte"
        );
        assert!(
            marker.exists(),
            "with enforce == off a write outside any allowlist succeeds (today's posture)"
        );
    }

    /// The ticket's core test gate: a contract command run under
    /// `enforce != off` provably executes INSIDE the profile. A write outside
    /// the allowlist (a sibling temp dir, and a file directly in the SHARED
    /// system temp root — the sibling-of-scratch case
    /// `sandbox-writable-scope` closed) FAILS under enforcement and SUCCEEDS
    /// with `enforce == off`; mission metadata writes are denied
    /// (Seatbelt) or evaporate into the bwrap masks with the host bytes
    /// untouched; a read of a denied authority path fails (`test -s` is the
    /// cross-backend probe: Seatbelt refuses the open, bwrap's /dev/null mask
    /// reads back empty). The `/dev/null` redirect probe guards the gate
    /// profile's device-write addition.
    ///
    /// The outside-write probes deliberately do NOT use the ambient `$HOME`:
    /// unrelated suite tests poison it concurrently (a test once read a
    /// tempdir-shaped `$HOME` here and the off-arm probe failed
    /// "No such file or directory"). The merge-gate HOME question has its own
    /// deterministic test below with a guarded fake HOME.
    // await_holding_lock: the std guard serializes real sandbox-exec/bwrap
    // spawns across tests; each #[tokio::test] runs on its own OS thread with
    // its own runtime, and the guard is only ever acquired at test start — a
    // blocked test has no awaits in flight yet, so no deadlock is possible.
    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn gate_sandbox_wrap_denies_outside_writes_metadata_and_authority_reads() {
        let _guard = GATE_SANDBOX_WRAP_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if !gate_wrap_enforcement_available() {
            return;
        }

        let (repo, mission) = gate_wrap_layout();
        let kranz_dir = repo.path().join(".kranz");
        let scratch = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        // A marker directly in the SHARED system temp root: a sibling of the
        // gate's scratch, never under a writable root.
        let temp_root_marker =
            std::env::temp_dir().join(format!("kranz-gate-wrap-{}", uuid::Uuid::new_v4()));

        let resolution = resolve_gate_sandbox(
            &fs_sandbox_config(crate::types::SandboxEnforce::Fs),
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
        )
        .unwrap();
        assert!(resolution.note.is_none());
        let sandbox = resolution.sandbox;
        assert!(sandbox.enforce() == crate::types::SandboxEnforce::Fs);
        let env = crate::agent_env::contract_command_env(scratch.path(), None, &[]);

        // Writable shape: the gate cwd and the private scratch stay writable.
        for allowed in [
            repo.path().join("src.txt"),
            scratch.path().join("notes.txt"),
        ] {
            let (ok, output) = run_shell_command_sandboxed(
                repo.path(),
                &format!("echo ok > '{}'", allowed.display()),
                &env,
                &sandbox,
            )
            .await;
            assert!(
                ok && allowed.exists(),
                "write inside the gate roots must succeed: {output}"
            );
        }

        // `/dev/null` redirects work (the gate profile's appended device
        // allow — without it `sh` fails the command at redirect setup).
        let (ok, output) =
            run_shell_command_sandboxed(repo.path(), "echo hi > /dev/null 2>&1", &env, &sandbox)
                .await;
        assert!(ok, "/dev/null redirect must succeed: {output}");

        // Writes OUTSIDE the allowlist fail under enforcement…
        let outside_file = outside.path().join("gate_sandbox_wrap_marker");
        for probe in [
            format!("echo x > '{}'", outside_file.display()),
            format!("echo x > '{}'", temp_root_marker.display()),
        ] {
            let (ok, output) =
                run_shell_command_sandboxed(repo.path(), &probe, &env, &sandbox).await;
            assert!(
                !ok,
                "write outside the allowlist must fail under enforcement: {probe}\n{output}"
            );
        }
        assert!(
            !outside_file.exists(),
            "denied write must not create the file"
        );
        assert!(
            !temp_root_marker.exists(),
            "denied temp-root write must not create the marker"
        );

        // Mission metadata: the write is denied (macOS) or evaporates into
        // the bwrap mask (linux) — either way the host bytes are untouched.
        let (ok, _) = run_shell_command_sandboxed(
            repo.path(),
            &format!(
                "echo tampered >> '{}'",
                mission.join("events.jsonl").display()
            ),
            &env,
            &sandbox,
        )
        .await;
        if cfg!(target_os = "macos") {
            assert!(!ok, "events.jsonl append must be denied under Seatbelt");
        }
        assert_eq!(
            std::fs::read_to_string(mission.join("events.jsonl")).unwrap(),
            "{\"seq\":1}\n",
            "the audit log must be untouched by the sandboxed gate"
        );
        let (ok, _) = run_shell_command_sandboxed(
            repo.path(),
            &format!(
                "echo x > '{}'",
                mission.join("control/approve.json").display()
            ),
            &env,
            &sandbox,
        )
        .await;
        if cfg!(target_os = "macos") {
            assert!(!ok, "control/ writes must be denied under Seatbelt");
        }
        assert!(
            std::fs::read_dir(mission.join("control"))
                .unwrap()
                .next()
                .is_none(),
            "the control inbox must stay empty on the host"
        );

        // Authority reads fail: Seatbelt refuses the open, bwrap masks the
        // content — `test -s` (non-empty) fails under both, while an ordinary
        // repo file still reads fine.
        for name in ["serve.token", "serve.read.token", "config.json"] {
            let (ok, output) = run_shell_command_sandboxed(
                repo.path(),
                &format!("test -s '{}'", kranz_dir.join(name).display()),
                &env,
                &sandbox,
            )
            .await;
            assert!(
                !ok,
                "a read of denied authority path .kranz/{name} must fail: {output}"
            );
        }
        let (ok, output) = run_shell_command_sandboxed(
            repo.path(),
            &format!("test -s '{}'", repo.path().join("public.txt").display()),
            &env,
            &sandbox,
        )
        .await;
        assert!(ok, "ordinary repo reads must keep working: {output}");

        // Anti-vacuity / the ticket's off arm: the SAME probes with
        // `enforce == off` succeed (the probe commands are valid; only the
        // profile denies them).
        let off_env = crate::agent_env::contract_command_env(scratch.path(), None, &[]);
        for probe in [
            format!("echo x > '{}'", outside_file.display()),
            format!("echo x > '{}'", temp_root_marker.display()),
            format!(
                "echo tampered >> '{}'",
                mission.join("events.jsonl").display()
            ),
            format!("test -s '{}'", kranz_dir.join("serve.token").display()),
        ] {
            let (ok, output) =
                run_shell_command_sandboxed(repo.path(), &probe, &off_env, &GateSandbox::Disabled)
                    .await;
            assert!(
                ok,
                "with enforce == off the probe succeeds (today's posture): {probe}\n{output}"
            );
        }
        // Undo the off-arm's metadata append so the layout stays honest, and
        // sweep the temp-root marker.
        std::fs::write(mission.join("events.jsonl"), "{\"seq\":1}\n").unwrap();
        let _ = std::fs::remove_file(&temp_root_marker);
    }

    /// The kill discipline reaches the whole tree THROUGH the wrapper: the
    /// sandbox wrapper (sandbox-exec/bwrap) leads the same new process group,
    /// so the timeout SIGKILL takes a backgrounded grandchild down with it.
    // await_holding_lock: see the note on
    // gate_sandbox_wrap_denies_outside_writes_metadata_and_authority_reads.
    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn gate_sandbox_wrap_timeout_kills_the_whole_process_tree() {
        let _guard = GATE_SANDBOX_WRAP_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if !gate_wrap_enforcement_available() {
            return;
        }

        let (repo, mission) = gate_wrap_layout();
        let scratch = tempfile::tempdir().unwrap();
        let resolution = resolve_gate_sandbox(
            &fs_sandbox_config(crate::types::SandboxEnforce::Fs),
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
        )
        .unwrap();
        let sandbox = resolution.sandbox;
        let env = crate::agent_env::contract_command_env(scratch.path(), None, &[]);

        let pidfile = scratch.path().join("child.pid");
        let command = format!("sleep 300 & echo $! > '{}'; wait", pidfile.display());
        let (code, output) = tokio::time::timeout(
            Duration::from_secs(15),
            run_shell_command_sandboxed_with_code(
                repo.path(),
                &command,
                Duration::from_millis(500),
                &env,
                &sandbox,
            ),
        )
        .await
        .expect("timed-out command must return promptly");
        assert_eq!(code, None, "a timeout yields no exit code: {output}");
        assert!(output.contains("timed out"), "got: {output}");

        let pid: i32 = std::fs::read_to_string(&pidfile)
            .expect("the wrapped shell wrote the background pid before the timeout")
            .trim()
            .parse()
            .expect("pidfile contains a pid");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while unsafe { libc::kill(pid, 0) } == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "background child {pid} survived the group kill through the sandbox wrapper"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The merge-gate HOME question (ticket open work), settled by evidence:
    /// under the profile the ambient-HOME pass-through is KEPT and the
    /// profile makes the real home READ-ONLY — `git config user.name` still
    /// resolves from `~/.gitconfig` while `touch $HOME/...` is denied. Also
    /// pinned: TMPDIR redirects into the per-run `kranz-gate-*` scratch (the
    /// ambient temp root is not writable) and the cache-only Cargo home
    /// lives inside that same scratch.
    #[cfg(unix)]
    #[test]
    fn gate_sandbox_wrap_merge_gate_reads_git_identity_from_read_only_home() {
        let _wrap_guard = GATE_SANDBOX_WRAP_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if !gate_wrap_enforcement_available() {
            return;
        }

        let (repo, mission) = gate_wrap_layout();
        let fake_home = tempfile::tempdir().unwrap();
        std::fs::write(
            fake_home.path().join(".gitconfig"),
            "[user]\n\tname = Gate Wrap Test\n",
        )
        .unwrap();
        let _home = crate::agent_env::EnvTestGuard::engage(&[(
            "HOME",
            fake_home.path().to_str().expect("utf-8 temp path"),
        )]);
        let policy = MergeGatePolicy {
            sandbox: fs_sandbox_config(crate::types::SandboxEnforce::Fs),
            mission_dir: mission.clone(),
        };
        assert!(policy.enforces_on_this_host());

        let (ok, output) = run_bounded_gate_command_sandboxed(
            repo.path(),
            "test \"$(git config user.name)\" = 'Gate Wrap Test' \
             && ! touch \"$HOME/gate_sandbox_wrap_marker\" \
             && case \"$TMPDIR\" in *kranz-gate-*/tmp) true ;; *) false ;; esac \
             && case \"$CARGO_HOME\" in *kranz-gate-*/.cargo-cache-only-*) true ;; *) false ;; esac",
            &policy,
        );
        assert!(
            ok,
            "git identity must read from the read-only HOME, $HOME writes must be \
             denied, and TMPDIR/CARGO_HOME must sit in the per-run scratch: {output}"
        );
        assert!(
            !fake_home.path().join("gate_sandbox_wrap_marker").exists(),
            "the denied $HOME write must not have created the marker"
        );

        // Anti-vacuity: the same $HOME write succeeds with enforcement off.
        let (ok, output) = run_bounded_gate_command_sandboxed(
            repo.path(),
            "touch \"$HOME/gate_sandbox_wrap_off_marker\"",
            &MergeGatePolicy::disabled(),
        );
        assert!(
            ok,
            "with enforce == off the $HOME write succeeds (today's posture): {output}"
        );
        let _ = std::fs::remove_file(fake_home.path().join("gate_sandbox_wrap_off_marker"));
    }

    /// The merge-gate off regression: a disabled policy delegates to the
    /// pre-wrap executor, so the gate shape is today's exactly — the
    /// cache-only Cargo home directly under the system temp root and the
    /// ambient TMPDIR untouched.
    #[cfg(unix)]
    #[test]
    fn gate_sandbox_wrap_disabled_merge_policy_matches_todays_gate_shape() {
        let dir = tempfile::tempdir().unwrap();
        // `dirname` normalizes the trailing slash macOS puts on $TMPDIR (and
        // therefore on std::env::temp_dir()); trim it for the comparison.
        let temp = std::env::temp_dir().display().to_string();
        let temp = temp.trim_end_matches('/');
        let ambient_tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "unset".to_string());
        let command = format!(
            "test \"$(dirname \"$CARGO_HOME\")\" = '{temp}' \
             && test \"${{TMPDIR:-unset}}\" = '{ambient_tmpdir}'"
        );
        let (ok, output) =
            run_bounded_gate_command_sandboxed(dir.path(), &command, &MergeGatePolicy::disabled());
        assert!(
            ok,
            "the off path must keep today's gate shape (cache-only home under the \
             system temp root, ambient TMPDIR): {output}"
        );
    }

    /// Under `fs+net` the wrapped gate is offline-by-cache:
    /// `CARGO_NET_OFFLINE=true` is injected so a cold cache fails with a
    /// clear cargo error instead of a kernel-denied socket (no egress proxy
    /// is wired for engine-side gates — see the module doc). `fs` and the
    /// Disabled posture leave the env untouched.
    #[test]
    fn gate_sandbox_wrap_fs_net_forces_cargo_offline() {
        let base: HashMap<String, String> = HashMap::new();
        let fs_net = GateSandbox::Seatbelt {
            enforce: crate::types::SandboxEnforce::FsNet,
            profile_path: std::path::PathBuf::from("/nonexistent"),
        };
        let env = gate_env_for_sandbox(&base, &fs_net);
        assert_eq!(
            env.get("CARGO_NET_OFFLINE").map(String::as_str),
            Some("true"),
            "fs+net gates run cargo offline-by-cache"
        );
        let fs = GateSandbox::Seatbelt {
            enforce: crate::types::SandboxEnforce::Fs,
            profile_path: std::path::PathBuf::from("/nonexistent"),
        };
        assert!(
            !gate_env_for_sandbox(&base, &fs).contains_key("CARGO_NET_OFFLINE"),
            "fs keeps full egress — no offline flag"
        );
        assert!(
            !gate_env_for_sandbox(&base, &GateSandbox::Disabled).contains_key("CARGO_NET_OFFLINE"),
            "the off path is byte-identical — no offline flag"
        );
        assert!(
            !base.contains_key("CARGO_NET_OFFLINE"),
            "the caller's env map is never mutated"
        );
    }

    /// MEASUREMENT HARNESS, not a CI gate (ticket
    /// engine-gates-sandbox-wrapped named a >~20% overhead as the opt-in
    /// threshold): times a real gate command through the merge-gate path,
    /// wrapped vs unwrapped, plus a `true` micro-benchmark isolating the
    /// per-spawn cost. Run manually:
    ///
    /// ```sh
    /// cargo test -p kranz-engine gate_sandbox_wrap_measure -- --ignored --nocapture
    /// KRANZ_GATE_MEASURE_CMD='cargo test --workspace' \
    ///   KRANZ_GATE_MEASURE_REPS=1 \
    ///   cargo test -p kranz-engine gate_sandbox_wrap_measure -- --ignored --nocapture
    /// ```
    ///
    /// The default payload skips the engine's own sandbox-hostile tests
    /// (probed 2026-08-03 under the wrap: 697 passed, 11 failed — every one
    /// of them a test of the sandbox/kill machinery itself): cross-process
    /// SIGKILL/`kill(pid,0)` liveness probes are EPERM under the session
    /// profile's `(allow signal (target self))` (the engine's own timeout
    /// kill is unaffected — it signals from OUTSIDE the sandbox), `ps`-based
    /// process identity likewise, and `sandbox_apply` from inside a sandbox
    /// is denied. Gate commands that self-manage process trees with signals
    /// are the one known shape the wrap cannot run; sessions live under the
    /// same clause, so it is parity, not a regression. kranz dogfooding its
    /// own full suite under enforcement would need those tests adapted —
    /// out of scope here.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "measurement harness — run manually, never a CI gate"]
    fn gate_sandbox_wrap_measure() {
        if !gate_wrap_sandbox_exec_can_apply() {
            return;
        }
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/engine has a repo-root ancestor")
            .to_path_buf();
        let payload = std::env::var("KRANZ_GATE_MEASURE_CMD").unwrap_or_else(|_| {
            "cargo test -p kranz-engine --lib -- \
             --skip timeout_kills \
             --skip kills_a_hung_binary \
             --skip approval_lint_runner_times_out_slow_command \
             --skip identity_token \
             --skip pid_reuse \
             --skip pool_checkpoint_hooks_disabled_against_planted_fsmonitor_and_hook \
             --skip sandbox_preflight_probes_disposable_worktree_not_primary"
                .to_string()
        });
        let reps: u32 = std::env::var("KRANZ_GATE_MEASURE_REPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);
        // The fake mission layout only feeds the deny computation — nothing
        // real is touched; the gate cwd (the repo root) is the writable root.
        let (_layout_guard, mission) = gate_wrap_layout();
        let policy = MergeGatePolicy {
            sandbox: fs_sandbox_config(crate::types::SandboxEnforce::Fs),
            mission_dir: mission,
        };

        let time = |label: &str, command: &str, wrapped: bool, reps: u32| {
            let mut samples = Vec::new();
            for _ in 0..reps {
                let start = std::time::Instant::now();
                let (ok, output) = if wrapped {
                    run_bounded_gate_command_sandboxed(&repo_root, command, &policy)
                } else {
                    run_bounded_gate_command(&repo_root, command)
                };
                let elapsed = start.elapsed();
                assert!(ok, "{label} run failed: {output}");
                samples.push(elapsed);
            }
            let total: Duration = samples.iter().sum();
            let mean = total / samples.len() as u32;
            let min = samples.iter().min().unwrap();
            println!("{label}: reps={reps} mean={mean:.3?} min={min:.3?} all={samples:?}");
            mean
        };

        let micro_unwrapped = time("micro  unwrapped (true)", "true", false, 50);
        let micro_wrapped = time("micro  wrapped   (true)", "true", true, 50);
        println!(
            "micro delta per spawn: {:?} ({:+.1}%)",
            micro_wrapped.saturating_sub(micro_unwrapped),
            (micro_wrapped.as_secs_f64() / micro_unwrapped.as_secs_f64() - 1.0) * 100.0
        );
        let gate_unwrapped = time("gate   unwrapped", &payload, false, reps);
        let gate_wrapped = time("gate   wrapped  ", &payload, true, reps);
        println!(
            "gate delta: {:?} ({:+.2}%) on `{}`",
            gate_wrapped.saturating_sub(gate_unwrapped),
            (gate_wrapped.as_secs_f64() / gate_unwrapped.as_secs_f64() - 1.0) * 100.0,
            payload
        );
    }
}
