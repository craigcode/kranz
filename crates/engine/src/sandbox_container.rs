//! Tier-3 container sandbox provider — run a worker/validator session inside
//! a container with the declared write/egress policy. See
//! docs/scoping/worker-sandboxing.md tier 3.
//!
//! Two network postures for `enforce = "fs+net"`, chosen by the egress list.
//! An EMPTY `egress` list runs `--network none` — a hard egress boundary that
//! works identically on macOS and Linux. Note the honest tradeoff: `none`
//! also blocks the agent's API egress, so it suits offline gates/validation.
//! A NON-EMPTY `egress` list keeps the runtime default bridge and points the
//! session env at the run's host-side filtering egress proxy
//! (`crate::egress_proxy`) via `host.docker.internal` — the proxy enforces
//! the per-host allowlist at CONNECT time and records structured denials.
//! The proxy hop is env-based (advisory on the bridge: a process that ignores
//! the proxy vars bypasses the filter), so `config::validate` REFUSES
//! `provider = "container"` with `enforce = "fs+net"` and a non-empty egress
//! list (fail closed — [`crate::types::SandboxProvider::enforces_hard_net_boundary`])
//! until a hard per-host container boundary (internal-network sidecar)
//! lands; the builder below keeps the proxy-routed argv for that follow-up.
//! API-driven workers that need
//! no egress list use `fs` (runtime default bridge/NAT, the same
//! permissiveness as the tier-2 fs tier).
//!
//! Write policy: the container's root filesystem is read-only; the writable
//! set is exactly the declared mounts — `session_cwd` (rw), `mission_dir`
//! (ro, so the engine-owned audit log / state / control inbox / transcripts
//! stay read-only inside the container even when the mission dir sits under
//! an rw-mounted `session_cwd`), the session-private scratch `tmpdir` (rw,
//! also `HOME`/`TMPDIR` inside the container — NOT the shared system temp
//! root, which would expose sibling missions' worktrees), and each
//! `extra_write` entry (rw). Everything else is denied by the
//! runtime, the container analogue of the tier-2 write allowlist.
//!
//! Worker image: the default `DEFAULT_IMAGE` proves the isolation boundary
//! but cannot run an agent. A production worker image needs the agent CLI +
//! Node on PATH plus the mission toolchain — the same layering the repo's
//! `Dockerfile` comment block spells out for the M6 cloud image (see the
//! "What this image intentionally does NOT bundle" section there).

use std::path::Path;

use crate::sandbox::SandboxInputs;

/// Image used when the role config does not name one. Minimal and
/// pullable anywhere; production use should set `sandbox.image`.
pub const DEFAULT_IMAGE: &str = "alpine:3";

/// Container runtimes kranz knows how to drive, in PATH preference order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerRuntime {
    Docker,
    Podman,
    Nerdctl,
    /// Apple's `container` CLI (github.com/apple/container). Last in
    /// preference; its argv is the docker-compatible common denominator.
    AppleContainer,
}

impl ContainerRuntime {
    /// All runtimes in detection preference order.
    const PREFERENCE_ORDER: &'static [ContainerRuntime] = &[
        ContainerRuntime::Docker,
        ContainerRuntime::Podman,
        ContainerRuntime::Nerdctl,
        ContainerRuntime::AppleContainer,
    ];

    /// The executable name resolved on PATH and spawned for `run`.
    pub fn binary(self) -> &'static str {
        match self {
            ContainerRuntime::Docker => "docker",
            ContainerRuntime::Podman => "podman",
            ContainerRuntime::Nerdctl => "nerdctl",
            ContainerRuntime::AppleContainer => "container",
        }
    }
}

/// Detect the preferred available container runtime on this host's PATH.
pub fn detect() -> Option<ContainerRuntime> {
    detect_with(crate::sandbox::command_available)
}

/// Detection with an injectable PATH lookup so tests control availability.
pub fn detect_with(lookup: impl Fn(&str) -> bool) -> Option<ContainerRuntime> {
    ContainerRuntime::PREFERENCE_ORDER
        .iter()
        .copied()
        .find(|runtime| lookup(runtime.binary()))
}

