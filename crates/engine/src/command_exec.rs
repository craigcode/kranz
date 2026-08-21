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
//! [`GateSandbox::Bubblewrap`] on Linux, and [`GateSandbox::AppContainer`]
//! on Windows — reusing `crate::sandbox`'s
//! writable-root computation, mission-metadata write denies, and authority
//! read denies. `enforce == off` (and the documented no-op postures below)
//! keeps the pre-wrap behavior byte-for-byte.
//!
//! The gate profile's writable shape is the gate's cwd (the worktree —
//! `target/` and everything else a build writes lives under it) plus a
//! private scratch (validation/final gate: the mission's `runs/contract-home`
//! the contract env already points HOME/TMPDIR/CARGO_HOME at; merge gate: a
//! per-run self-cleaning `kranz-gate-*` temp root). The gate profile also
//! appends one narrow extra the session profile lacks (see
//! [`gate_profile_extras`] for the evidence): a `/dev/null` write allow
//! (`deny default` otherwise rejects the redirects real gate scripts use
//! liberally — this repo's gascity merge-gate scripts alone carry 148 of
//! them). SBPL allows compose order-independently and denies still take
//! precedence, so the append cannot weaken the generated profile; the
//! agent-session profile itself is deliberately untouched.
//!
//! macOS xcrun posture (13th-pass review, P1 — prewarm + deny): the profile
//! used to append a name-anchored `xcrun_db*` write regex over the Darwin
//! per-user temp dir, because the xcrun shims behind `/usr/bin/git` et al.
//! refresh their tool-resolution cache there via confstr, IGNORING TMPDIR,
//! and a refresh under parallel spawns killed a wrapped `cargo test` with
//! EPERM. But that regex also let a wrapped gate WRITE the shared per-user
//! xcrun database — including the operator's existing one, a mutation
//! surface outside the mission that later developer-tool invocations rely
//! on. The regex is GONE: `prewarm_xcrun_cache_outside_sandbox` refreshes
//! the cache OUTSIDE the sandbox once per resolve (cheap, bounded,
//! failure-tolerant), and a shim refresh that still races stale inside the
//! sandbox now fails loudly with the shim's own EPERM — a documented edge,
//! never a silent hole. bwrap has no equivalent gap (`--dev /dev` covers
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
//! ## Container arm (ticket container-gate-wrapper)
//!
//! With `provider = "container"` and `enforce != off`, the gate command runs
//! INSIDE the mission container instead of on the host beside the
//! container-wrapped sessions: [`GateSandbox::Container`] builds a
//! `container_gate_run_args` argv (the same `run --rm -i --read-only` shape
//! agent sessions get — gate cwd rw, mission metadata ro, scratch rw,
//! authority files /dev/null-masked) and executes it through the same
//! bounded core, so timeout/tree-kill/drain discipline is identical. The
//! deltas from the session argv: the payload is `sh -c <command>`, the
//! container is NAMED so the timeout path can force-remove it (the bounded
//! core's group SIGKILL reaches the runtime client, not the in-container
//! tree — the daemon owns those processes), the gate's sanitized env crosses
//! via `-e` flags (a runtime client forwards no env), and the real Cargo
//! root is NEVER mounted (a credential directory; the gate's cache-only
//! `CARGO_HOME` under the rw scratch is forwarded instead — only the
//! credential-free `<cargo>/bin` shim dir crosses, alongside the read-only
//! rustup toolchain + npm cache the gate's toolchain resolution needs).
//! `fs+net` mirrors the session container's handling: empty egress →
//! `--network none` (the hard boundary); a non-empty list FAILS CLOSED at
//! resolve (no egress proxy exists engine-side, and the bridge would be
//! advisory-only — `config::validate` already refuses the pair up front).
//! A requested container with no runtime on PATH FAILS CLOSED at resolve,
//! mirroring session resolution (`runner::resolve_sandbox_or_refuse`) —
//! never a silent host-side gate under an enforced container config.
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
//!
//! ## Gate supervision policy (ticket gate-sandbox-supervision-dogfood)
//!
//! The wrap's initial posture was session-parity for process supervision:
//! `(allow signal (target self))`, no ps. kranz's OWN engine suite
//! legitimately spawns and supervises children (the sandbox/kill machinery
//! testing itself), so `cargo test --workspace` as a wrapped contract
//! command failed 11 self-referential tests (probed 2026-08-03) — a kranz
//! mission with process enforcement could not satisfy this repo's mandatory
//! gate. The fix is a gate-SPECIFIC policy, never a global widening (the
//! session profile generator is untouched; everything rides the
//! [`gate_profile_extras`] append seam):
//!
//! - `(allow signal (target same-sandbox))`: the wrapped gate may signal
//!   (kill / `kill(pid, 0)` / killpg) processes carrying its OWN sandbox
//!   label instance — precisely its descendant tree, hereditary across
//!   fork/exec — while launchd, unrelated same-uid host processes, and even
//!   sibling `sandbox-exec` invocations with the identical profile stay
//!   EPERM. Probe evidence is recorded in [`gate_profile_extras`].
//! - `proc_pidinfo`-first identity tokens (event_log.rs): `/bin/ps` is
//!   setuid root, and setuid exec is kernel-denied inside ANY sandbox
//!   (probed 2026-08-05 — EPERM even under `(allow default)`; not
//!   SBPL-expressible). The token path now reads `p_starttime` directly
//!   (ungated for same-uid pids, byte-identical rendering to `ps -o
//!   lstart=`), so lock-liveness probes work inside the wrap; the setuid ps
//!   spawn remains as the fallback for other-uid pids (pid 1).
//! - What NO policy can grant inside the wrap, so those suite tests skip
//!   with the detectable `SKIP-UNDER-WRAP (gate-sandbox-supervision-dogfood)`
//!   marker instead: executing `/bin/ps` at all (the ps-fixture tests), and
//!   nested `sandbox_apply` of any profile but the identical one (the
//!   preflight/sandbox-enforcement tests — kernel-denied regardless of
//!   SBPL content).
//!
//! The proving ground is a fixture, not a one-off:
//! `gate_sandbox_wrap_dogfood_supervision_workspace_suite` (ignored; run by
//! the `rust-macos-wrapped-suite` CI job) executes `cargo test --workspace`
//! through the real wrap and asserts a green exit, reporting the
//! skip-under-wrap marker count.

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
///
/// `all(test, unix)`: every caller is a unix-gated shell test — on Windows
/// test builds the function is dead code and clippy's `-D warnings` gates
/// it (run 30870594288).
#[cfg(all(test, unix))]
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
///
/// `all(test, unix)`: called only by [`run_shell_command`] and unix-gated
/// timeout tests — dead code on Windows test builds (same clippy class).
#[cfg(all(test, unix))]
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
/// still applies). Every enforced posture wraps: the process provider via
/// [`GateSandbox::Seatbelt`] (`sandbox-exec -f`) on macOS,
/// [`GateSandbox::Bubblewrap`] on Linux, or [`GateSandbox::AppContainer`] on
/// Windows, reusing `crate::sandbox`'s
/// writable-root computation, mission-metadata write denies, and authority
/// read denies; the container provider via [`GateSandbox::Container`] (the
/// mission container — ticket container-gate-wrapper). A platform
/// [`crate::sandbox::platform_support`] cannot honor, tooling that is
/// requested but missing (Linux without `bwrap`), and `provider: container`
/// with no runtime on PATH all FAIL CLOSED at resolve time (13th-pass
/// review, P1, and the container ticket: agent sessions already refuse to
/// run there; a standalone merge gate must fail loudly too, never run
/// unsandboxed under an enforced config).
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
    /// Windows stable AppContainer launcher. One resolved posture owns one
    /// disposable profile/ACL lease across its commands; every command still
    /// gets a private launch plan and an independently supervised process.
    AppContainer {
        inputs: Box<crate::sandbox::SandboxInputs>,
        #[cfg(windows)]
        context: crate::appcontainer_windows::AppContainerLaunchContext,
    },
    /// Tier-3 container: `<runtime> run --rm -i --read-only --name <name> …
    /// <image> sh -c <command>` (ticket container-gate-wrapper). Inputs and
    /// spec ride along because the argv — the gate's sanitized env included
    /// — is built per command.
    Container {
        inputs: Box<crate::sandbox::SandboxInputs>,
        spec: crate::sandbox_container::ContainerSpec,
    },
}

/// The `(program, args)` a resolved gate sandbox produces for one command,
/// plus the best-effort teardown the runner issues when the command did not
/// exit on its own.
pub(crate) struct WrappedCommand {
    pub program: std::path::PathBuf,
    pub args: Vec<String>,
    /// `<runtime> rm -f <name>` for the container arm: the bounded core's
    /// timeout SIGKILL reaches the runtime CLIENT's process group, but the
    /// in-container tree belongs to the daemon and can outlive the client
    /// (a parked `sleep 300` gate would otherwise run on, holding the rw
    /// mounts, until its command exits naturally). Force-removing the named
    /// container kills that tree. `None` for the process-sandbox arms —
    /// there the group SIGKILL IS the tree kill. Best-effort: a teardown
    /// failure (the runtime already reaped the container, an unsupported
    /// `rm -f`) is ignored, and `--rm` still reaps every normal exit.
    pub timeout_teardown: Option<(std::path::PathBuf, Vec<String>)>,
    /// Keeps the resolved posture's disposable profile and retained no-follow
    /// DACL handles alive through the wrapper process, even if its caller
    /// drops the `GateSandbox` first. Absent on non-Windows builds.
    #[cfg(windows)]
    _appcontainer_context: Option<crate::appcontainer_windows::AppContainerLaunchContext>,
}

impl GateSandbox {
    /// The enforcement level the wrap applies (`Off` when disabled) — the
    /// runner keys the fs+net offline-by-cache env adjustment on it.
    pub(crate) fn enforce(&self) -> crate::types::SandboxEnforce {
        match self {
            GateSandbox::Disabled => crate::types::SandboxEnforce::Off,
            GateSandbox::Seatbelt { enforce, .. } => *enforce,
            GateSandbox::Bubblewrap { inputs } => inputs.enforce,
            GateSandbox::AppContainer { inputs, .. } => inputs.enforce,
            GateSandbox::Container { inputs, .. } => inputs.enforce,
        }
    }

