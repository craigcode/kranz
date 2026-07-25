//! Tier-3 container sandbox provider — run a worker/validator session inside
//! a container with the declared write/egress policy. See
//! docs/scoping/worker-sandboxing.md tier 3.
//!
//! This is the macOS `fs+net` answer tier 2 could not give: Seatbelt cannot
//! express hostname egress rules, but a container runtime can cut the network
//! outright. With `enforce = "fs+net"` and an empty `egress` list the session
//! runs with `--network none` — a hard egress boundary that works identically
//! on macOS and Linux. Note the honest tradeoff: `none` also blocks the
//! agent's API egress, so `fs+net` suits offline gates/validation; API-driven
//! workers use `fs` (runtime default bridge/NAT, the same permissiveness as
//! the tier-2 fs tier). A non-empty `egress` list is REFUSED at resolve time:
//! per-host egress needs the filtering proxy from the egress-grant ticket,
//! and silently widening to a full bridge would be worse than failing closed.
//!
//! Write policy: the container's root filesystem is read-only; the writable
//! set is exactly the declared mounts — `session_cwd` (rw), `mission_dir`
//! (ro), the scratch `tmpdir` (rw, also `HOME`/`TMPDIR` inside the container),
//! and each `extra_write` entry (rw). Everything else is denied by the
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
/// Network: `fs+net` maps to `--network none` (resolve refuses a non-empty
/// egress list before this point, so FsNet here always means "no network");
/// `fs` passes no network flag, keeping the runtime's default bridge/NAT —
/// the same permissiveness as the tier-2 fs tier.
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
    add_mount(&inputs.mission_dir, true);
    // The scratch dir doubles as the container's HOME/TMPDIR, so it must be
    // writable and mounted at the identical host path.
    add_mount(&inputs.tmpdir, false);
    for extra in &inputs.extra_write {
        add_mount(extra, false);
    }
    for (host, ro) in mounts {
        out.push("-v".to_string());
        out.push(mount_arg(&host, ro));
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
    if inputs.enforce == crate::types::SandboxEnforce::FsNet {
        out.push("--network".to_string());
        out.push("none".to_string());
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
        );
        let network = args
            .windows(2)
            .find(|w| w[0] == "--network")
            .expect("fs+net must pass a --network flag");
        assert_eq!(network[1], "none");
    }

    #[test]
    fn container_run_args_fs_keeps_runtime_default_network() {
        let args = container_run_args(
            &inputs(SandboxEnforce::Fs),
            &spec(),
            Path::new("claude"),
            &[],
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
    fn container_run_args_respects_image_override() {
        let spec = ContainerSpec {
            runtime: ContainerRuntime::Podman,
            image: "ghcr.io/example/kranz-worker:1".to_string(),
        };
        let args = container_run_args(&inputs(SandboxEnforce::Fs), &spec, Path::new("claude"), &[]);
        assert!(
            args.iter().any(|a| a == "ghcr.io/example/kranz-worker:1"),
            "configured image must be used: {args:?}"
        );
        assert!(!args.iter().any(|a| a == DEFAULT_IMAGE));
    }

    /// Smoke: a trivial worker inside the provider lands a write inside the
    /// mounted session dir on the host, and a write outside the declared
    /// policy (`/etc`, read-only root fs) is denied. Skips on hosts with no
    /// container runtime (this macOS dev host); CI ubuntu-latest has docker.
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
                    "echo ok > {} && echo nope > /etc/nope.txt",
                    ok_file.display()
                ),
            ],
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
    }
}
