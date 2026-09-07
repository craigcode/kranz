//! Tier-3 container sandbox provider — run a worker/validator session inside
//! a container with the declared write/egress policy. See
//! docs/scoping/worker-sandboxing.md tier 3.
//!
//! Two network postures for `enforce = "fs+net"`, chosen by the egress list.
//! An EMPTY `egress` list runs `--network none` — a hard egress boundary on
//! the live-proven Linux host path. Note the honest tradeoff: `none`
//! also blocks the agent's API egress, so it suits offline gates/validation.
//! A NON-EMPTY `egress` list runs the worker on a unique Docker `--internal`
//! network. A trusted dual-homed relay is the only other container on that
//! network; it injects a run-secret authorization header before forwarding
//! CONNECT to the host-side filtering proxy (`crate::egress_proxy`). The
//! worker never receives that credential and has no default route, so
//! ignoring the proxy env cannot bypass the per-host filter. See
//! `crate::container_egress` for provisioning, teardown, and stale-resource
//! recovery. Runtimes other than Docker refuse this posture before spawn.
//! API-driven workers that need
//! no egress list use `fs` (runtime default bridge/NAT, the same
//! permissiveness as the tier-2 fs tier).
//!
//! Host support is evidence-gated, not a platform allowlist. Linux is
//! supported unconditionally: CI renews a receipt for the shipped bind-mount,
//! authority-mask, and egress contracts on every run. Windows is refused
//! unconditionally, because the shipped contract uses POSIX guest paths,
//! Linux images, and `/dev/null` authority masks that Windows containers do
//! not honor; macOS keeps the process provider's native Seatbelt boundary as
//! its default. Anything else must PROVE the mount contract on the host, at
//! run time, via [`prove_bind_mount`] — hosted macOS cannot renew a CI
//! receipt (its runners are guests without the virtualization a VM-backed
//! runtime needs), but a developer's own Mac can answer the same question
//! about itself in about a second.
//!
//! Runtime detection is not that evidence, and neither is a `-v` flag the
//! runtime accepted. A daemon that cannot see the host path creates an empty
//! directory inside its VM, mounts that, and exits 0, so the declared write
//! set silently does not exist. See [`MountProof`].
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
//!
//! Engine-run gates (ticket container-gate-wrapper): the same `run --rm -i
//! --read-only` shape also executes validation/final/merge gate commands
//! inside the mission container — see [`container_gate_run_args`] for the
//! gate-specific deltas (named container for timeout teardown, the gate's
//! sanitized env forwarded via `-e`, and a toolchain posture that mounts the
//! rustup toolchain + npm cache read-only but NEVER the real Cargo root:
//! the gate's `CARGO_HOME` is a seeded cache-only home precisely because the
//! real one is a credential directory).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use crate::sandbox::SandboxInputs;

/// Image used when the role config does not name one. Minimal and
/// pullable on the supported Linux container path; production use
/// should set `sandbox.image`.
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

    /// Host-side client configuration, separate from the environment forwarded
    /// into the container. A scratch HOME hides Docker/Colima contexts; copying
    /// the worker environment here can also retarget the daemon during teardown.
    pub(crate) fn client_env(self) -> std::collections::HashMap<String, String> {
        let mut keys = vec![
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TMPDIR",
            "XDG_CONFIG_HOME",
            "XDG_RUNTIME_DIR",
            "SSH_AUTH_SOCK",
            "USERPROFILE",
            "SystemRoot",
            "ComSpec",
            "APPDATA",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
        ];
        match self {
            Self::Docker => keys.extend([
                "DOCKER_HOST",
                "DOCKER_CONTEXT",
                "DOCKER_CONFIG",
                "DOCKER_TLS",
                "DOCKER_TLS_VERIFY",
                "DOCKER_CERT_PATH",
                "DOCKER_API_VERSION",
            ]),
            Self::Podman => {
                keys.extend(["CONTAINER_HOST", "CONTAINER_CONNECTION", "CONTAINER_SSHKEY"])
            }
            Self::Nerdctl => {
                keys.extend(["CONTAINERD_ADDRESS", "CONTAINERD_NAMESPACE", "NERDCTL_TOML"])
            }
            Self::AppleContainer => {}
        }
        keys.into_iter()
            .filter_map(|key| {
                std::env::var(key)
                    .ok()
                    .map(|value| (key.to_string(), value))
            })
            .collect()
    }
}

/// Detect the preferred available container runtime on this host's PATH.
pub fn detect() -> Option<ContainerRuntime> {
    detect_with(crate::sandbox::command_available)
}

/// Whether this host can actually honor the shipped container contract, as
/// opposed to merely having a runtime binary on PATH.
///
/// Detection answers "is there a runtime?"; this answers "is its host contract
/// supported?". They diverge by platform, and for different reasons.
///
/// Linux is supported unconditionally: CI renews a receipt for the shipped
/// bind-mount, authority-mask, and egress contracts on every run.
///
/// Windows is refused unconditionally. It may have `docker.exe`, but the
/// shipped contract uses POSIX guest paths, Linux images, and `/dev/null`
/// authority masks that Windows containers do not honor. This was masked
/// until `command_available` learned to consult `PATHEXT`; before that
/// `detect()` never saw `docker.exe` and the Windows container tests took
/// their silent skip path and reported `ok` without running.
///
/// macOS is supported exactly when THIS host proves it. Hosted runners cannot
/// renew a CI receipt, because they are already guests without the
/// virtualization a VM-backed runtime needs, so the evidence has to come from
/// the host at run time instead of from a lane that cannot execute. The proof
/// is a real bind-mount round trip over the paths a session mounts, which is
/// what separates a working developer machine from one whose runtime accepts
/// `-v` and shares nothing.
pub fn host_supports_container_contract() -> bool {
    if cfg!(target_os = "linux") {
        return true;
    }
    if cfg!(target_os = "windows") {
        return false;
    }
    let Some(runtime) = detect() else {
        return false;
    };
    matches!(host_mount_contract_proof(runtime), MountProof::Proven)
}

/// The proof behind [`host_supports_container_contract`], over the roots a
/// test or session actually mounts: the working tree and the system temp
/// root. Sharing is per path, so proving one says nothing about the other.
pub fn host_mount_contract_proof(runtime: ContainerRuntime) -> MountProof {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
    for root in [cwd.as_path(), std::env::temp_dir().as_path()] {
        match cached_bind_mount_proof(runtime, root, DEFAULT_IMAGE) {
            MountProof::Proven => {}
            failed => return failed,
        }
    }
    MountProof::Proven
}

/// Guest path the bind-mount proof mounts its probe directory at.
pub const MOUNT_PROOF_GUEST_DIR: &str = "/kranz-mount-proof";

/// How long one probe container may take before the proof gives up.
const MOUNT_PROOF_TIMEOUT: Duration = Duration::from_secs(90);

/// Whether this host's runtime actually shares a bind-mounted directory with
/// the container, as opposed to accepting the `-v` flag and sharing nothing.
///
/// A runtime that cannot see the host path does NOT fail. Docker creates an
/// empty directory inside its VM, mounts that, and exits 0. The declared
/// write set then silently does not exist: a worker writes into a VM that is
/// destroyed at teardown, and the validator judges a tree where nothing
/// landed. Nothing in the run reports an error.
///
/// Measured on an M4 Pro (2026-08-25) with Colima 0.10.3 and Docker 29.2.1.
/// Colima's default mount set is the home directory alone, macOS puts
/// `TMPDIR` under `/var/folders`, and a probe file written on the host before
/// the run was invisible inside the container with exit code 0 throughout.
/// The same hazard reaches any host whose daemon does not share its
/// filesystem: Docker Desktop's file-sharing list, a remote `DOCKER_HOST`, a
/// rootless daemon in its own mount namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountProof {
    /// A sentinel written on the host was read inside the container, and a
    /// sentinel written inside the container was read back on the host.
    Proven,
    /// The round trip did not close. Carries the operator-facing reason.
    Failed(String),
}

/// The probe argv: mount `host_dir` rw, read the host's sentinel from inside,
/// and write the guest's sentinel back out. One container run proves both
/// directions, because a mount can be visible one way and stale the other.
pub fn mount_proof_argv(host_dir: &Path, image: &str, guest_sentinel: &str) -> Vec<String> {
    vec![
        "run".to_string(),
        "--rm".to_string(),
        "-v".to_string(),
        format!(
            "{}:{MOUNT_PROOF_GUEST_DIR}",
            container_host_path(host_dir)
        ),
        image.to_string(),
        "sh".to_string(),
        "-c".to_string(),
        // Exit non-zero ONLY when the runtime itself fails. A missing
        // sentinel is a finding to report on stdout, not a shell error: if
        // `cat` decides the exit code, an unshared mount and a dead daemon
        // become the same failure, and only one of them has a remedy the
        // operator can act on.
        format!(
            "if [ -r {MOUNT_PROOF_GUEST_DIR}/host.txt ]; then cat {MOUNT_PROOF_GUEST_DIR}/host.txt; \
             else printf %s no-host-sentinel; fi; \
             printf %s {guest_sentinel} > {MOUNT_PROOF_GUEST_DIR}/guest.txt 2>/dev/null || true"
        ),
    ]
}