    /// Build the [`WrappedCommand`] that runs `command` under this posture.
    /// `Disabled` reproduces [`shell_argv`] EXACTLY, so the off path is
    /// byte-identical to the pre-wrap behavior. A bubblewrap mask-prep
    /// failure FAILS CLOSED — a gate that cannot be wrapped must not run
    /// unsandboxed under enforcement.
    ///
    /// `env` is the gate's FINAL (already sanitized, offline-adjusted)
    /// environment: the process-sandbox arms ignore it (their child inherits
    /// it from the bounded runner), but the container arm must bake it into
    /// the argv as `-e` flags — a runtime client forwards no env into the
    /// container.
    fn wrap_shell(
        &self,
        command: &str,
        env: &HashMap<String, String>,
    ) -> crate::error::Result<WrappedCommand> {
        match self {
            GateSandbox::Disabled => {
                let (program, args) = shell_argv(command);
                Ok(WrappedCommand {
                    program,
                    args,
                    timeout_teardown: None,
                    #[cfg(windows)]
                    _appcontainer_context: None,
                })
            }
            GateSandbox::Seatbelt { profile_path, .. } => {
                let (program, args) = crate::backend_claude::sandbox_command(
                    profile_path,
                    std::path::Path::new("/bin/sh"),
                    &["-c".to_string(), command.to_string()],
                );
                Ok(WrappedCommand {
                    program,
                    args,
                    timeout_teardown: None,
                    #[cfg(windows)]
                    _appcontainer_context: None,
                })
            }
            GateSandbox::Bubblewrap { inputs } => {
                let args = crate::sandbox::bubblewrap_args(
                    inputs,
                    std::path::Path::new("/bin/sh"),
                    &["-c".to_string(), command.to_string()],
                )?;
                Ok(WrappedCommand {
                    program: std::path::PathBuf::from("bwrap"),
                    args,
                    timeout_teardown: None,
                    #[cfg(windows)]
                    _appcontainer_context: None,
                })
            }
            GateSandbox::AppContainer {
                inputs,
                #[cfg(windows)]
                context,
            } => {
                #[cfg(windows)]
                {
                    let (program, args) = shell_argv(command);
                    let prepared = crate::appcontainer_windows::prepare_launch_in_context(
                        context, inputs, &program, &args, env,
                    )?;
                    Ok(WrappedCommand {
                        program: prepared.program,
                        args: prepared.args,
                        timeout_teardown: None,
                        _appcontainer_context: Some(context.clone()),
                    })
                }
                #[cfg(not(windows))]
                {
                    let _ = (inputs, command, env);
                    Err(crate::error::EngineError::Backend(
                        "AppContainer gate wrapper is unavailable on this host".to_string(),
                    ))
                }
            }
            GateSandbox::Container { inputs, spec } => {
                // Named per command (never per resolve): parallel gate
                // commands from one resolution must not collide on the name,
                // and the teardown below targets exactly this container.
                let name = format!("kranz-gate-{}", uuid::Uuid::new_v4().simple());
                let args = crate::sandbox_container::container_gate_run_args(
                    inputs, spec, command, env, &name,
                );
                Ok(WrappedCommand {
                    program: std::path::PathBuf::from(spec.runtime.binary()),
                    args,
                    timeout_teardown: Some((
                        std::path::PathBuf::from(spec.runtime.binary()),
                        vec!["rm".to_string(), "-f".to_string(), name],
                    )),
                    #[cfg(windows)]
                    _appcontainer_context: None,
                })
            }
        }
    }
}

/// The outcome of resolving a gate sandbox: the posture plus an optional
/// operator-facing note (surfaced as an orchestrator decision / merge log
/// line) should a future posture degrade to a no-op. Every CURRENT posture
/// either wraps (`note: None`) or fails closed at resolve (an Err naming the
/// missing support — an unsupported platform, linux without `bwrap`,
/// `provider:container` without a runtime): the note seam is kept so a
/// degraded no-op can never return SILENTLY — a posture that adds one must
/// also teach the callers to surface it.
#[derive(Debug)]
pub(crate) struct GateSandboxResolution {
    pub sandbox: GateSandbox,
    pub note: Option<String>,
    /// Whether the xcrun prewarm ran during THIS resolve (macOS Seatbelt
    /// arm only; always false elsewhere). Per-resolution state, so the
    /// once-per-resolve contract is assertable without a global counter —
    /// a process-wide counter races with parallel test threads resolving
    /// concurrently (rust-macos CI flake, run 30935850957). Read only by
    /// the macOS-gated test; everywhere else the field exists only to keep
    /// the resolution's shape platform-uniform.
    #[cfg_attr(not(all(test, target_os = "macos")), allow(dead_code))]
    pub prewarmed_xcrun: bool,
}

/// SBPL appended to the SESSION profile for gate use — never edited into
/// `crate::sandbox::generate_profile` (the agent-session profile is
/// deliberately untouched). SBPL allows compose order-independently and
/// denies still take precedence regardless of clause order (verified with
/// sandbox-exec), so appending cannot weaken the generated profile.
///
/// `(literal "/dev/null")` write allow: `deny default` otherwise rejects
/// `/dev/null` redirects (probed 2026-08-03: "Operation not permitted"),
/// which real gate lines and scripts use liberally (this repo's gascity
/// merge-gate scripts: 148 hits in one file).
///
/// 13th-pass review (P1): the macOS `xcrun_db*` write regex this function
/// used to append is GONE. It covered the shim cache refresh (see the
/// module doc), but it also let a wrapped gate WRITE the shared per-user
/// xcrun database — including the operator's existing one, a mutation
/// surface outside the mission. The replacement posture is prewarm + deny:
/// `prewarm_xcrun_cache_outside_sandbox` refreshes the cache unsandboxed
/// once per resolve, and a shim refresh that still races stale inside the
/// sandbox fails loudly with the shim's own EPERM (the documented edge).
///
/// `(allow signal (target same-sandbox))` — the gate-SPECIFIC supervision
/// policy (ticket gate-sandbox-supervision-dogfood). A wrapped gate runs
/// worker-authored build/test trees that legitimately spawn and supervise
/// their own descendants (timeout kills, process-group SIGKILL, `kill(pid,
/// 0)` liveness polls — kranz's OWN engine suite exercises exactly this, and
/// under the session parity clause `(allow signal (target self))` every one
/// of those probes is EPERM, so a kranz mission with process enforcement
/// could not satisfy this repo's mandatory `cargo test --workspace` gate).
/// `same-sandbox` scopes the allowance to processes carrying the SAME
/// sandbox label instance — precisely the wrapped tree (the label is
/// inherited across fork/exec and cannot be shed: applying a DIFFERENT
/// profile from inside is kernel-denied, so the posture is hereditary).
/// Probe evidence (2026-08-05, macOS 26.5.2, arm64, sandbox-exec):
///
/// - `kill`/`kill(pid, 0)`/`killpg` against children AND grandchildren
///   (the `sh -c` → background-child timeout-kill shape): allowed.
/// - `kill(pid, 0)` on a reaped child reports ESRCH, not EPERM, so
///   liveness-poll loops terminate correctly.
/// - launchd (pid 1), an unrelated same-uid host process, and a SIBLING
///   `sandbox-exec` invocation launched with the identical profile file:
///   all still EPERM — the scope is the sandbox instance (the tree), never
///   the profile content and never host-wide.
/// - `(target children)` was rejected as too narrow (direct children only;
///   grandchildren stay EPERM) and `(target others)` buys nothing (host
///   probes stay EPERM under it too) — `same-sandbox` is the only target
///   that covers exactly the descendant tree.
/// - What NO profile rule can grant (recorded so the gap is never
///   re-probed blindly): executing `/bin/ps` (setuid root on this host's
///   macOS — setuid exec is kernel-denied under ANY sandbox, even
///   `(allow default)`; a copied binary is AMFI-killed) and applying a
///   DIFFERENT nested profile (`sandbox_apply` EPERM regardless of
///   `process-exec` allowances; re-applying the IDENTICAL profile is a
///   permitted no-op). The suite's ps-fixture and nested-sandbox tests
///   therefore carry explicit skip-under-wrap markers instead — see the
///   module doc's supervision section. Process-info READS (`proc_pidinfo`)
///   were never sandbox-gated for same-uid targets and keep working under
///   `deny default` with no allowance at all (probed); only `/bin/ps`
///   itself is unreachable.
fn gate_profile_extras() -> String {
    // The pty device surface, probed 2026-08-06 under sandbox-exec (the
    // wrapped-suite failure: the three pty-driving tests died "out of pty
    // devices" inside the gate wrap). macOS pty allocation needs THREE
    // things the session profile's deny-default rejects: read+write on
    // /dev/ptmx (the multiplexer), read+write on the allocated slave node
    // (this host's pool names are BOTH /dev/tty[p-t]<hex> and the longer
    // /dev/ttysNNN — hence the `+`), and the grantpt/unlockpt ioctls —
    // `file-ioctl` is required for those two (proven: with it the whole
    // posix_openpt -> grantpt -> unlockpt -> ptsname -> slave-open chain
    // works; without it both ioctls EPERM). No ptmx, no pty: the harness
    // is validator tooling that deserves the same gate the rest of the
    // wrapped suite gets, not a skip.
    //
    // 14th-pass review (ticket gate-wrap-file-ioctl-unscoped): the ioctl
    // allow is SCOPED to exactly that pty surface — /dev/ptmx plus the
    // tty-slave regex — never the unrestricted `(allow file-ioctl)` every
    // wrapped gate used to get (an unscoped allow lets worker-authored gate
    // code ioctl any device it can open: terminal injection into the
    // operator's tty, TIOCSTI-class surfaces, disk ioctls). Re-probed
    // 2026-08-09 under sandbox-exec on macOS (arm64): the scoped shape
    // passes the full openpty + termios + TIOCSWINSZ + read/write chain
    // (PTY-OK, slave /dev/ttys003), and dropping the ioctl line entirely
    // EPERMs at openpty — the scoped filter is what the chain needs, no
    // more. The gate profile cannot know at resolve time whether the
    // contract carries pty assertions (merge gates never see one), so the
    // scoped lines ride every wrapped gate — the surface they open is the
    // pty device pair and nothing else.
    String::from(
        "\n(allow file-write* (literal \"/dev/null\") (literal \"/dev/ptmx\"))\n\
         (allow file-read* (literal \"/dev/ptmx\"))\n\
         (allow file-read* file-write* (regex #\"^/dev/tty[p-t][0-9a-f]+$\"))\n\
         (allow file-ioctl (literal \"/dev/ptmx\") (regex #\"^/dev/tty[p-t][0-9a-f]+$\"))\n\
         (allow signal (target same-sandbox))\n",
    )
}

/// Refresh the xcrun shims' tool-resolution cache OUTSIDE the sandbox, once
/// per gate-profile resolve (13th-pass review, P1 — prewarm + deny): the
/// gate profile no longer permits `xcrun_db` writes (see
/// [`gate_profile_extras`]), so the `/usr/bin/*` shims (git, clang, …)
/// behind a wrapped gate must find their cache FRESH in the Darwin per-user
/// temp dir (which they locate via confstr, IGNORING TMPDIR).
///
/// Per resolve, NOT per command: the cache is per-user and shared, so one
/// refresh covers every wrapped spawn the resolution produces. The probe is
/// `git --version` through the operator's PATH — on a stock macOS that IS
/// the `/usr/bin` shim, so the probe refreshes exactly the cache the gate's
/// shims consult. Bounded (10s), output discarded, spawn/exit status
/// ignored: a failed prewarm (no git, no dev tools, a shim that errors)
/// leaves the deny posture in force and the gate still runs — it just might
/// hit the loud edge (a stale-cache refresh inside the sandbox is EPERM,
/// surfaced as the shim's own error). That edge, and a brew-first PATH
/// whose `git` is not the shim, are the documented limits of the prewarm.
#[cfg(target_os = "macos")]
pub(crate) fn prewarm_xcrun_cache_outside_sandbox() {
    let _ = run_with_timeout(
        std::path::Path::new("git"),
        &["--version".to_string()],
        Duration::from_secs(10),
    );
}