/// The resolved container to run a session in: which runtime, which image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerSpec {
    pub runtime: ContainerRuntime,
    pub image: String,
}

/// Build the `<runtime> run` argv (excluding the runtime binary itself) for
/// running `binary args` under the resolved container sandbox.
///
/// Network: `fs+net` with an empty egress list maps to `--network none` (the
/// hard boundary); `fs+net` with a non-empty egress list keeps the runtime
/// default bridge and forwards the run's egress-proxy endpoint into the
/// container env (`proxy_url`, reaching the host-side proxy via
/// `host.docker.internal`; Linux docker additionally gets the `host-gateway`
/// hosts entry). That proxy-routed posture is advisory-only, so
/// `config::validate` refuses it (fail closed) until the internal-network
/// sidecar boundary lands — this branch remains for that follow-up. The
/// runner guarantees `proxy_url` is `Some` whenever a
/// proxy-routed container session spawns — a proxy start failure fails the
/// run closed before this point. `fs` passes no network flag, keeping the
/// runtime's default bridge/NAT — the same permissiveness as the tier-2 fs
/// tier.
/// One mount spec `host:host[:ro]` — the single format both the builder and
/// the tests use (POSIX and Windows path forms differ; tests derive
/// expectations through this helper rather than hardcoding POSIX literals).
fn mount_arg(host_abs: &str, read_only: bool) -> String {
    format!(
        "{host_abs}:{host_abs}{}",
        if read_only { ":ro" } else { "" }
    )
}