/// Run the round trip under `host_dir` and report whether the mount is real.
///
/// `host_dir` must be the directory the mission will actually mount under,
/// not a convenient one. The failure is path-dependent: on a default Colima
/// a probe under `$HOME` passes while the same probe under `TMPDIR` shares
/// nothing, so proving the wrong path proves nothing.
pub fn prove_bind_mount(runtime: ContainerRuntime, host_dir: &Path, image: &str) -> MountProof {
    let probe = host_dir.join(format!(
        "kranz-mount-proof-{}",
        uuid::Uuid::new_v4().simple()
    ));
    if let Err(error) = std::fs::create_dir_all(&probe) {
        return MountProof::Failed(format!(
            "could not create the mount probe directory {}: {error}",
            probe.display()
        ));
    }
    let host_sentinel = uuid::Uuid::new_v4().simple().to_string();
    let guest_sentinel = uuid::Uuid::new_v4().simple().to_string();
    let proof = run_mount_proof(
        runtime,
        host_dir,
        &probe,
        image,
        &host_sentinel,
        &guest_sentinel,
    );
    let _ = std::fs::remove_dir_all(&probe);
    proof
}

fn run_mount_proof(
    runtime: ContainerRuntime,
    host_dir: &Path,
    probe: &Path,
    image: &str,
    host_sentinel: &str,
    guest_sentinel: &str,
) -> MountProof {
    if let Err(error) = std::fs::write(probe.join("host.txt"), host_sentinel) {
        return MountProof::Failed(format!(
            "could not write the host sentinel in {}: {error}",
            probe.display()
        ));
    }
    let argv = mount_proof_argv(probe, image, guest_sentinel);
    let Some(output) = crate::command_exec::run_with_timeout(
        Path::new(runtime.binary()),
        &argv,
        MOUNT_PROOF_TIMEOUT,
    ) else {
        return MountProof::Failed(format!(
            "the {} mount proof did not finish within {}s: {}",
            runtime.binary(),
            MOUNT_PROOF_TIMEOUT.as_secs(),
            argv.join(" ")
        ));
    };
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() {
        return MountProof::Failed(format!(
            "the {} mount proof exited {:?}: {}",
            runtime.binary(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if stdout != host_sentinel {
        return MountProof::Failed(unshared_path_reason(
            runtime,
            host_dir,
            "the host sentinel was not visible inside the container",
        ));
    }
    match std::fs::read_to_string(probe.join("guest.txt")) {
        Ok(written) if written.trim() == guest_sentinel => MountProof::Proven,
        Ok(_) | Err(_) => MountProof::Failed(unshared_path_reason(
            runtime,
            host_dir,
            "the container's write did not reach the host",
        )),
    }
}

/// The message an operator can act on. Naming the path matters more than
/// naming the runtime, because the fix is almost always to share that path
/// or to move the mission's scratch under one the runtime already shares.
fn unshared_path_reason(runtime: ContainerRuntime, host_dir: &Path, symptom: &str) -> String {
    format!(
        "{} accepted a bind mount of {} and shared nothing: {symptom}. \
         The runtime's daemon cannot see this host path, so the declared write set would \
         not exist inside the container and a worker's output would be lost silently. \
         Share this path with the runtime (Colima mounts only the home directory by \
         default: `colima start --mount {}:w`; Docker Desktop keeps its own file-sharing \
         list) or point the mission's workspace at a path it already shares",
        runtime.binary(),
        host_dir.display(),
        host_dir.display()
    )
}

/// One proof per (runtime, path) for the life of the process.
///
/// The probe costs a container run. Session resolution happens per role and
/// per feature, so proving every time would add that cost to every spawn,
/// and the answer cannot change while a daemon keeps running.
fn proof_cache() -> &'static Mutex<HashMap<(String, String), MountProof>> {
    static CACHE: OnceLock<Mutex<HashMap<(String, String), MountProof>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// [`prove_bind_mount`] memoized per runtime and path.
pub fn cached_bind_mount_proof(
    runtime: ContainerRuntime,
    host_dir: &Path,
    image: &str,
) -> MountProof {
    let key = (
        runtime.binary().to_string(),
        host_dir.to_string_lossy().into_owned(),
    );
    if let Ok(cache) = proof_cache().lock() {
        if let Some(proof) = cache.get(&key) {
            return proof.clone();
        }
    }
    let proof = prove_bind_mount(runtime, host_dir, image);
    if let Ok(mut cache) = proof_cache().lock() {
        cache.insert(key, proof.clone());
    }
    proof
}

/// Prove every distinct host root a run will mount.
///
/// One probe is not enough. Sharing is per path on every runtime that has
/// this hazard, so a host can share the checkout and not the scratch: the
/// exact shape of the 2026-08-25 macOS failure, where the worktree under
/// `$HOME` mounted fine and `TMPDIR` under `/var/folders` did not. Proving
/// only the convenient root would reproduce the original bug with extra
/// ceremony, so every declared root is proven and the FIRST failure is
/// returned, naming the path the operator has to fix.
///
/// Roots are deduplicated by their proof cache key, so the common case of
/// several mounts under one shared root costs one container run.
pub fn prove_mount_roots(runtime: ContainerRuntime, roots: &[PathBuf], image: &str) -> MountProof {
    let mut seen = Vec::new();
    for root in roots {
        if root.as_os_str().is_empty() || seen.iter().any(|prior| prior == root) {
            continue;
        }
        seen.push(root.clone());
        match cached_bind_mount_proof(runtime, root, image) {
            MountProof::Proven => {}
            failed => return failed,
        }
    }
    MountProof::Proven
}

/// The roots a session or gate actually mounts, in the order the operator
/// would want them reported.
///
/// The checkout contributes its PARENT rather than the working tree itself:
/// the tree is a git worktree, and a directory appearing and vanishing inside
/// it can race a concurrent `git status` in a mission that cares about a
/// clean tree. The system temp root stands in for the per-session scratch,
/// which does not exist yet at resolution time but is created underneath it.
pub fn declared_mount_roots(
    session_cwd: &Path,
    mission_dir: &Path,
    extra_write: &[PathBuf],
) -> Vec<PathBuf> {
    let mut roots = vec![
        session_cwd.parent().unwrap_or(session_cwd).to_path_buf(),
        mission_dir.to_path_buf(),
        std::env::temp_dir(),
    ];
    roots.extend(extra_write.iter().cloned());
    roots
}

/// Why a live container test is skipping, in the host's own terms.
///
/// "Supported only on Linux" was true when the platform list was the whole
/// answer. Now a macOS host can qualify, so a skip has to say which fact
/// disqualified this one: no runtime at all, or a runtime whose mounts do
/// not round trip.
pub fn container_contract_skip_detail() -> String {
    if cfg!(target_os = "windows") {
        return "the container provider refuses Windows: POSIX guest paths, Linux images, \
                and /dev/null authority masks are not honored there"
            .to_string();
    }
    match detect() {
        None => "no docker/podman/nerdctl/container on PATH".to_string(),
        Some(runtime) => match host_mount_contract_proof(runtime) {
            MountProof::Proven => {
                "the host contract is supported; this skip should not have fired".to_string()
            }
            MountProof::Failed(reason) => reason,
        },
    }
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
    /// Unique internal network provisioned for one `fs+net` session with a
    /// non-empty egress list. `None` for every other posture. The runner sets
    /// this only after the relay and authenticated host proxy are ready.
    pub network: Option<String>,
    /// Daemon-owned worker container name paired with `network`. Naming lets
    /// boundary teardown force-remove the worker after a killed runtime
    /// client or timeout; `None` for postures without the per-run boundary.
    pub name: Option<String>,
}

/// Build the `<runtime> run` argv (excluding the runtime binary itself) for
/// running `binary args` under the resolved container sandbox.
///
/// Network: `fs+net` with an empty egress list maps to `--network none` (the
/// hard boundary); `fs+net` with a non-empty egress list joins the unique
/// internal network provisioned in `ContainerSpec::network` and forwards the
/// trusted relay endpoint into the container env. If either value is absent,
/// the builder falls back to `--network none`: a wiring bug bricks egress
/// rather than silently reopening the runtime bridge. `fs` passes no network
/// flag, keeping the
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

/// The host spelling a `-v` spec may carry.
///
/// Mount specs are colon-delimited, and a Windows VERBATIM path
/// (`\\?\C:\...`) makes the runtime's parser count too many colons:
///
/// ```text
/// docker: invalid spec: \\?\C:\...:\\?\C:\...: too many colons
/// ```
///
/// [`crate::sandbox::absolutize`] canonicalizes, and Windows canonicalization
/// ALWAYS returns the verbatim form, so every container mount on Windows hit
/// this. Strip the prefix exactly as `GitRepo::git_path_arg` does for git.
/// A verbatim UNC path (`\\?\UNC\server\share`) is left untouched — it has no
/// plain DOS spelling to fall back to.
fn container_host_path(path: &Path) -> String {
    let absolute = crate::sandbox::absolutize(path);
    let rendered = absolute.as_os_str().to_string_lossy();
    #[cfg(windows)]
    if let Some(rest) = rendered.strip_prefix(r"\\?\") {
        if !rest.starts_with("UNC") {
            return rest.to_string();
        }
    }
    rendered.into_owned()
}

/// Process-count bound for a worker/gate container. Generous next to the
/// relay's 64 (a `cargo build -j` or an `npm ci` legitimately forks wide)
/// but finite: without it a fork bomb inside the container takes the HOST
/// down, since the container shares the host's pid resources.
const CONTAINER_PIDS_LIMIT: &str = "512";

/// `run --rm -i --read-only` plus the hardening the egress relay already
/// gets — the shared prologue: the writable set is exactly the declared
/// mounts; everything else is denied by the runtime.
///
/// Cross-tier drift closed (2026-09-01 adversarial audit, MED-2): the relay
/// and its loader run `--user`, `--cap-drop ALL`,
/// `--security-opt no-new-privileges` and `--pids-limit`
/// (`crate::container_egress`), while the worker and gate containers ran
/// with none of them. That left the agent as uid 0 inside the container
/// with Docker's default capability set — `CAP_DAC_OVERRIDE`, `CAP_CHOWN`,
/// `CAP_FOWNER`, `CAP_SETUID`, `CAP_MKNOD`, `CAP_NET_RAW` — writing into
/// bind mounts that land at the IDENTICAL host path, so container-root
/// writes appeared in the operator's tree as uid 0 and a setuid-root binary
/// could be planted in a host-visible directory.
///
/// `--user` maps to the OWNER of the session cwd (the same derivation
/// `container_egress::credential_owner` applies to the relay's credential
/// dir), so writes through the rw mounts land as the operator, not root.
/// Unix only: there is no uid/gid to map on other hosts, and the container
/// provider already refuses Windows outright.
fn run_prologue(inputs: &SandboxInputs) -> Vec<String> {
    let mut out = vec![
        "run".to_string(),
        "--rm".to_string(),
        "-i".to_string(),
        "--read-only".to_string(),
        "--cap-drop".to_string(),
        "ALL".to_string(),
        "--security-opt".to_string(),
        "no-new-privileges".to_string(),
        "--pids-limit".to_string(),
        CONTAINER_PIDS_LIMIT.to_string(),
    ];
    if let Some(owner) = crate::container_egress::mount_owner(&inputs.session_cwd) {
        out.push("--user".to_string());
        out.push(owner);
    }
    // Docker config can inject proxy credentials into containers automatically.
    // Only the explicit sandbox/contract environment may supply these values.
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "FTP_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "ftp_proxy",
        "all_proxy",
        "no_proxy",
    ] {
        out.extend(["-e".to_string(), format!("{key}=")]);
    }
    out
}

/// The declared write/audit mount set: `session_cwd` (rw), `mission_dir`
/// (ro — the engine writes mission metadata from outside the sandbox, and
/// this ro mount stacks over the rw session_cwd mount when checkout mode
/// makes the mission dir its descendant — the container analogue of the
/// tier-2 mission-metadata write deny), the session-private scratch `tmpdir`
/// (rw, also `HOME`/`TMPDIR` inside the container — NOT the shared system
/// temp root, which would expose sibling missions' worktrees), and each
/// `extra_write` entry (rw). Deduplicated, `session_cwd` first so it is the
/// working directory's own mount; nested mounts stack deepest-last.
fn push_policy_mounts(out: &mut Vec<String>, inputs: &SandboxInputs) {
    let mut mounts: Vec<(String, bool)> = Vec::new();
    let denied_dirs: Vec<_> = crate::sandbox::authority_read_deny_dirs(inputs)
        .iter()
        .map(|path| crate::sandbox::absolutize(path))
        .collect();
    let denied_files: Vec<_> = crate::sandbox::authority_read_deny_paths(inputs)
        .iter()
        .map(|path| crate::sandbox::absolutize(path))
        .collect();
    let mut add_mount = |path: &Path, ro: bool| {
        let path = crate::sandbox::absolutize(path);
        // Docker's nested binds win over an enclosing tmpfs. extraWrite
        // must never reopen the operator authority directory.
        if denied_dirs.iter().any(|dir| path.starts_with(dir))
            || denied_files.iter().any(|file| path.starts_with(file))
        {
            return;
        }
        let host = container_host_path(&path);
        if !mounts.iter().any(|(existing, _)| existing == &host) {
            mounts.push((host, ro));
        }
    };
    add_mount(&inputs.session_cwd, false);
    if let Some(missions) = inputs
        .mission_dir
        .parent()
        .filter(|path| path.ends_with("missions") && path.is_dir())
    {
        // Checkout-mode workers must not write other missions' inboxes or
        // audit logs through the broad session mount.
        add_mount(missions, true);
    }
    add_mount(&inputs.mission_dir, true);
    add_mount(&inputs.tmpdir, false);
    for extra in &inputs.extra_write {
        if inputs
            .mission_dir
            .parent()
            .filter(|p| p.ends_with("missions"))
            .is_some_and(|missions| {
                crate::sandbox::absolutize(extra).starts_with(crate::sandbox::absolutize(missions))
            })
        {
            continue;
        }
        add_mount(extra, false);
    }
    for (host, ro) in mounts {
        out.push("-v".to_string());
        out.push(mount_arg(&host, ro));
    }
}

/// Whether `path` lies under one of the WRITABLE mounts
/// [`push_policy_mounts`] declares (`session_cwd`, the scratch `tmpdir`, each
/// `extra_write`). Only those can carry a host write out of the container, so
/// only those need a write-deny bind stacked over them — and binding anything
/// else would newly EXPOSE a path the container could not otherwise reach
/// (follow-up review, L-11).
fn under_writable_mount(path: &Path, inputs: &SandboxInputs) -> bool {
    let candidate = crate::sandbox::absolutize(path);
    std::iter::once(&inputs.session_cwd)
        .chain(std::iter::once(&inputs.tmpdir))
        .chain(inputs.extra_write.iter())
        .any(|root| candidate.starts_with(crate::sandbox::absolutize(root)))
}

/// Replace mounted authority directories with private read-only views that
/// exclude credentials, including files created or replaced after launch.
/// Keep engine-owned policy and Git metadata readable but immutable.
fn push_authority_masks(out: &mut Vec<String>, inputs: &SandboxInputs) {
    let masks: Vec<_> = crate::sandbox::authority_directory_masks(inputs)
        .into_iter()
        .filter(|mask| {
            mask.path.ancestors().any(|ancestor| {
                let path = container_host_path(ancestor);
                out.windows(2).any(|pair| {
                    pair[0] == "-v"
                        && (pair[1] == mount_arg(&path, false) || pair[1] == mount_arg(&path, true))
                })
            })
        })
        .collect();
    let masked_paths: std::collections::BTreeSet<_> =
        masks.iter().map(|mask| mask.path.clone()).collect();
    // Do not expose an otherwise-unmounted host directory just to hide its
    // secrets. Gate containers, in particular, only mount Cargo's bin/.
    // Replace direct mounts of each masked directory. Docker rejects two
    // mounts at one destination, and a nested bind would reopen a shadow.
    let mut filtered = Vec::new();
    let mut index = 0;
    while index < out.len() {
        if out[index] == "-v" && index + 1 < out.len() {
            let mount = &out[index + 1];
            if masks.iter().any(|mask| {
                let path = container_host_path(&mask.path);
                mount == &mount_arg(&path, false) || mount == &mount_arg(&path, true)
            }) {
                index += 2;
                continue;
            }
        }
        filtered.push(out[index].clone());
        index += 1;
    }
    *out = filtered;
    for mask in &masks {
        out.push("--tmpfs".to_string());
        out.push(format!(
            "{}:ro,noexec,nosuid,nodev,mode=755",
            container_host_path(&mask.path)
        ));
        for path in &mask.visible_entries {
            // A deeper private view owns this mountpoint. Rebinding its
            // host directory here would duplicate the destination in Docker.
            if masked_paths.contains(path) {
                continue;
            }
            let path = container_host_path(path);
            // Keep an existing narrower policy mount (e.g. missions ro or
            // session scratch rw); all other visible entries are read-only.
            if !out.windows(2).any(|pair| {
                pair[0] == "-v"
                    && (pair[1] == mount_arg(&path, false) || pair[1] == mount_arg(&path, true))
            }) {
                out.push("-v".to_string());
                out.push(mount_arg(&path, true));
            }
        }
    }

    // These paths remain readable but immutable. Never stack a host bind
    // over a private authority view, which would restore hidden content.
    let writes = crate::sandbox::authority_write_denies(inputs);
    let git = crate::sandbox::git_metadata_write_denies(inputs);
    for path in writes
        .files
        .iter()
        .chain(writes.dirs.iter())
        .chain(git.files.iter())
        .chain(git.dirs.iter())
    {
        if !under_writable_mount(path, inputs)
            || path.is_symlink()
            || !path.exists()
            || masks
                .iter()
                .any(|mask| crate::sandbox::absolutize(path).starts_with(&mask.path))
        {
            continue;
        }
        let host = container_host_path(path);
        if !out
            .windows(2)
            .any(|pair| pair[0] == "-v" && pair[1] == mount_arg(&host, true))
        {
            out.extend(["-v".to_string(), mount_arg(&host, true)]);
        }
    }
}

/// The working directory (the session/gate cwd itself) plus the scratch
/// env: the session-private scratch doubles as the container's HOME/TMPDIR,
/// so it is mounted at the identical host path and named in the env.
fn push_workdir_and_scratch_env(out: &mut Vec<String>, inputs: &SandboxInputs) {
    // `-w` and the HOME/TMPDIR values must name the SAME spelling the mounts
    // used, or the working directory and scratch env point at paths the
    // runtime never mounted.
    out.push("-w".to_string());
    out.push(container_host_path(&inputs.session_cwd));
    let scratch = container_host_path(&inputs.tmpdir);
    out.push("-e".to_string());
    out.push(format!("HOME={scratch}"));
    out.push("-e".to_string());
    out.push(format!("TMPDIR={scratch}"));
}

/// Which toolchain-cache posture [`push_toolchain_caches`] mounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolchainMount {
    /// Agent sessions: the CACHE SUBDIRS of the Cargo home cross read-only,
    /// never the root (2026-09-01 adversarial audit, H12). The pre-audit
    /// session posture mounted `$CARGO_HOME` whole with a matching `-e`,
    /// which carried `credentials.toml` and the legacy extensionless
    /// `credentials` — crates.io registry auth — into the container, while
    /// the tier-2 process sandbox explicitly read-DENIES exactly those two
    /// filenames. Tier 3, the tier `resolve_validator_containment_target`
    /// calls "already the stronger containment", was therefore strictly
    /// weaker than tier 2 for registry credentials. Session mode now gets
    /// the [`ToolchainMount::Gate`] treatment plus the shared caches:
    /// `<cargo>/bin`, `<cargo>/registry`, `<cargo>/git`.
    Session,
    /// Engine-run gates: the real Cargo root NEVER crosses — the gate's
    /// `CARGO_HOME` is a seeded cache-only home precisely because the real
    /// root carries registry credentials and credential-provider config
    /// (`agent_env::cache_only_cargo_home`), and ro-mounting it would reopen
    /// the exact read exposure that home exists to close. Only the shim dir
    /// (`<cargo>/bin` — rustup proxies and installed binaries, never
    /// credentials, which live at the root) is mounted so the forwarded
    /// PATH's `cargo` shim resolves; the gate env's own `CARGO_HOME` (under
    /// the rw scratch) crosses via the forwarded `-e` set instead.
    Gate,
}

/// Toolchain caches cross as READ-ONLY mounts + matching env (6th-pass
/// review: without them a container session cold-bootstraps a whole
/// rustup toolchain + registry into scratch, the container twin of the
/// m-533143 ENOSPC regression). rw would let a poisoned cache ride into
/// the operator's later builds — the same class as a shared target/, so
/// ro it is: a cache MISS (uncached crate) fails visibly inside the
/// container rather than writing through to the operator's cache.
fn push_toolchain_caches(out: &mut Vec<String>, mode: ToolchainMount) {
    let global = crate::sandbox::global_authority_dir();
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
            if global
                .as_ref()
                .is_some_and(|dir| crate::sandbox::absolutize(&host).starts_with(dir))
            {
                continue;
            }
            if var == "CARGO_HOME" {
                // The credential-bearing ROOT never crosses in either mode
                // (H12): `credentials.toml` and the legacy extensionless
                // `credentials` live there, and the process tier read-denies
                // both. Only leaf dirs are mounted, so the container's
                // CARGO_HOME contains exactly what was mounted into it and
                // nothing else.
                //
                // Gate mode takes the shim dir alone and forwards the gate
                // env's own cache-only CARGO_HOME instead of emitting one.
                // Session mode adds the shared registry/git caches (without
                // them a container session cold-bootstraps the whole
                // registry into scratch — the m-533143 ENOSPC shape) and
                // names the same path in `-e`: with the root unmounted, that
                // env value resolves to a CACHE-ONLY home inside the
                // container, assembled from the ro leaf mounts.
                let leaves: &[&str] = match mode {
                    ToolchainMount::Gate => &["bin"],
                    ToolchainMount::Session => &["bin", "registry", "git"],
                };
                let mut mounted_any = false;
                for leaf in leaves {
                    let dir = host.join(leaf);
                    if dir.is_dir() {
                        let mounted = container_host_path(&dir);
                        out.push("-v".to_string());
                        out.push(mount_arg(&mounted, true));
                        mounted_any = true;
                    }
                }
                if mode == ToolchainMount::Session && mounted_any {
                    out.push("-e".to_string());
                    out.push(format!("CARGO_HOME={}", container_host_path(&host)));
                }
                continue;
            }
            if host.is_dir() {
                let mounted = container_host_path(&host);
                out.push("-v".to_string());
                out.push(mount_arg(&mounted, true));
                out.push("-e".to_string());
                out.push(format!("{var}={mounted}"));
            }
        }
    }
}