/// The ONE note for the container provider's runtime-unavailable posture,
/// shared by [`resolve_gate_sandbox_target`] (whose Err the engine paths
/// surface — the gate refuses to run) and
/// [`MergeGatePolicy::degradation_note`] (which the server's merge path logs
/// once — it has no event log, and the gate run itself then fails closed at
/// resolve). The text must match on every path so an operator sees the SAME
/// explanation wherever the gate ran. Ticket container-gate-wrapper wraps
/// engine-run gates in the mission container whenever a runtime is detected;
/// this note is the fail-closed remainder: with no runtime on PATH the gate
/// must NOT degrade to a silent host-side run — container SESSIONS already
/// refuse to run unsandboxed there (`runner::resolve_sandbox_or_refuse`),
/// and engine-run gates mirror that posture.
fn container_gate_note(enforce: crate::types::SandboxEnforce) -> String {
    format!(
        "sandbox provider:container with enforce:{} wraps engine-run gates in the mission \
         container, but no container runtime (docker/podman/nerdctl/container) was found on \
         PATH; refusing to run engine-run gates unsandboxed (fail closed, mirroring container \
         session resolution) — install a runtime or set worker.sandbox.provider to \"process\"",
        enforce.as_str()
    )
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
        crate::sandbox_container::detect(),
    )
}

/// The gate-shaped [`crate::sandbox::SandboxInputs`], shared by every
/// enforced provider arm: the gate cwd fills the session profile's
/// `session_cwd` slot so the writable-root computation is REUSED, never
/// re-rolled; mission metadata write-denies and authority read-denies derive
/// from `mission_dir` exactly as sessions derive them; the validator
/// read-deny set is the validator-session wrap's, never a gate's (gates work
/// IN the real tree).
fn gate_sandbox_inputs(
    sandbox_cfg: &crate::types::SandboxConfig,
    gate_cwd: &std::path::Path,
    mission_dir: &std::path::Path,
    scratch_home: &std::path::Path,
) -> crate::sandbox::SandboxInputs {
    crate::sandbox::SandboxInputs {
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
        validator_read_deny_roots: Vec::new(),
    }
}

/// [`resolve_gate_sandbox`] parameterized on the target OS, bwrap
/// availability, and container runtime so the decision matrix is testable
/// cross-platform (mirrors `crate::sandbox::resolve_for_session_target`).
#[allow(clippy::too_many_arguments)]
fn resolve_gate_sandbox_target(
    sandbox_cfg: &crate::types::SandboxConfig,
    gate_cwd: &std::path::Path,
    mission_dir: &std::path::Path,
    scratch_home: &std::path::Path,
    profile_dir: &std::path::Path,
    target_os: &str,
    bwrap_available: bool,
    container_runtime: Option<crate::sandbox_container::ContainerRuntime>,
) -> crate::error::Result<GateSandboxResolution> {
    use crate::types::{SandboxEnforce, SandboxProvider};
    let disabled = |note: Option<String>| {
        Ok(GateSandboxResolution {
            sandbox: GateSandbox::Disabled,
            note,
            prewarmed_xcrun: false,
        })
    };
    if sandbox_cfg.enforce == SandboxEnforce::Off {
        return disabled(None);
    }
    if sandbox_cfg.provider == SandboxProvider::Container {
        // Ticket container-gate-wrapper: engine-run gates join the agent
        // sessions INSIDE the mission container. The fail postures mirror
        // session resolution exactly (`sandbox::resolve_container_target` +
        // `runner::resolve_sandbox_or_refuse`): a requested container with
        // no runtime on PATH is refused — never a silent host-side gate.
        // The container argv/mount contract is live-proven only for POSIX
        // container targets on macOS/Linux. On Windows, a detected
        // `docker.exe` says nothing about Linux-vs-Windows container mode,
        // guest path mapping, or the `/dev/null` authority masks. Refuse
        // before constructing an unverified gate command; session resolution
        // applies the identical posture.
        if !matches!(target_os, "macos" | "linux") {
            return Err(crate::error::EngineError::Config(format!(
                "sandbox provider:container with enforce:{} is not live-proven on target_os={target_os}; refusing to run engine-run gates under an unverified container mount contract",
                sandbox_cfg.enforce.as_str()
            )));
        }
        let Some(runtime) = container_runtime else {
            return Err(crate::error::EngineError::Config(container_gate_note(
                sandbox_cfg.enforce,
            )));
        };
        // `fs+net` with a non-empty egress list is proxy-env advisory on the
        // runtime bridge (no hard boundary), and engine-run gates are never
        // wired through the egress proxy (session infrastructure — see the
        // module doc). `config::validate` refuses the pair up front
        // (`SandboxProvider::enforces_hard_net_boundary`); this resolve
        // refuses it again so a standalone merge gate can never silently
        // bridge either.
        if sandbox_cfg.enforce == SandboxEnforce::FsNet
            && !sandbox_cfg
                .provider
                .enforces_hard_net_boundary(&sandbox_cfg.egress)
        {
            return Err(crate::error::EngineError::Config(
                "sandbox provider:container with enforce:fs+net and a non-empty egress list is \
                 advisory-only for engine-run gates (no egress proxy exists engine-side); use an \
                 empty egress list (the hard `--network none` boundary) or sandbox.provider \
                 \"process\" — refusing to run engine-run gates with an advisory boundary"
                    .to_string(),
            ));
        }
        return Ok(GateSandboxResolution {
            sandbox: GateSandbox::Container {
                inputs: Box::new(gate_sandbox_inputs(
                    sandbox_cfg,
                    gate_cwd,
                    mission_dir,
                    scratch_home,
                )),
                spec: crate::sandbox_container::ContainerSpec {
                    runtime,
                    image: sandbox_cfg
                        .image
                        .clone()
                        .unwrap_or_else(|| crate::sandbox_container::DEFAULT_IMAGE.to_string()),
                },
            },
            note: None,
            prewarmed_xcrun: false,
        });
    }
    match crate::sandbox::platform_support(sandbox_cfg.enforce, target_os) {
        // Unreachable (Off returns above) — platform_support is the shared
        // vocabulary, so the match stays exhaustive anyway.
        crate::sandbox::SandboxDecision::Off => disabled(None),
        // 13th-pass review (P1): FAIL CLOSED. Agent sessions already refuse
        // to run unsandboxed on an unsupported platform; a standalone merge
        // gate that resolved to Disabled here ran worker-authored code
        // unsandboxed under an enforced config — loudly is the only honest
        // posture.
        crate::sandbox::SandboxDecision::UnsupportedWarn => {
            Err(crate::error::EngineError::Config(format!(
                "sandbox enforce:{} requested but unsupported on target_os={target_os}; refusing \
                 to run engine-run gates unsandboxed",
                sandbox_cfg.enforce.as_str()
            )))
        }
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
            let inputs = gate_sandbox_inputs(sandbox_cfg, gate_cwd, mission_dir, scratch_home);
            match backend {
                crate::sandbox::SandboxBackend::Seatbelt => {
                    // 13th-pass (P1): the profile no longer permits xcrun_db
                    // writes, so refresh the shim cache OUTSIDE the sandbox
                    // once per resolve — never per command (the cache is
                    // per-user and shared; see prewarm's doc).
                    #[cfg(target_os = "macos")]
                    prewarm_xcrun_cache_outside_sandbox();
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
                        // The prewarm ran above (macOS-only call site).
                        prewarmed_xcrun: cfg!(target_os = "macos"),
                    })
                }
                crate::sandbox::SandboxBackend::Bubblewrap => Ok(GateSandboxResolution {
                    sandbox: GateSandbox::Bubblewrap {
                        inputs: Box::new(inputs),
                    },
                    note: None,
                    prewarmed_xcrun: false,
                }),
                crate::sandbox::SandboxBackend::AppContainer => Ok(GateSandboxResolution {
                    sandbox: GateSandbox::AppContainer {
                        inputs: Box::new(inputs),
                        #[cfg(windows)]
                        context: crate::appcontainer_windows::new_launch_context(),
                    },
                    note: None,
                    prewarmed_xcrun: false,
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

/// Prepare one gate command for execution OUTSIDE the bounded runner — the
/// pty harness (ticket `pty-functional-validation`) drives the wrapped argv
/// interactively, so it needs exactly what the bounded path computes per
/// command: the FINAL env (the fs+net offline-by-cache adjustment included,
/// which the container arm bakes into the argv) and the sandbox wrap (or its
/// fail-closed error). Keeping the pair computed here, in one place, means
/// a pty-driven assertion can never drift from the posture a bounded
/// contract command would get for the same command line.
pub(crate) fn prepare_gate_command(
    command: &str,
    env: &HashMap<String, String>,
    sandbox: &GateSandbox,
) -> crate::error::Result<(WrappedCommand, HashMap<String, String>)> {
    let env = gate_env_for_sandbox(env, sandbox);
    let wrapped = sandbox.wrap_shell(command, &env)?;
    Ok((wrapped, env))
}

/// The pre-wrap contract-command runner under a resolved gate sandbox:
/// validation-round contract commands, the final gate, and pack gates run
/// through here. [`GateSandbox::Disabled`] reproduces the pre-wrap `sh -c`
/// behavior byte-for-byte;
/// an enforced posture wraps the SAME `sh -c` in the resolved profile (or the
/// mission container — ticket container-gate-wrapper), and the wrapper still
/// leads the SAME new process group
/// ([`configure_bounded_child`]) — so the bounded core's timeout SIGKILL
/// reaches the whole tree, sandbox-exec/bwrap/the runtime client and every
/// descendant alike. The container arm additionally force-removes its NAMED
/// container when a run produced no exit code ([`WrappedCommand::timeout_teardown`]):
/// the group SIGKILL stops the runtime client, but the in-container tree
/// belongs to the daemon and would otherwise outlive the killed client.
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
    // The FINAL env first (the fs+net offline-by-cache adjustment included) —
    // the container arm bakes it into the argv as `-e` flags, so wrap_shell
    // must see the adjusted map, not the caller's original.
    let env = gate_env_for_sandbox(env, sandbox);
    let wrapped = match sandbox.wrap_shell(command, &env) {
        Ok(wrapped) => wrapped,
        Err(error) => {
            return (
                None,
                format!("gate sandbox wrap failed closed (the command did not run): {error}"),
            )
        }
    };
    let (code, output) =
        run_bounded_argv(cwd, &wrapped.program, &wrapped.args, timeout, &env).await;
    if code.is_none() {
        if let Some((program, args)) = wrapped.timeout_teardown {
            // Best-effort, bounded, off the async executor: a teardown
            // failure (the runtime already reaped the container) is ignored.
            let _ = tokio::task::spawn_blocking(move || {
                run_with_timeout(&program, &args, Duration::from_secs(30))
            })
            .await;
        }
    }
    (code, output)
}

/// Synchronous bounded runner for an already-resolved gate posture and its
/// complete cleared environment. Production validation/final-gate batches
/// resolve once and run several assertions through that same posture; the
/// native Windows normal-gate receipt uses this seam so its retained samples
/// measure per-command wrapping after the posture's one-time ACL preparation.
#[cfg(windows)]
pub(crate) fn run_bounded_gate_command_resolved_with_code(
    cwd: &std::path::Path,
    command: &str,
    env: &HashMap<String, String>,
    sandbox: &GateSandbox,
) -> (Option<i32>, String) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return (None, format!("failed to create gate runtime: {error}")),
    };
    runtime.block_on(run_shell_command_sandboxed_with_code(
        cwd,
        command,
        COMMAND_TIMEOUT,
        env,
        sandbox,
    ))
}

