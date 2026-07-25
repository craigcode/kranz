//! OS sandbox profile/argv generation — Tier 2 filesystem/network containment.
//! See docs/scoping/worker-sandboxing.md tier 2.
//!
//! macOS uses Seatbelt (`sandbox-exec`) for filesystem isolation. Seatbelt
//! cannot express hostname egress allowlists (it accepts only `*`/`localhost`
//! network hosts), so `fs+net` is refused on macOS rather than pretending to
//! contain network access. Linux uses bubblewrap for filesystem isolation;
//! `fs+net` fails closed with `--unshare-net` because bwrap alone cannot
//! express a hostname egress allowlist.

use std::path::{Path, PathBuf};

/// Default egress needed by Claude/Anthropic sessions under `fs+net`.
pub const DEFAULT_EGRESS: &[&str] = &["api.anthropic.com:443", "*.anthropic.com:443"];

/// Inputs used to build a session sandbox.
#[derive(Debug, Clone)]
pub struct SandboxInputs {
    pub enforce: crate::types::SandboxEnforce,
    pub session_cwd: PathBuf,
    pub mission_dir: PathBuf,
    pub tmpdir: PathBuf,
    pub extra_write: Vec<PathBuf>,
    pub egress: Vec<String>,
}

/// Concrete OS sandbox backend selected for this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackend {
    Seatbelt,
    Bubblewrap,
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
            SandboxDecision::UnsupportedWarn
        }
        crate::types::SandboxEnforce::Fs | crate::types::SandboxEnforce::FsNet
            if target_os == "linux" =>
        {
            SandboxDecision::Enforce(SandboxBackend::Bubblewrap)
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

/// Expand a leading `~/` in `raw` using the `HOME` env var; otherwise return
/// `raw` unchanged as a `PathBuf`.
fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
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
    resolve_for_session_target(
        role_sandbox,
        session_cwd,
        mission_dir,
        std::env::consts::OS,
        command_available("bwrap"),
        crate::sandbox_container::detect(),
    )
}

