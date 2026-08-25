//! OS sandbox profile/argv generation — Tier 2 filesystem/network containment.
//! See docs/scoping/worker-sandboxing.md tier 2.
//!
//! macOS uses Seatbelt (`sandbox-exec`) for filesystem isolation. Seatbelt
//! cannot express hostname egress allowlists (it accepts only `*`/`localhost`
//! network hosts), so `fs+net` on macOS restricts outbound TCP to loopback
//! and the run routes through the userspace filtering egress proxy
//! (`crate::egress_proxy`), which enforces the per-host allowlist at CONNECT
//! time. Linux uses bubblewrap for filesystem isolation; `fs+net` fails closed
//! with `--unshare-net` because bwrap alone cannot express a hostname egress
//! allowlist (and its netns cannot reach a host proxy — out of scope for v1).
//!
//! Write scope (P1, ticket sandbox-writable-scope): a session may write only
//! its working directory, its per-session private scratch root
//! (`SandboxInputs::tmpdir` — NOT the shared system temp root, which would
//! expose every sibling mission's worktrees and merge scratch), and
//! operator-declared `extraWrite` paths. The mission dir is never
//! worker-writable; its engine-owned metadata (audit log, state snapshot,
//! control inbox, transcripts) is additionally denied/masked so it stays
//! read-only even in checkout mode, where the writable `session_cwd` is an
//! ancestor of the mission dir.
//!
//! Mandatory validator containment (ticket `validator-mandatory-containment`):
//! VALIDATOR sessions are the one class wrapped regardless of
//! `sandbox.enforce` — the validator is the adversarial reader the whole
//! gate rests on, so its isolation cannot be operator-opt-in.
//! [`resolve_validator_containment`] resolves the posture: the role's own
//! enforced sandbox plus the real-checkout read-deny set when enforcement
//! is configured, the mandatory `fs`-tier wrap when it is not, and — where
//! the platform or the selected backend cannot contain — a FAIL-CLOSED
//! refusal by default (ticket `validator-containment-degrade-fail-closed`,
//! 14th-pass review: the loud degrade reopens the modify→use→restore path,
//! so it is now the explicit opt-in `validatorAllowUncontainedDegrade`, never
//! the default).
//! The read-deny set ([`validator_read_deny_entries`]) closes the broad
//! read allow over the real checkout's source tree — the snapshot
//! worktree is the sole writable root and the only tree the validator can
//! read — keeping the narrow `.git`/`.kranz` carve-outs the inspection
//! legitimately needs.

use std::path::{Path, PathBuf};

/// Default egress needed by Claude/Anthropic sessions under `fs+net`.
pub const DEFAULT_EGRESS: &[&str] = &["api.anthropic.com:443", "*.anthropic.com:443"];

/// Inputs used to build a session sandbox.
#[derive(Debug, Clone)]
pub struct SandboxInputs {
    pub enforce: crate::types::SandboxEnforce,
    pub session_cwd: PathBuf,
    pub mission_dir: PathBuf,
    /// The session-PRIVATE scratch root — the only TMPDIR-side path the
    /// session may write (the cleared child env points `HOME`/`TMPDIR`
    /// under it; see `crate::agent_env`). NOT the shared system temp root:
    /// allowing all of `TMPDIR` made every sibling mission's worktree and
    /// merge scratch worker-writable (P1, ticket sandbox-writable-scope).
    /// `build_inputs` defaults it to the mission's gitignored probe scratch;
    /// the runner overrides it per session with
    /// `crate::backend_claude::scratch_home_root(session_id)`.
    pub tmpdir: PathBuf,
    pub extra_write: Vec<PathBuf>,
    pub egress: Vec<String>,
    /// Mandatory validator containment (ticket
    /// `validator-mandatory-containment`): the REAL checkout roots a
    /// VALIDATOR session must not read — the checkout the snapshot was taken
    /// from, plus the primary checkout when worktree mode separates them.
    /// Empty for every non-validator session (workers, orchestrator turns)
    /// and for engine-run gates: those legitimately work in the real tree,
    /// and an empty set keeps the generated profile/argv byte-identical to
    /// the pre-containment shape. The validator's own snapshot worktree is
    /// never in this set — it lives under the mission dir, which the
    /// read-deny carve-outs (`<root>/.git`, `<root>/.kranz`) deliberately
    /// keep reachable; see [`validator_read_deny_entries`].
    pub validator_read_deny_roots: Vec<PathBuf>,
}

/// Concrete OS sandbox backend selected for this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackend {
    Seatbelt,
    Bubblewrap,
    /// Stable Win32 AppContainer profile + path-specific SID ACLs. The
    /// hostile child is created suspended and Job-owned before resume.
    AppContainer,
    /// Tier-3: run the session inside a container (see
    /// [`crate::sandbox_container`]); `ResolvedSandbox::container` is `Some`.
    Container,
}

/// How a role's `enforce` setting maps onto the current platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxDecision {
    /// Enforcement is off; no sandbox is attached.
    Off,
    /// Enforcement is requested and the platform supports it.
    Enforce(SandboxBackend),
    /// Enforcement is requested but the platform can't honor it; run-level
    /// callers must refuse rather than proceed unsandboxed.
    UnsupportedWarn,
}

fn enforce_label(enforce: crate::types::SandboxEnforce) -> &'static str {
    match enforce {
        crate::types::SandboxEnforce::Off => "off",
        crate::types::SandboxEnforce::Fs => "fs",
        crate::types::SandboxEnforce::FsNet => "fs+net",
    }
}

/// Pure decision fn: given a role's `enforce` setting and the target OS
/// (`std::env::consts::OS`-shaped string), decide whether the session gets an
/// enforced sandbox. Parameterized on `target_os` so it is testable
/// cross-platform.
pub fn platform_support(enforce: crate::types::SandboxEnforce, target_os: &str) -> SandboxDecision {
    match enforce {
        crate::types::SandboxEnforce::Off => SandboxDecision::Off,
        crate::types::SandboxEnforce::Fs if target_os == "macos" => {
            SandboxDecision::Enforce(SandboxBackend::Seatbelt)
        }
        crate::types::SandboxEnforce::FsNet if target_os == "macos" => {
            // Seatbelt's loopback-only egress profile plus the egress proxy's
            // per-host allowlist (crate::egress_proxy): the hostname rules the
            // SBPL cannot express live in the proxy, not the profile.
            SandboxDecision::Enforce(SandboxBackend::Seatbelt)
        }
        crate::types::SandboxEnforce::Fs | crate::types::SandboxEnforce::FsNet
            if target_os == "linux" =>
        {
            SandboxDecision::Enforce(SandboxBackend::Bubblewrap)
        }
        crate::types::SandboxEnforce::Fs | crate::types::SandboxEnforce::FsNet
            if target_os == "windows" =>
        {
            SandboxDecision::Enforce(SandboxBackend::AppContainer)
        }
        crate::types::SandboxEnforce::Fs | crate::types::SandboxEnforce::FsNet => {
            SandboxDecision::UnsupportedWarn
        }
    }
}

/// A resolved, enforced sandbox for one session.
#[derive(Debug, Clone)]
pub struct ResolvedSandbox {
    pub backend: SandboxBackend,
    pub inputs: SandboxInputs,
    /// Container runtime + image; `Some` iff `backend == Container`.
    pub container: Option<crate::sandbox_container::ContainerSpec>,
}