/// Synchronous bridge for gate execution from approval-time code that runs
/// inside an ambient Tokio runtime. The actual bounded/sandboxed executor is
/// async; attempting to build and `block_on` a second runtime on the caller's
/// runtime thread panics. A scoped OS thread owns the short-lived runtime,
/// while borrowed cwd/env/sandbox inputs remain valid until it joins.
///
/// `Some(code)` means the command reached an exit status; `None` covers
/// spawn/wrap failures, timeout/tree kill, signal termination, or runtime
/// setup failure. The output always carries the bounded diagnostic tail.
pub(crate) fn run_shell_command_sandboxed_blocking(
    cwd: &std::path::Path,
    command: &str,
    timeout: Duration,
    env: &HashMap<String, String>,
    sandbox: &GateSandbox,
) -> (Option<i32>, String) {
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    return (
                        None,
                        format!("failed to create approval gate runtime: {error}"),
                    )
                }
            };
            runtime.block_on(run_shell_command_sandboxed_with_code(
                cwd, command, timeout, env, sandbox,
            ))
        });
        worker.join().unwrap_or_else(|_| {
            (
                None,
                "approval gate runner panicked before producing a verdict".to_string(),
            )
        })
    })
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
    /// pre-existing executor seam. `false` only for `enforce: off` (the
    /// byte-identical pre-wrap path). Every requested enforcement returns
    /// `true` — the process provider on any platform (`platform_support`
    /// decides the wrap shape), the container provider with or without a
    /// detected runtime (ticket container-gate-wrapper: a runtime wraps the
    /// gate in the mission container; none FAILS CLOSED at resolve), and the
    /// fail-closed postures (a platform
    /// [`crate::sandbox::platform_support`] cannot honor, linux WITHOUT
    /// `bwrap`) — those route INTO the sandboxed runner so they error loudly
    /// at resolve rather than running unsandboxed (13th-pass review, P1).
    pub fn enforces_on_this_host(&self) -> bool {
        if self.sandbox.enforce == crate::types::SandboxEnforce::Off {
            return false;
        }
        match self.sandbox.provider {
            crate::types::SandboxProvider::Process => !matches!(
                crate::sandbox::platform_support(self.sandbox.enforce, std::env::consts::OS),
                crate::sandbox::SandboxDecision::Off
            ),
            crate::types::SandboxProvider::Container => true,
        }
    }

    /// The operator-visible note when this policy CANNOT wrap gates despite
    /// `enforce != off`: `provider: container` with no container runtime on
    /// PATH (ticket container-gate-wrapper). The merge gates themselves then
    /// FAIL CLOSED at resolve — the server's merge path has no event log and
    /// MUST log this note so the refusal reads as the operator's config
    /// problem it is, not a flaky gate. `None` for `enforce: off` (nothing
    /// to refuse), for the process provider (which wraps, or fails closed
    /// loudly at resolve — an unsupported platform or linux without `bwrap`
    /// needs no note because it errors), and for a container policy WITH a
    /// runtime (the gates wrap in the mission container — nothing degraded).
    pub fn degradation_note(&self) -> Option<String> {
        self.degradation_note_target(crate::sandbox_container::detect())
    }

    /// [`MergeGatePolicy::degradation_note`] parameterized on runtime
    /// detection so the decision is testable without a container runtime
    /// (mirrors [`resolve_gate_sandbox_target`]).
    pub(crate) fn degradation_note_target(
        &self,
        container_runtime: Option<crate::sandbox_container::ContainerRuntime>,
    ) -> Option<String> {
        if self.sandbox.provider == crate::types::SandboxProvider::Container
            && self.sandbox.enforce != crate::types::SandboxEnforce::Off
            && container_runtime.is_none()
        {
            Some(container_gate_note(self.sandbox.enforce))
        } else {
            None
        }
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
    let (code, output) = run_bounded_gate_command_sandboxed_with_code(cwd, command, policy);
    (code == Some(0), output)
}