/// The network posture: `fs+net` with an empty egress list maps to
/// `--network none` (the hard boundary); `fs+net` with a non-empty egress
/// list forwards the relay endpoint after `container_run_args` has attached
/// the unique internal network. Engine-run gates are never wired through the
/// relay and their resolution FAILS CLOSED on that pair. `fs` passes no
/// network flag.
fn push_network(out: &mut Vec<String>, inputs: &SandboxInputs, proxy_url: Option<&str>) {
    if inputs.enforce == crate::types::SandboxEnforce::FsNet {
        if inputs.egress.is_empty() {
            out.push("--network".to_string());
            out.push("none".to_string());
        } else if let Some(proxy_url) = proxy_url {
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
}

pub fn container_run_args(
    inputs: &SandboxInputs,
    spec: &ContainerSpec,
    binary: &Path,
    args: &[String],
    proxy_url: Option<&str>,
) -> Vec<String> {
    let mut out = run_prologue(inputs);
    if let Some(name) = &spec.name {
        out.push("--name".to_string());
        out.push(name.clone());
    }
    push_policy_mounts(&mut out, inputs);
    push_workdir_and_scratch_env(&mut out, inputs);
    push_toolchain_caches(&mut out, ToolchainMount::Session);
    push_authority_masks(&mut out, inputs);
    if inputs.enforce == crate::types::SandboxEnforce::FsNet && !inputs.egress.is_empty() {
        if let (Some(network), Some(_)) = (&spec.network, proxy_url) {
            out.push("--network".to_string());
            out.push(network.clone());
            push_network(&mut out, inputs, proxy_url);
        } else {
            // Defense in depth: a non-empty allowlist without a fully
            // provisioned boundary gets no network, never the default bridge.
            out.push("--network".to_string());
            out.push("none".to_string());
        }
    } else {
        push_network(&mut out, inputs, proxy_url);
    }
    out.push(spec.image.clone());
    out.push(binary.display().to_string());
    out.extend(args.iter().cloned());
    out
}

/// Env vars the gate builder itself emits (the scratch block's HOME/TMPDIR,
/// the cache block's RUSTUP_HOME/NPM_CONFIG_CACHE) or deliberately ignores
/// (the Windows TEMP pair — POSIX scratch TMPDIR is the in-container temp
/// posture): the forwarded caller env must not duplicate them. `CARGO_HOME`
/// is NOT skipped — gate mode suppresses the cache block's own CARGO_HOME
/// (the credential root never crosses), so the caller's cache-only home
/// under the rw scratch is the one the gate sees.
const GATE_FORWARD_ENV_SKIP: &[&str] = &[
    "HOME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "RUSTUP_HOME",
    "NPM_CONFIG_CACHE",
];

/// Build the `<runtime> run` argv for ONE engine-run gate command (ticket
/// container-gate-wrapper): the same read-only-root + declared-mount shape
/// an agent session gets, with four gate-specific deltas.
///
/// - The payload is `sh -c <command>` (contract/gate commands are
///   user-authored shell lines needing real shell semantics — the same
///   trust decision the host gate makes in `command_exec::shell_argv`), not
///   an agent binary.
/// - `--name <container_name>`: the bounded core's timeout SIGKILL reaches
///   the runtime CLIENT's process group, not the in-container tree (the
///   daemon owns those processes), so the caller force-removes the named
///   container on the timeout path. `--rm` still reaps every normal exit.
/// - The gate's COMPLETE sanitized env crosses via `-e` flags — `docker run`
///   forwards no client env into the container, and contract commands need
///   `KRANZ_BASE_SHA`, the cache-only `CARGO_HOME`, and PATH. The env the
///   caller hands over is already the allowlisted contract/merge env
///   (`agent_env::contract_command_env`, or `command_exec::sanitized_gate_env`
///   with its cache-only CARGO_HOME), never ambient secrets; the keys the
///   builder emits itself ([`GATE_FORWARD_ENV_SKIP`]) are excluded, and the
///   order is sorted so the argv is deterministic.
/// - Toolchain posture is [`ToolchainMount::Gate`]: the rustup toolchain and
///   npm cache cross read-only (the gate runs the repo's own toolchain from
///   the host's rustup — the established ro-mount pattern), the real Cargo
///   root NEVER crosses (credential directory — only `<cargo>/bin`'s shims
///   do). Image assumption: the configured `sandbox.image` must carry
///   whatever the host toolchain mounts do not (a non-rustup cargo, node,
///   go…) — the same assumption worker sessions already carry, documented in
///   the module doc; with `DEFAULT_IMAGE` a `cargo` gate fails loudly with
///   "not found", never silently on the host.
///
/// `fs+net` keeps the session handling (empty egress → `--network none`);
/// `fs+net` with a NON-EMPTY egress list must have been refused by the
/// resolution (fail closed — no proxy exists for engine-side gates), so
/// `push_network` is called with `proxy_url: None` here.
pub fn container_gate_run_args(
    inputs: &SandboxInputs,
    spec: &ContainerSpec,
    command: &str,
    env: &std::collections::HashMap<String, String>,
    container_name: &str,
) -> Vec<String> {
    let mut out = run_prologue(inputs);
    out.push("--name".to_string());
    out.push(container_name.to_string());
    push_policy_mounts(&mut out, inputs);
    push_workdir_and_scratch_env(&mut out, inputs);
    push_toolchain_caches(&mut out, ToolchainMount::Gate);
    push_authority_masks(&mut out, inputs);
    push_network(&mut out, inputs, None);
    let mut forwarded: Vec<(&String, &String)> = env.iter().collect();
    forwarded.sort_by_key(|(key, _)| *key);
    for (key, value) in forwarded {
        if GATE_FORWARD_ENV_SKIP.contains(&key.as_str()) {
            continue;
        }
        out.push("-e".to_string());
        out.push(format!("{key}={value}"));
    }
    out.push(spec.image.clone());
    out.push("sh".to_string());
    out.push("-c".to_string());
    out.push(command.to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SandboxInputs;
    use crate::types::SandboxEnforce;
    use std::path::PathBuf;

    #[test]
    fn mount_proof_argv_reads_the_host_sentinel_and_writes_the_guest_one() {
        // The host side is spelled by the platform, not by this test: on
        // Windows `absolutize` returns a drive path, and the verbatim form is
        // what once broke docker's colon-delimited parser. Assert the
        // COMPOSITION — host path, then the guest mount point — rather than a
        // POSIX literal that only holds on unix.
        let host = std::env::temp_dir();
        let argv = mount_proof_argv(&host, "alpine:3", "guestsentinel");
        let rendered = argv.join(" ");
        let expected_mount = format!("{}:/kranz-mount-proof", container_host_path(&host));
        assert!(rendered.contains(&expected_mount), "{rendered}");
        assert!(!expected_mount.starts_with(r"\\?\"), "{expected_mount}");
        // Both directions in one run: a mount can be visible one way and
        // stale the other.
        assert!(
            rendered.contains("cat /kranz-mount-proof/host.txt"),
            "{rendered}"
        );
        assert!(
            rendered.contains("printf %s guestsentinel > /kranz-mount-proof/guest.txt"),
            "{rendered}"
        );
        // A missing sentinel must not become a shell error, or an unshared
        // mount is indistinguishable from a dead daemon.
        assert!(rendered.contains("no-host-sentinel"), "{rendered}");
        assert!(rendered.starts_with("run --rm "), "{rendered}");
    }

    #[test]
    fn live_bind_mount_round_trip_closes_under_the_checkout() {
        // Windows refuses the provider whatever a probe says, so a probe
        // there proves nothing and would fail on the Linux image alone.
        if cfg!(target_os = "windows") {
            crate::test_capability::skip(
                crate::test_capability::capability::CONTAINER,
                "the container provider refuses Windows, so a bind-mount probe proves nothing",
            );
            return;
        }
        let Some(runtime) = detect() else {
            crate::test_capability::skip(
                crate::test_capability::capability::CONTAINER,
                "no container runtime on PATH, so the bind-mount round trip cannot be proven",
            );
            return;
        };
        // The checkout's parent, not a temp dir: a runtime can share one and
        // not the other, and this is the path a mission actually mounts.
        let checkout = std::env::current_dir().expect("a working directory");
        let root = checkout.parent().unwrap_or(&checkout);
        match prove_bind_mount(runtime, root, DEFAULT_IMAGE) {
            MountProof::Proven => {}
            MountProof::Failed(reason) => panic!(
                "the bind-mount round trip under {} did not close, so a mission's \
                 declared write set cannot be trusted here: {reason}",
                root.display()
            ),
        }
    }

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
            validator_read_deny_roots: Vec::new(),
        }
    }

    fn spec() -> ContainerSpec {
        ContainerSpec {
            runtime: ContainerRuntime::Docker,
            image: DEFAULT_IMAGE.to_string(),
            network: None,
            name: None,
        }
    }

    fn live_fixture() -> tempfile::TempDir {
        // Desktop VMs share the checkout but often not macOS /var/folders.
        // A host-only temp path can otherwise create a different empty VM
        // directory and make a mount test pass/fail for the wrong reason.
        tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap()
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
    fn container_run_args_fs_net_with_egress_uses_internal_network_and_relay_env() {
        let mut inputs = inputs(SandboxEnforce::FsNet);
        inputs.egress = vec!["crates.io:443".to_string()];
        let mut spec = spec();
        spec.network = Some("kranz-egress-test".to_string());
        spec.name = Some("kranz-egress-worker-test".to_string());
        let args = container_run_args(
            &inputs,
            &spec,
            Path::new("claude"),
            &["-p".to_string(), "hi".to_string()],
            Some("http://kranz-egress:3128"),
        );

        assert!(
            args.windows(2)
                .any(|w| w[0] == "--network" && w[1] == "kranz-egress-test"),
            "proxy-routed fs+net must use the per-run internal network: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--name" && w[1] == "kranz-egress-worker-test"),
            "the daemon-owned worker must be named for timeout teardown: {args:?}"
        );
        for var in ["HTTPS_PROXY", "HTTP_PROXY"] {
            assert!(
                args.windows(2)
                    .any(|w| w[0] == "-e" && w[1] == format!("{var}=http://kranz-egress:3128")),
                "missing -e {var}=…: {args:?}"
            );
        }
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-e" && w[1] == "NO_PROXY=localhost,127.0.0.1"),
            "missing -e NO_PROXY…: {args:?}"
        );
    }

    #[test]
    fn container_run_args_fs_net_with_egress_fails_closed_without_boundary() {
        let mut inputs = inputs(SandboxEnforce::FsNet);
        inputs.egress = vec!["crates.io:443".to_string()];
        let args = container_run_args(
            &inputs,
            &spec(),
            Path::new("claude"),
            &[],
            Some("http://kranz-egress:3128"),
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--network" && w[1] == "none"),
            "missing boundary state must disable networking: {args:?}"
        );
        assert!(
            args.iter()
                .filter(|a| a.starts_with("HTTPS_PROXY="))
                .all(|a| a == "HTTPS_PROXY="),
            "a relay env must not be emitted without its internal network: {args:?}"
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
        for path in [&session, &mission, &scratch, &cargo] {
            std::fs::create_dir_all(path).unwrap();
        }
        let inputs = SandboxInputs {
            enforce: SandboxEnforce::Fs,
            session_cwd: session.clone(),
            mission_dir: mission.clone(),
            tmpdir: scratch.clone(),
            extra_write: vec![cargo.clone()],
            egress: Vec::new(),
            validator_read_deny_roots: Vec::new(),
        };
        let args = container_run_args(
            &inputs,
            &spec(),
            Path::new("claude"),
            &["--print".to_string()],
            None,
        );
        let joined = args.join(" ");
        let abs = |p: &std::path::Path| container_host_path(p);

        assert!(args.contains(&"--rm".to_string()));
        assert!(args.contains(&"--read-only".to_string()));
        assert!(joined.contains(&mount_arg(&abs(&session), false)));
        assert!(joined.contains(&format!("--tmpfs {}:ro,", abs(&mission))));
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
        let masked_token_file = kranz_dir.join("serve.token");
        let config = kranz_dir.join("config.json");
        std::fs::write(&masked_token_file, "secret").unwrap();
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
        let abs = |p: &std::path::Path| container_host_path(p);

        assert!(joined.contains(&format!("--tmpfs {}:ro,", abs(&kranz_dir))));
        for name in ["serve.token", "serve.read.token", "config.json"] {
            assert!(
                !joined.contains(&abs(&kranz_dir.join(name))),
                "authority must stay outside the private view: {args:?}"
            );
        }
    }

    /// MED-3 (2026-09-01 adversarial audit): the mask set was a hand-copied
    /// three-name list that had already drifted from the process tier,
    /// missing `domain-terms.local`, the `hook-status/` projection, and the
    /// mission `control/` inbox — which the `:ro` mission mount made
    /// READABLE inside the container, the exact posture
    /// `authority_read_deny_dirs` exists to close. Driving the masks off the
    /// process tier's own sets is what stops the two drifting again.
    #[test]
    fn container_run_args_mask_the_whole_process_tier_authority_set() {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("session");
        let kranz = session.join(".kranz");
        let mission = kranz.join("missions").join("m-x");
        std::fs::create_dir_all(mission.join("control")).unwrap();
        std::fs::create_dir_all(kranz.join("hook-status")).unwrap();
        std::fs::create_dir_all(kranz.join("missions").join("m-other")).unwrap();
        std::fs::create_dir_all(kranz.join("queue")).unwrap();
        for name in ["serve.token", "config.json", "domain-terms.local"] {
            std::fs::write(kranz.join(name), "secret").unwrap();
        }
        let mut inputs = inputs(SandboxEnforce::Fs);
        inputs.session_cwd = session;
        inputs.mission_dir = mission.clone();

        let args = container_run_args(&inputs, &spec(), Path::new("claude"), &[], None);
        let joined = args.join(" ");
        let abs = |p: &std::path::Path| container_host_path(p);

        for name in [
            "serve.token",
            "config.json",
            "domain-terms.local",
            "hook-status",
        ] {
            assert!(
                !joined.contains(&abs(&kranz.join(name))),
                "authority must not be rebound: {args:?}"
            );
        }
        assert!(joined.contains(&format!("--tmpfs {}:ro,", abs(&kranz))));
        assert!(joined.contains(&format!("--tmpfs {}:ro,", abs(&mission))));
        assert!(!joined.contains(&abs(&mission.join("control"))));
        // The WRITE-deny half is readable-but-unwritable, never shadowed
        // (follow-up review, H-1): the engine-owned stores and the sibling
        // mission dir stay legible while the rw session mount cannot carry a
        // write back to them.
        for readable in [kranz.join("queue"), kranz.join("missions")] {
            assert!(
                joined.contains(&mount_arg(&abs(&readable), true)),
                "missing :ro self-bind for {}: {args:?}",
                readable.display()
            );
        }
    }

    /// H-1 (follow-up review): the WRITE-deny sets were folded into the two
    /// CONTENT-DESTROYING idioms — `/dev/null` file binds and empty `:ro`
    /// tmpfs shadows — so every TRACKED file under `.kranz/tickets/` and
    /// `.kranz/lessons/` read as deleted inside a checkout-mode container.
    /// The worker's own "commit your work" step then recorded the deletion of
    /// the whole ticket backlog onto the mission branch. The other two tiers
    /// implement the same deny as READABLE-but-unwritable (bwrap self
    /// ro-bind, a Windows ACE that keeps `FILE_GENERIC_READ`); this tier now
    /// does too, with the masks reserved for the READ-deny sets.
    #[test]
    fn container_run_args_keep_write_denied_kranz_content_readable() {
        let dir = tempfile::tempdir().unwrap();
        // Checkout mode: session_cwd IS the repo root, the hostile shape —
        // and the tracked ticket/lesson stores ride in on the rw session
        // mount.
        let session = dir.path().join("repo");
        let kranz = session.join(".kranz");
        let mission = kranz.join("missions").join("m-x");
        std::fs::create_dir_all(mission.join("control")).unwrap();
        std::fs::create_dir_all(kranz.join("hook-status")).unwrap();
        std::fs::create_dir_all(kranz.join("tickets")).unwrap();
        std::fs::create_dir_all(kranz.join("lessons")).unwrap();
        std::fs::create_dir_all(kranz.join("queue")).unwrap();
        std::fs::create_dir_all(kranz.join("missions").join("m-other")).unwrap();
        std::fs::write(kranz.join("tickets").join("some-ticket.md"), "# tracked").unwrap();
        std::fs::write(kranz.join("merge-gates.json"), "{}").unwrap();
        std::fs::write(kranz.join("secret-allowlist"), "OK_TOKEN\n").unwrap();
        for name in ["serve.token", "config.json"] {
            std::fs::write(kranz.join(name), "secret").unwrap();
        }
        let mut inputs = inputs(SandboxEnforce::Fs);
        inputs.session_cwd = session;
        inputs.mission_dir = mission.clone();

        let args = container_run_args(&inputs, &spec(), Path::new("claude"), &[], None);
        let joined = args.join(" ");
        let abs = |p: &std::path::Path| container_host_path(p);

        // Write-denied CONTENT: readable, unwritable — never masked.
        for readable in [
            kranz.join("tickets"),
            kranz.join("lessons"),
            kranz.join("queue"),
            kranz.join("missions"),
        ] {
            assert!(
                joined.contains(&mount_arg(&abs(&readable), true)),
                "{} must be a :ro self-bind, not a mask: {args:?}",
                readable.display()
            );
            assert!(
                !joined.contains(&format!("--tmpfs {}:ro", abs(&readable))),
                "{} must not be shadowed by an empty tmpfs: {args:?}",
                readable.display()
            );
        }
        for readable in [
            kranz.join("merge-gates.json"),
            kranz.join("secret-allowlist"),
        ] {
            assert!(
                joined.contains(&mount_arg(&abs(&readable), true)),
                "{} must be a :ro self-bind: {args:?}",
                readable.display()
            );
            assert!(
                !joined.contains(&format!("/dev/null:{}:ro", abs(&readable))),
                "{} must not read as zero bytes: {args:?}",
                readable.display()
            );
        }

        // Read-denied entries never cross the private directory views.
        for hidden in [
            kranz.join("serve.token"),
            kranz.join("config.json"),
            mission.join("control"),
            kranz.join("hook-status"),
        ] {
            assert!(
                !joined.contains(&abs(&hidden)),
                "read-denied entry was mounted: {args:?}"
            );
        }
    }

    /// MED-2 (2026-09-01 adversarial audit): the worker and gate containers
    /// got none of the hardening the egress relay already gets, so the agent
    /// ran as uid 0 with Docker's default capability set while its rw binds
    /// landed at the IDENTICAL host path.
    #[test]
    fn container_run_args_harden_the_worker_like_the_egress_relay() {
        // A REAL session dir: `--user` is derived by stat'ing the rw mount,
        // so a fixture path that does not exist would silently drop the flag.
        let session = tempfile::tempdir().unwrap();
        let mut inputs = inputs(SandboxEnforce::Fs);
        inputs.session_cwd = session.path().to_path_buf();
        let args = container_run_args(&inputs, &spec(), Path::new("claude"), &[], None);

        assert!(args
            .windows(2)
            .any(|w| w[0] == "--cap-drop" && w[1] == "ALL"));
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--security-opt" && w[1] == "no-new-privileges"));
        assert!(args
            .windows(2)
            .any(|w| w[0] == "--pids-limit" && w[1] == CONTAINER_PIDS_LIMIT));
        #[cfg(unix)]
        {
            // The owner of the rw session mount, so container writes land as
            // the operator rather than as root in the operator's own tree.
            let expected = crate::container_egress::mount_owner(session.path())
                .expect("a stat-able path yields an owner");
            assert!(
                args.windows(2)
                    .any(|w| w[0] == "--user" && w[1] == expected),
                "missing --user {expected}: {args:?}"
            );
        }
    }

    /// H12 (2026-09-01 adversarial audit): session mode mounted the whole
    /// `$CARGO_HOME` read-only with a matching `-e`, carrying
    /// `credentials.toml` (crates.io registry auth) into the container —
    /// while the tier-2 process sandbox explicitly read-DENIES exactly that
    /// file. Tier 3 was therefore weaker than tier 2 for registry
    /// credentials. Only the cache leaves cross now.
    #[test]
    fn container_run_args_never_mount_the_real_cargo_root_for_a_session() {
        let home = tempfile::tempdir().unwrap();
        let cargo = home.path().join(".cargo");
        for leaf in ["bin", "registry", "git"] {
            std::fs::create_dir_all(cargo.join(leaf)).unwrap();
        }
        std::fs::write(cargo.join("credentials.toml"), "[registry]\ntoken=\"x\"\n").unwrap();
        let _guard = crate::agent_env::EnvTestGuard::engage(&[
            ("CARGO_HOME", cargo.to_str().unwrap()),
            ("HOME", home.path().to_str().unwrap()),
        ]);

        let mut out = Vec::new();
        push_toolchain_caches(&mut out, ToolchainMount::Session);
        let joined = out.join(" ");
        let root = container_host_path(&cargo);

        assert!(
            !joined.contains(&mount_arg(&root, true)),
            "the credential-bearing Cargo root must never be mounted: {out:?}"
        );
        for leaf in ["bin", "registry", "git"] {
            let mounted = container_host_path(&cargo.join(leaf));
            assert!(
                joined.contains(&mount_arg(&mounted, true)),
                "the {leaf} cache leaf must still cross read-only: {out:?}"
            );
        }
        // The env still names a CARGO_HOME, but with the root unmounted it
        // resolves to a cache-only home assembled from the leaf mounts.
        assert!(
            out.windows(2)
                .any(|w| w[0] == "-e" && w[1] == format!("CARGO_HOME={root}")),
            "session mode must forward the cache-only CARGO_HOME: {out:?}"
        );
    }

    /// The gate argv shape (ticket container-gate-wrapper): the same
    /// declared-mount policy an agent session gets (gate cwd rw, mission dir
    /// ro, scratch rw, extra_write rw, authority masks, `-w`, scratch
    /// HOME/TMPDIR), PLUS the gate deltas — a named container, the caller's
    /// sanitized env forwarded as sorted `-e` flags (minus the keys the
    /// builder emits itself), and an `sh -c <command>` payload after the
    /// image. Expectations derive paths through the same absolutize +
    /// mount_arg the builder uses (POSIX/Windows path forms differ).
    #[test]
    fn container_gate_wrap_args_mounts_policy_forwards_env_and_payload() {
        let dir = tempfile::tempdir().unwrap();
        let gate = dir.path().join("gate");
        let mission = dir.path().join("mission");
        let scratch = dir.path().join("scratch");
        let extra = dir.path().join("extra");
        for dir in [&gate, &mission, &scratch, &extra] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let kranz_dir = gate.join(".kranz");
        std::fs::create_dir_all(&kranz_dir).unwrap();
        let masked_token_file = kranz_dir.join("serve.token");
        std::fs::write(&masked_token_file, "secret").unwrap();
        let inputs = SandboxInputs {
            enforce: SandboxEnforce::Fs,
            session_cwd: gate.clone(),
            mission_dir: mission.clone(),
            tmpdir: scratch.clone(),
            extra_write: vec![extra.clone()],
            egress: Vec::new(),
            validator_read_deny_roots: Vec::new(),
        };
        let env: std::collections::HashMap<String, String> = [
            ("ZZZ_BASE".to_string(), "deadbeef".to_string()),
            ("AAA_FIRST".to_string(), "1".to_string()),
            ("CARGO_HOME".to_string(), "/scratch/cache-only".to_string()),
            ("PATH".to_string(), "/usr/bin:/bin".to_string()),
            // The builder-owned keys: forwarded copies of these must NOT
            // appear with the caller's values.
            ("HOME".to_string(), "/caller/home".to_string()),
            ("TMPDIR".to_string(), "/caller/tmp".to_string()),
            ("RUSTUP_HOME".to_string(), "/caller/rustup".to_string()),
            ("NPM_CONFIG_CACHE".to_string(), "/caller/npm".to_string()),
        ]
        .into_iter()
        .collect();

        let args = container_gate_run_args(
            &inputs,
            &spec(),
            "cargo test --workspace",
            &env,
            "kranz-gate-test",
        );
        let joined = args.join(" ");
        let abs = |p: &std::path::Path| container_host_path(p);

        // The session mount policy, unchanged.
        assert!(args.contains(&"--read-only".to_string()));
        assert!(joined.contains(&mount_arg(&abs(&gate), false)));
        assert!(joined.contains(&format!("--tmpfs {}:ro,", abs(&mission))));
        assert!(joined.contains(&mount_arg(&abs(&scratch), false)));
        assert!(joined.contains(&mount_arg(&abs(&extra), false)));
        assert!(joined.contains(&format!("-w {}", abs(&gate))));
        assert!(joined.contains(&format!("-e HOME={}", abs(&scratch))));
        assert!(joined.contains(&format!("-e TMPDIR={}", abs(&scratch))));
        assert!(
            joined.contains(&format!("--tmpfs {}:ro,", abs(&kranz_dir)))
                && !joined.contains(&abs(&masked_token_file)),
            "authority material must stay outside the private directory: {args:?}"
        );

        // The gate deltas: named container, sh -c payload after the image.
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--name" && w[1] == "kranz-gate-test"),
            "the gate container must carry the caller-chosen name: {args:?}"
        );
        assert!(
            joined.ends_with(&format!("{DEFAULT_IMAGE} sh -c cargo test --workspace")),
            "image then sh -c payload: {args:?}"
        );

        // The caller env crosses — sorted (AAA before ZZZ)…
        let index_of = |needle: &str| {
            args.windows(2)
                .position(|w| w[0] == "-e" && w[1] == needle)
                .unwrap_or_else(|| panic!("missing -e {needle}: {args:?}"))
        };
        assert!(index_of("AAA_FIRST=1") < index_of("ZZZ_BASE=deadbeef"));
        index_of("CARGO_HOME=/scratch/cache-only");
        index_of("PATH=/usr/bin:/bin");
        // …minus the keys the builder emits itself (no caller-valued
        // duplicates of HOME/TMPDIR/the toolchain cache vars).
        for skipped in [
            "-e HOME=/caller/home",
            "-e TMPDIR=/caller/tmp",
            "-e RUSTUP_HOME=/caller/rustup",
            "-e NPM_CONFIG_CACHE=/caller/npm",
        ] {
            assert!(
                !joined.contains(skipped),
                "builder-owned env key must not be forwarded with the caller value: {skipped}\n{args:?}"
            );
        }
    }

    /// The gate toolchain posture (ticket container-gate-wrapper): the real
    /// Cargo root NEVER crosses — it is a credential directory
    /// (`credentials.toml` rides at its root), and the gate's cache-only
    /// CARGO_HOME exists precisely to keep those bytes away from
    /// worker-authored gate code. Only the credential-free `<cargo>/bin`
    /// shim dir is mounted (ro), so the forwarded PATH's rustup shim
    /// resolves; the caller's cache-only CARGO_HOME crosses via `-e`.
    #[test]
    fn container_gate_wrap_args_never_mounts_the_real_cargo_root() {
        let cargo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(cargo.path().join("bin")).unwrap();
        std::fs::write(cargo.path().join("credentials.toml"), "operator-secret").unwrap();
        let _guard = crate::agent_env::EnvTestGuard::engage(&[(
            "CARGO_HOME",
            cargo.path().to_str().expect("utf-8 temp path"),
        )]);

        let dir = tempfile::tempdir().unwrap();
        let inputs = SandboxInputs {
            enforce: SandboxEnforce::Fs,
            session_cwd: dir.path().join("gate"),
            mission_dir: dir.path().join("mission"),
            tmpdir: dir.path().join("scratch"),
            extra_write: Vec::new(),
            egress: Vec::new(),
            validator_read_deny_roots: Vec::new(),
        };
        let env: std::collections::HashMap<String, String> =
            [("CARGO_HOME".to_string(), "/scratch/cache-only".to_string())]
                .into_iter()
                .collect();
        let args = container_gate_run_args(&inputs, &spec(), "true", &env, "kranz-gate-test");
        let joined = args.join(" ");
        let abs = |p: &std::path::Path| container_host_path(p);

        let root = abs(cargo.path());
        let bin = abs(&cargo.path().join("bin"));
        assert!(
            joined.contains(&mount_arg(&bin, true)),
            "the shim dir must cross read-only: {args:?}"
        );
        assert!(
            !joined.contains(&mount_arg(&root, true)),
            "the credential-bearing Cargo root must NEVER be mounted: {args:?}"
        );
        assert!(
            !joined.contains(&format!("-e CARGO_HOME={root}")),
            "no -e may point CARGO_HOME at the real root: {args:?}"
        );
        assert!(
            joined.contains("-e CARGO_HOME=/scratch/cache-only"),
            "the caller's cache-only CARGO_HOME crosses instead: {args:?}"
        );
    }

    /// The gate network posture mirrors the session container's (ticket
    /// container-gate-wrapper): `fs+net` with an empty egress list is the
    /// hard `--network none` boundary (engine-run gates are never wired
    /// through the egress proxy, and the resolution FAILS CLOSED on a
    /// non-empty list, so the builder never sees the proxy-routed branch);
    /// `fs` keeps the runtime default bridge/NAT.
    #[test]
    fn container_gate_wrap_args_fs_net_empty_egress_disables_network() {
        let env = std::collections::HashMap::new();
        let fs_net = container_gate_run_args(
            &inputs(SandboxEnforce::FsNet),
            &spec(),
            "true",
            &env,
            "kranz-gate-test",
        );
        let network = fs_net
            .windows(2)
            .find(|w| w[0] == "--network")
            .expect("fs+net must pass a --network flag");
        assert_eq!(network[1], "none");
        assert!(
            fs_net
                .iter()
                .filter(|a| a.starts_with("HTTPS_PROXY="))
                .all(|a| a == "HTTPS_PROXY="),
            "offline gates must suppress inherited proxy configuration: {fs_net:?}"
        );

        let fs = container_gate_run_args(
            &inputs(SandboxEnforce::Fs),
            &spec(),
            "true",
            &env,
            "kranz-gate-test",
        );
        assert!(
            !fs.iter().any(|a| a == "--network"),
            "fs must not restrict the network (runtime default bridge): {fs:?}"
        );
    }

    #[test]
    fn container_run_args_respects_image_override() {
        let spec = ContainerSpec {
            runtime: ContainerRuntime::Podman,
            image: "ghcr.io/example/kranz-worker:1".to_string(),
            network: None,
            name: None,
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
    /// Skips outside the live-proven Linux host path or without a runtime;
    /// CI ubuntu-latest has Docker.
    #[test]
    fn container_provider_runs_a_trivial_worker_and_enforces_the_write_boundary() {
        if !host_supports_container_contract() {
            crate::test_capability::skip(
                crate::test_capability::capability::CONTAINER,
                &container_contract_skip_detail(),
            );
            return;
        }
        let Some(runtime) = detect() else {
            crate::test_capability::skip(
                crate::test_capability::capability::CONTAINER,
                "no docker/podman/nerdctl/container on PATH",
            );
            return;
        };

        let session = live_fixture();
        let mission = live_fixture();
        let scratch = live_fixture();
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
            validator_read_deny_roots: Vec::new(),
        };
        let spec = ContainerSpec {
            runtime,
            image: DEFAULT_IMAGE.to_string(),
            network: None,
            name: None,
        };
        let ok_file = session.path().join("ok.txt");
        let args = container_run_args(
            &inputs,
            &spec,
            Path::new("sh"),
            &[
                "-c".to_string(),
                format!(
                    "echo ok > {} && ! cat {} && echo nope > /etc/nope.txt",
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

    #[test]
    fn container_authority_directory_mask_covers_absent_and_future_tokens() {
        if crate::agent_env::isolated_global_home_test("sandbox_container::tests::container_authority_directory_mask_covers_absent_and_future_tokens") { return; }
        let home = tempfile::tempdir().unwrap();
        let _env = crate::agent_env::EnvTestGuard::engage(&[(
            if cfg!(windows) { "USERPROFILE" } else { "HOME" },
            home.path().to_str().unwrap(),
        )]);
        let global = home.path().join(".kranz");
        assert!(!global.exists());
        let mut inputs = inputs(SandboxEnforce::Fs);
        inputs.extra_write.extend([
            home.path().to_path_buf(),
            global.clone(),
            global.join("serve"),
        ]);
        for args in [
            container_run_args(&inputs, &spec(), Path::new("sh"), &[], None),
            container_gate_run_args(&inputs, &spec(), "true", &Default::default(), "test"),
        ] {
            assert!(
                args.windows(2).any(|pair| pair[0] == "--tmpfs"
                    && pair[1]
                        == format!(
                            "{}:ro,noexec,nosuid,nodev,mode=755",
                            container_host_path(&global)
                        )),
                "authority mask missing: {args:?}"
            );
            assert!(
                !args
                    .windows(2)
                    .any(|pair| pair[0] == "-v"
                        && pair[1].starts_with(&container_host_path(&global))),
                "nested mounts must not reopen global authority: {args:?}"
            );
        }
        // Building argv must never create placeholder credentials or mutate HOME.
        assert!(!global.exists());
    }

    #[cfg(unix)]
    #[test]
    fn container_authority_directory_hides_tokens_created_after_start() {
        if crate::agent_env::isolated_global_home_test("sandbox_container::tests::container_authority_directory_hides_tokens_created_after_start") { return; }
        use std::io::{BufRead as _, Write as _};
        let Some(runtime) = detect() else {
            eprintln!("no container runtime; skipping live authority test");
            return;
        };
        let dir = live_fixture();
        let home = dir.path().join("operator");
        let session = dir.path().join("session");
        let mission = session.join(".kranz/missions/m-test");
        let scratch = dir.path().join("scratch");
        std::fs::create_dir_all(&home).unwrap();
        let authority_target = dir.path().join("private-authority");
        std::fs::create_dir(&authority_target).unwrap();
        std::os::unix::fs::symlink(&authority_target, home.join(".kranz")).unwrap();
        std::fs::create_dir_all(&mission).unwrap();
        std::fs::create_dir(&scratch).unwrap();
        let authority = home.join(".kranz/serve/later.token");
        let global_config = home.join(".kranz/config.json");
        let cargo = home.join(".cargo");
        std::fs::create_dir(&cargo).unwrap();
        let repo_token_path = session.join(".kranz/serve.token");
        let repo_read_token_path = session.join(".kranz/serve.read.token");
        let repo_config = session.join(".kranz/config.json");
        let cargo_credentials = cargo.join("credentials.toml");
        let policy = session.join(".kranz/merge-gates.json");
        std::fs::write(&repo_token_path, "original-token").unwrap();
        std::fs::write(&policy, "visible-policy").unwrap();
        let input = SandboxInputs {
            enforce: SandboxEnforce::FsNet,
            session_cwd: session.clone(),
            mission_dir: mission,
            tmpdir: scratch,
            extra_write: vec![home.clone(), home.join(".kranz/serve")],
            egress: Vec::new(),
            validator_read_deny_roots: Vec::new(),
        };
        let args = {
            let _env = crate::agent_env::EnvTestGuard::engage(&[
                ("HOME", home.to_str().unwrap()),
                ("CARGO_HOME", cargo.to_str().unwrap()),
            ]);
            container_run_args(
                &input,
                &ContainerSpec {
                    runtime,
                    network: None,
                    name: None,
                    image: DEFAULT_IMAGE.to_string(),
                },
                Path::new("sh"),
                &[
                    "-c".to_string(),
                    "printf 'ready\\n'; read -r proceed; test -s \"$1\" || exit 2; \
                     for secret in \"$2\" \"$3\" \"$4\" \"$5\" \"$6\" \"$7\"; do \
                     if cat \"$secret\"; then exit 3; fi; \
                     if printf forged > \"$secret\"; then exit 4; fi; done; \
                     if rm \"$9\"; then exit 5; fi; \
                     test \"$(cat \"$8\")\" = visible-policy || exit 8; \
                     printf work > \"$1-worker\""
                        .to_string(),
                    "test".to_string(),
                    session.join("host-witness").display().to_string(),
                    authority.display().to_string(),
                    global_config.display().to_string(),
                    repo_token_path.display().to_string(),
                    repo_read_token_path.display().to_string(),
                    repo_config.display().to_string(),
                    cargo_credentials.display().to_string(),
                    policy.display().to_string(),
                    home.join(".kranz").display().to_string(),
                ],
                None,
            )
        };
        // Keep Docker's HOME/context stable while other tests relocate HOME.
        let _env = crate::agent_env::EnvTestGuard::engage(&[]);
        let mut child = std::process::Command::new(runtime.binary())
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        if line != "ready\n" {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!(
                "container did not start: {line:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        // The host creates both the directory and its tokens after the worker
        // is running. A visible witness proves its ordinary bind is live.
        std::fs::create_dir(authority.parent().unwrap()).unwrap();
        std::fs::write(&authority, "fake-authority").unwrap();
        std::fs::write(&global_config, "fake-config").unwrap();
        for path in [&repo_read_token_path, &repo_config, &cargo_credentials] {
            assert!(
                !path.exists(),
                "mount setup created a placeholder credential"
            );
            std::fs::write(path, "fake-authority").unwrap();
        }
        let rotated = session.join(".kranz/rotated.tmp");
        std::fs::write(&rotated, "rotated-token").unwrap();
        std::fs::rename(rotated, &repo_token_path).unwrap();
        std::fs::write(session.join("host-witness"), "visible").unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"continue\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(session.join("host-witness-worker").exists());
        assert!(
            home.join(".kranz").is_symlink(),
            "authority alias was replaced"
        );
        assert_eq!(
            std::fs::read_to_string(repo_token_path).unwrap(),
            "rotated-token"
        );
        for path in [&repo_read_token_path, &repo_config, &cargo_credentials] {
            assert_eq!(std::fs::read_to_string(path).unwrap(), "fake-authority");
        }
        assert_eq!(
            std::fs::read_to_string(global_config).unwrap(),
            "fake-config"
        );
    }
}