pub fn container_run_args(
    inputs: &SandboxInputs,
    spec: &ContainerSpec,
    binary: &Path,
    args: &[String],
    proxy_url: Option<&str>,
) -> Vec<String> {
    let mut out: Vec<String> = vec![
        "run".to_string(),
        "--rm".to_string(),
        "-i".to_string(),
        // Writable set = the declared mounts below; everything else denied.
        "--read-only".to_string(),
    ];
    // (path, read-only) mounts, deduplicated; session_cwd first so it is the
    // working directory's own mount.
    let mut mounts: Vec<(String, bool)> = Vec::new();
    let mut add_mount = |path: &Path, ro: bool| {
        let host = crate::sandbox::absolutize(path).display().to_string();
        if !mounts.iter().any(|(existing, _)| existing == &host) {
            mounts.push((host, ro));
        }
    };
    add_mount(&inputs.session_cwd, false);
    // Read-only: the engine writes mission metadata from outside the
    // sandbox, and this ro mount stacks over the rw session_cwd mount when
    // checkout mode makes the mission dir its descendant — the container
    // analogue of the tier-2 mission-metadata write deny.
    add_mount(&inputs.mission_dir, true);
    // The session-private scratch doubles as the container's HOME/TMPDIR, so
    // it must be writable and mounted at the identical host path.
    add_mount(&inputs.tmpdir, false);
    for extra in &inputs.extra_write {
        add_mount(extra, false);
    }
    for (host, ro) in mounts {
        out.push("-v".to_string());
        out.push(mount_arg(&host, ro));
    }
    // Authority material must stay unreadable inside the container: the
    // session_cwd mount otherwise carries the repo's `.kranz/serve.token`
    // (mutation authority over `kranz serve` on loopback) and `config.json`
    // (Slack/remote-workspace credentials) in with it. Mask each file that
    // exists at spawn time with a /dev/null bind — the container analogue of
    // the tier-2 read deny (crate::sandbox::authority_read_deny_paths derives
    // the same set from the mission dir for the process sandboxes; here the
    // session mount is the only path that can carry them). A token file
    // created AFTER spawn is a residual gap the Seatbelt profile lacks.
    let session_root = crate::sandbox::absolutize(&inputs.session_cwd);
    for name in ["serve.token", "serve.read.token", "config.json"] {
        let authority = session_root.join(".kranz").join(name);
        if authority.exists() {
            out.push("-v".to_string());
            out.push(format!("/dev/null:{}:ro", authority.display()));
        }
    }
    out.push("-w".to_string());
    out.push(
        crate::sandbox::absolutize(&inputs.session_cwd)
            .display()
            .to_string(),
    );
    let scratch = crate::sandbox::absolutize(&inputs.tmpdir)
        .display()
        .to_string();
    out.push("-e".to_string());
    out.push(format!("HOME={scratch}"));
    out.push("-e".to_string());
    out.push(format!("TMPDIR={scratch}"));
    // Toolchain caches cross as READ-ONLY mounts + matching env (6th-pass
    // review: without them a container session cold-bootstraps a whole
    // rustup toolchain + registry into scratch, the container twin of the
    // m-533143 ENOSPC regression). rw would let a poisoned cache ride into
    // the operator's later builds — the same class as a shared target/, so
    // ro it is: a cache MISS (uncached crate) fails visibly inside the
    // container rather than writing through to the operator's cache.
    for (var, default_subdir) in [
        ("RUSTUP_HOME", ".rustup"),
        ("CARGO_HOME", ".cargo"),
        ("NPM_CONFIG_CACHE", ".npm"),
    ] {
        let host = std::env::var_os(var)
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(default_subdir))
            });
        if let Some(host) = host {
            if host.is_dir() {
                let mounted = crate::sandbox::absolutize(&host).display().to_string();
                out.push("-v".to_string());
                out.push(mount_arg(&mounted, true));
                out.push("-e".to_string());
                out.push(format!("{var}={mounted}"));
            }
        }
    }
    if inputs.enforce == crate::types::SandboxEnforce::FsNet {
        if inputs.egress.is_empty() {
            out.push("--network".to_string());
            out.push("none".to_string());
        } else if let Some(proxy_url) = proxy_url {
            // Proxy-routed fs+net: the session's HTTPS egress goes to the
            // host-side filtering proxy. Linux docker has no built-in
            // host.docker.internal mapping, so give it the gateway entry.
            #[cfg(target_os = "linux")]
            {
                out.push("--add-host".to_string());
                out.push("host.docker.internal:host-gateway".to_string());
            }
            out.push("-e".to_string());
            out.push(format!(
                "{}={proxy_url}",
                crate::egress_proxy::HTTPS_PROXY_ENV
            ));
            out.push("-e".to_string());
            out.push(format!(
                "{}={proxy_url}",
                crate::egress_proxy::HTTP_PROXY_ENV
            ));
            out.push("-e".to_string());
            out.push(format!(
                "{}={}",
                crate::egress_proxy::NO_PROXY_ENV,
                crate::egress_proxy::NO_PROXY_VALUE
            ));
        }
    }
    out.push(spec.image.clone());
    out.push(binary.display().to_string());
    out.extend(args.iter().cloned());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxInputs;
    use crate::types::SandboxEnforce;
    use std::path::PathBuf;

    #[test]
    fn detect_prefers_docker_then_podman_then_nerdctl_then_apple_container() {
        assert_eq!(detect_with(|_| false), None);
        assert_eq!(
            detect_with(|name| name == "container"),
            Some(ContainerRuntime::AppleContainer)
        );
        assert_eq!(
            detect_with(|name| name == "nerdctl" || name == "container"),
            Some(ContainerRuntime::Nerdctl)
        );
        assert_eq!(
            detect_with(|name| name == "podman" || name == "nerdctl"),
            Some(ContainerRuntime::Podman)
        );
        assert_eq!(
            detect_with(|name| name == "docker" || name == "podman"),
            Some(ContainerRuntime::Docker)
        );
    }

    fn inputs(enforce: SandboxEnforce) -> SandboxInputs {
        SandboxInputs {
            enforce,
            session_cwd: PathBuf::from("/work/session"),
            mission_dir: PathBuf::from("/work/mission"),
            tmpdir: PathBuf::from("/work/scratch"),
            extra_write: vec![PathBuf::from("/home/op/.cargo")],
            egress: Vec::new(),
        }
    }

    fn spec() -> ContainerSpec {
        ContainerSpec {
            runtime: ContainerRuntime::Docker,
            image: DEFAULT_IMAGE.to_string(),
        }
    }

    #[test]
    fn container_run_args_fs_net_with_empty_egress_disables_network() {
        let args = container_run_args(
            &inputs(SandboxEnforce::FsNet),
            &spec(),
            Path::new("claude"),
            &["-p".to_string(), "hi".to_string()],
            None,
        );
        let network = args
            .windows(2)
            .find(|w| w[0] == "--network")
            .expect("fs+net must pass a --network flag");
        assert_eq!(network[1], "none");
    }

    #[test]
    fn container_run_args_fs_net_with_egress_bridges_and_forwards_proxy_env() {
        let mut inputs = inputs(SandboxEnforce::FsNet);
        inputs.egress = vec!["crates.io:443".to_string()];
        let args = container_run_args(
            &inputs,
            &spec(),
            Path::new("claude"),
            &["-p".to_string(), "hi".to_string()],
            Some("http://host.docker.internal:8123"),
        );

        assert!(
            !args.iter().any(|a| a == "--network"),
            "proxy-routed fs+net keeps the runtime default bridge: {args:?}"
        );
        for var in ["HTTPS_PROXY", "HTTP_PROXY"] {
            assert!(
                args.windows(2)
                    .any(|w| w[0] == "-e"
                        && w[1] == format!("{var}=http://host.docker.internal:8123")),
                "missing -e {var}=…: {args:?}"
            );
        }
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-e" && w[1] == "NO_PROXY=localhost,127.0.0.1"),
            "missing -e NO_PROXY…: {args:?}"
        );
        #[cfg(target_os = "linux")]
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--add-host" && w[1] == "host.docker.internal:host-gateway"),
            "linux docker needs the host-gateway entry: {args:?}"
        );
    }

    #[test]
    fn container_run_args_fs_keeps_runtime_default_network() {
        let args = container_run_args(
            &inputs(SandboxEnforce::Fs),
            &spec(),
            Path::new("claude"),
            &[],
            None,
        );
        assert!(
            !args.iter().any(|a| a == "--network"),
            "fs must not restrict the network (runtime default bridge): {args:?}"
        );
    }

    #[test]
    fn container_run_args_mounts_policy_and_runs_image() {
        // Platform-native fixture paths: /work literals absolutize to
        // drive-lettered/backslashed forms on Windows, so expectations are
        // derived through the same absolutize + mount_arg the builder uses.
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session");
        let mission = dir.path().join("mission");
        let scratch = dir.path().join("scratch");
        let cargo = dir.path().join("cargo");
        let inputs = SandboxInputs {
            enforce: SandboxEnforce::Fs,
            session_cwd: session.clone(),
            mission_dir: mission.clone(),
            tmpdir: scratch.clone(),
            extra_write: vec![cargo.clone()],
            egress: Vec::new(),
        };
        let args = container_run_args(
            &inputs,
            &spec(),
            Path::new("claude"),
            &["--print".to_string()],
            None,
        );
        let joined = args.join(" ");
        let abs = |p: &std::path::Path| crate::sandbox::absolutize(p).display().to_string();

        assert!(args.contains(&"--rm".to_string()));
        assert!(args.contains(&"--read-only".to_string()));
        assert!(joined.contains(&mount_arg(&abs(&session), false)));
        assert!(joined.contains(&mount_arg(&abs(&mission), true)));
        assert!(joined.contains(&mount_arg(&abs(&scratch), false)));
        assert!(joined.contains(&mount_arg(&abs(&cargo), false)));
        assert!(joined.contains(&format!("-w {}", abs(&session))));
        assert!(joined.contains(&format!("-e HOME={}", abs(&scratch))));
        assert!(
            joined.ends_with(&format!("{DEFAULT_IMAGE} claude --print")),
            "image then binary then args: {args:?}"
        );
    }

    #[test]
    fn container_run_args_mask_authority_material_under_session_root() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session");
        let kranz_dir = session.join(".kranz");
        std::fs::create_dir_all(&kranz_dir).unwrap();
        let serve_token = kranz_dir.join("serve.token");
        let config = kranz_dir.join("config.json");
        std::fs::write(&serve_token, "secret").unwrap();
        std::fs::write(&config, "{}").unwrap();
        let mut inputs = inputs(SandboxEnforce::Fs);
        inputs.session_cwd = session;

        let args = container_run_args(
            &inputs,
            &spec(),
            Path::new("claude"),
            &["--print".to_string()],
            None,
        );
        let joined = args.join(" ");
        let abs = |p: &std::path::Path| crate::sandbox::absolutize(p).display().to_string();

        for masked in [&serve_token, &config] {
            assert!(
                joined.contains(&format!("/dev/null:{}:ro", abs(masked))),
                "missing /dev/null mask for {}: {args:?}",
                masked.display()
            );
        }
        // Absent files are not masked — a bind target must exist.
        assert!(
            !joined.contains("serve.read.token"),
            "absent authority files must not be masked: {args:?}"
        );
    }

    #[test]
    fn container_run_args_respects_image_override() {
        let spec = ContainerSpec {
            runtime: ContainerRuntime::Podman,
            image: "ghcr.io/example/kranz-worker:1".to_string(),
        };
        let args = container_run_args(
            &inputs(SandboxEnforce::Fs),
            &spec,
            Path::new("claude"),
            &[],
            None,
        );
        assert!(
            args.iter().any(|a| a == "ghcr.io/example/kranz-worker:1"),
            "configured image must be used: {args:?}"
        );
        assert!(!args.iter().any(|a| a == DEFAULT_IMAGE));
    }

    /// Smoke: a trivial worker inside the provider lands a write inside the
    /// mounted session dir on the host, a write outside the declared policy
    /// (`/etc`, read-only root fs) is denied, and authority material under the
    /// session root (`.kranz/serve.token`) is masked by its /dev/null bind.
    /// Skips on hosts with no container runtime (this macOS dev host); CI
    /// ubuntu-latest has docker.
    #[test]
    fn container_provider_runs_a_trivial_worker_and_enforces_the_write_boundary() {
        let Some(runtime) = detect() else {
            eprintln!(
                "no container runtime (docker/podman/nerdctl/container) on PATH; skipping container smoke test"
            );
            return;
        };

        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let kranz_dir = session.path().join(".kranz");
        std::fs::create_dir_all(&kranz_dir).unwrap();
        std::fs::write(kranz_dir.join("serve.token"), "secret").unwrap();
        let inputs = SandboxInputs {
            enforce: SandboxEnforce::FsNet,
            session_cwd: session.path().to_path_buf(),
            mission_dir: mission.path().to_path_buf(),
            tmpdir: scratch.path().to_path_buf(),
            extra_write: Vec::new(),
            egress: Vec::new(),
        };
        let spec = ContainerSpec {
            runtime,
            image: DEFAULT_IMAGE.to_string(),
        };
        let ok_file = session.path().join("ok.txt");
        let args = container_run_args(
            &inputs,
            &spec,
            Path::new("sh"),
            &[
                "-c".to_string(),
                format!(
                    "echo ok > {} && cat {} && echo nope > /etc/nope.txt",
                    ok_file.display(),
                    kranz_dir.join("serve.token").display()
                ),
            ],
            None,
        );
        let output = std::process::Command::new(runtime.binary())
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("failed to spawn container runtime");

        assert!(
            ok_file.exists(),
            "write inside the mounted session_cwd must land on the host: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !output.status.success(),
            "write outside the declared policy (/etc) must be denied, failing the worker: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("secret"),
            "the /dev/null mask must hide serve.token content inside the container"
        );
    }
}