fn resolve_for_session_target(
    role_sandbox: &crate::types::SandboxConfig,
    session_cwd: &Path,
    mission_dir: &Path,
    target_os: &str,
    bwrap_available: bool,
    container_runtime: Option<crate::sandbox_container::ContainerRuntime>,
) -> (Option<ResolvedSandbox>, Option<String>) {
    if role_sandbox.provider == crate::types::SandboxProvider::Container {
        return resolve_container_target(role_sandbox, session_cwd, mission_dir, container_runtime);
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
/// `fs+net` with a non-empty egress list is refused (per-host egress needs
/// the filtering proxy from the egress-grant ticket — fail closed, never
/// silently widen); a requested container with no runtime on PATH is refused.
fn resolve_container_target(
    role_sandbox: &crate::types::SandboxConfig,
    session_cwd: &Path,
    mission_dir: &Path,
    runtime: Option<crate::sandbox_container::ContainerRuntime>,
) -> (Option<ResolvedSandbox>, Option<String>) {
    if role_sandbox.enforce == crate::types::SandboxEnforce::Off {
        return (None, None);
    }
    if role_sandbox.enforce == crate::types::SandboxEnforce::FsNet
        && !role_sandbox.egress.is_empty()
    {
        return (
            None,
            Some(
                "sandbox provider:container with enforce:fs+net does not support a per-host egress allowlist yet (that needs the filtering proxy from the egress-grant ticket); refusing to run unsandboxed"
                    .to_string(),
            ),
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
    let tmpdir = std::env::var_os("TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let extra_write = role_sandbox
        .extra_write
        .iter()
        .map(|s| expand_tilde(s))
        .collect();
    SandboxInputs {
        enforce: role_sandbox.enforce,
        session_cwd: session_cwd.to_path_buf(),
        mission_dir: mission_dir.to_path_buf(),
        tmpdir,
        extra_write,
        egress: role_sandbox.egress.clone(),
    }
}

pub(crate) fn command_available(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
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

/// Escape a path for embedding in an SBPL string literal.
fn escape_sbpl_literal(path: &Path) -> String {
    escape_sbpl_string(&path.to_string_lossy())
}

fn escape_sbpl_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn write_allowlist(inputs: &SandboxInputs) -> Vec<PathBuf> {
    let mut write_paths: Vec<PathBuf> = vec![
        absolutize(&inputs.session_cwd),
        absolutize(&inputs.mission_dir),
        absolutize(&inputs.tmpdir),
    ];
    write_paths.extend(inputs.extra_write.iter().map(|p| absolutize(p)));
    write_paths.sort();
    write_paths.dedup();
    write_paths
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
/// usefully scope toolchain/dyld reads without breaking `/bin/sh`), write
/// limited to subpaths of `session_cwd`, `mission_dir`, `tmpdir`, and each
/// `extra_write` entry. `fs` allows network — the profile wraps the agent
/// binary itself, so denying egress bricks Anthropic/API sessions; write
/// containment is the fs-tier promise. `fs+net` restricts outbound TCP to
/// the configured egress list plus the default Anthropic endpoints. macOS
/// no longer resolves `fs+net` to Seatbelt because `sandbox-exec` rejects
/// those hostname rules; this generator remains covered so the fail-closed
/// proof can exercise the rejected profile shape.
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
    // Secrecy is not the fs-tier promise — write containment is.
    profile.push_str("(allow file-read*)\n");
    profile.push('\n');
    match inputs.enforce {
        crate::types::SandboxEnforce::FsNet => {
            profile.push_str("(allow network-outbound\n");
            for dest in effective_egress(&inputs.egress) {
                profile.push_str(&format!(
                    "  (remote tcp \"{}\")\n",
                    escape_sbpl_string(&dest)
                ));
            }
            profile.push_str(")\n");
        }
        // `fs` (and Off) must allow network: this profile wraps the agent
        // binary, so `deny network*` bricks API egress. Egress restriction
        // is an `fs+net` concern (and currently unsupported-warn on macOS).
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
    for raw in [&inputs.session_cwd, &inputs.mission_dir, &inputs.tmpdir]
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

    profile
}

/// Build the bubblewrap argv tail for running `binary args` under the resolved
/// sandbox. The caller uses program `bwrap` and passes this vector as args.
pub fn bubblewrap_args(inputs: &SandboxInputs, binary: &Path, args: &[String]) -> Vec<String> {
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
    out.push("--chdir".to_string());
    out.push(absolutize(&inputs.session_cwd).display().to_string());
    out.push("--".to_string());
    out.push(binary.display().to_string());
    out.extend(args.iter().cloned());
    out
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
    fn bwrap_can_apply() -> bool {
        if !command_available("bwrap") {
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
        // `fs` must allow network so the sandboxed agent can reach its API.
        assert!(profile.contains("(allow network*)"));
        assert!(!profile.contains("(deny network*)"));

        let session_abs = absolutize(session.path());
        let mission_abs = absolutize(mission.path());
        let tmp_abs = absolutize(tmp.path());
        let extra_abs = absolutize(extra.path());

        for p in [&session_abs, &mission_abs, &tmp_abs, &extra_abs] {
            let expected = format!("(subpath \"{}\")", escape_sbpl_literal(p));
            assert!(
                profile.contains(&expected),
                "profile missing subpath rule for {:?}:\n{}",
                p,
                profile
            );
        }
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
    fn sandbox_profile_fs_net_uses_egress_allowlist() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut inputs = inputs(session.path(), mission.path(), tmp.path(), vec![]);
        inputs.enforce = crate::types::SandboxEnforce::FsNet;
        inputs.egress = vec!["crates.io:443".into(), "api.anthropic.com:443".into()];

        let profile = generate_profile(&inputs);

        assert!(!profile.contains("(allow network*)"));
        assert!(profile.contains("(allow network-outbound"));
        assert!(profile.contains("(remote tcp \"api.anthropic.com:443\")"));
        assert!(profile.contains("(remote tcp \"*.anthropic.com:443\")"));
        assert!(profile.contains("(remote tcp \"crates.io:443\")"));
        assert_eq!(
            profile.matches("api.anthropic.com:443").count(),
            1,
            "configured egress must not duplicate the default"
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

        let args = bubblewrap_args(&inputs, Path::new("/usr/bin/claude"), &["--print".into()]);
        let joined = args.join(" ");

        assert!(args.contains(&"--unshare-net".to_string()));
        for path in [
            absolutize(session.path()),
            absolutize(mission.path()),
            absolutize(tmp.path()),
            absolutize(extra.path()),
        ] {
            assert!(
                joined.contains(&format!("--bind {0} {0}", path.display())),
                "bubblewrap args missing bind for {}: {args:?}",
                path.display()
            );
        }
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
            SandboxDecision::UnsupportedWarn
        );
        assert_eq!(
            platform_support(SandboxEnforce::FsNet, "linux"),
            SandboxDecision::Enforce(SandboxBackend::Bubblewrap)
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
    fn container_provider_fs_net_with_egress_list_is_refused() {
        let cfg = container_cfg(
            crate::types::SandboxEnforce::FsNet,
            vec!["crates.io:443".to_string()],
        );
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        // Refusal happens before runtime detection: it must hold even where a
        // runtime IS available, so inject one.
        let (resolved, warn) = resolve_for_session_target(
            &cfg,
            session.path(),
            mission.path(),
            "linux",
            false,
            Some(crate::sandbox_container::ContainerRuntime::Docker),
        );
        assert!(resolved.is_none());
        let warn = warn.expect("per-host egress under provider:container must be refused");
        assert!(warn.contains("egress"), "{warn}");
        assert!(warn.contains("refusing to run unsandboxed"), "{warn}");
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
            "macos",
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
            "macos",
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
    fn sandbox_resolve_fs_net_on_macos_refuses_hostname_egress() {
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

        assert!(resolved.is_none());
        let warn = warn.expect("fs+net on macOS should produce a warning");
        assert!(warn.contains("fs+net"), "{warn}");
        assert!(warn.contains("unsupported"), "{warn}");
        assert!(warn.contains("refusing"), "{warn}");
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
    #[ignore = "run alone: an invalid hostname profile can poison later sandbox-exec calls in this test binary"]
    fn sandbox_enforcement_macos_fs_net_hostname_profile_fails_closed() {
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

        let profile = generate_profile(&inputs);
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        let denied = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/usr/bin/true")
            .output()
            .expect("failed to run sandbox-exec");
        assert!(
            !denied.status.success(),
            "hostname-based fs+net profile must fail closed on macOS rather than run with invalid egress rules"
        );
        let stderr = String::from_utf8_lossy(&denied.stderr);
        assert!(
            stderr.contains("host must be * or localhost"),
            "unexpected sandbox-exec error for hostname egress limitation: {stderr}"
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
        );
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
        );
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
}