/// Expand a leading `~/` (or `~\` on Windows) in `raw` using the platform home
/// variable; otherwise return `raw` unchanged as a `PathBuf`. `pub(crate)` so
/// the engine-run gate wrap (`crate::command_exec::resolve_gate_sandbox`)
/// builds `extra_write` inputs with the SAME expansion sessions get — never a
/// second hand-rolled rule.
///
/// Windows reads `USERPROFILE`, matching [`crate::paths::global_config`]. A
/// natively launched `kranz.exe` has no `HOME` (only shells like Git Bash
/// inject one), so keying solely off `HOME` silently left `~/...` literal and
/// the sandbox grant then pointed at a directory named `~`.
pub(crate) fn expand_tilde(raw: &str) -> PathBuf {
    let rest = raw.strip_prefix("~/").or_else(|| {
        if cfg!(windows) {
            raw.strip_prefix("~\\")
        } else {
            None
        }
    });
    if let Some(rest) = rest {
        if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .filter(|value| !value.is_empty())
        {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Resolve a role's sandbox config into an (optional) enforced sandbox for
/// one session, plus an optional one-time warning string.
///
/// Returns `(Some(ResolvedSandbox), None)` when enforcement is requested and
/// supported, `(None, None)` when enforcement is off, and
/// `(None, Some(warning))` when enforcement is requested but unsupported on
/// this platform.
pub fn resolve_for_session(
    role_sandbox: &crate::types::SandboxConfig,
    session_cwd: &Path,
    mission_dir: &Path,
) -> (Option<ResolvedSandbox>, Option<String>) {
    let resolved = resolve_for_session_target(
        role_sandbox,
        session_cwd,
        mission_dir,
        std::env::consts::OS,
        command_available("bwrap"),
        crate::sandbox_container::detect(),
    );
    prewarm_xcrun_for_resolved_seatbelt(resolved.0.as_ref());
    resolved
}

/// Apple command-line-tool shims refresh a per-user `xcrun_db*` file even for
/// read-only Git commands. The profile correctly refuses that shared write,
/// so refresh the cache outside the sandbox once per resolved Seatbelt
/// session. Gate wrappers use the same bounded helper; command wrapping never
/// performs the prewarm itself.
#[cfg(target_os = "macos")]
fn prewarm_xcrun_for_resolved_seatbelt(sandbox: Option<&ResolvedSandbox>) {
    if sandbox.is_some_and(|sandbox| sandbox.backend == SandboxBackend::Seatbelt) {
        crate::command_exec::prewarm_xcrun_cache_outside_sandbox();
    }
}

#[cfg(not(target_os = "macos"))]
fn prewarm_xcrun_for_resolved_seatbelt(_sandbox: Option<&ResolvedSandbox>) {}

fn resolve_for_session_target(
    role_sandbox: &crate::types::SandboxConfig,
    session_cwd: &Path,
    mission_dir: &Path,
    target_os: &str,
    bwrap_available: bool,
    container_runtime: Option<crate::sandbox_container::ContainerRuntime>,
) -> (Option<ResolvedSandbox>, Option<String>) {
    if role_sandbox.provider == crate::types::SandboxProvider::Container {
        return resolve_container_target(
            role_sandbox,
            session_cwd,
            mission_dir,
            target_os,
            container_runtime,
        );
    }
    match platform_support(role_sandbox.enforce, target_os) {
        SandboxDecision::Off => (None, None),
        SandboxDecision::UnsupportedWarn => (
            None,
            Some(format!(
                "sandbox enforce:{} requested but unsupported on target_os={target_os}; refusing to run unsandboxed",
                enforce_label(role_sandbox.enforce)
            )),
        ),
        SandboxDecision::Enforce(SandboxBackend::Bubblewrap) if !bwrap_available => (
            None,
            Some(format!(
                "sandbox enforce:{} requested on linux but `bwrap` was not found; refusing to run unsandboxed",
                enforce_label(role_sandbox.enforce)
            )),
        ),
        SandboxDecision::Enforce(backend) => (
            Some(ResolvedSandbox {
                backend,
                inputs: build_inputs(role_sandbox, session_cwd, mission_dir),
                container: None,
            }),
            None,
        ),
    }
}

/// Resolve the tier-3 container provider: `enforce: off` stays unsandboxed;
/// `fs+net` with an empty egress list keeps the `--network none` hard egress
/// boundary. A non-empty list is supported only by Docker: the runner creates
/// a unique internal network and authenticated filtering relay before spawn.
/// Other runtimes refuse rather than silently falling back to their bridge.
/// A requested container with no runtime on PATH is refused.
fn resolve_container_target(
    role_sandbox: &crate::types::SandboxConfig,
    session_cwd: &Path,
    mission_dir: &Path,
    target_os: &str,
    runtime: Option<crate::sandbox_container::ContainerRuntime>,
) -> (Option<ResolvedSandbox>, Option<String>) {
    if role_sandbox.enforce == crate::types::SandboxEnforce::Off {
        return (None, None);
    }
    // The shipped container argv/mount contract is release-supported only on
    // Linux. A macOS operator receipt exists, but hosted macOS cannot provision
    // the VM-backed runtime needed to renew it as a CI release gate; Windows
    // containers do not honor the POSIX guest-path and `/dev/null`
    // authority-mask contract. Runtime presence alone cannot make either
    // platform supported. Refuse before spawn; macOS uses the process
    // provider's native Seatbelt boundary instead.
    if target_os != "linux" {
        return (
            None,
            Some(format!(
                "sandbox provider:container with enforce:{} is supported only on target_os=linux, not target_os={target_os}; refusing to run unsandboxed (or under an unverified container mount contract); use sandbox.provider=\"process\" for native host containment",
                enforce_label(role_sandbox.enforce)
            )),
        );
    }
    let Some(runtime) = runtime else {
        return (
            None,
            Some(
                "sandbox provider:container requested but no container runtime (docker/podman/nerdctl/container) found on PATH; refusing to run unsandboxed"
                    .to_string(),
            ),
        );
    };
    if role_sandbox.enforce == crate::types::SandboxEnforce::FsNet
        && !role_sandbox.egress.is_empty()
        && runtime != crate::sandbox_container::ContainerRuntime::Docker
    {
        return (
            None,
            Some(format!(
                "sandbox provider:container with enforce:fs+net and a non-empty egress list requires Docker's internal-network boundary; runtime {} is not live-proven for that posture — refusing to run",
                runtime.binary()
            )),
        );
    }
    (
        Some(ResolvedSandbox {
            backend: SandboxBackend::Container,
            inputs: build_inputs(role_sandbox, session_cwd, mission_dir),
            container: Some(crate::sandbox_container::ContainerSpec {
                runtime,
                image: role_sandbox
                    .image
                    .clone()
                    .unwrap_or_else(|| crate::sandbox_container::DEFAULT_IMAGE.to_string()),
                network: None,
                name: None,
            }),
        }),
        None,
    )
}

fn build_inputs(
    role_sandbox: &crate::types::SandboxConfig,
    session_cwd: &Path,
    mission_dir: &Path,
) -> SandboxInputs {
    let extra_write = role_sandbox
        .extra_write
        .iter()
        .map(|s| expand_tilde(s))
        .collect();
    SandboxInputs {
        enforce: role_sandbox.enforce,
        session_cwd: session_cwd.to_path_buf(),
        mission_dir: mission_dir.to_path_buf(),
        // Default session-private scratch: the mission's gitignored
        // contract/sandbox scratch home — the shape the engine's own probes
        // (preflight contract commands, whose cleared env points HOME/TMPDIR
        // at `runs/contract-home`) execute under. The runner overrides this
        // per session with the session's private scratch root (see the
        // `SandboxInputs::tmpdir` doc); warn-only resolves never execute
        // under the profile, so the default is never their concern.
        tmpdir: mission_dir.join("runs").join("contract-home"),
        extra_write,
        egress: role_sandbox.egress.clone(),
        // Session sandboxes never read-deny the tree they work in — the
        // validator containment resolution sets this explicitly.
        validator_read_deny_roots: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Mandatory validator containment (ticket validator-mandatory-containment)
// ---------------------------------------------------------------------------

/// The outcome of resolving one validator session's MANDATORY containment
/// (ticket `validator-mandatory-containment`). The validator is the
/// adversarial reader the whole gate rests on; its isolation must not depend
/// on the operator opting into enforcement, so `sandbox.enforce: off` (the
/// default) no longer means an unwrapped validator — where the platform has
/// a process-sandbox tier and the selected backend can apply it, the session
/// is wrapped regardless.
#[derive(Debug)]
pub struct ValidatorContainment {
    /// The sandbox to attach to the validator's [`crate::backend::SessionSpec`]
    /// — `Some` whenever a wrap applies (the role's own enforced sandbox
    /// plus the read-deny roots, or the mandatory `fs`-tier wrap under
    /// `enforce: off`), `None` only when containment degraded (see `note`).
    pub sandbox: Option<ResolvedSandbox>,
    /// The LOUD operator-facing posture note when containment could not be
    /// applied AND the operator opted into the degrade
    /// (`validatorAllowUncontainedDegrade`) — an unsupported platform, a
    /// linux without `bwrap`, or a backend that does not honor the resolved
    /// sandbox. The orchestrator surfaces it as a decision per validator
    /// spawn (so every validation round carries it); `None` when the session
    /// is contained. Without the opt-in the resolution FAILS CLOSED instead
    /// (ticket `validator-containment-degrade-fail-closed`, 14th-pass
    /// review): the degrade reopens the modify→use→restore path the
    /// mandatory-containment work was built to close, so snapshot
    /// separation plus the after-fingerprint tripwire alone are no longer
    /// the default posture.
    pub note: Option<String>,
}

/// Resolve the containment posture for one validator session (both roles —
/// scrutiny and functional run the same shape).
///
/// `session_cwd` is the throwaway snapshot worktree — the profile's sole
/// writable root alongside the session-private scratch. `read_deny_roots`
/// are the REAL checkout roots the snapshot was taken from (the active tree,
/// plus the primary checkout when worktree mode separates them); their
/// source trees become read-denied in the generated profile/argv
/// ([`validator_read_deny_entries`]). `backend` decides whether the wrap can
/// be honored at all: only the claude backend applies a resolved sandbox
/// ([`crate::types::BackendKind::supports_sandbox_enforcement`]).
///
/// `enforce != off` keeps today's fail-closed posture byte-for-byte: the
/// role's own resolution governs (an unsupported platform or missing `bwrap`
/// is an Err, mirroring the runner's `resolve_sandbox_or_refuse`), with the
/// read-deny roots ATTACHED on the process tier. The container provider
/// resolves untouched — its read-only rootfs and named mounts are already
/// the stronger containment, and the real tree is simply not mounted.
///
/// `enforce == off` is the case this ticket exists for: the mandatory
/// `fs`-tier wrap (write containment with the validator's API egress intact;
/// no operator `extraWrite` widening — the snapshot is the sole writable
/// root) wherever the platform supports it and the backend can apply it.
/// Everywhere else the resolution FAILS CLOSED (ticket
/// `validator-containment-degrade-fail-closed`, 14th-pass review — this
/// reverses the 224fa73 loud-degrade default) unless
/// `allow_uncontained_degrade` (the `validatorAllowUncontainedDegrade`
/// config flag) opts this repo back into the loud degradation note.
pub fn resolve_validator_containment(
    role_sandbox: &crate::types::SandboxConfig,
    backend: crate::types::BackendKind,
    session_cwd: &Path,
    mission_dir: &Path,
    read_deny_roots: &[PathBuf],
    allow_uncontained_degrade: bool,
) -> crate::error::Result<ValidatorContainment> {
    let resolved = resolve_validator_containment_target(
        role_sandbox,
        backend,
        session_cwd,
        mission_dir,
        read_deny_roots,
        allow_uncontained_degrade,
        std::env::consts::OS,
        command_available("bwrap"),
        crate::sandbox_container::detect(),
    );
    if let Ok(containment) = &resolved {
        prewarm_xcrun_for_resolved_seatbelt(containment.sandbox.as_ref());
    }
    resolved
}

/// [`resolve_validator_containment`] parameterized on the target OS, `bwrap`
/// availability, and container runtime so the decision matrix is testable
/// cross-platform (mirrors [`resolve_for_session_target`] /
/// `crate::command_exec::resolve_gate_sandbox_target`).
#[allow(clippy::too_many_arguments)]
fn resolve_validator_containment_target(
    role_sandbox: &crate::types::SandboxConfig,
    backend: crate::types::BackendKind,
    session_cwd: &Path,
    mission_dir: &Path,
    read_deny_roots: &[PathBuf],
    allow_uncontained_degrade: bool,
    target_os: &str,
    bwrap_available: bool,
    container_runtime: Option<crate::sandbox_container::ContainerRuntime>,
) -> crate::error::Result<ValidatorContainment> {
    if role_sandbox.enforce != crate::types::SandboxEnforce::Off {
        // The role's own resolution governs; an enforced pair with a
        // backend that cannot honor it is already refused by
        // `config::validate` (fail closed) before a mission reaches here.
        let (sandbox, warn) = resolve_for_session_target(
            role_sandbox,
            session_cwd,
            mission_dir,
            target_os,
            bwrap_available,
            container_runtime,
        );
        return match sandbox {
            Some(mut resolved) => {
                // Process-tier wraps (Seatbelt/bwrap) get the read-deny
                // roots; the container tier's mounts are the containment
                // and simply do not include the real tree.
                if resolved.backend != SandboxBackend::Container {
                    resolved.inputs.validator_read_deny_roots = read_deny_roots.to_vec();
                }
                Ok(ValidatorContainment {
                    sandbox: Some(resolved),
                    note: None,
                })
            }
            // Fail closed, mirroring resolve_sandbox_or_refuse: enforcement
            // was requested and cannot be honored on this platform.
            None => Err(crate::error::EngineError::Backend(warn.unwrap_or_else(|| {
                format!(
                    "sandbox enforce:{} requested but no sandbox could be resolved; refusing to run unsandboxed",
                    role_sandbox.enforce.as_str()
                )
            }))),
        };
    }

    // enforce: off — MANDATORY containment. The provider is ignored here:
    // `provider: container` with `enforce: off` documents "no sandboxing,
    // same as today", and the mandatory wrap is the process tier.
    //
    // Where the wrap cannot apply, the default is FAIL CLOSED (ticket
    // validator-containment-degrade-fail-closed, 14th-pass review — this
    // REVERSES the 224fa73 loud-degrade-by-default decision: a degraded
    // validator runs with snapshot separation and the tripwire only, which
    // reopens the modify→use→restore path the wrap exists to close).
    // `validatorAllowUncontainedDegrade` opts this repo back into the loud
    // per-round degradation note.
    let uncontained = |why: String, note: String| -> crate::error::Result<ValidatorContainment> {
        if !allow_uncontained_degrade {
            return Err(crate::error::EngineError::Config(format!(
                "mandatory validator containment cannot apply ({why}); refusing to run an \
                 uncontained validator — the degraded posture reopens the modify→use→restore \
                 path the wrap exists to close (ticket \
                 validator-containment-degrade-fail-closed). To run validators here anyway, \
                 set \"validatorAllowUncontainedDegrade\": true in .kranz/config.json (the \
                 loud per-round degrade returns); otherwise use a containable platform \
                 (macOS, or linux with `bwrap` on PATH) and the claude validator backend"
            )));
        }
        Ok(ValidatorContainment {
            sandbox: None,
            note: Some(note),
        })
    };
    if !backend.supports_sandbox_enforcement() {
        return uncontained(
            format!(
                "the {} backend does not apply the resolved sandbox profile",
                backend.as_str()
            ),
            format!(
                "validator sessions on the {} backend cannot be OS-sandbox-contained (only the \
                 claude backend applies the resolved sandbox profile); \
                 validatorAllowUncontainedDegrade is set, so this validator runs with \
                 snapshot isolation and the after-fingerprint tripwire only — the real checkout \
                 is reachable from the session. Select a claude validator backend for mandatory \
                 containment (ticket validator-mandatory-containment; the degrade is opt-in per \
                 validator-containment-degrade-fail-closed)",
                backend.as_str()
            ),
        );
    }
    let degraded = |why: String| {
        uncontained(
            why.clone(),
            format!(
                "validator sessions are NOT OS-sandbox-contained ({why}); \
                 validatorAllowUncontainedDegrade is set, so the validator still runs in its \
                 throwaway snapshot with the after-fingerprint tripwire on the real checkout, \
                 but hostile validator code could walk to the real checkout and restore bytes \
                 before the fingerprint — containment here is the snapshot's physical \
                 separation only (ticket validator-mandatory-containment; the degrade is \
                 opt-in per validator-containment-degrade-fail-closed)"
            ),
        )
    };
    match platform_support(crate::types::SandboxEnforce::Fs, target_os) {
        // Unreachable (Fs is not Off) — platform_support is the shared
        // vocabulary, so the match stays exhaustive anyway.
        SandboxDecision::Off => unreachable!("fs never decides Off"),
        SandboxDecision::UnsupportedWarn => {
            degraded(format!("target_os={target_os} has no process-sandbox tier"))
        }
        SandboxDecision::Enforce(SandboxBackend::Bubblewrap) if !bwrap_available => {
            degraded("linux without `bwrap` on PATH".to_string())
        }
        // platform_support never selects Container (that resolution is
        // resolve_container_target's, and the enforce!=off arm above owns
        // the provider) — the match stays exhaustive anyway.
        SandboxDecision::Enforce(SandboxBackend::Container) => {
            unreachable!("process tier only")
        }
        SandboxDecision::Enforce(backend_kind) => Ok(ValidatorContainment {
            sandbox: Some(ResolvedSandbox {
                backend: backend_kind,
                inputs: SandboxInputs {
                    // The fs tier: write containment with network intact
                    // (denying egress would brick the validator's API
                    // session — the same reason the session profile
                    // allows network under fs).
                    enforce: crate::types::SandboxEnforce::Fs,
                    session_cwd: session_cwd.to_path_buf(),
                    mission_dir: mission_dir.to_path_buf(),
                    // Pinned per session by the runner to the session's
                    // private scratch root (the same pin
                    // resolve_sandbox_or_refuse applies); the default
                    // here is the probe-shaped contract home.
                    tmpdir: mission_dir.join("runs").join("contract-home"),
                    // NO operator extraWrite widening under the mandatory
                    // wrap: the snapshot worktree is the sole writable
                    // root (plus the session-private scratch).
                    extra_write: Vec::new(),
                    egress: Vec::new(),
                    validator_read_deny_roots: read_deny_roots.to_vec(),
                },
                container: None,
            }),
            note: None,
        }),
    }
}

pub(crate) fn command_available(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    // Windows PATH entries carry no extension; the executable suffixes live in
    // PATHEXT. Probing the bare name alone reports every Windows executable as
    // missing (`grep` vs `grep.exe`).
    let mut candidates = vec![name.to_string()];
    if cfg!(windows) {
        let pathext =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
        candidates.extend(
            pathext
                .split(';')
                .filter(|ext| !ext.is_empty())
                .map(|ext| format!("{name}{ext}")),
        );
    }
    std::env::split_paths(&path).any(|dir| candidates.iter().any(|name| dir.join(name).is_file()))
}

/// Absolutize a path without requiring it to exist: canonicalize if possible,
/// otherwise join it onto the current directory when relative.
pub(crate) fn absolutize(path: &Path) -> PathBuf {
    if let Ok(canon) = path.canonicalize() {
        return canon;
    }
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Make a path absolute without resolving any component. Mount destinations
/// must name the inspected leaf itself; canonicalizing a hostile symlink
/// would instead mask its target and leave the leaf replaceable.
fn lexical_absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Escape a path for embedding in an SBPL string literal.
fn escape_sbpl_literal(path: &Path) -> String {
    escape_sbpl_string(&path.to_string_lossy())
}

fn escape_sbpl_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape a path for embedding in an SBPL `#"..."` regex literal: every
/// regex metacharacter is backslash-escaped so the path matches literally
/// (temp-dir names carry no metacharacters in practice, but a repo root
/// might — `.` in a directory name must not become an any-char match).
pub(crate) fn escape_sbpl_regex(path: &Path) -> String {
    let mut out = String::new();
    for ch in path.to_string_lossy().chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c if "^.+$*?()[]{}|".contains(c) => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// The session's writable roots: its working directory (worktree or, in
/// checkout mode, the repo root), its private scratch root, and each
/// operator-declared `extraWrite` entry. The mission dir and the shared
/// system temp root are deliberately NOT here (ticket
/// sandbox-writable-scope): the engine writes mission metadata from OUTSIDE
/// the sandbox, and whole-`TMPDIR` access made sibling missions' worktrees
/// writable. Mission metadata that would still be reachable through an
/// allowed ancestor (checkout mode: `session_cwd` is the repo root) is
/// carved back out by [`mission_write_denies`].
pub(crate) fn write_allowlist(inputs: &SandboxInputs) -> Vec<PathBuf> {
    let mut write_paths: Vec<PathBuf> =
        vec![absolutize(&inputs.session_cwd), absolutize(&inputs.tmpdir)];
    write_paths.extend(inputs.extra_write.iter().map(|p| absolutize(p)));
    write_paths.sort();
    write_paths.dedup();
    write_paths
}

/// The mission-metadata write-deny set: engine-owned files a sandboxed
/// session must never write even when an allowed ancestor (checkout mode's
/// `session_cwd` = repo root) would otherwise cover them. A worker that
/// could rewrite `events.jsonl` defeats the append-only audit log; one that
/// could drop files into `control/` injects control commands; one that
/// could rewrite `runs/*.jsonl` forges transcripts. Like
/// [`authority_read_deny_paths`], both the raw and canonical mission-dir
/// forms are expanded (Seatbelt matches canonical paths; the child may
/// address either form).
pub(crate) struct MissionWriteDenies {
    /// Engine-written files at the mission-dir root: literal write denies
    /// (Seatbelt) / `/dev/null` masks (bwrap).
    pub files: Vec<PathBuf>,
    /// The `control/` inbox dir: subpath write deny / tmpfs shadow.
    pub control_dirs: Vec<PathBuf>,
    /// The `runs/` transcript dirs: `runs/*.jsonl` regex write deny
    /// (Seatbelt) / per-file `/dev/null` masks for files present at spawn
    /// (bwrap). Subdirectories of `runs/` (the session scratch, the
    /// preflight worktree) stay writable — the regex never crosses `/`.
    pub runs_dirs: Vec<PathBuf>,
}

/// Engine-written files at the mission-dir root a sandboxed session must
/// never write (see [`MissionWriteDenies`]).
pub(crate) const MISSION_METADATA_FILES: &[&str] = &[
    "events.jsonl",
    "events.jsonl.lock",
    "state.json",
    "state.json.tmp",
    "estimate.json",
];

pub(crate) fn mission_write_denies(inputs: &SandboxInputs) -> MissionWriteDenies {
    let mut denies = MissionWriteDenies {
        files: Vec::new(),
        control_dirs: Vec::new(),
        runs_dirs: Vec::new(),
    };
    for mission_dir in [inputs.mission_dir.clone(), absolutize(&inputs.mission_dir)] {
        for name in MISSION_METADATA_FILES {
            denies.files.push(mission_dir.join(name));
        }
        denies.control_dirs.push(mission_dir.join("control"));
        denies.runs_dirs.push(mission_dir.join("runs"));
    }
    denies
}

/// The operator's real Cargo home: ambient `CARGO_HOME` when set, else
/// `~/.cargo` when HOME is set — the same resolution
/// `crate::agent_env::toolchain_var_value("CARGO_HOME", ".cargo")` applies
/// when it builds the isolated contract home. The two MUST stay in
/// lockstep: whatever the isolated home can LINK is what the profile must
/// be able to DENY writes to (see [`cargo_cache_write_deny_paths`]).
fn operator_cargo_home() -> Option<PathBuf> {
    std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
}

/// The operator's REAL shared Cargo cache directories —
/// `<cargo home>/registry` and `<cargo home>/git` — in both raw and
/// canonicalized forms (the `/var` ↔ `/private/var` idiom the write
/// allowlist already uses; Seatbelt matches canonical paths and a child
/// may address either form). Above the copy ceiling the isolated contract
/// home LINKS these in (`crate::agent_env::cache_only_cargo_home`'s
/// documented residual trade), so every sandbox profile must deny WRITES
/// to them explicitly (13th-pass review, P1): deny-default covers the
/// common case, but only an explicit deny survives EVERY allow — an
/// operator `extraWrite` of `$HOME`, or any future broadened writable
/// root, would otherwise silently re-widen the linked cache to writes
/// from worker-authored contract code, poisoning later builds. PRECISE
/// scope: the two cache dirs only, never the whole cargo home —
/// `~/.cargo/bin`'s rustup shims keep their ordinary posture. Reads stay
/// allowed: the linked cache is the session/gate's registry.
pub(crate) fn cargo_cache_write_deny_paths() -> Vec<PathBuf> {
    let Some(cargo_home) = operator_cargo_home() else {
        return Vec::new();
    };
    let mut paths = Vec::with_capacity(4);
    for base in [cargo_home.clone(), absolutize(&cargo_home)] {
        paths.push(base.join("registry"));
        paths.push(base.join("git"));
    }
    paths
}

/// Authority files a sandboxed session must never read, even under the broad
/// read allow: a read of `serve.token` IS mutation authority over `kranz
/// serve` (loopback is reachable from every sandbox tier), `serve.read.token`
/// is its GET-side sibling, `config.json` carries Slack tokens and
/// remote-workspace credentials, and `domain-terms.local` is the plaintext
/// clean-room lint vocabulary that must never be readable outside the
/// engine-side lint (14th-pass review: the mandatory validator wrap's
/// `.kranz` carve-out — kept for the snapshot — otherwise leaks it).
/// Derived from the mission dir's canonical `<repo>/.kranz/missions/<id>`
/// layout. Both the raw and the canonical mission-dir forms are expanded (the
/// dir exists at spawn time even when the token files do not yet), because
/// Seatbelt matches against canonical paths — the same `/var` ↔
/// `/private/var` split the write allowlist handles.
///
/// Also denied: `$CARGO_HOME/credentials.toml` AND the legacy extensionless
/// `$CARGO_HOME/credentials` (or `~/.cargo/...` when CARGO_HOME is unset) —
/// CARGO_HOME crosses into child envs for registry-cache locality
/// ([`crate::agent_env`]), but cargo reads BOTH filenames for registry auth
/// tokens (the legacy one is still supported and takes precedence where
/// present), so both are the same credential class as the serve token.
pub(crate) fn authority_read_deny_paths(inputs: &SandboxInputs) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for mission_dir in [inputs.mission_dir.clone(), absolutize(&inputs.mission_dir)] {
        if let Some(kranz_dir) = mission_dir.parent().and_then(Path::parent) {
            for name in [
                "serve.token",
                "serve.read.token",
                "config.json",
                "domain-terms.local",
            ] {
                paths.push(kranz_dir.join(name));
            }
        }
    }
    if let Some(cargo_home) = operator_cargo_home() {
        for base in [cargo_home.clone(), absolutize(&cargo_home)] {
            paths.push(base.join("credentials.toml"));
            paths.push(base.join("credentials"));
        }
    }
    paths
}

/// Authority DIRECTORIES a sandboxed session must never read (14th-pass
/// review — the directory half of [`authority_read_deny_paths`], denied as
/// Seatbelt subpaths / bwrap tmpfs shadows):
///
/// - `<repo>/.kranz/hook-status/` — the hook-signal projection
///   (registrations + per-run capability-token hashes). The in-sandbox
///   `kranz hook-status` relay reads only its session-private spec and POSTs
///   loopback; the server reads the projection from OUTSIDE the sandbox.
/// - `<mission_dir>/control/` — the operator→engine control inbox (approve /
///   pause / config-change commands). The orchestrator polls it from outside
///   the sandbox; no session ever legitimately reads it. The bwrap write
///   shadow already hid its contents — this aligns the Seatbelt read posture.
pub(crate) fn authority_read_deny_dirs(inputs: &SandboxInputs) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for mission_dir in [inputs.mission_dir.clone(), absolutize(&inputs.mission_dir)] {
        dirs.push(mission_dir.join("control"));
        if let Some(kranz_dir) = mission_dir.parent().and_then(Path::parent) {
            dirs.push(kranz_dir.join("hook-status"));
        }
    }
    dirs
}

/// One entry of the validator read-deny set: a top-level path of a real
/// checkout root the validator must not read, classified so the Seatbelt
/// profile can pick `subpath` vs `literal` and the bwrap argv can pick a
/// tmpfs shadow vs a `/dev/null` mask.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ValidatorReadDenyEntry {
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Top-level names a validator read-deny root ALWAYS keeps readable, because
/// the validator's own machinery cannot work without them:
///
/// - `.git` — the shared git directory. The snapshot is a WORKTREE: its
///   `.git` file points into `<root>/.git/worktrees/<n>`, and every
///   `git log`/`diff`/`show` the scrutiny/inspection flow runs resolves
///   objects and refs through the common dir. This is the narrow
///   `.git` surface the ticket keeps: READABLE (the fold needs it), never
///   writable (deny-default; a ref move is the tripwire's `for-each-ref`
///   half). `.git/config` stays readable for the same reason git itself
///   reads it — the same posture today's broad-read sandbox has.
/// - `.kranz` — the mission dir lives here, and the validator's snapshot
///   worktree sits under it (`<root>/.kranz/missions/<id>/runs/`). The
///   engine-owned metadata inside stays write-denied
///   ([`mission_write_denies`]) and the authority files read-denied
///   ([`authority_read_deny_paths`]) exactly as for any session; the rest
///   (tracked `workspace.json`, tickets) is content the snapshot already
///   carries.
const VALIDATOR_READ_DENY_CARVEOUTS: &[&str] = &[".git", ".kranz"];

/// The validator read-deny set (ticket `validator-mandatory-containment`):
/// every TOP-LEVEL entry of each [`SandboxInputs::validator_read_deny_roots`]
/// root EXCEPT the [`VALIDATOR_READ_DENY_CARVEOUTS`]. Denying whole top-level
/// entries covers the source tree without naming the root itself as a
/// subpath (which would swallow the carved-out `.git`/`.kranz` beneath it —
/// SBPL denies take precedence over every allow, so no allow could carve
/// them back out).
///
/// Entries are classified by `std::fs::metadata` — which FOLLOWS symlinks —
/// so a symlinked top-level dir is denied as a dir (and the canonical form
/// emitted alongside covers the link TARGET, the same raw+canonical idiom
/// [`authority_read_deny_paths`] uses; Seatbelt matches canonical paths).
/// Entries whose metadata fails (a broken symlink, a racer's unlink) are
/// skipped: a dangling link leaks nothing, and a vanished entry is gone.
///
/// The root ITSELF is not in this set: a literal deny on the root dir would
/// block stat/readdir of it, and coreutils `mkdir -p` stats every ancestor
/// of an absolute path — denying the root broke `mkdir -p` under the
/// snapshot (probed 2026-08-04). The root's directory LISTING therefore
/// stays readable on both tiers (names, never contents — the bwrap side
/// cannot express a listing deny without masking the carve-outs anyway).
///
/// One documented residual gap, outside the threat model (validator code can
/// create NOTHING at a deny root — writes there are deny-default): an entry
/// created at a root AFTER profile generation is not in the set — the same
/// spawn-time shape the bwrap authority masks already accept.
pub(crate) fn validator_read_deny_entries(inputs: &SandboxInputs) -> Vec<ValidatorReadDenyEntry> {
    let mut entries = std::collections::BTreeSet::new();
    for root in &inputs.validator_read_deny_roots {
        let Ok(read_dir) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in read_dir.flatten() {
            let name = entry.file_name();
            if VALIDATOR_READ_DENY_CARVEOUTS.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            let path = entry.path();
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            let is_dir = metadata.is_dir();
            entries.insert(ValidatorReadDenyEntry {
                path: path.clone(),
                is_dir,
            });
            let canonical = absolutize(&path);
            if canonical != path {
                entries.insert(ValidatorReadDenyEntry {
                    path: canonical,
                    is_dir,
                });
            }
        }
    }
    entries.into_iter().collect()
}

/// Default Anthropic egress plus mission-configured additions, trimmed and
/// de-duplicated in stable order.
pub fn effective_egress(configured: &[String]) -> Vec<String> {
    let mut out: Vec<String> = DEFAULT_EGRESS.iter().map(|s| (*s).to_string()).collect();
    for item in configured {
        let item = item.trim();
        if !item.is_empty() && !out.iter().any(|existing| existing == item) {
            out.push(item.to_string());
        }
    }
    out
}

/// Generate an SBPL profile: deny-by-default, broad read (Seatbelt cannot
/// usefully scope toolchain/dyld reads without breaking `/bin/sh`) with the
/// [`authority_read_deny_paths`]/[`authority_read_deny_dirs`] carve-out,
/// write limited to subpaths of
/// `session_cwd`, the session-private scratch `tmpdir`, and each
/// `extra_write` entry — with the mission metadata of
/// [`mission_write_denies`] carved back OUT by explicit write denies, so the
/// audit log / state snapshot / control inbox / transcripts stay read-only
/// to the session even in checkout mode (where `session_cwd` is the repo
/// root and the mission dir sits under it). `fs` allows network — the profile wraps the agent
/// binary itself, so denying egress bricks Anthropic/API sessions; write
/// containment is the fs-tier promise. `fs+net` restricts outbound TCP to
/// loopback: Seatbelt rejects hostname egress rules (`host must be * or
/// localhost`), so the per-host allowlist is enforced by the run's egress
/// proxy (`crate::egress_proxy`) — the only reachable way out. The
/// operator's real Cargo registry/git caches carry an explicit write deny
/// of their own (13th-pass review, P1 — [`cargo_cache_write_deny_paths`]):
/// the isolated contract home may LINK them in above the copy ceiling, and
/// the linked target must stay read-only under every allow.
///
/// Mandatory validator containment (ticket `validator-mandatory-containment`):
/// when [`SandboxInputs::validator_read_deny_roots`] is non-empty (validator
/// sessions only), a second read-deny block closes the broad read allow over
/// the REAL checkout's source tree — every top-level entry of each root
/// except the `.git`/`.kranz` carve-outs ([`validator_read_deny_entries`]) —
/// plus the `/dev/null` write allow every shell/git needs under deny-default
/// (the gate wrap's documented finding). The root's own listing stays
/// readable (names, never contents — a literal deny on the root breaks
/// `mkdir -p` under the snapshot, which stats every ancestor; the bwrap
/// side cannot express the listing deny at all). The snapshot worktree
/// (under `<root>/.kranz/...`) and the shared git dir stay readable; the
/// validator provably reads only its snapshot's contents. Denies take
/// precedence over the broad allow regardless of clause order (the same
/// guarantee the authority deny above relies on). Every Seatbelt session
/// also keeps `/dev/null` writable: shells, Git, and agent tool runners use
/// it for ordinary redirects even outside validator sessions.
pub fn generate_profile(inputs: &SandboxInputs) -> String {
    let write_paths = write_allowlist(inputs);

    let mut profile = String::new();
    profile.push_str("(version 1)\n");
    profile.push_str("(deny default)\n");
    profile.push('\n');
    profile.push_str("(allow process*)\n");
    profile.push_str("(allow signal (target self))\n");
    profile.push_str("(allow sysctl-read)\n");
    profile.push_str("(allow mach-lookup)\n");
    profile.push_str("(allow mach-register)\n");
    profile.push_str("(allow iokit-open)\n");
    profile.push('\n');
    // Reads stay broad: Seatbelt cannot usefully express "toolchain + dyld +
    // locale" without a long allowlist that still breaks `/bin/sh` redirects.
    // Secrecy is not the fs-tier promise — write containment is — with ONE
    // carve-out: the authority material below.
    profile.push_str("(allow file-read*)\n");
    profile.push('\n');
    // Serve tokens, the repo config, the plaintext lint vocabulary, the
    // hook-status projection, and the control inbox must stay unreadable even
    // under the broad read allow (see authority_read_deny_paths /
    // authority_read_deny_dirs). SBPL denies take precedence over allows
    // regardless of clause order (verified with sandbox-exec), so placing
    // the deny after the allow is documentary.
    let mut deny_literals = std::collections::BTreeSet::new();
    for path in authority_read_deny_paths(inputs) {
        deny_literals.insert(escape_sbpl_literal(&path));
    }
    let mut deny_subpaths = std::collections::BTreeSet::new();
    for dir in authority_read_deny_dirs(inputs) {
        deny_subpaths.insert(escape_sbpl_literal(&dir));
    }
    if !deny_literals.is_empty() || !deny_subpaths.is_empty() {
        profile.push_str("(deny file-read*\n");
        for lit in &deny_subpaths {
            profile.push_str(&format!("  (subpath \"{lit}\")\n"));
        }
        for lit in &deny_literals {
            profile.push_str(&format!("  (literal \"{lit}\")\n"));
        }
        profile.push_str(")\n");
        profile.push('\n');
    }
    // Mandatory validator containment (see the fn doc): read-deny the real
    // checkout's source tree. Directories deny as subpaths (the whole
    // subtree), files as literals. Deny wins over the broad read allow
    // regardless of clause order — placement after it is documentary. The
    // root ITSELF is deliberately NOT denied: a literal deny on the root
    // dir blocks stat/readdir of it, and coreutils `mkdir -p` stats every
    // ancestor of an absolute path — denying the root broke `mkdir -p`
    // under the snapshot (probed 2026-08-04). The root's directory LISTING
    // stays visible (names, never contents) — the same posture the bwrap
    // side is limited to anyway.
    let validator_denies = validator_read_deny_entries(inputs);
    if !validator_denies.is_empty() {
        let mut subpaths = std::collections::BTreeSet::new();
        let mut literals = std::collections::BTreeSet::new();
        for entry in &validator_denies {
            if entry.is_dir {
                subpaths.insert(escape_sbpl_literal(&entry.path));
            } else {
                literals.insert(escape_sbpl_literal(&entry.path));
            }
        }
        profile.push_str("(deny file-read*\n");
        for lit in &subpaths {
            profile.push_str(&format!("  (subpath \"{lit}\")\n"));
        }
        for lit in &literals {
            profile.push_str(&format!("  (literal \"{lit}\")\n"));
        }
        profile.push_str(")\n");
        profile.push('\n');
    }
    // `/dev/null` must stay writable even under deny-default (the gate
    // wrap's documented finding, `gate_profile_extras` — probed
    // 2026-08-03): Git, shells, and agent tool runners open it O_RDWR in
    // ordinary operation. This applies to every session, not only the
    // validator shape above.
    profile.push_str("(allow file-write* (literal \"/dev/null\"))\n");
    profile.push('\n');
    match inputs.enforce {
        crate::types::SandboxEnforce::FsNet => {
            // Loopback-only egress: the session's proxy hops (CONNECT to
            // 127.0.0.1) are legal, and every non-localhost destination is
            // denied here at the kernel boundary — the egress proxy is the
            // only way out and applies the hostname allowlist.
            profile.push_str("(allow network-outbound (remote tcp \"localhost:*\"))\n");
        }
        // `fs` (and Off) must allow network: this profile wraps the agent
        // binary, so `deny network*` bricks API egress. Egress restriction
        // is an `fs+net` concern.
        crate::types::SandboxEnforce::Fs | crate::types::SandboxEnforce::Off => {
            profile.push_str("(allow network*)\n");
        }
    }
    profile.push('\n');
    // Write allowlist: include both the canonical path and the path as given
    // (macOS `/var` ↔ `/private/var`) so shell redirects using either form match.
    profile.push_str("(allow file-write*\n");
    let mut write_literals = std::collections::BTreeSet::new();
    for p in &write_paths {
        write_literals.insert(escape_sbpl_literal(p));
    }
    for raw in [&inputs.session_cwd, &inputs.tmpdir]
        .into_iter()
        .chain(inputs.extra_write.iter())
    {
        write_literals.insert(escape_sbpl_literal(raw));
        write_literals.insert(escape_sbpl_literal(&absolutize(raw)));
    }
    for lit in &write_literals {
        profile.push_str(&format!("  (subpath \"{lit}\")\n"));
    }
    profile.push_str(")\n");
    profile.push('\n');
    // Mission metadata write deny: the allowlist no longer names the mission
    // dir, but in checkout mode `session_cwd` IS the repo root and the
    // mission dir sits under it — without these denies the audit log, state
    // snapshot, control inbox, and transcripts would be writable through the
    // session-cwd subpath allow. SBPL denies take precedence over allows
    // regardless of clause order (the same guarantee the read deny above
    // relies on), so placement after the allow is documentary.
    let write_denies = mission_write_denies(inputs);
    profile.push_str("(deny file-write*\n");
    let mut deny_literals = std::collections::BTreeSet::new();
    for p in &write_denies.files {
        deny_literals.insert(escape_sbpl_literal(p));
    }
    for lit in &deny_literals {
        profile.push_str(&format!("  (literal \"{lit}\")\n"));
    }
    let mut deny_subpaths = std::collections::BTreeSet::new();
    for p in &write_denies.control_dirs {
        deny_subpaths.insert(escape_sbpl_literal(p));
    }
    for lit in &deny_subpaths {
        profile.push_str(&format!("  (subpath \"{lit}\")\n"));
    }
    // Transcripts are `runs/*.jsonl` files directly under the runs dir; the
    // `[^/]*` keeps `runs/` SUBDIRECTORIES (session scratch, preflight
    // worktree) writable.
    let mut deny_regexes = std::collections::BTreeSet::new();
    for p in &write_denies.runs_dirs {
        deny_regexes.insert(escape_sbpl_regex(p));
    }
    for lit in &deny_regexes {
        profile.push_str(&format!("  (regex #\"^{lit}/[^/]*\\.jsonl$\")\n"));
    }
    profile.push_str(")\n");

    // Shared-Cargo-cache write deny (13th-pass review, P1 — see
    // cargo_cache_write_deny_paths for the full why): when the shared
    // registry/git cache exceeds the copy ceiling, the isolated contract
    // home LINKS it in, and only an EXPLICIT deny keeps the link target
    // read-only under every allow (an operator extraWrite of $HOME would
    // otherwise re-widen it to worker-authored contract code). Precise
    // scope: registry/ and git/ only. Denies take precedence over allows
    // regardless of clause order (the same guarantee the blocks above
    // rely on), so placement after the allow is documentary.
    let mut cache_denies = std::collections::BTreeSet::new();
    for path in cargo_cache_write_deny_paths() {
        cache_denies.insert(escape_sbpl_literal(&path));
    }
    if !cache_denies.is_empty() {
        profile.push_str("(deny file-write*\n");
        for lit in &cache_denies {
            profile.push_str(&format!("  (subpath \"{lit}\")\n"));
        }
        profile.push_str(")\n");
    }

    profile
}

/// Build the bubblewrap argv tail for running `binary args` under the resolved
/// sandbox. The caller uses program `bwrap` and passes this vector as args.
///
/// Write scope mirrors the Seatbelt profile: the whole filesystem is bound
/// read-only, then `session_cwd`, the session-private scratch `tmpdir`, and
/// each `extra_write` entry are bound writable — the mission dir and the
/// shared system temp root are NOT writable (ticket sandbox-writable-scope).
/// Mission metadata that an rw ancestor bind would otherwise cover (checkout
/// mode) is masked back out, the bwrap analogue of the profile's write deny;
/// the operator's real Cargo registry/git caches get explicit stacked
/// ro-binds for the same reason (13th-pass review — they stay readable, a
/// linked cache is the session's registry, but never writable).
///
/// Mandatory validator containment (ticket `validator-mandatory-containment`
/// — the bwrap analogue of the profile's validator read-deny block): when
/// [`SandboxInputs::validator_read_deny_roots`] is non-empty, each top-level
/// source-tree entry of those roots ([`validator_read_deny_entries`]) is
/// masked — directories shadowed by an empty tmpfs, files by a `/dev/null`
/// ro-bind — so the whole-fs ro-bind no longer exposes the real checkout's
/// contents. The `.git`/`.kranz` carve-outs stay (git needs the shared
/// object store; the snapshot lives under `.kranz`). The root's own
/// directory LISTING stays visible on both tiers (names, never contents):
/// bwrap cannot close it without masking the carve-outs, and the Seatbelt
/// side declines to (a literal deny on the root breaks `mkdir -p` under
/// the snapshot). `/dev/null` needs no allow here — the bwrap argv mounts
/// a real `/dev` (`--dev /dev`).
pub fn bubblewrap_args(
    inputs: &SandboxInputs,
    binary: &Path,
    args: &[String],
) -> crate::error::Result<Vec<String>> {
    let mut out = vec![
        "--die-with-parent".to_string(),
        "--ro-bind".to_string(),
        "/".to_string(),
        "/".to_string(),
        "--dev".to_string(),
        "/dev".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
    ];
    if inputs.enforce == crate::types::SandboxEnforce::FsNet {
        out.push("--unshare-net".to_string());
    }
    for path in write_allowlist(inputs) {
        let path = path.display().to_string();
        out.push("--bind".to_string());
        out.push(path.clone());
        out.push(path);
    }
    // bwrap has no per-path read deny, so mask the authority material the
    // whole-fs ro-bind would otherwise expose: a /dev/null bind stacked over
    // each file that exists at spawn time (later binds win; the destination
    // must exist, hence the filter). A token file created AFTER spawn stays
    // a residual gap the Seatbelt profile does not have.
    let masks: std::collections::BTreeSet<String> = authority_read_deny_paths(inputs)
        .iter()
        .filter(|path| path.exists())
        .map(|path| absolutize(path).display().to_string())
        .collect();
    for mask in masks {
        out.push("--ro-bind".to_string());
        out.push("/dev/null".to_string());
        out.push(mask);
    }
    // The bwrap analogue of the profile's validator read-deny block (ticket
    // validator-mandatory-containment — see the fn doc): shadow each real
    // source-tree entry so the whole-fs ro-bind stops exposing it. Dirs get
    // an empty tmpfs (the existing control/-shadow idiom), files a
    // /dev/null ro-bind (the authority-mask idiom). Entries were enumerated
    // from the live fs and exist at spawn; later binds win, and this block
    // lands after every rw bind, so a wide writable root cannot re-expose a
    // denied entry — the carve-outs (`.git`, `.kranz`) were never in the
    // set, so the snapshot and the shared git dir stay as their binds left
    // them.
    for entry in validator_read_deny_entries(inputs) {
        let display = entry.path.display().to_string();
        if entry.is_dir {
            out.push("--tmpfs".to_string());
            out.push(display);
        } else {
            out.push("--ro-bind".to_string());
            out.push("/dev/null".to_string());
            out.push(display);
        }
    }
    // The bwrap analogue of the profile's shared-Cargo-cache write deny
    // (13th-pass review, P1 — cargo_cache_write_deny_paths): the `/`
    // ro-bind already mounts the real caches read-only, but an rw bind
    // covering an ancestor (an extraWrite of $HOME) would silently
    // re-widen them — stack an explicit ro-bind OVER each cache dir
    // present at spawn (later binds win; the destination must exist,
    // hence the is_dir filter). The dir stays READABLE — a linked cache
    // is the session/gate's registry — only writes close. Same residual
    // gap as the authority masks: a cache dir created AFTER spawn is
    // unmasked until the next session.
    let cache_ro_binds: std::collections::BTreeSet<String> = cargo_cache_write_deny_paths()
        .iter()
        .filter(|path| path.is_dir())
        .map(|path| path.display().to_string())
        .collect();
    for bind in cache_ro_binds {
        out.push("--ro-bind".to_string());
        out.push(bind.clone());
        out.push(bind);
    }
    // Mission metadata must stay unwritable inside the sandbox: in checkout
    // mode the rw `session_cwd` bind covers the mission dir, so the audit
    // log, state snapshot, control inbox, and transcripts need the same
    // spawn-time masking the authority material gets (bwrap has no per-path
    // write deny to stack over an rw bind). Files are masked with /dev/null
    // binds; the control dir gets an empty tmpfs shadow (writes evaporate,
    // the host dir is untouched). Same residual gap as the authority masks:
    // a metadata file created AFTER spawn is unmasked until the next
    // session — the engine creates all of these before the first session,
    // EXCEPT `state.json.tmp`, which is transient (recreated on every atomic
    // state write and routinely absent at spawn). Create just that one so
    // the mask binds — an empty placeholder is inert: the engine truncates
    // it on use and nothing ever reads it. (The other metadata files are NOT
    // pre-created here: an empty `state.json` would turn a clean NotFound
    // into a parse error for first-run flows.) A creation failure is NOT
    // best-effort: proceeding would silently leave the metadata writable —
    // fail the spawn instead (3rd-pass review).
    let write_denies = mission_write_denies(inputs);
    for path in &write_denies.files {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(crate::error::EngineError::InvalidState(format!(
                    "bwrap mask prep: {} exists and is not a regular file",
                    path.display()
                )))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    for path in write_denies
        .files
        .iter()
        .filter(|p| p.ends_with("state.json.tmp"))
    {
        let metadata = std::fs::symlink_metadata(path);
        match metadata {
            // A regular file already there: mask binds over it as-is.
            Ok(m) if m.file_type().is_file() => continue,
            // Anything else present (symlink, dir, fifo, device): refuse.
            // In particular, accepting a symlink and later canonicalizing it
            // masks the target rather than the state.json.tmp mount point.
            Ok(_) => {
                return Err(crate::error::EngineError::InvalidState(format!(
                    "bwrap mask prep: {} exists and is not a regular file",
                    path.display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        // Pin the mission parent from the trusted repository anchor, then
        // create the leaf relative to that capability with no-follow. This
        // closes both the hostile-parent canonicalization gap and the leaf
        // swap between metadata and open.
        let (parent, name) = crate::paths::open_parent_nofollow(path)?;
        use cap_fs_ext::OpenOptionsFollowExt as _;
        use cap_primitives::fs::FollowSymlinks;
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .create(true)
            .write(true)
            .truncate(false)
            .follow(FollowSymlinks::No);
        let open_result = parent.open_with(name, &options);
        open_result.map_err(|e| {
            crate::error::EngineError::Io(std::io::Error::new(
                e.kind(),
                format!("bwrap mask prep: create {}: {e}", path.display()),
            ))
        })?;
    }
    let mut write_masks: std::collections::BTreeSet<String> = write_denies
        .files
        .iter()
        .filter(|path| {
            std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
        })
        .map(|path| lexical_absolute(path).display().to_string())
        .collect();
    // Transcripts: bwrap cannot express the Seatbelt `runs/*.jsonl` regex,
    // so mask each transcript file present at spawn individually. The sweep
    // is shallow — `runs/` subdirectories (session scratch, preflight
    // worktree) are not transcripts and stay as their binds left them.
    for runs_dir in &write_denies.runs_dirs {
        if let Ok(entries) = std::fs::read_dir(runs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_type().is_ok_and(|kind| kind.is_file())
                    && path.extension().is_some_and(|ext| ext == "jsonl")
                {
                    write_masks.insert(lexical_absolute(&path).display().to_string());
                }
            }
        }
    }
    for mask in write_masks {
        out.push("--ro-bind".to_string());
        out.push("/dev/null".to_string());
        out.push(mask);
    }
    // tmpfs shadows for dirs a session must not read or write: the mission
    // control inbox (the write-deny idiom — a shadow hides writes AND reads)
    // plus the authority read-deny dirs (14th-pass review — the hook-status
    // projection under the validator wrap's `.kranz` carve-out;
    // authority_read_deny_dirs overlaps control_dirs on the inbox, the set
    // dedups). Same spawn-time existence filter as the masks.
    let dir_shadows: std::collections::BTreeSet<String> = write_denies
        .control_dirs
        .iter()
        .chain(authority_read_deny_dirs(inputs).iter())
        .filter(|path| path.is_dir())
        .map(|path| absolutize(path).display().to_string())
        .collect();
    for shadow in dir_shadows {
        out.push("--tmpfs".to_string());
        out.push(shadow);
    }
    out.push("--chdir".to_string());
    out.push(absolutize(&inputs.session_cwd).display().to_string());
    out.push("--".to_string());
    out.push(binary.display().to_string());
    out.extend(args.iter().cloned());
    Ok(out)
}

/// Write the profile to a uniquely-named file under `dir`, returning its path.
pub fn write_profile_file(dir: &Path, profile: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("kranz-sandbox-{}.sb", uuid::Uuid::new_v4()));
    std::fs::write(&path, profile)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    use std::sync::Mutex;

    /// `extra_write: ["~/cache"]` must resolve against the platform home.
    /// Regression: this read `HOME` only, which a natively launched
    /// `kranz.exe` does not have (Git Bash injects one; cmd/PowerShell/
    /// Explorer do not), so the entry stayed the literal `~/cache` and the
    /// sandbox grant silently targeted a directory named `~`.
    #[test]
    fn expand_tilde_uses_the_platform_home_variable() {
        assert_eq!(
            expand_tilde("relative/path"),
            PathBuf::from("relative/path")
        );
        assert_eq!(expand_tilde("~notatilde"), PathBuf::from("~notatilde"));

        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .expect("the platform home variable is always set on a real host");
        let expanded = expand_tilde("~/cache");
        assert_eq!(expanded, PathBuf::from(&home).join("cache"));
        assert!(
            expanded.is_absolute(),
            "an expanded home path must be absolute: {expanded:?}"
        );

        #[cfg(windows)]
        assert_eq!(expand_tilde(r"~\cache"), PathBuf::from(&home).join("cache"));
    }

    #[cfg(target_os = "macos")]
    static SANDBOX_EXEC_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(target_os = "macos")]
    fn sandbox_exec_can_apply() -> bool {
        let found = std::process::Command::new("which")
            .arg("sandbox-exec")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !found {
            crate::test_capability::skip(
                crate::test_capability::capability::SANDBOX_EXEC,
                "sandbox-exec not found on this host",
            );
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
    fn bwrap_can_apply() -> bool {
        if !command_available("bwrap") {
            crate::test_capability::skip(
                crate::test_capability::capability::BWRAP,
                "bwrap not found on this host",
            );
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

    fn inputs(
        session_cwd: &Path,
        mission_dir: &Path,
        tmpdir: &Path,
        extra: Vec<PathBuf>,
    ) -> SandboxInputs {
        SandboxInputs {
            enforce: crate::types::SandboxEnforce::Fs,
            session_cwd: session_cwd.to_path_buf(),
            mission_dir: mission_dir.to_path_buf(),
            tmpdir: tmpdir.to_path_buf(),
            extra_write: extra,
            egress: Vec::new(),
            validator_read_deny_roots: Vec::new(),
        }
    }

    #[test]
    fn sandbox_profile_contains_required_clauses() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(
            session.path(),
            mission.path(),
            tmp.path(),
            vec![extra.path().to_path_buf()],
        ));

        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow file-read*)"));
        assert!(profile.contains("(allow file-write* (literal \"/dev/null\"))"));
        // `fs` must allow network so the sandboxed agent can reach its API.
        assert!(profile.contains("(allow network*)"));
        assert!(!profile.contains("(deny network*)"));

        let session_abs = absolutize(session.path());
        let tmp_abs = absolutize(tmp.path());
        let extra_abs = absolutize(extra.path());

        // The writable set: session cwd, the session-private scratch, and
        // each extraWrite entry.
        for p in [&session_abs, &tmp_abs, &extra_abs] {
            let expected = format!("(subpath \"{}\")", escape_sbpl_literal(p));
            assert!(
                profile.contains(&expected),
                "profile missing subpath rule for {:?}:\n{}",
                p,
                profile
            );
        }

        // The mission dir is NOT writable (its engine-owned metadata carries
        // explicit write denies instead — see
        // sandbox_profile_denies_mission_metadata_writes).
        let mission_abs = absolutize(mission.path());
        let mission_rule = format!("(subpath \"{}\")", escape_sbpl_literal(&mission_abs));
        assert!(
            !profile.contains(&mission_rule),
            "profile must not allow writes to the whole mission dir:\n{profile}"
        );
    }

    #[test]
    fn sandbox_profile_denies_authority_material_reads() {
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let tmp = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(repo.path(), &mission, tmp.path(), vec![]));

        // Broad reads stay, with the authority material carved out by explicit
        // denies (SBPL denies take precedence over the allow).
        assert!(profile.contains("(allow file-read*)"));
        assert!(profile.contains("(deny file-read*"));
        let kranz_dir = repo.path().join(".kranz");
        for name in [
            "serve.token",
            "serve.read.token",
            "config.json",
            // The plaintext clean-room lint vocabulary (14th-pass review).
            "domain-terms.local",
        ] {
            for base in [kranz_dir.clone(), absolutize(&kranz_dir)] {
                let expected = format!("(literal \"{}\")", escape_sbpl_literal(&base.join(name)));
                assert!(
                    profile.contains(&expected),
                    "profile missing read deny for {}:\n{profile}",
                    base.join(name).display()
                );
            }
        }
        // The cargo registry token file is denied too (CARGO_HOME crosses
        // into child envs for cache locality; its credentials must not
        // ride along).
        assert!(
            profile.contains("credentials.toml"),
            "profile missing read deny for cargo credentials:\n{profile}"
        );
    }

    /// Composition audit (ticket `config-fail-open-audit`): the effective
    /// egress list EXTENDS the compiled-in Anthropic floor — a mission's
    /// configured `egress[]` (and, downstream, its operator-approved egress
    /// grants) can only add destinations, never drop or narrow the defaults.
    /// A replace-shaped regression here strands the sandboxed session's own
    /// API access, or worse, goes unnoticed while the operator believes the
    /// floor is still composed in.
    #[test]
    fn composition_audit_effective_egress_extends_never_replaces_the_default_floor() {
        let configured = vec![
            " crates.io:443 ".to_string(),       // trimmed on the way in
            "api.anthropic.com:443".to_string(), // a duplicate of the floor
            "registry.npmjs.org:443".to_string(),
        ];
        let out = effective_egress(&configured);
        assert_eq!(
            out,
            vec![
                "api.anthropic.com:443".to_string(),
                "*.anthropic.com:443".to_string(),
                "crates.io:443".to_string(),
                "registry.npmjs.org:443".to_string(),
            ]
        );
        // An empty configured list still yields the full default floor.
        assert_eq!(effective_egress(&[]).len(), DEFAULT_EGRESS.len());
    }

    /// Composition audit: `extraWrite` EXTENDS the writable floor (session
    /// cwd + session-private scratch) — the floor itself is not configurable
    /// away, so no config shape can un-write the session's own worktree or
    /// its private scratch.
    #[test]
    fn composition_audit_extra_write_extends_never_replaces_the_writable_floor() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let inputs = inputs(
            session.path(),
            mission.path(),
            tmp.path(),
            vec![extra.path().to_path_buf()],
        );
        let writable = write_allowlist(&inputs);
        for floor in [absolutize(session.path()), absolutize(tmp.path())] {
            assert!(
                writable.contains(&floor),
                "the writable floor {floor:?} must survive any extraWrite list"
            );
        }
        assert!(writable.contains(&absolutize(extra.path())));
    }

    /// Composition audit: the explicit deny sets (mission metadata writes,
    /// authority reads) survive an `extraWrite` broad enough to COVER them.
    /// SBPL denies take precedence over every allow regardless of clause
    /// order, so the deny clauses must still be emitted when the allow side
    /// is at its widest — this is the deny-wins pin for the sandbox surface.
    #[test]
    fn composition_audit_explicit_denies_survive_a_covering_extra_write_allow() {
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        // extraWrite = the repo root: every mission file now sits under an
        // allowed subpath — the widest realistic allow shape.
        let profile = generate_profile(&inputs(
            repo.path(),
            &mission,
            tmp.path(),
            vec![repo.path().to_path_buf()],
        ));
        // The covering allow IS emitted...
        assert!(
            profile.contains(&format!(
                "(subpath \"{}\")",
                escape_sbpl_literal(&absolutize(repo.path()))
            )),
            "the covering extraWrite allow must be present:\n{profile}"
        );
        // ...and the metadata write denies still are too: the audit log,
        // state snapshot, and control inbox stay unwritable through the
        // allow because SBPL denies win over it.
        assert!(profile.contains("(deny file-write*"));
        for name in ["events.jsonl", "state.json"] {
            assert!(
                profile.contains(&escape_sbpl_literal(&mission.join(name))),
                "the write deny for {name} must survive the covering allow:\n{profile}"
            );
        }
        // Authority reads (serve.token) stay denied under the broad read
        // allow for the same reason.
        assert!(
            profile.contains(&escape_sbpl_literal(
                &repo.path().join(".kranz").join("serve.token")
            )),
            "the read deny for serve.token must survive the covering allow:\n{profile}"
        );
    }

    #[test]
    fn bubblewrap_args_mask_authority_material_with_dev_null() {
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let serve_token = repo.path().join(".kranz").join("serve.token");
        std::fs::write(&serve_token, "secret").unwrap();

        let args = bubblewrap_args(
            &inputs(repo.path(), &mission, tmp.path(), vec![]),
            Path::new("/usr/bin/claude"),
            &[],
        )
        .unwrap();
        let joined = args.join(" ");

        let expected = format!("--ro-bind /dev/null {}", absolutize(&serve_token).display());
        assert!(
            joined.contains(&expected),
            "missing /dev/null mask for serve.token: {args:?}"
        );
        // Absent files are not masked — bwrap requires the destination to exist.
        assert!(
            !joined.contains("serve.read.token"),
            "absent authority files must not be masked: {args:?}"
        );
    }

    #[test]
    fn sandbox_profile_denies_mission_metadata_writes() {
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let scratch = tempfile::tempdir().unwrap();

        // Checkout-mode shape: the session cwd is the repo root, an ANCESTOR
        // of the mission dir — without explicit write denies the audit log,
        // state snapshot, control inbox, and transcripts would be writable
        // through the session-cwd subpath allow.
        let profile = generate_profile(&inputs(repo.path(), &mission, scratch.path(), vec![]));

        assert!(
            profile.contains("(deny file-write*"),
            "missing write deny block:\n{profile}"
        );
        for name in MISSION_METADATA_FILES {
            for base in [mission.clone(), absolutize(&mission)] {
                let expected = format!("(literal \"{}\")", escape_sbpl_literal(&base.join(name)));
                assert!(
                    profile.contains(&expected),
                    "profile missing write deny for {}:\n{profile}",
                    base.join(name).display()
                );
            }
        }
        for base in [mission.clone(), absolutize(&mission)] {
            let control = format!(
                "(subpath \"{}\")",
                escape_sbpl_literal(&base.join("control"))
            );
            assert!(
                profile.contains(&control),
                "profile missing control/ write deny:\n{profile}"
            );
            let runs = format!(
                "(regex #\"^{}/[^/]*\\.jsonl$\")",
                escape_sbpl_regex(&base.join("runs"))
            );
            assert!(
                profile.contains(&runs),
                "profile missing transcript write deny:\n{profile}"
            );
        }
        // The mission dir root itself is not in the allow set.
        let mission_rule = format!(
            "(subpath \"{}\")",
            escape_sbpl_literal(&absolutize(&mission))
        );
        assert!(
            !profile.contains(&mission_rule),
            "the whole mission dir must not be writable:\n{profile}"
        );
    }

    /// 13th-pass review (P1): above the copy ceiling the isolated contract
    /// Cargo home LINKS the operator's registry/git caches in — the profile
    /// must deny writes to those REAL cache dirs (raw AND canonical forms)
    /// so the linked target stays read-only under every allow. The deny is
    /// PRECISE: the two cache dirs, never the whole cargo home (rustup/cargo
    /// binaries keep their ordinary posture).
    #[test]
    fn cache_write_deny_profile_denies_real_cache_dirs_precisely() {
        let cargo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cargo.path().join("registry")).unwrap();
        std::fs::create_dir_all(cargo.path().join("git")).unwrap();
        let _guard = crate::agent_env::EnvTestGuard::engage(&[(
            "CARGO_HOME",
            cargo.path().to_str().expect("utf-8 temp path"),
        )]);
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(
            session.path(),
            mission.path(),
            scratch.path(),
            vec![],
        ));
        for base in [cargo.path().to_path_buf(), absolutize(cargo.path())] {
            for name in ["registry", "git"] {
                let expected = format!("(subpath \"{}\")", escape_sbpl_literal(&base.join(name)));
                assert!(
                    profile.contains(&expected),
                    "profile missing cache write deny for {}:\n{profile}",
                    base.join(name).display()
                );
            }
        }
        // Precision: the cargo home ITSELF is not in the deny set — the
        // closing `"` after the home path makes this an exact-line check
        // (the registry/git lines carry a longer path and cannot match).
        for base in [cargo.path().to_path_buf(), absolutize(cargo.path())] {
            let whole_home = format!("(subpath \"{}\")", escape_sbpl_literal(&base));
            assert!(
                !profile.contains(&whole_home),
                "the deny must be precise to the cache dirs, not the whole cargo home:\n{profile}"
            );
        }
    }

    /// The bwrap analogue: the real cache dirs present at spawn get explicit
    /// ro-binds stacked AFTER the rw binds (later binds win — an rw
    /// extraWrite covering an ancestor must not re-widen them), and absent
    /// dirs are skipped (bwrap requires the destination to exist).
    #[test]
    fn cache_write_deny_bwrap_stacks_ro_binds_over_real_cache() {
        let cargo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cargo.path().join("registry")).unwrap();
        // git/ deliberately absent → not bound (the is_dir filter).
        let _guard = crate::agent_env::EnvTestGuard::engage(&[(
            "CARGO_HOME",
            cargo.path().to_str().expect("utf-8 temp path"),
        )]);
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let args = bubblewrap_args(
            &inputs(session.path(), mission.path(), scratch.path(), vec![]),
            Path::new("/usr/bin/claude"),
            &[],
        )
        .unwrap();
        let joined = args.join(" ");

        let registry = absolutize(&cargo.path().join("registry"));
        let expected = format!("--ro-bind {0} {0}", registry.display());
        assert!(
            joined.contains(&expected),
            "missing stacked ro-bind for the real registry cache: {args:?}"
        );
        let git_cache = cargo.path().join("git");
        assert!(
            !joined.contains(&git_cache.display().to_string()),
            "an absent cache dir must not be bound: {args:?}"
        );
        // Ordering is load-bearing: the cache ro-bind must land AFTER every
        // rw `--bind`, or a wide writable root would re-cover it.
        let last_rw = args
            .iter()
            .rposition(|arg| arg == "--bind")
            .expect("the writable roots are rw-bound");
        let registry_arg = registry.display().to_string();
        let cache_pos = args
            .windows(3)
            .position(|w| w[0] == "--ro-bind" && w[1] == registry_arg && w[2] == registry_arg)
            .expect("the cache ro-bind pair exists");
        assert!(
            cache_pos > last_rw,
            "the cache ro-bind must stack after the rw binds: {args:?}"
        );
    }

    #[test]
    fn sandbox_profile_keeps_sibling_temp_neighbors_unwritable() {
        // The worktree-mode layout the finding named: integration/feature
        // worktrees for ALL missions sit side by side under the shared temp
        // root. The session's own worktree + private scratch must be
        // writable; the sibling mission's worktree, the sibling's scratch,
        // and the shared temp root itself must not.
        let root = tempfile::tempdir().unwrap();
        let session = root.path().join("kranz-wt-aaa-m1-f-1-1");
        let scratch = root.path().join("kranz-worker-home-sess-1");
        let sibling = root.path().join("kranz-wt-bbb-m2-_integration");
        let sibling_scratch = root.path().join("kranz-worker-home-sess-2");
        for d in [&session, &scratch, &sibling, &sibling_scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        let mission = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(&session, mission.path(), &scratch, vec![]));

        for allowed in [&session, &scratch] {
            let expected = format!(
                "(subpath \"{}\")",
                escape_sbpl_literal(&absolutize(allowed))
            );
            assert!(
                profile.contains(&expected),
                "profile missing allow for {}:\n{profile}",
                allowed.display()
            );
        }
        for denied in [&sibling, &sibling_scratch, &root.path().to_path_buf()] {
            let rule = format!("(subpath \"{}\")", escape_sbpl_literal(&absolutize(denied)));
            assert!(
                !profile.contains(&rule),
                "{} must not be writable:\n{profile}",
                denied.display()
            );
        }
    }

    #[test]
    fn bubblewrap_args_mask_mission_metadata() {
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        let runs = mission.join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        // Engine-owned metadata present at spawn.
        let events = mission.join("events.jsonl");
        let state = mission.join("state.json");
        let transcript = runs.join("run-1.jsonl");
        let denials = runs.join("egress-denials.jsonl");
        for f in [&events, &state, &transcript, &denials] {
            std::fs::write(f, "engine").unwrap();
        }
        let control = mission.join("control");
        std::fs::create_dir_all(&control).unwrap();
        // A runs/ SUBDIRECTORY of session scratch: its jsonl files are not
        // transcripts and must NOT be masked.
        let contract_home = runs.join("contract-home");
        std::fs::create_dir_all(&contract_home).unwrap();
        let scratch_jsonl = contract_home.join("notes.jsonl");
        std::fs::write(&scratch_jsonl, "session").unwrap();

        let args = bubblewrap_args(
            &inputs(repo.path(), &mission, scratch.path(), vec![]),
            Path::new("/usr/bin/claude"),
            &[],
        )
        .unwrap();
        let joined = args.join(" ");

        for f in [&events, &state, &transcript, &denials] {
            let expected = format!("--ro-bind /dev/null {}", absolutize(f).display());
            assert!(
                joined.contains(&expected),
                "missing /dev/null mask for {}: {args:?}",
                f.display()
            );
        }
        let control_abs = absolutize(&control).display().to_string();
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--tmpfs" && w[1] == control_abs),
            "missing tmpfs shadow for control/: {args:?}"
        );
        // Absent metadata files are not masked (bwrap needs the destination
        // to exist).
        assert!(
            !joined.contains("estimate.json"),
            "absent metadata files must not be masked: {args:?}"
        );
        // No rw bind of the mission dir, and runs/-subdir scratch files stay
        // unmasked.
        let mission_abs = absolutize(&mission);
        assert!(
            !joined.contains(&format!("--bind {0} {0}", mission_abs.display())),
            "mission dir must not be rw-bound: {args:?}"
        );
        assert!(
            !joined.contains(&scratch_jsonl.display().to_string()),
            "runs/ subdir scratch files must not be masked: {args:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bubblewrap_mask_prep_rejects_preexisting_state_tmp_symlink() {
        use std::os::unix::fs::symlink;
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(mission.join("runs")).unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("outside");
        std::fs::write(&target, "unchanged").unwrap();
        symlink(&target, mission.join("state.json.tmp")).unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let error = bubblewrap_args(
            &inputs(repo.path(), &mission, scratch.path(), vec![]),
            Path::new("/usr/bin/claude"),
            &[],
        )
        .expect_err("a symlink cannot become a bwrap mask mount point");

        assert!(error.to_string().contains("not a regular file"), "{error}");
        assert_eq!(std::fs::read_to_string(target).unwrap(), "unchanged");
        assert!(
            std::fs::symlink_metadata(mission.join("state.json.tmp"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "mask preparation must not replace or follow the hostile leaf"
        );
    }

    #[test]
    fn sandbox_profile_excludes_paths_outside_allowlist() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let outsider = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(session.path(), mission.path(), tmp.path(), vec![]));

        let outsider_abs = absolutize(outsider.path());
        let forbidden = format!("(subpath \"{}\")", escape_sbpl_literal(&outsider_abs));
        assert!(
            !profile.contains(&forbidden),
            "profile unexpectedly allows write to path outside the allowlist"
        );
    }

    #[test]
    fn sandbox_profile_fs_net_restricts_egress_to_loopback() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut inputs = inputs(session.path(), mission.path(), tmp.path(), vec![]);
        inputs.enforce = crate::types::SandboxEnforce::FsNet;
        inputs.egress = vec!["crates.io:443".into(), "api.anthropic.com:443".into()];

        let profile = generate_profile(&inputs);

        // Seatbelt rejects hostname egress rules, so the profile cuts outbound
        // TCP to loopback only; the per-host allowlist (including the
        // configured entries above) is the egress proxy's job, not the SBPL's.
        assert!(!profile.contains("(allow network*)"));
        assert!(profile.contains("(allow network-outbound (remote tcp \"localhost:*\"))"));
        assert!(
            !profile.contains("crates.io") && !profile.contains("anthropic.com"),
            "no per-host egress rules in the profile:\n{profile}"
        );
    }

    #[test]
    fn bubblewrap_args_bind_write_roots_and_unshare_network_for_fs_net() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let mut inputs = inputs(
            session.path(),
            mission.path(),
            tmp.path(),
            vec![extra.path().to_path_buf()],
        );
        inputs.enforce = crate::types::SandboxEnforce::FsNet;

        let args =
            bubblewrap_args(&inputs, Path::new("/usr/bin/claude"), &["--print".into()]).unwrap();
        let joined = args.join(" ");

        assert!(args.contains(&"--unshare-net".to_string()));
        for path in [
            absolutize(session.path()),
            absolutize(tmp.path()),
            absolutize(extra.path()),
        ] {
            assert!(
                joined.contains(&format!("--bind {0} {0}", path.display())),
                "bubblewrap args missing bind for {}: {args:?}",
                path.display()
            );
        }
        // The mission dir is bound read-only via the whole-fs ro-bind only —
        // never re-bound writable.
        let mission_abs = absolutize(mission.path());
        assert!(
            !joined.contains(&format!("--bind {0} {0}", mission_abs.display())),
            "bubblewrap args must not rw-bind the mission dir: {args:?}"
        );
        assert!(joined.contains("--ro-bind / /"));
        assert!(joined.ends_with("/usr/bin/claude --print"));
    }

    #[test]
    fn sandbox_profile_write_profile_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let profile = "(version 1)\n(deny default)\n";
        let path = write_profile_file(dir.path(), profile).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), profile);
        assert!(path.starts_with(dir.path()));
    }

    #[test]
    fn sandbox_platform_support_matrix() {
        use crate::types::SandboxEnforce;

        assert_eq!(
            platform_support(SandboxEnforce::Off, "macos"),
            SandboxDecision::Off
        );
        assert_eq!(
            platform_support(SandboxEnforce::Fs, "macos"),
            SandboxDecision::Enforce(SandboxBackend::Seatbelt)
        );
        assert_eq!(
            platform_support(SandboxEnforce::Fs, "linux"),
            SandboxDecision::Enforce(SandboxBackend::Bubblewrap)
        );
        assert_eq!(
            platform_support(SandboxEnforce::FsNet, "macos"),
            SandboxDecision::Enforce(SandboxBackend::Seatbelt)
        );
        assert_eq!(
            platform_support(SandboxEnforce::FsNet, "linux"),
            SandboxDecision::Enforce(SandboxBackend::Bubblewrap)
        );
        assert_eq!(
            platform_support(SandboxEnforce::Fs, "windows"),
            SandboxDecision::Enforce(SandboxBackend::AppContainer)
        );
        assert_eq!(
            platform_support(SandboxEnforce::FsNet, "windows"),
            SandboxDecision::Enforce(SandboxBackend::AppContainer)
        );
        assert_eq!(
            platform_support(SandboxEnforce::Off, "linux"),
            SandboxDecision::Off
        );
    }

    #[test]
    fn sandbox_resolve_off_yields_none() {
        let cfg = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::Off,
            provider: crate::types::SandboxProvider::Process,
            image: None,
            extra_write: vec![],
            egress: vec![],
        };
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let (resolved, warn) = resolve_for_session(&cfg, session.path(), mission.path());
        assert!(resolved.is_none());
        assert!(warn.is_none());
    }

    #[test]
    fn sandbox_resolve_linux_requires_bwrap() {
        let cfg = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::Fs,
            provider: crate::types::SandboxProvider::Process,
            image: None,
            extra_write: vec![],
            egress: vec![],
        };
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let (resolved, warn) =
            resolve_for_session_target(&cfg, session.path(), mission.path(), "linux", false, None);
        assert!(resolved.is_none());
        assert!(
            warn.unwrap().contains("bwrap"),
            "missing-bwrap warning should name bwrap"
        );

        let (resolved, warn) =
            resolve_for_session_target(&cfg, session.path(), mission.path(), "linux", true, None);
        assert!(warn.is_none());
        assert_eq!(
            resolved.expect("bwrap present").backend,
            SandboxBackend::Bubblewrap
        );
    }

    fn container_cfg(
        enforce: crate::types::SandboxEnforce,
        egress: Vec<String>,
    ) -> crate::types::SandboxConfig {
        crate::types::SandboxConfig {
            enforce,
            provider: crate::types::SandboxProvider::Container,
            image: None,
            extra_write: vec![],
            egress,
        }
    }

    #[test]
    fn container_provider_off_stays_unsandboxed() {
        let cfg = container_cfg(crate::types::SandboxEnforce::Off, vec![]);
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let (resolved, warn) =
            resolve_for_session_target(&cfg, session.path(), mission.path(), "macos", false, None);
        assert!(resolved.is_none());
        assert!(warn.is_none());
    }

    #[test]
    fn container_provider_without_runtime_fails_closed() {
        let cfg = container_cfg(crate::types::SandboxEnforce::Fs, vec![]);
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let (resolved, warn) =
            resolve_for_session_target(&cfg, session.path(), mission.path(), "linux", false, None);
        assert!(resolved.is_none());
        let warn = warn.expect("missing runtime must produce a warning");
        assert!(warn.contains("provider:container"), "{warn}");
        assert!(warn.contains("docker/podman/nerdctl/container"), "{warn}");
        assert!(warn.contains("refusing to run unsandboxed"), "{warn}");
    }

    #[test]
    fn macos_enforced_container_provider_fails_closed_to_native_process_guidance() {
        let cfg = container_cfg(crate::types::SandboxEnforce::Fs, vec![]);
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let (resolved, warning) = resolve_for_session_target(
            &cfg,
            session.path(),
            mission.path(),
            "macos",
            false,
            Some(crate::sandbox_container::ContainerRuntime::Docker),
        );
        assert!(resolved.is_none());
        let warning = warning.expect("an unproved macOS container must refuse");
        assert!(
            warning.contains("supported only on target_os=linux"),
            "{warning}"
        );
        assert!(
            warning.contains("sandbox.provider=\"process\""),
            "{warning}"
        );
        assert!(warning.contains("native host containment"), "{warning}");
    }

    /// M7 Windows parity, phase 4: the process provider resolves the stable
    /// AppContainer backend. Merely finding `docker.exe` still does not prove
    /// the Windows container mount contract, so that provider stays refused;
    /// `off` remains the operator's explicit unsandboxed posture.
    #[test]
    fn windows_enforced_session_process_resolves_appcontainer_while_container_fails_closed() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        for enforce in [
            crate::types::SandboxEnforce::Fs,
            crate::types::SandboxEnforce::FsNet,
        ] {
            let process = crate::types::SandboxConfig {
                enforce,
                provider: crate::types::SandboxProvider::Process,
                image: None,
                extra_write: vec![],
                egress: vec![],
            };
            let (resolved, warning) = resolve_for_session_target(
                &process,
                session.path(),
                mission.path(),
                "windows",
                false,
                Some(crate::sandbox_container::ContainerRuntime::Docker),
            );
            assert!(warning.is_none(), "{warning:?}");
            let resolved = resolved.expect("Windows process enforcement resolves");
            assert_eq!(resolved.backend, SandboxBackend::AppContainer);
            assert_eq!(resolved.inputs.enforce, enforce);
            assert_eq!(resolved.inputs.session_cwd, session.path());
            assert_eq!(resolved.inputs.mission_dir, mission.path());

            let container = container_cfg(enforce, vec![]);
            let (resolved, warning) = resolve_for_session_target(
                &container,
                session.path(),
                mission.path(),
                "windows",
                false,
                Some(crate::sandbox_container::ContainerRuntime::Docker),
            );
            assert!(resolved.is_none());
            let warning = warning.expect("an unproved Windows container must refuse");
            assert!(
                warning.contains("supported only on target_os=linux"),
                "{warning}"
            );
            assert!(
                warning.contains("unverified container mount contract"),
                "{warning}"
            );
        }

        let off = container_cfg(crate::types::SandboxEnforce::Off, vec![]);
        let (resolved, warning) = resolve_for_session_target(
            &off,
            session.path(),
            mission.path(),
            "windows",
            false,
            Some(crate::sandbox_container::ContainerRuntime::Docker),
        );
        assert!(resolved.is_none());
        assert!(warning.is_none());
    }

    #[test]
    fn container_provider_fs_net_with_egress_list_resolves_for_the_proxy() {
        let cfg = container_cfg(
            crate::types::SandboxEnforce::FsNet,
            vec!["crates.io:443".to_string()],
        );
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        // Docker resolves the posture; the runner provisions the unique
        // internal network + authenticated relay before session spawn.
        let (resolved, warn) = resolve_for_session_target(
            &cfg,
            session.path(),
            mission.path(),
            "linux",
            false,
            Some(crate::sandbox_container::ContainerRuntime::Docker),
        );
        assert!(warn.is_none(), "{warn:?}");
        let resolved = resolved.expect("container fs+net with egress must resolve");
        assert_eq!(resolved.backend, SandboxBackend::Container);
        assert_eq!(resolved.inputs.egress, vec!["crates.io:443".to_string()]);
    }

    #[test]
    fn container_provider_fs_net_with_egress_refuses_non_docker_runtime() {
        let cfg = container_cfg(
            crate::types::SandboxEnforce::FsNet,
            vec!["crates.io:443".to_string()],
        );
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let (resolved, warning) = resolve_for_session_target(
            &cfg,
            session.path(),
            mission.path(),
            "linux",
            false,
            Some(crate::sandbox_container::ContainerRuntime::Podman),
        );
        assert!(resolved.is_none());
        let warning = warning.expect("unproved runtime must fail closed");
        assert!(warning.contains("requires Docker"), "{warning}");
        assert!(warning.contains("podman"), "{warning}");
    }

    #[test]
    fn container_provider_resolves_runtime_and_image() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        // Default image when config names none.
        let cfg = container_cfg(crate::types::SandboxEnforce::FsNet, vec![]);
        let (resolved, warn) = resolve_for_session_target(
            &cfg,
            session.path(),
            mission.path(),
            "linux",
            false,
            Some(crate::sandbox_container::ContainerRuntime::Podman),
        );
        assert!(warn.is_none());
        let resolved = resolved.expect("runtime present and policy supportable");
        assert_eq!(resolved.backend, SandboxBackend::Container);
        let container = resolved.container.expect("container spec must be set");
        assert_eq!(
            container.runtime,
            crate::sandbox_container::ContainerRuntime::Podman
        );
        assert_eq!(container.image, crate::sandbox_container::DEFAULT_IMAGE);

        // Configured image overrides the default.
        let mut cfg = container_cfg(crate::types::SandboxEnforce::Fs, vec![]);
        cfg.image = Some("ghcr.io/example/kranz-worker:1".to_string());
        let (resolved, warn) = resolve_for_session_target(
            &cfg,
            session.path(),
            mission.path(),
            "linux",
            false,
            Some(crate::sandbox_container::ContainerRuntime::Docker),
        );
        assert!(warn.is_none());
        assert_eq!(
            resolved
                .expect("runtime present")
                .container
                .expect("container spec")
                .image,
            "ghcr.io/example/kranz-worker:1"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_resolve_fs_on_macos_yields_resolved_sandbox() {
        let cfg = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::Fs,
            provider: crate::types::SandboxProvider::Process,
            image: None,
            extra_write: vec![],
            egress: vec![],
        };
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let (resolved, warn) = resolve_for_session(&cfg, session.path(), mission.path());
        assert!(warn.is_none());
        let resolved = resolved.expect("expected an enforced sandbox on macos");
        assert_eq!(resolved.backend, SandboxBackend::Seatbelt);
        assert_eq!(resolved.inputs.session_cwd, session.path());
        assert_eq!(resolved.inputs.mission_dir, mission.path());
        assert!(!resolved.inputs.tmpdir.as_os_str().is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_resolve_prewarms_apple_git_cache_before_profile_use() {
        use std::process::Command;

        let _guard = SANDBOX_EXEC_TEST_LOCK.lock().unwrap();
        if !sandbox_exec_can_apply() {
            return;
        }

        let repo = tempfile::tempdir().unwrap();
        let init = Command::new("/usr/bin/git")
            .args(["init", "--quiet"])
            .current_dir(repo.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .expect("initialize disposable repository");
        assert!(init.success());
        let mission = tempfile::tempdir().unwrap();
        let cfg = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::Fs,
            provider: crate::types::SandboxProvider::Process,
            image: None,
            extra_write: vec![],
            egress: vec![],
        };

        // Resolution performs the bounded host-side prewarm before the
        // generated profile can deny the shared xcrun cache refresh.
        let (resolved, warn) = resolve_for_session(&cfg, repo.path(), mission.path());
        assert!(warn.is_none());
        let resolved = resolved.expect("Seatbelt resolves on macOS");
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path =
            write_profile_file(profile_dir.path(), &generate_profile(&resolved.inputs)).unwrap();
        let output = Command::new("sandbox-exec")
            .arg("-f")
            .arg(profile_path)
            .arg("/usr/bin/git")
            .args(["status", "--short"])
            .current_dir(repo.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("run Apple Git under the resolved profile");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "Apple Git must run: {stderr}");
        assert!(
            !stderr.contains("xcrun_db"),
            "the host-side prewarm must prevent an in-sandbox cache refresh: {stderr}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_resolve_expands_tilde_extra_write_via_home() {
        let cfg = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::Fs,
            provider: crate::types::SandboxProvider::Process,
            image: None,
            extra_write: vec!["~/.cargo".to_string()],
            egress: vec![],
        };
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let home = std::env::var("HOME").expect("HOME must be set to run this test");

        let (resolved, _warn) = resolve_for_session(&cfg, session.path(), mission.path());
        let resolved = resolved.expect("expected an enforced sandbox on macos");
        assert_eq!(
            resolved.inputs.extra_write,
            vec![PathBuf::from(home).join(".cargo")]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_resolve_fs_net_on_macos_yields_seatbelt_with_loopback_profile() {
        let cfg = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::FsNet,
            provider: crate::types::SandboxProvider::Process,
            image: None,
            extra_write: vec![],
            egress: vec![],
        };
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let (resolved, warn) = resolve_for_session(&cfg, session.path(), mission.path());

        assert!(warn.is_none(), "fs+net on macOS resolves: {warn:?}");
        let resolved = resolved.expect("fs+net on macOS resolves to Seatbelt");
        assert_eq!(resolved.backend, SandboxBackend::Seatbelt);
        let profile = generate_profile(&resolved.inputs);
        assert!(
            profile.contains("(allow network-outbound (remote tcp \"localhost:*\"))"),
            "fs+net profile must restrict egress to loopback so only the egress proxy is reachable:\n{profile}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_enforcement_macos_allows_inside_denies_outside() {
        use std::process::Command;

        let _guard = SANDBOX_EXEC_TEST_LOCK.lock().unwrap();

        if !sandbox_exec_can_apply() {
            return;
        }

        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(session.path(), mission.path(), tmp.path(), vec![]));
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        let inside_file = session.path().join("inside.txt");
        let inside_status = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!("echo hi > {}", inside_file.display()))
            .status()
            .expect("failed to run sandbox-exec");
        assert!(
            inside_status.success(),
            "expected write inside session_cwd to succeed"
        );
        assert!(inside_file.exists(), "expected inside file to be created");

        let dev_null_status = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/sh")
            .arg("-c")
            .arg("echo hi > /dev/null 2>&1")
            .status()
            .expect("failed to run sandbox-exec");
        assert!(
            dev_null_status.success(),
            "ordinary shell redirects to /dev/null must succeed"
        );

        let outside_file = outside.path().join(format!(
            "kranz_sandbox_should_fail_{}",
            uuid::Uuid::new_v4()
        ));
        let outside_status = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!("echo hi > {}", outside_file.display()))
            .status()
            .expect("failed to run sandbox-exec");
        assert!(
            !outside_status.success(),
            "expected write outside allowlist to be denied"
        );
        assert!(
            !outside_file.exists(),
            "denied write must not have created the file"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_enforcement_macos_denies_authority_material_reads() {
        use std::process::Command;

        let _guard = SANDBOX_EXEC_TEST_LOCK.lock().unwrap();

        if !sandbox_exec_can_apply() {
            return;
        }

        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let kranz_dir = repo.path().join(".kranz");
        for name in [
            "serve.token",
            "serve.read.token",
            "config.json",
            "domain-terms.local",
        ] {
            std::fs::write(kranz_dir.join(name), "secret").unwrap();
        }
        let public = repo.path().join("public.txt");
        std::fs::write(&public, "public").unwrap();

        let profile = generate_profile(&inputs(repo.path(), &mission, tmp.path(), vec![]));
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        for name in [
            "serve.token",
            "serve.read.token",
            "config.json",
            "domain-terms.local",
        ] {
            let status = Command::new("sandbox-exec")
                .arg("-f")
                .arg(&profile_path)
                .arg("/bin/cat")
                .arg(kranz_dir.join(name))
                .status()
                .expect("failed to run sandbox-exec");
            assert!(
                !status.success(),
                "sandboxed read of .kranz/{name} must be denied"
            );
        }

        // Ordinary repo reads keep working under the same profile.
        let output = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/cat")
            .arg(&public)
            .output()
            .expect("failed to run sandbox-exec");
        assert!(
            output.status.success(),
            "ordinary repo reads must keep working: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "public");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_enforcement_macos_denies_mission_metadata_writes() {
        use std::process::Command;

        let _guard = SANDBOX_EXEC_TEST_LOCK.lock().unwrap();

        if !sandbox_exec_can_apply() {
            return;
        }

        // Checkout-mode shape: session_cwd is the repo root, an ANCESTOR of
        // the mission dir — the hostile case the write denies exist for.
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        let runs = mission.join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        let control = mission.join("control");
        std::fs::create_dir_all(&control).unwrap();
        let contract_home = runs.join("contract-home");
        std::fs::create_dir_all(&contract_home).unwrap();
        let events = mission.join("events.jsonl");
        let state = mission.join("state.json");
        let old_transcript = runs.join("run-old.jsonl");
        std::fs::write(&events, "{\"seq\":1}\n").unwrap();
        std::fs::write(&state, "{}").unwrap();
        std::fs::write(&old_transcript, "original\n").unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(repo.path(), &mission, scratch.path(), vec![]));
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        // Engine-owned paths refuse writes — including a NEW runs/*.jsonl
        // (the transcript regex denies creation, not just modification).
        let denied_writes = [
            format!("echo tampered >> {}", events.display()),
            format!("echo tampered > {}", state.display()),
            format!("echo x > {}", control.join("approve.json").display()),
            format!("echo forged >> {}", old_transcript.display()),
            format!("echo forged > {}", runs.join("run-new.jsonl").display()),
        ];
        for write in denied_writes {
            let status = Command::new("sandbox-exec")
                .arg("-f")
                .arg(&profile_path)
                .arg("/bin/sh")
                .arg("-c")
                .arg(&write)
                .status()
                .expect("failed to run sandbox-exec");
            assert!(!status.success(), "write must be denied: {write}");
        }
        assert_eq!(std::fs::read_to_string(&events).unwrap(), "{\"seq\":1}\n");
        assert_eq!(std::fs::read_to_string(&state).unwrap(), "{}");
        assert_eq!(
            std::fs::read_to_string(&old_transcript).unwrap(),
            "original\n"
        );
        assert!(!runs.join("run-new.jsonl").exists());
        assert!(std::fs::read_dir(&control).unwrap().next().is_none());

        // The session's own work continues under the same profile: the repo
        // tree, the private scratch, and runs/ SUBDIRECTORIES stay writable.
        let allowed_writes = [
            repo.path().join("src.txt"),
            scratch.path().join("notes.txt"),
            contract_home.join("out.txt"),
        ];
        for target in allowed_writes {
            let status = Command::new("sandbox-exec")
                .arg("-f")
                .arg(&profile_path)
                .arg("/bin/sh")
                .arg("-c")
                .arg(format!("echo ok > {}", target.display()))
                .status()
                .expect("failed to run sandbox-exec");
            assert!(
                status.success(),
                "write must be allowed: {}",
                target.display()
            );
            assert!(target.exists());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_enforcement_macos_denies_sibling_temp_neighbors() {
        use std::process::Command;

        let _guard = SANDBOX_EXEC_TEST_LOCK.lock().unwrap();

        if !sandbox_exec_can_apply() {
            return;
        }

        // The finding's layout: every mission's integration/feature
        // worktrees and scratch homes sit side by side under the shared
        // temp root. A session must write its own worktree + scratch and
        // nothing beside them.
        let root = tempfile::tempdir().unwrap();
        let session = root.path().join("kranz-wt-aaa-m1-f-1-1");
        let scratch = root.path().join("kranz-worker-home-sess-1");
        let scratch_home = scratch.join("home");
        let sibling = root.path().join("kranz-wt-bbb-m2-_integration");
        let sibling_scratch = root.path().join("kranz-worker-home-sess-2");
        for d in [&session, &scratch_home, &sibling, &sibling_scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        let mission = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(&session, mission.path(), &scratch, vec![]));
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        for allowed in [session.join("code.rs"), scratch_home.join("notes.txt")] {
            let status = Command::new("sandbox-exec")
                .arg("-f")
                .arg(&profile_path)
                .arg("/bin/sh")
                .arg("-c")
                .arg(format!("echo ok > {}", allowed.display()))
                .status()
                .expect("failed to run sandbox-exec");
            assert!(
                status.success(),
                "write inside the session's own roots must be allowed: {}",
                allowed.display()
            );
            assert!(allowed.exists());
        }

        for denied in [
            sibling.join("evil.txt"),
            sibling_scratch.join("evil.txt"),
            root.path().join("evil.txt"),
        ] {
            let status = Command::new("sandbox-exec")
                .arg("-f")
                .arg(&profile_path)
                .arg("/bin/sh")
                .arg("-c")
                .arg(format!("echo evil > {}", denied.display()))
                .status()
                .expect("failed to run sandbox-exec");
            assert!(
                !status.success(),
                "write to a temp neighbor must be denied: {}",
                denied.display()
            );
            assert!(!denied.exists());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_enforcement_macos_fs_net_loopback_profile_applies() {
        use std::process::Command;

        let _guard = SANDBOX_EXEC_TEST_LOCK.lock().unwrap();

        if !sandbox_exec_can_apply() {
            return;
        }

        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut inputs = inputs(session.path(), mission.path(), tmp.path(), vec![]);
        inputs.enforce = crate::types::SandboxEnforce::FsNet;

        // The fs+net profile (loopback-only egress) must be ACCEPTED by
        // sandbox-exec — unlike the hostname-rule shape Seatbelt rejects with
        // "host must be * or localhost" — or fs+net sessions could not run.
        let profile = generate_profile(&inputs);
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        let applied = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/usr/bin/true")
            .output()
            .expect("failed to run sandbox-exec");
        assert!(
            applied.status.success(),
            "loopback-only fs+net profile must apply cleanly on macOS: {}",
            String::from_utf8_lossy(&applied.stderr)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sandbox_enforcement_linux_bwrap_allows_inside_denies_outside() {
        use std::process::Command;

        if !bwrap_can_apply() {
            return;
        }

        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inputs = inputs(session.path(), mission.path(), tmp.path(), vec![]);

        let inside_file = session.path().join("inside.txt");
        let inside_args = bubblewrap_args(
            &inputs,
            Path::new("/bin/sh"),
            &["-c".into(), format!("echo hi > {}", inside_file.display())],
        )
        .unwrap();
        let inside_status = Command::new("bwrap")
            .args(inside_args)
            .status()
            .expect("failed to run bwrap");
        assert!(
            inside_status.success(),
            "expected write inside session_cwd to succeed"
        );
        assert!(inside_file.exists(), "expected inside file to be created");

        let outside_file = outside
            .path()
            .join(format!("kranz_bwrap_should_fail_{}", uuid::Uuid::new_v4()));
        let outside_args = bubblewrap_args(
            &inputs,
            Path::new("/bin/sh"),
            &["-c".into(), format!("echo hi > {}", outside_file.display())],
        )
        .unwrap();
        let outside_status = Command::new("bwrap")
            .args(outside_args)
            .status()
            .expect("failed to run bwrap");
        assert!(
            !outside_status.success(),
            "expected write outside allowlist to be denied"
        );
        assert!(
            !outside_file.exists(),
            "denied write must not have created the file"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sandbox_enforcement_linux_bwrap_masks_authority_material() {
        use std::process::Command;

        if !bwrap_can_apply() {
            return;
        }

        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let serve_token = repo.path().join(".kranz").join("serve.token");
        std::fs::write(&serve_token, "secret").unwrap();
        let public = repo.path().join("public.txt");
        std::fs::write(&public, "public").unwrap();
        let inputs = inputs(repo.path(), &mission, tmp.path(), vec![]);

        // A /dev/null mask can surface as an empty successful read or EACCES,
        // depending on the host's user-namespace/AppArmor policy. Both are a
        // valid read-deny boundary; the public-file control below proves the
        // child itself can still read ordinary repository content.
        let masked = Command::new("bwrap")
            .args(
                bubblewrap_args(
                    &inputs,
                    Path::new("/bin/cat"),
                    &[serve_token.display().to_string()],
                )
                .unwrap(),
            )
            .output()
            .expect("failed to run bwrap");
        assert!(
            masked.stdout.is_empty(),
            "serve.token content must be masked inside the sandbox: {}",
            String::from_utf8_lossy(&masked.stdout)
        );

        let control = Command::new("bwrap")
            .args(
                bubblewrap_args(
                    &inputs,
                    Path::new("/bin/cat"),
                    &[public.display().to_string()],
                )
                .unwrap(),
            )
            .output()
            .expect("failed to run bwrap");
        assert_eq!(String::from_utf8_lossy(&control.stdout), "public");
    }

    // -----------------------------------------------------------------------
    // Mandatory validator containment (ticket validator-mandatory-containment)
    // -----------------------------------------------------------------------

    /// A fake real-checkout root in the exact production layout: a source
    /// tree (dir + files, including a dotfile secret), the shared `.git`
    /// dir, and the `.kranz` mission layout with the validator's snapshot
    /// worktree underneath. Returns (tempdir guard, root, snapshot, mission).
    fn validator_containment_fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("secret.rs"), "fn secret() {}\n").unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\n").unwrap();
        // The .env is authority material by NAME (path-based deny); its
        // content is irrelevant to the test and deliberately not
        // secret-shaped (the range scanner fires on TOKEN= shapes — the
        // path is bound separately so no .env + value adjacency exists).
        let dotenv_path = root.join(".env");
        std::fs::write(&dotenv_path, "placeholder-content\n").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let mission = root.join(".kranz").join("missions").join("m-x");
        let snapshot = mission.join("runs").join("validator-snapshot-scrutiny");
        std::fs::create_dir_all(&snapshot).unwrap();
        std::fs::write(snapshot.join("README.md"), "snapshot copy\n").unwrap();
        std::fs::write(root.join(".kranz").join("serve.token"), "secret-token").unwrap();
        // The sensitive .kranz runtime the 14th-pass over-read finding names
        // (ticket validator-containment-kranz-overread): the plaintext lint
        // vocabulary, the hook-status projection, and the mission control
        // inbox — all reachable through the .kranz carve-out unless the
        // authority read-deny set covers them.
        std::fs::write(
            root.join(".kranz").join("domain-terms.local"),
            "acme widget\n",
        )
        .unwrap();
        let hook_status = root.join(".kranz").join("hook-status").join("m-x");
        std::fs::create_dir_all(&hook_status).unwrap();
        std::fs::write(hook_status.join("run-1.json"), "{\"tokenHash\":\"abc\"}\n").unwrap();
        let control = mission.join("control");
        std::fs::create_dir_all(&control).unwrap();
        std::fs::write(control.join("approve.json"), "{}\n").unwrap();
        (dir, root, snapshot, mission)
    }

    /// A REAL git repo in the same layout (one committed file + a committed
    /// `src/` dir, `.kranz/` ignored, the snapshot as a detached worktree
    /// under the mission's `runs/`) for the applied probes that exercise
    /// the git surface. None when git is not on PATH (mirrors the
    /// orchestrator tests' `lessons_test_repo` skip).
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn validator_containment_git_fixture() -> Option<(tempfile::TempDir, PathBuf, PathBuf, PathBuf)>
    {
        let git_ok = std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !git_ok {
            crate::test_capability::skip(
                crate::test_capability::capability::GIT,
                "git is not on PATH",
            );
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        if !std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&root)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            run(&["init"]);
            run(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        }
        run(&["config", "user.name", "test"]);
        run(&["config", "user.email", "test@example.com"]);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src").join("secret.rs"), "fn secret() {}\n").unwrap();
        std::fs::write(root.join("tracked.rs"), "fn tracked() {}\n").unwrap();
        std::fs::write(root.join(".gitignore"), ".kranz/\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-m", "init"]);
        let mission = root.join(".kranz").join("missions").join("m-x");
        let snapshot = mission.join("runs").join("validator-snapshot-scrutiny");
        std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
        run(&[
            "worktree",
            "add",
            "--detach",
            snapshot.to_str().expect("utf-8 temp path"),
        ]);
        // Engine-owned metadata + the authority material the denies cover.
        std::fs::write(mission.join("events.jsonl"), "{\"seq\":1}\n").unwrap();
        std::fs::write(root.join(".kranz").join("serve.token"), "secret-token").unwrap();
        Some((dir, root, snapshot, mission))
    }

    /// The mandatory-wrap inputs shape: the snapshot as the sole writable
    /// session root, the real checkout as the read-deny root.
    fn validator_containment_inputs(
        root: &Path,
        snapshot: &Path,
        mission: &Path,
        tmpdir: &Path,
    ) -> SandboxInputs {
        SandboxInputs {
            enforce: crate::types::SandboxEnforce::Fs,
            session_cwd: snapshot.to_path_buf(),
            mission_dir: mission.to_path_buf(),
            tmpdir: tmpdir.to_path_buf(),
            extra_write: Vec::new(),
            egress: Vec::new(),
            validator_read_deny_roots: vec![root.to_path_buf()],
        }
    }

    /// The read-deny set covers the whole source tree — dirs classified as
    /// dirs, files as files, raw AND canonical forms — and NEVER names the
    /// `.git`/`.kranz` carve-outs.
    #[test]
    fn validator_containment_entries_cover_source_tree_and_carve_out_git_and_kranz() {
        let (_dir, root, snapshot, mission) = validator_containment_fixture();
        let scratch = tempfile::tempdir().unwrap();
        let inputs = validator_containment_inputs(&root, &snapshot, &mission, scratch.path());
        let entries = validator_read_deny_entries(&inputs);

        for base in [root.clone(), absolutize(&root)] {
            let src = base.join("src");
            assert!(
                entries.contains(&ValidatorReadDenyEntry {
                    path: src.clone(),
                    is_dir: true
                }),
                "src/ must be a denied dir: {entries:?}"
            );
            for file in ["Cargo.toml", ".env"] {
                assert!(
                    entries.contains(&ValidatorReadDenyEntry {
                        path: base.join(file),
                        is_dir: false
                    }),
                    "{file} must be a denied file: {entries:?}"
                );
            }
        }
        assert!(
            entries.iter().all(|e| e
                .path
                .file_name()
                .is_some_and(|n| n != ".git" && n != ".kranz")),
            "the carve-outs must never be denied: {entries:?}"
        );
    }

    /// The generated profile: a second read-deny block closes the broad read
    /// allow over the real checkout (dirs as subpaths, files and the root
    /// itself as literals) while the snapshot stays writable and the shared
    /// git dir + mission dir stay reachable.
    #[test]
    fn validator_containment_profile_read_denies_source_tree_and_keeps_carveouts() {
        let (_dir, root, snapshot, mission) = validator_containment_fixture();
        let scratch = tempfile::tempdir().unwrap();
        let profile = generate_profile(&validator_containment_inputs(
            &root,
            &snapshot,
            &mission,
            scratch.path(),
        ));

        for base in [root.clone(), absolutize(&root)] {
            let src = format!("(subpath \"{}\")", escape_sbpl_literal(&base.join("src")));
            assert!(
                profile.contains(&src),
                "profile missing read deny for src/:\n{profile}"
            );
            for file in ["Cargo.toml", ".env"] {
                let lit = format!("(literal \"{}\")", escape_sbpl_literal(&base.join(file)));
                assert!(
                    profile.contains(&lit),
                    "profile missing read deny for {file}:\n{profile}"
                );
            }
            let root_lit = format!("(literal \"{}\")", escape_sbpl_literal(&base));
            assert!(
                !profile.contains(&root_lit),
                "the root itself is deliberately NOT denied (a literal deny breaks \
                 coreutils `mkdir -p`, which stats every ancestor):\n{profile}"
            );
            // The carve-outs are never denied: no rule names the .git or
            // .kranz DIRS themselves (the closing quote makes this exact).
            let git_rule = format!("\"{}\"", escape_sbpl_literal(&base.join(".git")));
            assert!(
                !profile.contains(&git_rule),
                ".git must stay readable (the inspection's git surface):\n{profile}"
            );
            let kranz_rule = format!("\"{}\"", escape_sbpl_literal(&base.join(".kranz")));
            assert!(
                !profile.contains(&kranz_rule),
                ".kranz must stay reachable (the snapshot lives under it):\n{profile}"
            );
        }
        // …and the .kranz carve-out does not reopen the authority material.
        for base in [root.join(".kranz"), absolutize(&root.join(".kranz"))] {
            let token = format!(
                "(literal \"{}\")",
                escape_sbpl_literal(&base.join("serve.token"))
            );
            assert!(
                profile.contains(&token),
                "the authority read deny must survive the carve-out:\n{profile}"
            );
        }
        // The snapshot stays the writable root.
        let snap_rule = format!(
            "(subpath \"{}\")",
            escape_sbpl_literal(&absolutize(&snapshot))
        );
        assert!(
            profile.contains(&snap_rule),
            "the snapshot must stay writable:\n{profile}"
        );
        // …and /dev/null stays writable (the gate wrap's documented finding:
        // git and the shell open it O_RDWR in ordinary operation).
        assert!(
            profile.contains("(allow file-write* (literal \"/dev/null\"))"),
            "validator profiles must keep /dev/null writable:\n{profile}"
        );
    }

    /// 14th-pass review (ticket `validator-containment-kranz-overread`): the
    /// `.kranz` carve-out the snapshot lives under must not reopen the
    /// sensitive runtime beneath it — the plaintext lint vocabulary
    /// (`domain-terms.local`), the hook-status projection, and the mission
    /// control inbox are read-denied (literal for the file, subpaths for the
    /// dirs) in BOTH raw and canonical forms, exactly like the serve-token
    /// authority material.
    #[test]
    fn validator_containment_profile_denies_sensitive_kranz_runtime_reads() {
        let (_dir, root, snapshot, mission) = validator_containment_fixture();
        let scratch = tempfile::tempdir().unwrap();
        let profile = generate_profile(&validator_containment_inputs(
            &root,
            &snapshot,
            &mission,
            scratch.path(),
        ));

        let kranz = root.join(".kranz");
        for base in [kranz.clone(), absolutize(&kranz)] {
            let terms = format!(
                "(literal \"{}\")",
                escape_sbpl_literal(&base.join("domain-terms.local"))
            );
            assert!(
                profile.contains(&terms),
                "profile missing read deny for domain-terms.local:\n{profile}"
            );
            let hook = format!(
                "(subpath \"{}\")",
                escape_sbpl_literal(&base.join("hook-status"))
            );
            assert!(
                profile.contains(&hook),
                "profile missing read deny for hook-status/:\n{profile}"
            );
        }
        for base in [mission.clone(), absolutize(&mission)] {
            let control = format!(
                "(subpath \"{}\")",
                escape_sbpl_literal(&base.join("control"))
            );
            assert!(
                profile.contains(&control),
                "profile missing read deny for the control inbox:\n{profile}"
            );
        }
        // …while the carve-out itself stays: no deny names the .kranz DIR
        // (the closing quote makes this exact).
        for base in [kranz.clone(), absolutize(&kranz)] {
            let kranz_rule = format!("\"{}\"", escape_sbpl_literal(&base));
            assert!(
                !profile.contains(&kranz_rule),
                ".kranz must stay reachable (the snapshot lives under it):\n{profile}"
            );
        }
    }

    /// Non-validator sessions (empty roots) get byte-stable profiles: exactly
    /// the pre-containment shape, i.e. only the authority read-deny block.
    #[test]
    fn validator_containment_empty_roots_emit_no_deny_block() {
        let (_dir, root, snapshot, mission) = validator_containment_fixture();
        let scratch = tempfile::tempdir().unwrap();
        let mut inputs = validator_containment_inputs(&root, &snapshot, &mission, scratch.path());
        inputs.validator_read_deny_roots = Vec::new();
        let profile = generate_profile(&inputs);
        assert_eq!(
            profile.matches("(deny file-read*").count(),
            1,
            "empty roots must leave the pre-containment profile shape alone:\n{profile}"
        );

        let profile = generate_profile(&validator_containment_inputs(
            &root,
            &snapshot,
            &mission,
            scratch.path(),
        ));
        assert_eq!(
            profile.matches("(deny file-read*").count(),
            2,
            "the validator read-deny block must land when roots are set:\n{profile}"
        );
    }

    /// The bwrap analogue: source dirs shadowed by tmpfs, source files
    /// masked with /dev/null, carve-outs untouched, the snapshot rw-bound.
    #[test]
    fn validator_containment_bwrap_masks_source_tree_and_keeps_carveouts() {
        let (_dir, root, snapshot, mission) = validator_containment_fixture();
        let scratch = tempfile::tempdir().unwrap();
        let args = bubblewrap_args(
            &validator_containment_inputs(&root, &snapshot, &mission, scratch.path()),
            Path::new("/usr/bin/claude"),
            &[],
        )
        .unwrap();
        let joined = args.join(" ");

        let src = absolutize(&root.join("src")).display().to_string();
        assert!(
            args.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == src),
            "missing tmpfs shadow for src/: {args:?}"
        );
        let env_file = absolutize(&root.join(".env")).display().to_string();
        assert!(
            joined.contains(&format!("--ro-bind /dev/null {env_file}")),
            "missing /dev/null mask for .env: {args:?}"
        );
        // The carve-outs are never masked, and the root itself is not
        // shadowed (bwrap cannot close the listing without hiding them).
        let git = absolutize(&root.join(".git")).display().to_string();
        assert!(
            !joined.contains(&git),
            ".git must not be masked (the inspection's git surface): {args:?}"
        );
        assert!(
            !args
                .windows(2)
                .any(|w| w[0] == "--tmpfs" && w[1] == root.display().to_string()),
            "the root itself must not be shadowed: {args:?}"
        );
        // The snapshot stays rw-bound.
        let snap = absolutize(&snapshot).display().to_string();
        assert!(
            joined.contains(&format!("--bind {snap} {snap}")),
            "the snapshot must stay rw-bound: {args:?}"
        );
    }

    /// The bwrap analogue of the 14th-pass over-read fix (ticket
    /// `validator-containment-kranz-overread`): the plaintext lint
    /// vocabulary gets a `/dev/null` mask, and the hook-status projection +
    /// the control inbox get tmpfs shadows (the control/ shadow was already
    /// the write-deny idiom; the same mechanism now hides hook-status/).
    #[test]
    fn validator_containment_bwrap_masks_sensitive_kranz_runtime() {
        let (_dir, root, snapshot, mission) = validator_containment_fixture();
        let scratch = tempfile::tempdir().unwrap();
        let args = bubblewrap_args(
            &validator_containment_inputs(&root, &snapshot, &mission, scratch.path()),
            Path::new("/usr/bin/claude"),
            &[],
        )
        .unwrap();
        let joined = args.join(" ");

        let terms = absolutize(&root.join(".kranz").join("domain-terms.local"))
            .display()
            .to_string();
        assert!(
            joined.contains(&format!("--ro-bind /dev/null {terms}")),
            "missing /dev/null mask for domain-terms.local: {args:?}"
        );
        for dir in [
            root.join(".kranz").join("hook-status"),
            mission.join("control"),
        ] {
            let shadow = absolutize(&dir).display().to_string();
            assert!(
                args.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == shadow),
                "missing tmpfs shadow for {}: {args:?}",
                dir.display()
            );
        }
    }

    // --- the resolution matrix -----------------------------------------------

    fn off_cfg() -> crate::types::SandboxConfig {
        crate::types::SandboxConfig::default()
    }

    fn fs_cfg() -> crate::types::SandboxConfig {
        crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::Fs,
            ..crate::types::SandboxConfig::default()
        }
    }

    /// The case the ticket exists for: `enforce: off` (the default) STILL
    /// wraps the validator on macOS — the mandatory fs-tier wrap with the
    /// real checkout read-denied and NO operator extraWrite widening.
    #[test]
    fn validator_containment_off_macos_wraps_mandatory_seatbelt() {
        let mut cfg = off_cfg();
        cfg.extra_write = vec!["~/elsewhere".to_string()];
        let roots = vec![PathBuf::from("/repo")];
        let containment = resolve_validator_containment_target(
            &cfg,
            crate::types::BackendKind::Claude,
            Path::new("/repo/.kranz/missions/m-x/runs/snap"),
            Path::new("/repo/.kranz/missions/m-x"),
            &roots,
            false,
            "macos",
            false,
            None,
        )
        .expect("off+macos resolves the mandatory wrap");
        assert!(containment.note.is_none(), "{:?}", containment.note);
        let sandbox = containment.sandbox.expect("a wrap applies");
        assert_eq!(sandbox.backend, SandboxBackend::Seatbelt);
        assert_eq!(
            sandbox.inputs.enforce,
            crate::types::SandboxEnforce::Fs,
            "the mandatory wrap is the fs tier (egress stays open for the API)"
        );
        assert_eq!(sandbox.inputs.validator_read_deny_roots, roots);
        assert!(
            sandbox.inputs.extra_write.is_empty(),
            "no operator extraWrite widening under the mandatory wrap"
        );
        assert_eq!(
            sandbox.inputs.session_cwd,
            PathBuf::from("/repo/.kranz/missions/m-x/runs/snap"),
            "the snapshot is the writable root"
        );
    }

    /// Linux: the mandatory wrap needs `bwrap`; without it the resolution
    /// FAILS CLOSED by default (naming the platform limit and the flag), and
    /// only the explicit `validatorAllowUncontainedDegrade` opt-in restores
    /// the loud degrade note (ticket
    /// validator-containment-degrade-fail-closed).
    #[test]
    fn validator_containment_off_linux_without_bwrap_fails_closed_unless_opted_in() {
        let roots = vec![PathBuf::from("/repo")];
        let err = resolve_validator_containment_target(
            &off_cfg(),
            crate::types::BackendKind::Claude,
            Path::new("/snap"),
            Path::new("/mission"),
            &roots,
            false,
            "linux",
            false,
            None,
        )
        .expect_err("no bwrap and no opt-in: fail closed");
        let err = err.to_string();
        assert!(err.contains("bwrap"), "{err}");
        assert!(err.contains("validatorAllowUncontainedDegrade"), "{err}");
        assert!(
            err.contains("refusing to run an uncontained validator"),
            "{err}"
        );

        let containment = resolve_validator_containment_target(
            &off_cfg(),
            crate::types::BackendKind::Claude,
            Path::new("/snap"),
            Path::new("/mission"),
            &roots,
            true,
            "linux",
            false,
            None,
        )
        .expect("the opt-in restores the loud degrade");
        assert!(containment.sandbox.is_none());
        let note = containment.note.expect("the loud note");
        assert!(note.contains("bwrap"), "{note}");
        assert!(note.contains("validator-mandatory-containment"), "{note}");

        let containment = resolve_validator_containment_target(
            &off_cfg(),
            crate::types::BackendKind::Claude,
            Path::new("/snap"),
            Path::new("/mission"),
            &roots,
            false,
            "linux",
            true,
            None,
        )
        .expect("off+linux+bwrap resolves");
        assert!(containment.note.is_none(), "{:?}", containment.note);
        assert_eq!(
            containment.sandbox.expect("a wrap applies").backend,
            SandboxBackend::Bubblewrap
        );
    }

    /// M7 Windows parity, phase 4: validators resolve the same mandatory
    /// AppContainer fs-tier wrap as other containable platforms, regardless
    /// of the legacy uncontained-degrade opt-in.
    #[test]
    fn validator_containment_off_windows_resolves_appcontainer() {
        let roots = vec![PathBuf::from("C:\\repo")];
        for allow_uncontained_degrade in [false, true] {
            let containment = resolve_validator_containment_target(
                &off_cfg(),
                crate::types::BackendKind::Claude,
                Path::new("C:\\snap"),
                Path::new("C:\\mission"),
                &roots,
                allow_uncontained_degrade,
                "windows",
                false,
                None,
            )
            .expect("Windows resolves the mandatory AppContainer wrap");
            assert!(containment.note.is_none(), "{:?}", containment.note);
            let sandbox = containment.sandbox.expect("a wrap applies");
            assert_eq!(sandbox.backend, SandboxBackend::AppContainer);
            assert_eq!(sandbox.inputs.enforce, crate::types::SandboxEnforce::Fs);
            assert_eq!(sandbox.inputs.session_cwd, PathBuf::from("C:\\snap"));
            assert_eq!(sandbox.inputs.validator_read_deny_roots, roots);
            assert!(sandbox.inputs.extra_write.is_empty());
        }
    }

    /// A backend that cannot honor the resolved sandbox must never silently
    /// run bare: by default the resolution FAILS CLOSED naming the backend
    /// and the flag; with the opt-in the wrap is skipped and the note names
    /// the backend.
    #[test]
    fn validator_containment_off_non_claude_backend_fails_closed_unless_opted_in() {
        for backend in [
            crate::types::BackendKind::Codex,
            crate::types::BackendKind::Droid,
            crate::types::BackendKind::Kimi,
            crate::types::BackendKind::Local,
            crate::types::BackendKind::Acp,
            crate::types::BackendKind::Cursor,
        ] {
            let err = resolve_validator_containment_target(
                &off_cfg(),
                backend,
                Path::new("/snap"),
                Path::new("/mission"),
                &[PathBuf::from("/repo")],
                false,
                "macos",
                false,
                None,
            )
            .expect_err("an uncontainable backend fails closed by default");
            let err = err.to_string();
            assert!(err.contains(backend.as_str()), "{err}");
            assert!(err.contains("validatorAllowUncontainedDegrade"), "{err}");

            let containment = resolve_validator_containment_target(
                &off_cfg(),
                backend,
                Path::new("/snap"),
                Path::new("/mission"),
                &[PathBuf::from("/repo")],
                true,
                "macos",
                false,
                None,
            )
            .expect("the opt-in restores the loud degrade");
            assert!(
                containment.sandbox.is_none(),
                "{backend:?} must not get a wrap it cannot honor"
            );
            let note = containment.note.expect("the loud note");
            assert!(note.contains(backend.as_str()), "{note}");
            assert!(note.contains("validator-mandatory-containment"), "{note}");
        }
    }

    /// `enforce != off` keeps the role's own resolution AND gains the
    /// read-deny roots on the process tier; the operator's extraWrite stays
    /// (the mandatory no-widening rule is the off-case wrap's).
    #[test]
    fn validator_containment_enforced_role_resolves_and_attaches_roots() {
        let mut cfg = fs_cfg();
        cfg.extra_write = vec!["~/keep".to_string()];
        let roots = vec![PathBuf::from("/repo")];
        let containment = resolve_validator_containment_target(
            &cfg,
            crate::types::BackendKind::Claude,
            Path::new("/repo/.kranz/missions/m-x/runs/snap"),
            Path::new("/repo/.kranz/missions/m-x"),
            &roots,
            false,
            "macos",
            false,
            None,
        )
        .expect("fs on macos resolves");
        assert!(containment.note.is_none(), "{:?}", containment.note);
        let sandbox = containment.sandbox.expect("the role's wrap");
        assert_eq!(sandbox.backend, SandboxBackend::Seatbelt);
        assert_eq!(sandbox.inputs.validator_read_deny_roots, roots);
        assert!(
            !sandbox.inputs.extra_write.is_empty(),
            "an enforced role keeps its declared extraWrite"
        );
    }

    /// `enforce != off` stays fail-closed on an unknown platform (the
    /// runner's resolve_sandbox_or_refuse posture, unchanged).
    #[test]
    fn validator_containment_enforced_role_still_fails_closed_where_unsupported() {
        let err = resolve_validator_containment_target(
            &fs_cfg(),
            crate::types::BackendKind::Claude,
            Path::new("/snap"),
            Path::new("/mission"),
            &[PathBuf::from("/repo")],
            false,
            "solaris",
            false,
            None,
        )
        .expect_err("enforcement requested but unhonorable must fail closed");
        assert!(err.to_string().contains("unsupported"), "{err}");
    }

    /// The container provider keeps its own (stronger) containment: resolved
    /// untouched, no read-deny roots attached (the real tree is simply not
    /// mounted). Under `enforce: off` the provider is ignored — the
    /// mandatory wrap is the process tier.
    #[test]
    fn validator_containment_container_provider_posture() {
        let cfg = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::Fs,
            provider: crate::types::SandboxProvider::Container,
            ..crate::types::SandboxConfig::default()
        };
        let containment = resolve_validator_containment_target(
            &cfg,
            crate::types::BackendKind::Claude,
            Path::new("/snap"),
            Path::new("/mission"),
            &[PathBuf::from("/repo")],
            false,
            "linux",
            false,
            Some(crate::sandbox_container::ContainerRuntime::Docker),
        )
        .expect("container resolves with a runtime");
        let sandbox = containment.sandbox.expect("the container wrap");
        assert_eq!(sandbox.backend, SandboxBackend::Container);
        assert!(
            sandbox.inputs.validator_read_deny_roots.is_empty(),
            "the container's mounts are the containment — no process-tier deny set"
        );

        let mut off_container = off_cfg();
        off_container.provider = crate::types::SandboxProvider::Container;
        let containment = resolve_validator_containment_target(
            &off_container,
            crate::types::BackendKind::Claude,
            Path::new("/snap"),
            Path::new("/mission"),
            &[PathBuf::from("/repo")],
            false,
            "macos",
            false,
            None,
        )
        .expect("off+container still gets the mandatory process-tier wrap");
        assert_eq!(
            containment.sandbox.expect("a wrap applies").backend,
            SandboxBackend::Seatbelt,
            "provider:container with enforce:off documents 'no sandboxing'; the mandatory wrap is process-tier"
        );
    }

    /// Applied proof on macOS (the ticket's test gate): a validator-session
    /// fixture under `enforce: off`-shape inputs provably CANNOT read the
    /// real checkout's source tree or the authority material, while the
    /// shared git dir and the snapshot stay readable.
    #[cfg(target_os = "macos")]
    #[test]
    fn validator_containment_macos_denies_real_checkout_reads() {
        use std::process::Command;

        let _guard = SANDBOX_EXEC_TEST_LOCK.lock().unwrap();
        if !sandbox_exec_can_apply() {
            return;
        }
        let (_dir, root, snapshot, mission) = validator_containment_fixture();
        let scratch = tempfile::tempdir().unwrap();
        let profile = generate_profile(&validator_containment_inputs(
            &root,
            &snapshot,
            &mission,
            scratch.path(),
        ));
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        let read = |path: &Path| {
            Command::new("sandbox-exec")
                .arg("-f")
                .arg(&profile_path)
                .arg("/bin/cat")
                .arg(path)
                .status()
                .expect("failed to run sandbox-exec")
        };
        // The real checkout's source tree is unreadable…
        for denied in [
            root.join("src").join("secret.rs"),
            root.join("Cargo.toml"),
            root.join(".env"),
        ] {
            assert!(
                !read(&denied).success(),
                "read of the real tree must be denied: {}",
                denied.display()
            );
        }
        // …and so is the sensitive .kranz runtime the carve-out would
        // otherwise reopen (14th-pass review,
        // validator-containment-kranz-overread): the plaintext lint
        // vocabulary, the hook-status projection, and the control inbox.
        for denied in [
            root.join(".kranz").join("domain-terms.local"),
            root.join(".kranz")
                .join("hook-status")
                .join("m-x")
                .join("run-1.json"),
            mission.join("control").join("approve.json"),
        ] {
            assert!(
                !read(&denied).success(),
                "read of the sensitive .kranz runtime must be denied: {}",
                denied.display()
            );
        }
        // …the root LISTING stays visible (names, never contents — a
        // literal deny on the root breaks coreutils `mkdir -p`, which stats
        // every ancestor; documented on the entries helper)…
        let listing = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/ls")
            .arg(&root)
            .status()
            .expect("failed to run sandbox-exec");
        assert!(
            listing.success(),
            "the root listing stays open (names, never contents)"
        );
        // …and the authority material stays denied through the carve-out.
        assert!(
            !read(&root.join(".kranz").join("serve.token")).success(),
            "the authority read deny must survive the .kranz carve-out"
        );
        // The narrow legitimate surfaces stay readable: the shared git dir
        // and the validator's own snapshot worktree.
        for allowed in [root.join(".git").join("HEAD"), snapshot.join("README.md")] {
            assert!(
                read(&allowed).success(),
                "read must keep working: {}",
                allowed.display()
            );
        }
    }

    /// The second applied half: writes outside the snapshot are denied
    /// (source tree, mission metadata, and the shared git refs — the
    /// tripwire's domain, now hard-denied), while the snapshot stays
    /// writable and read-only git (`log`/`status`/`diff` — the inspection's
    /// surface) keeps working: the validation round still completes.
    #[cfg(target_os = "macos")]
    #[test]
    fn validator_containment_macos_keeps_snapshot_writes_and_readonly_git() {
        use std::process::Command;

        let _guard = SANDBOX_EXEC_TEST_LOCK.lock().unwrap();
        if !sandbox_exec_can_apply() {
            return;
        }
        let Some((_dir, root, snapshot, mission)) = validator_containment_git_fixture() else {
            return;
        };
        let scratch = tempfile::tempdir().unwrap();
        let profile = generate_profile(&validator_containment_inputs(
            &root,
            &snapshot,
            &mission,
            scratch.path(),
        ));
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();
        let sh = |command: &str| {
            Command::new("sandbox-exec")
                .arg("-f")
                .arg(&profile_path)
                .arg("/bin/sh")
                .arg("-c")
                .arg(command)
                .status()
                .expect("failed to run sandbox-exec")
        };

        // Write denies: the real tree, the root, the engine's metadata, and
        // the shared git plumbing (index + refs).
        for command in [
            format!("echo x >> {}", root.join("tracked.rs").display()),
            format!("echo x > {}", root.join("new.txt").display()),
            format!("echo x >> {}", mission.join("events.jsonl").display()),
            format!("git -C {} add -A", snapshot.display()),
            format!("git -C {} branch -f side HEAD", snapshot.display()),
        ] {
            assert!(!sh(&command).success(), "must be denied: {command}");
        }
        // The snapshot stays fully writable (the warmed-target shape)…
        assert!(sh(&format!(
            "mkdir -p {0}/target && echo built > {0}/target/out && echo note > {0}/notes.txt",
            snapshot.display()
        ))
        .success());
        // …and the read-only git inspection surface works — the functional
        // and scrutiny validators' whole job in the snapshot.
        let git_log = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("git")
            .arg("-C")
            .arg(&snapshot)
            .arg("log")
            .arg("--oneline")
            .output()
            .expect("failed to run sandbox-exec");
        assert!(
            git_log.status.success(),
            "read-only git must work in the snapshot: {}",
            String::from_utf8_lossy(&git_log.stderr)
        );
        assert!(String::from_utf8_lossy(&git_log.stdout).contains("init"));
        assert!(sh(&format!("git -C {} status --porcelain", snapshot.display())).success());
        assert!(sh(&format!("git -C {} diff HEAD", snapshot.display())).success());
        // The snapshot's own copy of the source tree reads fine.
        assert!(sh(&format!("cat {}", snapshot.join("tracked.rs").display())).success());
    }

    /// The linux applied analogue: bwrap masks the real tree (dirs ENOENT
    /// under the tmpfs shadow, files empty under /dev/null), keeps the
    /// carve-outs and the snapshot, and read-only git still works.
    #[cfg(target_os = "linux")]
    #[test]
    fn validator_containment_linux_bwrap_denies_real_checkout_and_keeps_snapshot() {
        use std::process::Command;

        if !bwrap_can_apply() {
            return;
        }
        let Some((_dir, root, snapshot, mission)) = validator_containment_git_fixture() else {
            return;
        };
        let scratch = tempfile::tempdir().unwrap();
        let inputs = validator_containment_inputs(&root, &snapshot, &mission, scratch.path());
        let run = |command: &str| {
            Command::new("bwrap")
                .args(
                    bubblewrap_args(
                        &inputs,
                        Path::new("/bin/sh"),
                        &["-c".to_string(), command.to_string()],
                    )
                    .unwrap(),
                )
                .output()
                .expect("failed to run bwrap")
        };

        // A source DIR is shadowed: reads underneath fail outright.
        let shadowed = run(&format!(
            "cat {}",
            root.join("src").join("secret.rs").display()
        ));
        assert!(
            !shadowed.status.success(),
            "the tmpfs-shadowed source dir must not resolve: {}",
            String::from_utf8_lossy(&shadowed.stderr)
        );
        // A source FILE is /dev/null-masked: the open succeeds, the content
        // does not cross (the authority-mask idiom).
        let masked = run(&format!("cat {}", root.join("tracked.rs").display()));
        assert!(
            !String::from_utf8_lossy(&masked.stdout).contains("tracked"),
            "the masked source file must not yield its content"
        );
        // The authority material is masked too.
        let authority_read = run(&format!(
            "cat {}",
            root.join(".kranz").join("serve.token").display()
        ));
        assert!(
            !String::from_utf8_lossy(&authority_read.stdout).contains("secret-token"),
            "the authority material must stay masked"
        );
        // The carve-outs and the snapshot read fine.
        let git_head = run(&format!("cat {}", root.join(".git").join("HEAD").display()));
        assert!(git_head.status.success());
        let snap_read = run(&format!("cat {}", snapshot.join("tracked.rs").display()));
        assert!(
            String::from_utf8_lossy(&snap_read.stdout).contains("tracked"),
            "the snapshot's own copy reads fine"
        );
        // Writes outside the snapshot fail (the whole fs is ro-bound); the
        // snapshot and the git plumbing behave like the Seatbelt side.
        for command in [
            format!("echo x >> {}", root.join("tracked.rs").display()),
            format!("echo x >> {}", mission.join("events.jsonl").display()),
            format!("git -C {} branch -f side HEAD", snapshot.display()),
        ] {
            assert!(!run(&command).status.success(), "must be denied: {command}");
        }
        assert!(
            run(&format!("echo built > {}/target-out", snapshot.display()))
                .status
                .success()
        );
        let git_log = run(&format!("git -C {} log --oneline", snapshot.display()));
        assert!(
            git_log.status.success(),
            "read-only git must work in the snapshot: {}",
            String::from_utf8_lossy(&git_log.stderr)
        );
    }
}