/// The production sandboxed merge-gate runner with its exact child exit
/// status retained. Normal callers need only the stable bool/output API
/// above; the Windows production receipt keeps the status so a native CI
/// failure can distinguish a missing output marker from a process failure.
pub(crate) fn run_bounded_gate_command_sandboxed_with_code(
    cwd: &std::path::Path,
    command: &str,
    policy: &MergeGatePolicy,
) -> (Option<i32>, String) {
    if !policy.enforces_on_this_host() {
        let (ok, output) = run_bounded_gate_command(cwd, command);
        return (Some(i32::from(!ok)), output);
    }
    let scratch =
        std::env::temp_dir().join(format!("kranz-gate-{}", uuid::Uuid::new_v4().simple()));
    if std::fs::create_dir_all(scratch.join("tmp")).is_err() {
        return (
            None,
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
            None,
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
                None,
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
    #[cfg(windows)]
    crate::agent_env::redirect_windows_profile_env(&mut env, &scratch);
    #[cfg(not(windows))]
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
            return (None, format!("failed to create gate runtime: {error}"));
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
    (code, output)
}

pub(crate) fn sanitized_gate_env() -> HashMap<String, String> {
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
        "RUSTUP_HOME",
        "NPM_CONFIG_CACHE",
        "CI",
        "TERM",
        "LANG",
        "LC_ALL",
        "TZ",
    ];
    let env: HashMap<String, String> = SAFE
        .iter()
        .filter_map(|key| {
            std::env::var_os(key).map(|value| ((*key).to_string(), value.to_string_lossy().into()))
        })
        .collect();
    #[cfg(windows)]
    let env = {
        let mut env = env;
        crate::agent_env::extend_windows_process_env(&mut env);
        // USERPROFILE is redirected to gate scratch before the child starts.
        // Resolve the operator's rustup home now so standard installations
        // that leave RUSTUP_HOME unset still find their toolchain. CARGO_HOME
        // remains absent here and is replaced with the cache-only root by the
        // gate runners.
        crate::agent_env::extend_noncredential_toolchain_env(&mut env);
        env
    };
    env
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
                | "APPDATA"
                | "LOCALAPPDATA"
                | "SystemRoot"
                | "ComSpec"
                | "PATHEXT"
                | "SystemDrive"
                | "windir"
                | "OS"
                | "PROCESSOR_ARCHITECTURE"
                | "PSModulePath"
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

        // Inspect names separately from values. `run_shell_command` retains a
        // bounded output tail, and launcher-managed PATH values can themselves
        // exceed that bound; a raw `env` dump could therefore discard the
        // leading `PATH=` and make this boundary test host-PATH-dependent.
        let (ok, names) =
            run_shell_command(dir.path(), "env | sed 's/=.*//' | LC_ALL=C sort", &env).await;
        assert!(ok, "{names}");
        for leaked in ["GH_TOKEN", "SLACK_BOT_TOKEN", "AWS_SECRET_ACCESS_KEY"] {
            assert!(
                !names.lines().any(|name| name == leaked),
                "contract env leaked {leaked}:\n{names}"
            );
        }
        assert!(
            names.lines().any(|name| name == "PATH"),
            "PATH must cross:\n{names}"
        );

        let (ok, managed) = run_shell_command(
            dir.path(),
            "printf 'HOME=%s\nKRANZ_BASE_SHA=%s\nCARGO_HOME=%s\n' \"$HOME\" \"$KRANZ_BASE_SHA\" \"$CARGO_HOME\"",
            &env,
        )
        .await;
        assert!(ok, "{managed}");
        assert!(
            managed.contains(&format!("HOME={}", scratch.path().display())),
            "HOME must be the per-mission scratch:\n{managed}"
        );
        assert!(
            managed.contains("KRANZ_BASE_SHA=deadbeef"),
            "base sha must reach the contract env:\n{managed}"
        );
        let cargo_home = env.get("CARGO_HOME").expect("CARGO_HOME");
        assert!(
            std::path::Path::new(cargo_home).starts_with(scratch.path()),
            "contract CARGO_HOME must live under mission scratch: {cargo_home}"
        );
        assert!(
            managed.contains(&format!("CARGO_HOME={cargo_home}")),
            "cache-only Cargo home must reach the child:\n{managed}"
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
    /// allow appended, NO xcrun write allow — 13th-pass prewarm + deny,
    /// denies + writable roots in shape), linux resolves Bubblewrap and
    /// fails CLOSED without bwrap, Windows resolves AppContainer, an unknown
    /// platform fails CLOSED, and the container provider wraps in the mission
    /// container with a runtime and fails CLOSED without one (ticket
    /// container-gate-wrapper).
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
            None,
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
            None,
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
            profile.contains("(literal \"/dev/null\")"),
            "the gate profile must add the /dev/null device write allow:\n{profile}"
        );
        assert!(
            profile.contains("(literal \"/dev/ptmx\")"),
            "pty harness support (pty-functional-validation): the gate profile must \
             permit the ptmx multiplexer:\n{profile}"
        );
        // 14th-pass review (ticket gate-wrap-file-ioctl-unscoped): the ioctl
        // allow is pinned SCOPED to the pty device pair — a bare
        // `(allow file-ioctl)` re-widen must fail loudly here.
        assert!(
            profile.contains(
                "(allow file-ioctl (literal \"/dev/ptmx\") (regex #\"^/dev/tty[p-t][0-9a-f]+$\"))"
            ),
            "the grantpt/unlockpt ioctl allow must be scoped to /dev/ptmx and the \
             tty slave nodes:\n{profile}"
        );
        assert!(
            !profile.contains("(allow file-ioctl)"),
            "the ioctl allow must never be unscoped again (every device the gate \
             can open becomes ioctl-able):\n{profile}"
        );
        assert!(
            !profile.contains("xcrun_db"),
            "13th-pass review (P1): the gate profile must NOT permit writes to the \
             shared per-user xcrun cache (prewarm + deny posture):\n{profile}"
        );
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
            None,
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
            None,
        )
        .expect_err("linux without bwrap must fail closed");
        assert!(error.to_string().contains("bwrap"), "{error}");

        // fs on Windows → stable AppContainer inputs shaped like the gate.
        let resolution = resolve_gate_sandbox_target(
            &fs,
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "windows",
            false,
            None,
        )
        .expect("Windows process gates resolve AppContainer");
        let GateSandbox::AppContainer { inputs, .. } = &resolution.sandbox else {
            panic!("fs on Windows must resolve AppContainer");
        };
        assert_eq!(inputs.session_cwd, repo.path());
        assert_eq!(inputs.tmpdir, scratch.path());
        assert_eq!(inputs.mission_dir, mission);

        // fs on an unknown platform → FAIL CLOSED (13th-pass review,
        // P1): agent sessions already refuse to run there, and a standalone
        // merge gate must fail loudly too — never run unsandboxed under an
        // enforced config.
        let error = resolve_gate_sandbox_target(
            &fs,
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "solaris",
            false,
            None,
        )
        .expect_err("an unknown platform must fail closed");
        assert!(error.to_string().contains("unsupported"), "{error}");
        assert!(
            error
                .to_string()
                .contains("refusing to run engine-run gates unsandboxed"),
            "{error}"
        );
    }

    /// The pty-era extras, pinned as TEXT (ticket
    /// gate-wrap-file-ioctl-unscoped, 14th-pass review): the file-ioctl
    /// allow must stay scoped to exactly the pty device pair the harness
    /// needs — `/dev/ptmx` (grantpt/unlockpt land on the master fd) plus
    /// the tty-slave regex (termios/winsize on the slave) — so a future
    /// re-widen to the unrestricted `(allow file-ioctl)` fails loudly.
    /// Scoped-for-every-gate is deliberate: the gate profile cannot know at
    /// resolve time whether the contract carries pty assertions (merge
    /// gates never see one), and the scoped surface is the pty pair alone.
    #[test]
    fn gate_profile_extras_scopes_file_ioctl_to_pty_devices() {
        let extras = gate_profile_extras();
        assert!(
            extras.contains(
                "(allow file-ioctl (literal \"/dev/ptmx\") (regex #\"^/dev/tty[p-t][0-9a-f]+$\"))"
            ),
            "the ioctl allow must be scoped to the pty device pair:\n{extras}"
        );
        assert!(
            !extras.contains("(allow file-ioctl)"),
            "the unrestricted ioctl allow must not return:\n{extras}"
        );
        // The rest of the pty surface stays (multiplexer read+write, slave
        // read+write) — the scoped ioctl is useless without them.
        assert!(extras.contains("(literal \"/dev/ptmx\")"), "{extras}");
        assert!(extras.contains("^/dev/tty[p-t][0-9a-f]+$"), "{extras}");
        assert!(
            extras.contains("(allow signal (target same-sandbox))"),
            "{extras}"
        );
    }

    /// The container arm of the resolve matrix (ticket container-gate-wrapper):
    /// provider:container + enforce != off + a detected runtime resolves to
    /// [`GateSandbox::Container`] with gate-shaped inputs (the gate cwd as the
    /// writable root, the scratch as tmpdir, the mission dir for the metadata
    /// denies) and the configured/default image — on the live-proven macOS
    /// and Linux hosts. Windows fails closed even when `docker.exe` exists:
    /// runtime presence does not prove guest path or authority-mask semantics.
    /// No runtime FAILS CLOSED with the shared note (mirroring session
    /// resolution — never a silent host-side gate); `fs+net` with a non-empty
    /// egress list FAILS CLOSED (advisory-only on the bridge, and no egress
    /// proxy exists engine-side); `enforce: off` stays Disabled.
    #[test]
    fn container_gate_wrap_resolve_matrix() {
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let container = |enforce| crate::types::SandboxConfig {
            enforce,
            provider: crate::types::SandboxProvider::Container,
            image: None,
            extra_write: vec![],
            egress: vec![],
        };
        let runtime = Some(crate::sandbox_container::ContainerRuntime::Docker);

        // A detected runtime → the container wrap on live-proven hosts.
        for target_os in ["macos", "linux"] {
            let resolution = resolve_gate_sandbox_target(
                &container(crate::types::SandboxEnforce::Fs),
                repo.path(),
                &mission,
                scratch.path(),
                scratch.path(),
                target_os,
                false,
                runtime,
            )
            .unwrap();
            assert!(resolution.note.is_none());
            let GateSandbox::Container { inputs, spec } = &resolution.sandbox else {
                panic!("container + runtime must resolve to GateSandbox::Container on {target_os}");
            };
            assert_eq!(inputs.session_cwd, repo.path());
            assert_eq!(inputs.tmpdir, scratch.path());
            assert_eq!(inputs.mission_dir, mission);
            assert_eq!(inputs.enforce, crate::types::SandboxEnforce::Fs);
            assert_eq!(
                spec.runtime,
                crate::sandbox_container::ContainerRuntime::Docker
            );
            assert_eq!(spec.image, crate::sandbox_container::DEFAULT_IMAGE);
        }

        // A detected Windows runtime is not containment evidence. The
        // shipped mount contract uses POSIX guest paths and `/dev/null`
        // authority masks, neither of which has a Windows hostile-host
        // receipt. Fail before the gate process starts.
        let error = resolve_gate_sandbox_target(
            &container(crate::types::SandboxEnforce::Fs),
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "windows",
            false,
            runtime,
        )
        .expect_err("an unproved Windows container gate must fail closed");
        assert!(
            error
                .to_string()
                .contains("not live-proven on target_os=windows"),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("unverified container mount contract"),
            "{error}"
        );

        // A configured image rides into the spec (the mission container
        // image carries the gate's toolchain — the documented assumption).
        let mut imaged = container(crate::types::SandboxEnforce::Fs);
        imaged.image = Some("ghcr.io/example/kranz-worker:1".to_string());
        let resolution = resolve_gate_sandbox_target(
            &imaged,
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "macos",
            false,
            runtime,
        )
        .unwrap();
        let GateSandbox::Container { spec, .. } = &resolution.sandbox else {
            panic!("container + runtime must resolve to GateSandbox::Container");
        };
        assert_eq!(spec.image, "ghcr.io/example/kranz-worker:1");

        // NO runtime → FAIL CLOSED with the shared note text (the same text
        // MergeGatePolicy::degradation_note surfaces on the merge path).
        let error = resolve_gate_sandbox_target(
            &container(crate::types::SandboxEnforce::Fs),
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "macos",
            false,
            None,
        )
        .expect_err("container without a runtime must fail closed");
        assert!(
            error.to_string().contains("no container runtime"),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("refusing to run engine-run gates unsandboxed"),
            "{error}"
        );

        // fs+net with a NON-EMPTY egress list → FAIL CLOSED: advisory-only
        // on the runtime bridge and no egress proxy exists engine-side, so
        // the gate must never silently keep the bridge.
        let mut egress = container(crate::types::SandboxEnforce::FsNet);
        egress.egress = vec!["crates.io:443".to_string()];
        let error = resolve_gate_sandbox_target(
            &egress,
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "linux",
            false,
            runtime,
        )
        .expect_err("container fs+net with an egress list must fail closed");
        assert!(error.to_string().contains("advisory"), "{error}");

        // fs+net with an EMPTY egress list wraps (`--network none` is the
        // hard boundary) — and the wrap carries fs+net for the runner's
        // offline-by-cache env adjustment.
        let resolution = resolve_gate_sandbox_target(
            &container(crate::types::SandboxEnforce::FsNet),
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "linux",
            false,
            runtime,
        )
        .unwrap();
        assert_eq!(
            resolution.sandbox.enforce(),
            crate::types::SandboxEnforce::FsNet
        );

        // enforce: off + container → Disabled, no note (the off check
        // precedes the provider — no runtime is required either).
        let resolution = resolve_gate_sandbox_target(
            &container(crate::types::SandboxEnforce::Off),
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "macos",
            false,
            None,
        )
        .unwrap();
        assert!(matches!(resolution.sandbox, GateSandbox::Disabled));
        assert!(resolution.note.is_none());
    }

    /// M7 Windows parity, phase 4: engine-run process gates resolve the stable
    /// AppContainer wrapper. A detected `docker.exe` still does not prove the
    /// Windows container mount/authority-mask contract, so that provider
    /// continues to fail closed.
    #[test]
    fn windows_enforced_gate_process_resolves_appcontainer_while_container_fails_closed() {
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let runtime = Some(crate::sandbox_container::ContainerRuntime::Docker);

        for enforce in [
            crate::types::SandboxEnforce::Fs,
            crate::types::SandboxEnforce::FsNet,
        ] {
            let process = fs_sandbox_config(enforce);
            let resolution = resolve_gate_sandbox_target(
                &process,
                repo.path(),
                &mission,
                scratch.path(),
                scratch.path(),
                "windows",
                false,
                runtime,
            )
            .expect("Windows process gate enforcement resolves");
            assert!(resolution.note.is_none(), "{:?}", resolution.note);
            let GateSandbox::AppContainer { inputs, .. } = resolution.sandbox else {
                panic!("Windows process gate must resolve AppContainer");
            };
            assert_eq!(inputs.enforce, enforce);
            assert_eq!(inputs.session_cwd, repo.path());
            assert_eq!(inputs.mission_dir, mission);

            let container = crate::types::SandboxConfig {
                enforce,
                provider: crate::types::SandboxProvider::Container,
                image: None,
                extra_write: vec![],
                egress: vec![],
            };
            let error = resolve_gate_sandbox_target(
                &container,
                repo.path(),
                &mission,
                scratch.path(),
                scratch.path(),
                "windows",
                false,
                runtime,
            )
            .expect_err("an unproved Windows container gate must fail closed");
            assert!(error
                .to_string()
                .contains("not live-proven on target_os=windows"));
            assert!(error
                .to_string()
                .contains("unverified container mount contract"));
        }
    }

    /// 13th-pass review (P1), the prewarm half of the macOS xcrun posture:
    /// the shim cache is refreshed OUTSIDE the sandbox ONCE PER RESOLVE —
    /// never per command (the cache is per-user and shared, so one refresh
    /// covers every wrapped spawn the resolution produces). Counted through
    /// the GATE_XCRUN_PREWARM_SPAWNS test seam. macOS-only: the prewarm is
    /// compiled out elsewhere.
    #[cfg(target_os = "macos")]
    #[test]
    fn gate_xcrun_deny_prewarm_runs_once_per_resolve_not_per_command() {
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let cfg = fs_sandbox_config(crate::types::SandboxEnforce::Fs);
        let resolve = || {
            resolve_gate_sandbox(&cfg, repo.path(), &mission, scratch.path(), scratch.path())
                .unwrap()
        };

        // Per-resolution state, never a global counter: a process-wide
        // counter races with parallel test threads resolving concurrently
        // (the rust-macos CI flake this replaced).
        let resolution = resolve();
        assert!(resolution.prewarmed_xcrun, "one prewarm per resolve");

        // Wrapping commands from this resolution prewarms NOTHING further —
        // the wrap is argv construction, the prewarm lives in resolve.
        let env = std::collections::HashMap::new();
        let _argv_one = resolution.sandbox.wrap_shell("true", &env).unwrap();
        let _argv_two = resolution.sandbox.wrap_shell("echo hi", &env).unwrap();
        assert!(
            resolution.prewarmed_xcrun,
            "command wraps neither prewarm nor reset the record"
        );

        // A second resolve prewarms again — per resolve, not once globally.
        let second = resolve();
        assert!(second.prewarmed_xcrun, "each resolve prewarms exactly once");
    }

    /// Ticket container-gate-wrapper, the merge-policy half: a
    /// provider:container policy ENFORCES on every host (the pre-check
    /// routes into the sandboxed runner, which wraps the gate in the mission
    /// container when a runtime is detected), and the merge path's note
    /// fires ONLY for the fail-closed remainder — no runtime on PATH. The
    /// note text the policy logs and the resolve error the gate run fails
    /// with are the SAME text (one explanation on every path).
    #[test]
    fn container_gate_wrap_merge_policy_enforces_or_notes_the_fail_closed() {
        let container = |enforce| crate::types::SandboxConfig {
            enforce,
            provider: crate::types::SandboxProvider::Container,
            image: None,
            extra_write: vec![],
            egress: vec![],
        };
        let policy = MergeGatePolicy {
            sandbox: container(crate::types::SandboxEnforce::Fs),
            mission_dir: std::path::PathBuf::new(),
        };
        // Enforces on EVERY host (host-independent: the wrap needs a
        // runtime, not a platform tier; runtime-absent fails closed inside
        // the sandboxed runner rather than routing to the unsandboxed seam).
        assert!(policy.enforces_on_this_host());
        // With a runtime the gates wrap — nothing degraded, no note.
        assert!(policy
            .degradation_note_target(Some(crate::sandbox_container::ContainerRuntime::Docker))
            .is_none());
        // Without one the merge path MUST log the fail-closed note…
        let note = policy
            .degradation_note_target(None)
            .expect("the runtime-unavailable container posture must be noted");
        assert!(note.contains("no container runtime"), "{note}");
        assert!(
            note.contains("refusing to run engine-run gates unsandboxed"),
            "{note}"
        );
        // …and the resolve error the gate run then fails with carries the
        // SAME text verbatim (the EngineError::Config display prefix is the
        // error-variant decoration, not part of the note).
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let error = resolve_gate_sandbox_target(
            &policy.sandbox,
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
            "macos",
            false,
            None,
        )
        .expect_err("container without a runtime must fail closed");
        assert_eq!(
            error.to_string(),
            format!("configuration error: {note}"),
            "the engine-path resolve error and the merge-path note must match"
        );

        // enforce: off + container: nothing to enforce, nothing to note. A
        // process-provider policy has no note either — it wraps, or fails
        // closed loudly.
        let off = MergeGatePolicy {
            sandbox: container(crate::types::SandboxEnforce::Off),
            mission_dir: std::path::PathBuf::new(),
        };
        assert!(off.degradation_note_target(None).is_none());
        assert!(!off.enforces_on_this_host());
        let process = MergeGatePolicy {
            sandbox: fs_sandbox_config(crate::types::SandboxEnforce::Fs),
            mission_dir: std::path::PathBuf::new(),
        };
        assert!(process.degradation_note_target(None).is_none());
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

    /// The gate-SPECIFIC supervision policy, asserted end-to-end (ticket
    /// gate-sandbox-supervision-dogfood): the wrapped gate may signal
    /// processes INSIDE its own sandboxed tree (`(allow signal (target
    /// same-sandbox))` — see [`gate_profile_extras`]), and it gains NO
    /// host-wide capability. Probes against the resolved wrap:
    ///
    /// - `kill -0` + `kill -TERM` against a child the wrapped command
    ///   spawned itself (the engine suite's timeout-kill / liveness-poll
    ///   shape): ALLOWED.
    /// - `kill -0` against a SAME-UID host process started OUTSIDE the
    ///   sandbox (its pid baked into the command): DENIED.
    /// - `ps` inspection of that host process: DENIED — `/bin/ps` is setuid
    ///   root and setuid exec is kernel-denied inside ANY sandbox (probed
    ///   2026-08-05, not SBPL-expressible), so ps-based inspection of ANY
    ///   process is unreachable inside the wrap; the in-tree inspection
    ///   need is served by `proc_pidinfo` instead (event_log's identity
    ///   tokens, covered by the wrapped-suite fixture below).
    ///
    /// Anti-vacuity: with enforcement off the SAME host probes succeed, so
    /// the denials above are the sandbox's, not a broken probe. macOS-only:
    /// the policy being pinned is an SBPL clause — bwrap has no signal tier
    /// to scope (the host probe succeeds there by design). Under a wrapped
    /// `cargo test` the nested smoke-apply in
    /// [`gate_wrap_sandbox_exec_can_apply`] fails and this test skips
    /// cleanly, like every enforcement test.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn gate_sandbox_wrap_dogfood_supervision_allows_tree_denies_host() {
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

        // The "unrelated host process": a same-uid sleeper spawned OUTSIDE
        // the wrap (never under its label), killed and reaped on scope exit.
        let mut host = std::process::Command::new("sleep")
            .arg("300")
            .spawn()
            .expect("spawn host sleeper");
        let host_pid = host.id();

        // In-tree supervision works: the wrapped command spawns a child,
        // liveness-probes it, and kills it — the exact shape the engine
        // suite's timeout-kill tests need.
        let (ok, output) = run_shell_command_sandboxed(
            repo.path(),
            "sleep 300 & child=$!; kill -0 \"$child\" && kill -TERM \"$child\"",
            &env,
            &sandbox,
        )
        .await;
        assert!(
            ok,
            "the wrapped gate must signal its own tree (same-sandbox): {output}"
        );

        // Host-wide supervision stays denied: signal AND ps inspection of
        // the outside process both fail inside the wrap.
        let (ok, output) = run_shell_command_sandboxed(
            repo.path(),
            &format!("kill -0 {host_pid}"),
            &env,
            &sandbox,
        )
        .await;
        assert!(
            !ok,
            "no host-wide signal capability under the wrap (EPERM expected): {output}"
        );
        let (ok, output) = run_shell_command_sandboxed(
            repo.path(),
            &format!("ps -p {host_pid} -o command="),
            &env,
            &sandbox,
        )
        .await;
        assert!(
            !ok,
            "no ps inspection under the wrap (setuid exec denied): {output}"
        );

        // Anti-vacuity: the SAME host probes succeed with enforcement off —
        // the denials above are the sandbox's doing, not a broken probe.
        let (ok, output) = run_shell_command_sandboxed(
            repo.path(),
            &format!("kill -0 {host_pid} && ps -p {host_pid} -o command="),
            &env,
            &GateSandbox::Disabled,
        )
        .await;
        assert!(
            ok,
            "with enforce == off the host probes succeed (today's posture): {output}"
        );

        let _ = host.kill();
        let _ = host.wait();
    }

    /// THE DOGFOOD PROVING GROUND (ticket gate-sandbox-supervision-dogfood):
    /// this repo's mandatory merge gate — `cargo test --workspace` — run as
    /// a WRAPPED contract command through the real gate-wrap path
    /// ([`resolve_gate_sandbox`] + the bounded sandboxed runner, `enforce:
    /// fs`, gate cwd = the repo root). The self-referential failures the
    /// module doc's measurement section records must be GONE: the
    /// signal/liveness class is covered by the `same-sandbox` supervision
    /// extra, the own-pid token class by proc_pidinfo-first identity
    /// tokens, and the tests NO sandbox can host (setuid `/bin/ps` exec,
    /// nested `sandbox_apply` of a different profile — both kernel-denied,
    /// see [`gate_profile_extras`]) skip with the detectable
    /// `SKIP-UNDER-WRAP (gate-sandbox-supervision-dogfood)` marker, which
    /// this fixture counts and reports from the captured suite log.
    ///
    /// Ignored by default — a full wrapped workspace suite is far too slow
    /// for the normal gate; the `rust-macos-wrapped-suite` CI job runs it
    /// explicitly. Run manually:
    ///
    /// ```sh
    /// cargo test -p kranz-engine dogfood_supervision -- --ignored --nocapture
    /// ```
    ///
    /// `KRANZ_DOGFOOD_SUITE_CMD` overrides the payload (scoping during
    /// development); the default is the ticket's gate verbatim. The
    /// `rust-macos-wrapped-suite` CI job overrides it to
    /// `cargo test --workspace -- --nocapture`: the asserted exit code is
    /// unchanged, but libtest then streams the suite's SKIP-UNDER-WRAP
    /// markers into the job log — with the default capture the markers are
    /// swallowed and the count below reads 0 even though the skips fired
    /// (verified 2026-08-05 by running the six premise-gated tests under a
    /// hand-built gate-shaped profile with --nocapture: every marker
    /// fires). The runner gets a generous wall clock rather than
    /// COMMAND_TIMEOUT: the production 600s contract-command cap is
    /// deliberately untouched, and a wrapped full-workspace suite is known
    /// to run past it (measured 2026-08-05 on a loaded M-series host:
    /// 2713s green end-to-end, most of it the in-sandbox dependency
    /// rebuild the cache-only CARGO_HOME forces — the same cost a
    /// production wrapped gate pays) — the fixture proves the SUPERVISION
    /// POLICY, not the production timeout budget.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "wrapped-suite proving ground — run manually or via the rust-macos-wrapped-suite CI job"]
    fn gate_sandbox_wrap_dogfood_supervision_workspace_suite() {
        if !gate_wrap_sandbox_exec_can_apply() {
            return;
        }
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/engine has a repo-root ancestor")
            .to_path_buf();
        let payload = std::env::var("KRANZ_DOGFOOD_SUITE_CMD")
            .unwrap_or_else(|_| "cargo test --workspace".to_string());

        // Mirror run_bounded_gate_command_sandboxed's setup (per-run
        // scratch, TMPDIR redirect, cache-only Cargo home inside it) so the
        // wrap the suite runs under IS the production merge-gate wrap; only
        // the wall clock differs (see the doc above). The suite log lands
        // in the scratch via a plain redirect — never a pipe, so the bare
        // cargo exit code is what gets asserted.
        let scratch =
            std::env::temp_dir().join(format!("kranz-gate-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(scratch.join("tmp")).unwrap();
        let cargo_home = crate::agent_env::cache_only_cargo_home(scratch.as_path());
        assert!(
            cargo_home.is_dir(),
            "could not create the fixture's cache-only Cargo home at {}",
            cargo_home.display()
        );
        // The fake mission layout only feeds the deny computation — nothing
        // real is touched; the repo root is the writable gate cwd.
        let (_layout_guard, mission) = gate_wrap_layout();
        let resolution = resolve_gate_sandbox(
            &fs_sandbox_config(crate::types::SandboxEnforce::Fs),
            &repo_root,
            &mission,
            &scratch,
            &scratch,
        )
        .expect("the fixture's gate sandbox resolves on a host that applied the smoke profile");
        let mut env = sanitized_gate_env();
        env.insert("CARGO_HOME".to_string(), cargo_home.display().to_string());
        for var in ["TMPDIR", "TMP", "TEMP"] {
            env.insert(var.to_string(), scratch.join("tmp").display().to_string());
        }
        let suite_log = scratch.join("tmp").join("dogfood-suite.log");
        let command = format!("{payload} > '{}' 2>&1", suite_log.display());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("fixture runtime");
        let start = std::time::Instant::now();
        let (code, output) = runtime.block_on(run_shell_command_sandboxed_with_code(
            &repo_root,
            &command,
            Duration::from_secs(3600),
            &env,
            &resolution.sandbox,
        ));
        let elapsed = start.elapsed();

        let log = std::fs::read_to_string(&suite_log)
            .unwrap_or_else(|_| format!("<no suite log captured; runner tail: {output}>"));
        let skip_count = log
            .matches("SKIP-UNDER-WRAP (gate-sandbox-supervision-dogfood)")
            .count();
        println!(
            "dogfood wrapped suite `{payload}`: exit={code:?} elapsed={elapsed:.1?} \
             skip-under-wrap markers={skip_count} log={}",
            suite_log.display()
        );
        for line in log.lines().filter(|l| l.contains("test result:")) {
            println!("  {line}");
        }
        // On failure the assert MUST carry the log tail — CI runners are
        // ephemeral and the scratch path alone is no evidence (the c6845f7
        // rust-macos-wrapped-suite failure gave an untailorable exit 101).
        let tail: Vec<&str> = log.lines().collect();
        let tail = &tail[tail.len().saturating_sub(40)..];
        assert_eq!(
            code,
            Some(0),
            "cargo test --workspace must run GREEN as a wrapped contract command \
             (skip-under-wrap markers seen: {skip_count})\n--- suite log tail ---\n{}",
            tail.join("\n")
        );
        // Cleanup only on success: on failure the assert above has already
        // panicked with the log's path, and the scratch (suite log, profile,
        // scratch home) survives for post-mortem debugging — the same
        // self-cleaning shape as the production path, minus the
        // evidence-destroying failure case.
        let _ = std::fs::remove_dir_all(&scratch);
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

    /// 13th-pass review (P1), the gate half of the shared-Cargo-cache deny:
    /// a wrapped gate READS the operator's real registry/git cache (the
    /// link target the over-ceiling isolated home points at — the read is
    /// the cache's whole purpose) but cannot WRITE it: the profile's
    /// explicit cache write deny holds regardless of the gate's writable
    /// roots. `enforce == off` keeps the documented trade (the write
    /// succeeds) — the anti-vacuity arm.
    // await_holding_lock: see the note on
    // gate_sandbox_wrap_denies_outside_writes_metadata_and_authority_reads.
    #[cfg(unix)]
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn gate_sandbox_wrap_cache_write_deny_reads_cache_but_cannot_write() {
        let _guard = GATE_SANDBOX_WRAP_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if !gate_wrap_enforcement_available() {
            return;
        }

        let (repo, mission) = gate_wrap_layout();
        let scratch = tempfile::tempdir().unwrap();
        // The "operator's" shared cache, armed via CARGO_HOME so BOTH the
        // profile deny computation and the cache-only home seeding resolve
        // it (the same paths cache_only_cargo_home links).
        let cargo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cargo.path().join("registry")).unwrap();
        std::fs::write(cargo.path().join("registry/cache-marker"), "cached").unwrap();
        let _cargo = crate::agent_env::EnvTestGuard::engage(&[(
            "CARGO_HOME",
            cargo.path().to_str().expect("utf-8 temp path"),
        )]);

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

        // The cache READS fine (broad read allow / ro-bind)…
        let (ok, output) = run_shell_command_sandboxed(
            repo.path(),
            &format!(
                "test -s '{}'",
                cargo.path().join("registry/cache-marker").display()
            ),
            &env,
            &sandbox,
        )
        .await;
        assert!(ok, "the wrapped gate must read the shared cache: {output}");

        // …but a WRITE to the real cache dir is denied, and the bytes stay
        // off the host either way (Seatbelt denies; bwrap's stacked ro-bind
        // refuses).
        let poison = cargo.path().join("registry/poisoned-crate");
        let (ok, output) = run_shell_command_sandboxed(
            repo.path(),
            &format!("echo x > '{}'", poison.display()),
            &env,
            &sandbox,
        )
        .await;
        assert!(
            !ok,
            "a write to the operator's real cargo cache must fail under enforcement: {output}"
        );
        assert!(
            !poison.exists(),
            "the denied cache write must not create the file"
        );

        // Anti-vacuity: the SAME write succeeds with enforcement off (the
        // documented trade the operator opts into with enforce: off).
        let (ok, output) = run_shell_command_sandboxed(
            repo.path(),
            &format!("echo x > '{}'", poison.display()),
            &env,
            &GateSandbox::Disabled,
        )
        .await;
        assert!(
            ok,
            "with enforce == off the cache write succeeds (documented trade): {output}"
        );
        let _ = std::fs::remove_file(&poison);
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
        // The container arm keys off the same `enforce()`: fs+net inside the
        // mission container is `--network none`, so cargo must run
        // offline-by-cache there too.
        let container_fs_net = GateSandbox::Container {
            inputs: Box::new(crate::sandbox::SandboxInputs {
                enforce: crate::types::SandboxEnforce::FsNet,
                session_cwd: std::path::PathBuf::from("/nonexistent"),
                mission_dir: std::path::PathBuf::from("/nonexistent"),
                tmpdir: std::path::PathBuf::from("/nonexistent"),
                extra_write: Vec::new(),
                egress: Vec::new(),
                validator_read_deny_roots: Vec::new(),
            }),
            spec: crate::sandbox_container::ContainerSpec {
                runtime: crate::sandbox_container::ContainerRuntime::Docker,
                image: crate::sandbox_container::DEFAULT_IMAGE.to_string(),
            },
        };
        assert_eq!(
            gate_env_for_sandbox(&base, &container_fs_net)
                .get("CARGO_NET_OFFLINE")
                .map(String::as_str),
            Some("true"),
            "fs+net container gates run cargo offline-by-cache"
        );
        assert!(
            !base.contains_key("CARGO_NET_OFFLINE"),
            "the caller's env map is never mutated"
        );
    }

    /// The container arm's per-command wrap shape (ticket
    /// container-gate-wrapper): the runtime binary is the program, the argv
    /// names a UNIQUE per-command container (`kranz-gate-*`) and carries the
    /// command as the image's `sh -c` payload, and the timeout teardown is
    /// `<runtime> rm -f <name>` targeting exactly that container (the
    /// bounded core's group SIGKILL stops the runtime client; the teardown
    /// stops the daemon-owned in-container tree). The process-sandbox arms
    /// have NO teardown — the group kill IS the tree kill there.
    #[test]
    fn container_gate_wrap_shell_shape_names_the_container_and_teardown() {
        let inputs = crate::sandbox::SandboxInputs {
            enforce: crate::types::SandboxEnforce::Fs,
            session_cwd: std::path::PathBuf::from("/nonexistent"),
            mission_dir: std::path::PathBuf::from("/nonexistent-m"),
            tmpdir: std::path::PathBuf::from("/nonexistent-s"),
            extra_write: Vec::new(),
            egress: Vec::new(),
            validator_read_deny_roots: Vec::new(),
        };
        let container = GateSandbox::Container {
            inputs: Box::new(inputs),
            spec: crate::sandbox_container::ContainerSpec {
                runtime: crate::sandbox_container::ContainerRuntime::Docker,
                image: crate::sandbox_container::DEFAULT_IMAGE.to_string(),
            },
        };
        let env: HashMap<String, String> = [("KRANZ_BASE_SHA".to_string(), "deadbeef".to_string())]
            .into_iter()
            .collect();

        let one = container.wrap_shell("echo hi", &env).unwrap();
        let two = container.wrap_shell("echo hi", &env).unwrap();
        assert_eq!(one.program, std::path::PathBuf::from("docker"));
        let name_of = |wrapped: &WrappedCommand| {
            wrapped
                .args
                .windows(2)
                .find(|w| w[0] == "--name")
                .map(|w| w[1].clone())
                .expect("the container argv must name its container")
        };
        let (name_one, name_two) = (name_of(&one), name_of(&two));
        assert!(
            name_one.starts_with("kranz-gate-"),
            "gate containers carry the kranz-gate- prefix: {name_one}"
        );
        assert_ne!(
            name_one, name_two,
            "container names are per command, never per resolve — parallel \
             gate commands from one resolution must not collide"
        );
        assert_eq!(
            one.timeout_teardown,
            Some((
                std::path::PathBuf::from("docker"),
                vec!["rm".to_string(), "-f".to_string(), name_one]
            )),
            "the teardown force-removes exactly this command's container"
        );
        assert!(
            one.args.ends_with(&[
                crate::sandbox_container::DEFAULT_IMAGE.to_string(),
                "sh".to_string(),
                "-c".to_string(),
                "echo hi".to_string()
            ]),
            "image then sh -c payload: {:?}",
            one.args
        );

        // The process-sandbox arms and Disabled carry no teardown.
        let seatbelt = GateSandbox::Seatbelt {
            enforce: crate::types::SandboxEnforce::Fs,
            profile_path: std::path::PathBuf::from("/nonexistent"),
        };
        assert!(seatbelt
            .wrap_shell("true", &env)
            .unwrap()
            .timeout_teardown
            .is_none());
        assert!(GateSandbox::Disabled
            .wrap_shell("true", &env)
            .unwrap()
            .timeout_teardown
            .is_none());
    }

    /// The ticket's core test gate: with provider:container + enforce != off
    /// a contract command provably executes INSIDE the mission container —
    /// reads/writes on the mount set work (the gate cwd write lands on the
    /// host, the scratch is writable via $HOME, `KRANZ_BASE_SHA` crosses via
    /// the forwarded `-e`), writes OUTSIDE the mount set fail (`/etc` on the
    /// read-only rootfs, and a sibling host temp dir the container never
    /// mounts), the mission metadata mount is read-only (the events.jsonl
    /// append fails and the host bytes are untouched), and the host's
    /// `.kranz/serve.token` is unreachable (its /dev/null mask reads back
    /// empty, so `test -s` fails). The off arm (GateSandbox::Disabled on the
    /// host) proves the probes are valid — the SAME probes succeed there, so
    /// the container is what denies them.
    ///
    /// Skips cleanly on hosts with no container runtime (this macOS dev
    /// host); CI ubuntu-latest has docker. Unix-only: the probes are POSIX
    /// shell inside the container and POSIX tempfile paths on the host. No
    /// GATE_SANDBOX_WRAP_LOCK: that lock serializes sandbox-exec/bwrap spawn
    /// contention, and this test spawns only the container runtime.
    #[cfg(unix)]
    #[tokio::test]
    async fn container_gate_wrap_runs_contract_command_inside_the_container() {
        if crate::sandbox_container::detect().is_none() {
            eprintln!(
                "no container runtime (docker/podman/nerdctl/container) on PATH; skipping \
                 container gate wrap fixture"
            );
            return;
        }

        let (repo, mission) = gate_wrap_layout();
        let kranz_dir = repo.path().join(".kranz");
        let scratch = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let container_cfg = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::Fs,
            provider: crate::types::SandboxProvider::Container,
            image: None,
            extra_write: vec![],
            egress: vec![],
        };
        let resolution = resolve_gate_sandbox(
            &container_cfg,
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
        )
        .unwrap();
        assert!(resolution.note.is_none());
        let sandbox = resolution.sandbox;
        assert!(
            matches!(sandbox, GateSandbox::Container { .. }),
            "provider:container with a runtime must resolve to the container wrap"
        );
        let env = crate::agent_env::contract_command_env(scratch.path(), Some("deadbeef"), &[]);

        // Reads/writes on the mount set work: the gate cwd write lands on
        // the host, the scratch (HOME) is writable, and the contract env
        // crossed into the container.
        let ok_file = repo.path().join("container_gate_wrap_ok.txt");
        let (ok, output) = run_shell_command_sandboxed(
            repo.path(),
            &format!(
                "echo ok > '{}' && echo scratch > \"$HOME/container_gate_wrap_scratch.txt\" \
                 && test \"$KRANZ_BASE_SHA\" = deadbeef",
                ok_file.display()
            ),
            &env,
            &sandbox,
        )
        .await;
        assert!(
            ok && ok_file.exists()
                && scratch
                    .path()
                    .join("container_gate_wrap_scratch.txt")
                    .exists(),
            "writes inside the mount set and the forwarded env must work: {output}"
        );

        // Writes OUTSIDE the mount set fail: /etc (read-only rootfs) and a
        // sibling host temp dir the container never mounts.
        let outside_file = outside.path().join("container_gate_wrap_marker");
        for probe in [
            "echo nope > /etc/container_gate_wrap_nope".to_string(),
            format!("echo x > '{}'", outside_file.display()),
        ] {
            let (ok, output) =
                run_shell_command_sandboxed(repo.path(), &probe, &env, &sandbox).await;
            assert!(
                !ok,
                "write outside the mount set must fail inside the container: {probe}\n{output}"
            );
        }
        assert!(
            !outside_file.exists(),
            "the denied write must not create the host file"
        );

        // Mission metadata is read-only: the append fails and the audit log
        // keeps its host bytes.
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
        assert!(!ok, "the events.jsonl append must fail on the ro mount");
        assert_eq!(
            std::fs::read_to_string(mission.join("events.jsonl")).unwrap(),
            "{\"seq\":1}\n",
            "the audit log must be untouched by the container gate"
        );

        // The host's authority material is unreachable: the /dev/null mask
        // reads back EMPTY (test -s fails) while an ordinary repo file still
        // reads fine.
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
                ".kranz/{name} must be /dev/null-masked inside the container: {output}"
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

        // Anti-vacuity: the SAME probes succeed with enforcement off (the
        // probe commands are valid; only the container denies them).
        let (ok, output) = run_shell_command_sandboxed(
            repo.path(),
            &format!(
                "echo x > '{}' && test -s '{}'",
                outside_file.display(),
                kranz_dir.join("serve.token").display()
            ),
            &env,
            &GateSandbox::Disabled,
        )
        .await;
        assert!(
            ok,
            "with enforce == off the probes succeed (today's posture): {output}"
        );
        let _ = std::fs::remove_file(&outside_file);
    }

    /// Real-host M7 receipt for Linux. The dedicated CI invocation installs
    /// bubblewrap, then runs this exact ignored test with `--nocapture` so the
    /// retained timing and containment evidence is visible in the job log.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "live bubblewrap receipt — run by the protected Linux CI leg"]
    #[allow(clippy::await_holding_lock)]
    async fn linux_bubblewrap_hostile_live_receipt() {
        let _guard = GATE_SANDBOX_WRAP_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            gate_wrap_bwrap_can_apply(),
            "the live-proof host must provide a working bubblewrap boundary"
        );

        let primary = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/engine has a repository root");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(primary)
                .output()
                .expect("git must run on the live-proof checkout");
            assert!(output.status.success(), "git {args:?} failed");
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        let head_before = git(&["rev-parse", "HEAD"]);
        let status_before = git(&["status", "--porcelain", "--untracked-files=no"]);
        assert!(
            status_before.is_empty(),
            "the live proof requires a clean tracked primary checkout: {status_before}"
        );

        let (repo, mission) = gate_wrap_layout();
        let scratch = tempfile::tempdir().expect("private proof scratch");
        let outside = tempfile::tempdir().expect("sibling canary root");
        let resolution = resolve_gate_sandbox(
            &fs_sandbox_config(crate::types::SandboxEnforce::FsNet),
            repo.path(),
            &mission,
            scratch.path(),
            scratch.path(),
        )
        .expect("fs+net must resolve to bubblewrap on the proof host");
        assert!(resolution.note.is_none());
        assert!(matches!(resolution.sandbox, GateSandbox::Bubblewrap { .. }));
        let sandbox = resolution.sandbox;
        let env = crate::agent_env::contract_command_env(scratch.path(), None, &[]);

        let canary = outside.path().join("kranz-linux-hostile-canary");
        let (write_ok, write_output) = run_shell_command_sandboxed(
            repo.path(),
            &format!("printf escaped > '{}'", canary.display()),
            &env,
            &sandbox,
        )
        .await;
        assert!(
            !write_ok,
            "sibling write escaped bubblewrap: {write_output}"
        );
        assert!(
            !canary.exists(),
            "the denied sibling canary must stay absent"
        );

        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").expect("host loopback proof listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking proof listener");
        let port = listener.local_addr().expect("listener address").port();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();
        let acceptor = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let mut accepted = 0usize;
            while started.elapsed() < Duration::from_secs(10) {
                match listener.accept() {
                    Ok(_) => accepted += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("proof listener failed: {error}"),
                }
                if stop_rx.try_recv().is_ok() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            accepted
        });
        let connect = format!(
            "python3 -c 'import socket; socket.create_connection((\"127.0.0.1\", {port}), 2).close()'"
        );
        let (off_connect_ok, off_connect_output) =
            run_shell_command_sandboxed(repo.path(), &connect, &env, &GateSandbox::Disabled).await;
        assert!(
            off_connect_ok,
            "the network anti-vacuity probe must reach the host listener without enforcement: {off_connect_output}"
        );
        let (wrapped_connect_ok, wrapped_connect_output) =
            run_shell_command_sandboxed(repo.path(), &connect, &env, &sandbox).await;
        assert!(
            !wrapped_connect_ok,
            "the fs+net namespace reached the host listener: {wrapped_connect_output}"
        );
        let _ = stop_tx.send(());
        assert_eq!(
            acceptor.join().expect("proof listener thread"),
            1,
            "only the unwrapped anti-vacuity connection may reach the host"
        );

        let gate = "node -e \"let n=0; for(let i=0;i<100000;i++)n=(n+i)>>>0; if(n!==704982704)process.exit(2); setTimeout(()=>console.log('kranz-linux-node-ok'),750)\"";
        for (label, posture) in [
            ("unwrapped warm-up", &GateSandbox::Disabled),
            ("bubblewrap warm-up", &sandbox),
        ] {
            let (ok, output) = run_shell_command_sandboxed(repo.path(), gate, &env, posture).await;
            assert!(
                ok && output.contains("kranz-linux-node-ok"),
                "{label} failed: {output}"
            );
        }

        let mut off_samples_ms = Vec::with_capacity(7);
        let mut wrapped_samples_ms = Vec::with_capacity(7);
        for index in 0..7 {
            for wrapped in [index % 2 == 1, index % 2 == 0] {
                let started = std::time::Instant::now();
                let posture = if wrapped {
                    &sandbox
                } else {
                    &GateSandbox::Disabled
                };
                let (ok, output) =
                    run_shell_command_sandboxed(repo.path(), gate, &env, posture).await;
                assert!(
                    ok && output.contains("kranz-linux-node-ok"),
                    "timed gate failed: {output}"
                );
                let elapsed = started.elapsed().as_secs_f64() * 1_000.0;
                if wrapped {
                    wrapped_samples_ms.push(elapsed);
                } else {
                    off_samples_ms.push(elapsed);
                }
            }
        }
        let median = |samples: &[f64]| {
            let mut sorted = samples.to_vec();
            sorted.sort_by(f64::total_cmp);
            sorted[sorted.len() / 2]
        };
        let off_median_ms = median(&off_samples_ms);
        let wrapped_median_ms = median(&wrapped_samples_ms);
        let overhead_percent = (wrapped_median_ms / off_median_ms - 1.0) * 100.0;

        let head_after = git(&["rev-parse", "HEAD"]);
        let status_after = git(&["status", "--porcelain", "--untracked-files=no"]);
        assert_eq!(head_after, head_before, "the primary checkout HEAD moved");
        assert_eq!(
            status_after, status_before,
            "the primary checkout's tracked bytes changed"
        );

        let host = |program: &str, args: &[&str]| {
            std::process::Command::new(program)
                .args(args)
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
                .unwrap_or_else(|| "unavailable".to_string())
        };
        let receipt = serde_json::json!({
            "hostOs": std::env::consts::OS,
            "hostArch": std::env::consts::ARCH,
            "kernel": host("uname", &["-sr"]),
            "bubblewrap": host("bwrap", &["--version"]),
            "node": host("node", &["--version"]),
            "enforcement": "fs+net",
            "provider": "process/bubblewrap",
            "siblingWriteDenied": !write_ok && !canary.exists(),
            "networkDenied": !wrapped_connect_ok,
            "networkAntiVacuityPassed": off_connect_ok,
            "normalGatePassed": true,
            "primaryCheckoutUntouched": head_after == head_before && status_after == status_before,
            "repetitions": 7,
            "offSamplesMs": off_samples_ms,
            "bubblewrapSamplesMs": wrapped_samples_ms,
            "offMedianMs": off_median_ms,
            "bubblewrapMedianMs": wrapped_median_ms,
            "overheadPercent": overhead_percent,
            "overheadTargetPercent": 10.0,
            "withinTarget": overhead_percent <= 10.0,
            "head": head_before,
        });
        println!("KRANZ_LINUX_LIVE_RECEIPT={receipt}");
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
    /// SIGKILL/`kill(pid,0)` liveness probes were EPERM under the session
    /// profile's `(allow signal (target self))` (the engine's own timeout
    /// kill is unaffected — it signals from OUTSIDE the sandbox), `ps`-based
    /// process identity likewise, and `sandbox_apply` from inside a sandbox
    /// is denied. Those 11 are the repro set of ticket
    /// gate-sandbox-supervision-dogfood: the signal/liveness class is now
    /// covered by the gate-specific `(allow signal (target same-sandbox))`
    /// extra, the own-pid token class by proc_pidinfo-first identity
    /// tokens, and the classes no sandbox can host (setuid `/bin/ps` exec,
    /// nested `sandbox_apply`) skip under the wrap with a detectable marker
    /// — see the module doc's supervision section and the
    /// `gate_sandbox_wrap_dogfood_supervision_*` fixtures. The skips below
    /// stay in this MEASUREMENT payload so the overhead number is not
    /// polluted by the slow self-referential tests; the wrapped-suite
    /// fixture (not this harness) is the green-gate proof.
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
