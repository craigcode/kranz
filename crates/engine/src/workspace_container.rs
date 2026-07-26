//! Local-container WorkspaceProvider (ticket
//! `.kranz/tickets/local-container-workspace.md`, design D-B implementation
//! #2 in `docs/scoping/workspace-contract.md`) — gives a mission an isolated
//! RUNNABLE environment: a per-mission compose project, dynamic host ports,
//! and contract health/readiness executed INSIDE the container network —
//! solving the host port clashes and Docker daemon contention that bare
//! worktrees cannot.
//!
//! Relationship to [`crate::sandbox_container`] (M7 tier 3): this provider
//! REUSES its runtime detection ([`ContainerRuntime`]/[`detect`]) but the
//! concepts stay separate ("the APIs stay separate"): the sandbox is the
//! process blast radius for agent CLIs — its tier-3 `--network none`
//! semantics live in THAT layer — while the workspace is the runnable
//! environment (bootstrap/services/readiness/previews). The network model
//! here is the fs-tier bridge (the runtime's default NAT): registry egress
//! works for bootstrap, and this provider never passes `--network none`.
//!
//! Isolation unit: one compose project per mission,
//! `kranz-ws-<sanitized-mission-id>` — parallel missions get distinct
//! projects, hence distinct networks and no shared port namespace. Projects
//! are mission-owned (the name carries the mission id) and never shared.
//! `docker compose` (or the runtime's compose subcommand) is required; a
//! runtime without one fails closed with the reason named, and a host with
//! no runtime at all fails closed at provision (run start, before spend).
//!
//! Ports (the contract's `services[].port.policy`):
//! - `dynamic` → published as host port 0 (OS-assigned); the assigned port
//!   is read back from the runtime once the service is up and recorded on
//!   the handle. Preview urlTemplates get `{port}` substitution ONLY with an
//!   actually-assigned dynamic port (never fabricated), and only when the
//!   contract declares exactly one dynamic service — a template never binds
//!   an arbitrary service's port.
//! - `fixed: N` → published as `N:N`; provision REFUSES before any container
//!   starts when N is already bound on the host, naming the service and the
//!   port — never a silent rebind.
//!
//! Provision: render the compose file (from the contract's `services[]` +
//! `bootstrap`/`readiness` + `mounts[]`) into the mission-owned runtime dir
//! (`<mission-dir>/workspace/compose.json`, gitignored — JSON is valid YAML
//! 1.2, so `compose -f` parses it and the provider never hand-rolls YAML
//! escaping), `compose up -d`, then wait for each declared healthCheck by
//! polling it inside its service container. The PRIMARY CHECKOUT IS NEVER
//! MOUNTED OR WRITTEN: the mount set is exactly the mission execution root
//! (the integration worktree in worktree mode) plus the contract's
//! `mounts[]`, each at its identical path.
//!
//! Readiness: the contract's `bootstrap[]` then `readiness[]` run INSIDE the
//! container network via `compose exec -T workspace sh -c …` — the
//! `workspace` service holds the worktree mount — reporting through the same
//! gate phase shapes as the local-worktree provider
//! ([`crate::workspace_provider::report_gate_outcomes`]), so block reasons
//! and decision lines are byte-identical to the host path.
//!
//! Teardown: [`TeardownMode::Keep`] leaves the project running (documented:
//! previews stay live); `Hibernate` is `compose stop` (containers paused,
//! project kept); `Destroy` is `compose down -v` (project + volumes
//! removed), then the contract's `disk.prune` hint runs on the host when
//! declared. v1's run loop still only ever drives `Keep`; the other modes
//! are live provider semantics for the idle-hibernate ticket and are
//! exercised directly by tests.
//!
//! v1 honesty notes: every container runs the shared default image
//! ([`crate::sandbox_container::DEFAULT_IMAGE`]) with its declared
//! start/healthCheck command — per-service images are a later additive
//! contract field (the schema's `deny_unknown_fields` fails closed on an
//! `image` key today). A contract-less provision starts no containers and
//! needs no runtime (D-H: never imply a runnable environment that does not
//! exist).

use crate::error::{EngineError, Result};
use crate::sandbox_container::{self, ContainerRuntime};
use crate::workspace_contract::{PortPolicy, PreviewSpec, WorkspaceContract};
use crate::workspace_gate::{
    CommandOutcome, GatePhase, BOOTSTRAP_SUMMARY_PREFIX, READINESS_SUMMARY_PREFIX,
};
use crate::workspace_provider::{
    report_gate_outcomes, PreviewPlaceholder, ProgressSink, ProvisionSpec, ReadinessOutcome,
    TeardownMode, WorkspaceHandle, WorkspaceProvider, WorkspaceProviderKind,
};
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Compose project name prefix — the sanitized mission id follows, so a
/// project is mission-owned and never shared between missions.
const PROJECT_PREFIX: &str = "kranz-ws-";

/// The service every contract bootstrap/readiness command execs into; it
/// owns the worktree mount and just stays alive (`sleep infinity`).
const WORKSPACE_SERVICE: &str = "workspace";

/// Container-side port dynamic services publish from. Inside the
/// per-project bridge network this never collides (each service is its own
/// container); the HOST side is what gets OS-assigned (`0`) and read back.
const DYNAMIC_CONTAINER_PORT: u16 = 8080;

/// The rendered compose file name inside `<mission-dir>/workspace/`.
const COMPOSE_FILE_NAME: &str = "compose.json";

/// The compose file's subdirectory inside the mission runtime dir.
const WORKSPACE_DIR: &str = "workspace";

/// Bound on the `compose version` availability probe and the `compose port`
/// readback.
const COMPOSE_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// `up -d` may pull the image on first use — give it the contract-command
/// budget.
const COMPOSE_UP_TIMEOUT: Duration = Duration::from_secs(600);
/// One exec'd bootstrap/readiness command (mirrors the host gate's cap).
const EXEC_TIMEOUT: Duration = Duration::from_secs(600);
/// A health check must pass within this overall window…
const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(120);
/// …polled at this interval, each attempt bounded so a hung check cannot
/// outlive the window.
const HEALTH_POLL_INTERVAL: Duration = Duration::from_secs(1);
const HEALTH_EXEC_TIMEOUT: Duration = Duration::from_secs(30);
/// `compose stop` / `compose down -v`.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(300);
/// Combined stdout+stderr tail kept in error messages.
const OUTPUT_TAIL: usize = 1500;

/// The container provider's handle state — everything `readiness` and
/// `teardown` need across the seam's three calls (the provider itself stays
/// stateless).
#[derive(Debug, Clone)]
pub struct ContainerWorkspace {
    pub runtime: ContainerRuntime,
    /// The mission-owned compose project (`kranz-ws-<sanitized-mission-id>`).
    pub project: String,
    /// The rendered compose file in the mission runtime dir.
    pub compose_file: PathBuf,
    /// `(service, OS-assigned host port)` for each dynamic service, read
    /// back from the runtime after the service came up — previews substitute
    /// only these ports (never fabricated).
    pub assigned_ports: Vec<(String, u16)>,
}

/// What runtime detection sees — the production hook is the host's real
/// [`sandbox_container::detect`].
type DetectHook = Arc<dyn Fn() -> Option<ContainerRuntime> + Send + Sync>;

/// How one argv runs, bounded: `(exit code, combined-output tail)` — the
/// production hook is [`spawn_bounded`].
type RunHook = Arc<dyn Fn(&[String], Duration) -> (Option<i32>, String) + Send + Sync>;

/// The runtime boundary, injectable for tests: what detection sees, and how
/// argv runs. Production uses the host's real detection
/// ([`sandbox_container::detect`]) and a bounded process spawn; tests drive
/// the full provision/readiness/teardown logic against a scripted fake with
/// no container runtime (the real spawn path is covered by the
/// runtime-gated smoke test).
#[derive(Clone)]
pub(crate) struct RuntimeHooks {
    detect: DetectHook,
    run: RunHook,
}

impl RuntimeHooks {
    fn host() -> Self {
        Self {
            detect: Arc::new(sandbox_container::detect),
            run: Arc::new(spawn_bounded),
        }
    }
}

/// Bounded argv spawn with a combined-output tail — the production `run`
/// hook. Reuses the shared `command_exec` runner (timeout kill discipline)
/// rather than open-coding a spawn.
fn spawn_bounded(argv: &[String], timeout: Duration) -> (Option<i32>, String) {
    let Some((program, args)) = argv.split_first() else {
        return (None, "empty argv".to_string());
    };
    match crate::command_exec::run_with_timeout(Path::new(program), args, timeout) {
        Some(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&stderr);
            }
            (
                output.status.code(),
                crate::command_exec::last_chars_local(&text, OUTPUT_TAIL),
            )
        }
        None => (
            None,
            format!(
                "no exit code (spawn failure or timeout after {}s)",
                timeout.as_secs()
            ),
        ),
    }
}

/// The local-container provider: per-mission compose project, dynamic
/// ports, in-network contract readiness. See the module docs.
pub struct LocalContainerProvider {
    hooks: RuntimeHooks,
}

impl LocalContainerProvider {
    /// The production provider: host runtime detection + bounded spawns.
    pub fn new() -> Self {
        Self {
            hooks: RuntimeHooks::host(),
        }
    }

    /// A provider driving a scripted runtime boundary: the full
    /// provision/readiness/teardown logic runs without a container runtime.
    #[cfg(test)]
    pub(crate) fn with_hooks(hooks: RuntimeHooks) -> Self {
        Self { hooks }
    }

    /// Run one argv through the runtime hook (blocking spawn, so off the
    /// async executor's way).
    async fn run_argv(&self, argv: &[String], timeout: Duration) -> (Option<i32>, String) {
        let run = self.hooks.run.clone();
        let argv = argv.to_vec();
        tokio::task::spawn_blocking(move || run(&argv, timeout))
            .await
            .unwrap_or_else(|e| (None, format!("runtime invocation failed to complete: {e}")))
    }

    /// Best-effort `compose down -v` after a failed provision — a
    /// half-started project must not leak containers the engine never got a
    /// handle to.
    async fn cleanup_project(&self, runtime: ContainerRuntime, project: &str, compose_file: &Path) {
        let argv = compose_argv(runtime, project, compose_file, &["down", "-v"]);
        let _ = self.run_argv(&argv, TEARDOWN_TIMEOUT).await;
    }
}

impl Default for LocalContainerProvider {
    fn default() -> Self {
        Self::new()
    }
}

/// `kranz-ws-<sanitized-mission-id>` — compose project names must match
/// `[a-z0-9][a-z0-9_-]*`; the prefix guarantees a valid leading character
/// even when the mission id sanitizes to nothing.
pub(crate) fn compose_project_name(mission_id: &str) -> String {
    let sanitized: String = mission_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else if c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() {
        format!("{PROJECT_PREFIX}mission")
    } else {
        format!("{PROJECT_PREFIX}{sanitized}")
    }
}

/// `[bin, "compose", "-p", project, "-f", compose_file, ...args]`.
fn compose_argv(
    runtime: ContainerRuntime,
    project: &str,
    compose_file: &Path,
    args: &[&str],
) -> Vec<String> {
    let mut argv = vec![
        runtime.binary().to_string(),
        "compose".to_string(),
        "-p".to_string(),
        project.to_string(),
        "-f".to_string(),
        compose_file.display().to_string(),
    ];
    argv.extend(args.iter().map(|arg| (*arg).to_string()));
    argv
}

/// `<compose argv> exec -T <service> sh -c <command>` — the command goes in
/// as ONE argv entry (no host shell re-parsing); the container's `sh` gets
/// it verbatim, exactly like the host gate hands commands to `sh -c`.
fn exec_argv(
    runtime: ContainerRuntime,
    project: &str,
    compose_file: &Path,
    service: &str,
    command: &str,
) -> Vec<String> {
    compose_argv(
        runtime,
        project,
        compose_file,
        &["exec", "-T", service, "sh", "-c", command],
    )
}

/// How a runtime invocation ended, for error messages (`Some(n)` a real
/// exit code, `None` spawn/timeout/signal).
fn code_phrase(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("exit code {code}"),
        None => "no exit code (spawn failure or timeout)".to_string(),
    }
}

/// Refuse any contract fixed port already bound on the host — naming the
/// service and the port — BEFORE a container starts (never a silent
/// rebind). Runs before runtime detection: the refusal is a host-local fact
/// and must not depend on a runtime being installed. A successful probe
/// bind means the port is free; the listener drops immediately.
///
/// The probe models docker's default publish: the wildcard address. That
/// catches exactly what `docker-proxy` would fail to bind (an existing
/// wildcard bind fails on every platform, SO_REUSEADDR semantics
/// notwithstanding); a loopback-only squat is deliberately NOT a collision
/// — docker's own reuse-semantics bind would succeed there too.
fn check_fixed_port_collisions(contract: &WorkspaceContract) -> Result<()> {
    for service in &contract.services {
        if let PortPolicy::Fixed(port) = service.port.policy {
            // Compose publishes on 0.0.0.0 by default, so probe the wildcard
            // address: it collides with any existing bind on that port.
            if std::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).is_err() {
                return Err(EngineError::InvalidState(format!(
                    "workspace.provider \"container\": fixed port {port} for service {:?} is \
                     already bound on the host; refusing rather than silently rebinding \
                     (owner: repo-setup — free the port or pick another fixed port)",
                    service.name
                )));
            }
        }
    }
    Ok(())
}

/// Long-syntax bind mount (`source` → `target`) — unlike the `a:b` short
/// form this is unambiguous for every host path shape, Windows drive
/// letters included.
fn bind_mount(source: &str, target: &str) -> serde_json::Value {
    json!({ "type": "bind", "source": source, "target": target })
}

/// Render the compose document (as JSON — valid YAML 1.2, so `compose -f`
/// parses it) from the contract: the `workspace` service (worktree +
/// declared mounts, env, stay-alive command) plus one service per contract
/// `services[]` entry. `bootstrap`/`readiness` are recorded under the
/// top-level `x-kranz` extension so the artifact is self-describing; the
/// provider still executes them via `compose exec`.
pub(crate) fn render_compose_file(
    project: &str,
    contract: &WorkspaceContract,
    worktree: &Path,
    env: &HashMap<String, String>,
) -> serde_json::Value {
    let worktree_abs = crate::sandbox::absolutize(worktree).display().to_string();
    let mut volumes = vec![bind_mount(&worktree_abs, &worktree_abs)];
    for mount in &contract.mounts {
        volumes.push(bind_mount(mount, mount));
    }
    let mut workspace = json!({
        "image": sandbox_container::DEFAULT_IMAGE,
        "command": ["sleep", "infinity"],
        "working_dir": worktree_abs,
        "volumes": volumes,
    });
    if !env.is_empty() {
        workspace["environment"] = json!(env);
    }

    let mut services = serde_json::Map::new();
    services.insert(WORKSPACE_SERVICE.to_string(), workspace);
    for service in &contract.services {
        let mut def = json!({
            "image": sandbox_container::DEFAULT_IMAGE,
            "command": ["sh", "-c", service.start],
        });
        let ports = match &service.port.policy {
            // Host port 0 = OS-assigned; read back after the service is up.
            PortPolicy::Dynamic => vec![format!("0:{DYNAMIC_CONTAINER_PORT}")],
            // Must bind exactly N on both sides (collision refused earlier).
            PortPolicy::Fixed(port) => vec![format!("{port}:{port}")],
        };
        def["ports"] = json!(ports);
        if let Some(health_check) = &service.health_check {
            def["healthcheck"] = json!({
                "test": ["CMD-SH", health_check],
                "interval": "2s",
                "timeout": "10s",
                "retries": 30,
                "start_period": "5s",
            });
        }
        services.insert(service.name.clone(), def);
    }

    json!({
        "name": project,
        "services": services,
        "x-kranz": {
            "workspaceService": WORKSPACE_SERVICE,
            "bootstrap": contract.bootstrap,
            "readiness": contract.readiness,
        },
    })
}

/// Parse the OS-assigned host port out of `compose port <svc> <port>`
/// output (`0.0.0.0:32768`, possibly several lines with an `[::]` row).
fn parse_compose_port(output: &str) -> Option<u16> {
    output
        .lines()
        .filter_map(|line| line.trim().rsplit(':').next())
        .find_map(|segment| segment.parse::<u16>().ok())
}

/// Fill preview templates: substitute `{port}` ONLY with an
/// actually-assigned dynamic host port (read back from the runtime — never
/// fabricated), and only when exactly one dynamic port was assigned; with
/// zero or several, the mapping is ambiguous and the template stays
/// unfilled (D-E).
fn fill_previews(
    previews: &[PreviewSpec],
    assigned_ports: &[(String, u16)],
) -> Vec<PreviewPlaceholder> {
    previews
        .iter()
        .map(|preview| {
            let url_template = match assigned_ports {
                [(_, port)] => preview.url_template.replace("{port}", &port.to_string()),
                _ => preview.url_template.clone(),
            };
            PreviewPlaceholder {
                name: preview.name.clone(),
                url_template,
            }
        })
        .collect()
}

#[async_trait::async_trait]
impl WorkspaceProvider for LocalContainerProvider {
    fn kind(&self) -> WorkspaceProviderKind {
        WorkspaceProviderKind::Container
    }

    async fn provision(&self, spec: &ProvisionSpec) -> Result<WorkspaceHandle> {
        if !spec.repo_root.is_dir() {
            return Err(EngineError::InvalidState(format!(
                "container provision: execution cwd {} does not exist",
                spec.repo_root.display()
            )));
        }
        let env = crate::runner::contract_env(spec.base_sha.as_deref());
        let Some(contract) = &spec.contract else {
            // No contract ⇒ nothing runnable to isolate (D-H): no
            // containers, no runtime required — the handle is the plain
            // execution cwd, mirroring the local provider's contract-less
            // behavior.
            return Ok(WorkspaceHandle {
                cwd: spec.repo_root.clone(),
                env,
                previews: Vec::new(),
                contract: None,
                detail: None,
                container: None,
            });
        };

        // Host-local precondition first (its refusal must not depend on a
        // runtime being installed), then the runtime boundary.
        check_fixed_port_collisions(contract)?;
        let runtime = (self.hooks.detect)().ok_or_else(|| {
            EngineError::Config(
                "workspace.provider \"container\" needs a container runtime on PATH (docker, \
                 podman, or nerdctl — none found; owner: operator — install a runtime or choose \
                 another workspace.provider); refusing rather than sharing the host port namespace"
                    .to_string(),
            )
        })?;
        if runtime == ContainerRuntime::AppleContainer {
            return Err(EngineError::Config(
                "workspace.provider \"container\": the `container` runtime (Apple Container) has \
                 no compose subcommand; install docker, podman, or nerdctl (owner: operator) or \
                 choose another workspace.provider"
                    .to_string(),
            ));
        }
        let probe = self
            .run_argv(
                &[
                    runtime.binary().to_string(),
                    "compose".to_string(),
                    "version".to_string(),
                ],
                COMPOSE_PROBE_TIMEOUT,
            )
            .await;
        if probe.0 != Some(0) {
            return Err(EngineError::Config(format!(
                "workspace.provider \"container\": `{} compose` is unavailable ({}); the \
                 local-container provider needs a compose subcommand — install the compose \
                 plugin (owner: operator) or choose another workspace.provider",
                runtime.binary(),
                crate::scrub::scrub(&probe.1)
            )));
        }

        // The compose project is mission-owned: rendered into the mission
        // runtime dir (gitignored), named for the mission, never shared.
        let project = compose_project_name(&spec.mission_id);
        let workspace_dir = spec.runtime_dir.join(WORKSPACE_DIR);
        std::fs::create_dir_all(&workspace_dir)?;
        let compose_file = workspace_dir.join(COMPOSE_FILE_NAME);
        let doc = render_compose_file(&project, contract, &spec.repo_root, &env);
        std::fs::write(&compose_file, serde_json::to_vec_pretty(&doc)?)?;

        let up = self
            .run_argv(
                &compose_argv(runtime, &project, &compose_file, &["up", "-d"]),
                COMPOSE_UP_TIMEOUT,
            )
            .await;
        if up.0 != Some(0) {
            self.cleanup_project(runtime, &project, &compose_file).await;
            return Err(EngineError::InvalidState(format!(
                "container provision: `{} compose up -d` failed ({}): {}",
                runtime.binary(),
                code_phrase(up.0),
                crate::scrub::scrub(&up.1)
            )));
        }

        // Wait for the declared health checks by polling each one INSIDE its
        // service container (the runtime's own health status is rendered
        // into the compose file too, but this poll is the portable gate).
        for service in contract
            .services
            .iter()
            .filter(|service| service.health_check.is_some())
        {
            let health_check = service.health_check.as_deref().expect("filtered on Some");
            let deadline = std::time::Instant::now() + HEALTH_CHECK_TIMEOUT;
            loop {
                let argv = exec_argv(
                    runtime,
                    &project,
                    &compose_file,
                    &service.name,
                    health_check,
                );
                let (code, _tail) = self.run_argv(&argv, HEALTH_EXEC_TIMEOUT).await;
                if code == Some(0) {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    self.cleanup_project(runtime, &project, &compose_file).await;
                    return Err(EngineError::InvalidState(format!(
                        "container provision: health check for service {:?} did not pass within \
                         {}s: `{health_check}`",
                        service.name,
                        HEALTH_CHECK_TIMEOUT.as_secs()
                    )));
                }
                tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
            }
        }

        // Read back the OS-assigned host port of every dynamic service —
        // the ONLY ports previews may substitute (never fabricated).
        let mut assigned_ports = Vec::new();
        for service in contract
            .services
            .iter()
            .filter(|service| matches!(service.port.policy, PortPolicy::Dynamic))
        {
            let argv = compose_argv(
                runtime,
                &project,
                &compose_file,
                &["port", &service.name, &DYNAMIC_CONTAINER_PORT.to_string()],
            );
            let (code, output) = self.run_argv(&argv, COMPOSE_PROBE_TIMEOUT).await;
            let port = if code == Some(0) {
                parse_compose_port(&output)
            } else {
                None
            };
            let Some(port) = port else {
                self.cleanup_project(runtime, &project, &compose_file).await;
                return Err(EngineError::InvalidState(format!(
                    "container provision: could not read back the OS-assigned host port for \
                     dynamic service {:?} (`compose port` {}): {}",
                    service.name,
                    code_phrase(code),
                    crate::scrub::scrub(&output)
                )));
            };
            assigned_ports.push((service.name.clone(), port));
        }

        Ok(WorkspaceHandle {
            cwd: spec.repo_root.clone(),
            env,
            previews: fill_previews(&contract.previews, &assigned_ports),
            contract: Some(contract.clone()),
            detail: Some(format!("compose project {project}")),
            container: Some(ContainerWorkspace {
                runtime,
                project,
                compose_file,
                assigned_ports,
            }),
        })
    }

    async fn readiness(
        &self,
        handle: &WorkspaceHandle,
        progress: &mut ProgressSink<'_>,
    ) -> Result<ReadinessOutcome> {
        let Some(contract) = &handle.contract else {
            // No contract: the gate is a no-op (same as the host path) —
            // trivially ready, no progress lines.
            return Ok(ReadinessOutcome::Ready);
        };
        let Some(workspace) = &handle.container else {
            return Err(EngineError::InvalidState(
                "container readiness: the handle carries a contract but no compose project — \
                 provision did not complete"
                    .to_string(),
            ));
        };

        // 1. bootstrap — ordered, stop at first failure. Same gate semantics
        //    as the host path, but exec'd INSIDE the container network (the
        //    `workspace` service holds the worktree mount), so checks reach
        //    the project's services by in-network names.
        let phase = GatePhase {
            kind: "bootstrap command",
            unit: "command",
            plural: "commands",
            prefix: BOOTSTRAP_SUMMARY_PREFIX,
            commands: &contract.bootstrap,
            stop_at_first_failure: true,
        };
        if let Some(failed) = self.run_exec_phase(workspace, &phase, progress).await? {
            return Ok(ReadinessOutcome::Failed {
                kind: "bootstrap command",
                failed,
            });
        }

        // 2. readiness — every check runs; all must pass.
        let phase = GatePhase {
            kind: "readiness check",
            unit: "check",
            plural: "checks",
            prefix: READINESS_SUMMARY_PREFIX,
            commands: &contract.readiness,
            stop_at_first_failure: false,
        };
        if let Some(failed) = self.run_exec_phase(workspace, &phase, progress).await? {
            return Ok(ReadinessOutcome::Failed {
                kind: "readiness check",
                failed,
            });
        }

        Ok(ReadinessOutcome::Ready)
    }

    async fn teardown(&self, handle: WorkspaceHandle, mode: TeardownMode) -> Result<()> {
        let Some(workspace) = &handle.container else {
            return Ok(()); // contract-less provision: nothing is running
        };
        match mode {
            // Leave the project running (documented: previews stay live).
            TeardownMode::Keep => Ok(()),
            // Containers paused, project kept.
            TeardownMode::Hibernate => {
                let (code, tail) = self
                    .run_argv(
                        &compose_argv(
                            workspace.runtime,
                            &workspace.project,
                            &workspace.compose_file,
                            &["stop"],
                        ),
                        TEARDOWN_TIMEOUT,
                    )
                    .await;
                if code == Some(0) {
                    Ok(())
                } else {
                    Err(EngineError::InvalidState(format!(
                        "container teardown (hibernate): `compose stop` failed ({}): {}",
                        code_phrase(code),
                        crate::scrub::scrub(&tail)
                    )))
                }
            }
            // Project + volumes removed; then honor the contract's disk
            // prune hint when declared (a HOST command — it prunes the
            // daemon, not anything inside the container network).
            TeardownMode::Destroy => {
                let (code, tail) = self
                    .run_argv(
                        &compose_argv(
                            workspace.runtime,
                            &workspace.project,
                            &workspace.compose_file,
                            &["down", "-v"],
                        ),
                        TEARDOWN_TIMEOUT,
                    )
                    .await;
                if code != Some(0) {
                    return Err(EngineError::InvalidState(format!(
                        "container teardown (destroy): `compose down -v` failed ({}): {}",
                        code_phrase(code),
                        crate::scrub::scrub(&tail)
                    )));
                }
                if let Some(prune) = handle
                    .contract
                    .as_ref()
                    .and_then(|contract| contract.disk.as_ref())
                    .and_then(|disk| disk.prune.as_deref())
                {
                    let cwd = workspace
                        .compose_file
                        .parent()
                        .unwrap_or(handle.cwd.as_path());
                    let (code, tail) =
                        crate::command_exec::run_shell_command_with_code(cwd, prune, &handle.env)
                            .await;
                    if code != Some(0) {
                        return Err(EngineError::InvalidState(format!(
                            "container teardown (destroy): disk prune command `{prune}` failed \
                             ({}): {}",
                            code_phrase(code),
                            crate::scrub::scrub(&tail)
                        )));
                    }
                }
                Ok(())
            }
        }
    }
}

impl LocalContainerProvider {
    /// Run one gate phase's commands via `compose exec` inside the
    /// workspace container, then report the pass/fail decision lines
    /// through the seam's shared [`report_gate_outcomes`] — byte-identical
    /// to the host gate's reporting.
    async fn run_exec_phase(
        &self,
        workspace: &ContainerWorkspace,
        phase: &GatePhase<'_>,
        progress: &mut ProgressSink<'_>,
    ) -> Result<Option<CommandOutcome>> {
        progress(
            &format!(
                "{} running {} {}",
                phase.prefix,
                phase.commands.len(),
                phase.plural
            ),
            None,
        )?;
        let total = phase.commands.len();
        let mut outcomes = Vec::with_capacity(total);
        for (i, command) in phase.commands.iter().enumerate() {
            let argv = exec_argv(
                workspace.runtime,
                &workspace.project,
                &workspace.compose_file,
                WORKSPACE_SERVICE,
                command,
            );
            let (code, output_tail) = self.run_argv(&argv, EXEC_TIMEOUT).await;
            let outcome = CommandOutcome {
                ordinal: i + 1,
                total,
                command: command.clone(),
                code,
                output_tail,
            };
            let failed = !outcome.ok();
            outcomes.push(outcome);
            if failed && phase.stop_at_first_failure {
                break;
            }
        }
        report_gate_outcomes(phase, outcomes, progress)
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_contract::parse_workspace_contract;
    use std::sync::Mutex;

    fn spec(root: &Path, mission_id: &str, contract: Option<WorkspaceContract>) -> ProvisionSpec {
        ProvisionSpec {
            mission_id: mission_id.to_string(),
            repo_root: root.to_path_buf(),
            runtime_dir: root.join(".kranz").join("missions").join(mission_id),
            base_sha: Some("deadbeefcafe".to_string()),
            contract,
        }
    }

    fn contract(json: &[u8]) -> WorkspaceContract {
        parse_workspace_contract(json).expect("valid contract")
    }

    fn minimal_contract() -> WorkspaceContract {
        contract(br#"{"schemaVersion": 1, "readiness": ["true"]}"#)
    }

    /// One dynamic service with a health check, bootstrap + readiness, a
    /// preview, and a disk prune hint (the no-op is shell-portable; Destroy
    /// runs it with the compose dir as cwd, so the relative marker lands at
    /// a known path — the portable idiom, no absolute-path interpolation).
    fn fake_contract() -> WorkspaceContract {
        contract(
            br#"{
                "schemaVersion": 1,
                "bootstrap": ["echo boot > .boot-marker"],
                "services": [
                    {
                        "name": "api",
                        "start": "sleep infinity",
                        "healthCheck": "true",
                        "port": { "policy": "dynamic" }
                    }
                ],
                "readiness": ["test -f .boot-marker"],
                "previews": [{ "name": "app", "urlTemplate": "http://localhost:{port}/" }],
                "disk": { "prune": "echo pruned > prune-marker.txt" }
            }"#,
        )
    }

    /// Collect progress lines the gate would emit (same helper shape as the
    /// seam's own tests).
    #[derive(Default)]
    struct Progress(Vec<(String, Option<String>)>);

    impl Progress {
        fn sink(&mut self) -> impl FnMut(&str, Option<String>) -> Result<()> + Send + use<'_> {
            |summary, detail| {
                self.0.push((summary.to_string(), detail));
                Ok(())
            }
        }

        fn summaries(&self) -> Vec<&str> {
            self.0.iter().map(|(s, _)| s.as_str()).collect()
        }
    }

    /// A scripted runtime boundary: records every argv and answers the
    /// compose subcommands the provider issues. `fail_exec_containing`
    /// makes matching exec'd commands fail (the readiness-failure path)
    /// while health checks and everything else succeed.
    #[derive(Default)]
    struct FakeRuntime {
        calls: Mutex<Vec<Vec<String>>>,
        fail_exec_containing: Option<String>,
    }

    impl FakeRuntime {
        fn hooks(self: &Arc<Self>) -> RuntimeHooks {
            let this = Arc::clone(self);
            RuntimeHooks {
                detect: Arc::new(|| Some(ContainerRuntime::Docker)),
                run: Arc::new(move |argv, _timeout| this.answer(argv)),
            }
        }

        fn answer(&self, argv: &[String]) -> (Option<i32>, String) {
            self.calls.lock().unwrap().push(argv.to_vec());
            let args: Vec<&str> = argv.iter().map(String::as_str).collect();
            // The availability probe: [bin, "compose", "version"].
            if args == ["docker", "compose", "version"] {
                return (Some(0), "v2.27.0".to_string());
            }
            // Everything else: [bin, "compose", "-p", P, "-f", F, ...rest].
            let rest = &args[6..];
            match rest[0] {
                "up" | "stop" | "down" | "ps" => (Some(0), String::new()),
                "port" => (Some(0), "0.0.0.0:32768\n".to_string()),
                "exec" => {
                    // rest = ["exec", "-T", service, "sh", "-c", command]
                    let command = rest[5];
                    if let Some(needle) = &self.fail_exec_containing {
                        if command.contains(needle.as_str()) {
                            return (Some(3), format!("boom running `{command}`"));
                        }
                    }
                    (Some(0), String::new())
                }
                other => (
                    Some(1),
                    format!("fake runtime: unexpected subcommand {other:?}"),
                ),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }

        /// Any recorded argv whose tail is exactly `suffix`.
        fn called_with(&self, suffix: &[&str]) -> bool {
            self.calls().iter().any(|argv| {
                let args: Vec<&str> = argv.iter().map(String::as_str).collect();
                args.ends_with(suffix)
            })
        }

        /// Any recorded argv exec'ing `command` inside `service`.
        fn execed(&self, service: &str, command: &str) -> bool {
            self.calls().iter().any(|argv| {
                let args: Vec<&str> = argv.iter().map(String::as_str).collect();
                args.windows(6)
                    .any(|w| w == ["exec", "-T", service, "sh", "-c", command])
            })
        }
    }

    #[test]
    fn compose_project_name_is_sanitized_and_mission_owned() {
        assert_eq!(compose_project_name("m-abc123"), "kranz-ws-m-abc123");
        assert_eq!(
            compose_project_name("m-Foo_Bar/baz.qux"),
            "kranz-ws-m-foo_bar-baz-qux"
        );
        assert_eq!(compose_project_name(""), "kranz-ws-mission");
        assert_ne!(
            compose_project_name("m-a"),
            compose_project_name("m-b"),
            "parallel missions never share a project"
        );
    }

    #[test]
    fn render_compose_file_maps_contract_to_services_ports_and_volumes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contract = contract(
            br#"{
                "schemaVersion": 1,
                "bootstrap": ["cargo fetch"],
                "services": [
                    {
                        "name": "api",
                        "start": "./run-api",
                        "healthCheck": "curl -sf localhost:8080/health",
                        "port": { "policy": "dynamic" }
                    },
                    {
                        "name": "db",
                        "start": "./run-db",
                        "port": { "policy": { "fixed": 5432 } }
                    }
                ],
                "readiness": ["curl -sf localhost:8080/health"],
                "mounts": ["/var/cache/cargo"],
                "previews": [{ "name": "app", "urlTemplate": "http://localhost:{port}/" }]
            }"#,
        );
        let env = crate::runner::contract_env(Some("deadbeefcafe"));
        let doc = render_compose_file("kranz-ws-m-render1", &contract, dir.path(), &env);

        assert_eq!(doc["name"], "kranz-ws-m-render1");

        // The workspace service: default image, stay-alive command, the
        // worktree bind-mounted at its identical (platform-derived)
        // absolute path, the declared mounts as volumes, env stamped.
        let abs = crate::sandbox::absolutize(dir.path()).display().to_string();
        let workspace = &doc["services"][WORKSPACE_SERVICE];
        assert_eq!(workspace["image"], sandbox_container::DEFAULT_IMAGE);
        assert_eq!(workspace["command"], json!(["sleep", "infinity"]));
        assert_eq!(workspace["working_dir"], json!(abs));
        assert_eq!(workspace["environment"]["KRANZ_BASE_SHA"], "deadbeefcafe");
        let volumes = workspace["volumes"].as_array().expect("volumes array");
        assert!(
            volumes.contains(&bind_mount(&abs, &abs)),
            "worktree bind mount: {volumes:?}"
        );
        assert!(
            volumes.contains(&bind_mount("/var/cache/cargo", "/var/cache/cargo")),
            "declared mounts[] become volumes: {volumes:?}"
        );

        // Ports: dynamic → host 0 (OS-assigned); fixed → N:N.
        assert_eq!(
            doc["services"]["api"]["ports"],
            json!([format!("0:{DYNAMIC_CONTAINER_PORT}")])
        );
        assert_eq!(doc["services"]["db"]["ports"], json!(["5432:5432"]));

        // The health check renders as a compose healthcheck; a service
        // without one gets none.
        assert_eq!(
            doc["services"]["api"]["healthcheck"]["test"],
            json!(["CMD-SH", "curl -sf localhost:8080/health"])
        );
        assert!(doc["services"]["db"].get("healthcheck").is_none());

        // Service start runs via sh -c; bootstrap/readiness are recorded on
        // the artifact (the provider executes them via compose exec).
        assert_eq!(
            doc["services"]["api"]["command"],
            json!(["sh", "-c", "./run-api"])
        );
        assert_eq!(doc["x-kranz"]["bootstrap"], json!(["cargo fetch"]));
        assert_eq!(
            doc["x-kranz"]["readiness"],
            json!(["curl -sf localhost:8080/health"])
        );
    }

    #[tokio::test]
    async fn fixed_port_collision_refuses_at_provision_naming_service_and_port() {
        // Hold the port on the wildcard address — what docker publishes by
        // default and what the provider probes. (A loopback hold would not
        // collide with a wildcard probe on BSD/macOS, where both sockets
        // carry SO_REUSEADDR; the wildcard hold collides everywhere.)
        let probe = std::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))
            .expect("bind probe socket");
        let port = probe.local_addr().expect("local addr").port();
        let dir = tempfile::tempdir().expect("tempdir");
        let contract_json = format!(
            r#"{{"schemaVersion": 1, "services": [
                {{"name": "db", "start": "./run-db", "port": {{"policy": {{"fixed": {port}}}}}}}
            ]}}"#
        );
        // No runtime hooks needed: the refusal precedes runtime detection,
        // so it is identical on runtime-less and docker hosts.
        let err = LocalContainerProvider::new()
            .provision(&spec(
                dir.path(),
                "m-collision",
                Some(contract(contract_json.as_bytes())),
            ))
            .await
            .expect_err("a host-bound fixed port refuses provision");
        let msg = err.to_string();
        assert!(msg.contains("\"db\""), "{msg}");
        assert!(msg.contains(&port.to_string()), "{msg}");
        assert!(msg.contains("refusing"), "{msg}");
        assert!(
            !dir.path()
                .join(".kranz/missions/m-collision/workspace/compose.json")
                .exists(),
            "the refusal precedes any runtime work: no compose file written"
        );
    }

    #[tokio::test]
    async fn provision_fails_closed_when_no_runtime_is_detected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = LocalContainerProvider::with_hooks(RuntimeHooks {
            detect: Arc::new(|| None),
            run: Arc::new(|_argv, _timeout| {
                panic!("no runtime invocation may happen without a detected runtime")
            }),
        });
        let err = provider
            .provision(&spec(dir.path(), "m-noruntime", Some(minimal_contract())))
            .await
            .expect_err("no runtime ⇒ fail closed");
        let msg = err.to_string();
        assert!(msg.contains("workspace.provider"), "{msg}");
        assert!(msg.contains("docker"), "{msg}");
        assert!(msg.contains("podman"), "{msg}");
        assert!(msg.contains("nerdctl"), "{msg}");
        assert!(msg.contains("owner: operator"), "{msg}");
    }

    /// The REAL host boundary on a runtime-less host (this dev host has
    /// none): production detection must hit exactly the fail-closed path.
    /// Skips on hosts with a runtime, where the deterministic
    /// `provision_fails_closed_when_no_runtime_is_detected` covers the
    /// refusal.
    #[tokio::test]
    async fn provision_fails_closed_on_a_runtimeless_host() {
        if sandbox_container::detect().is_some() {
            eprintln!(
                "host has a container runtime; skipping the real-detection refusal \
                 (covered deterministically by the injected-detection test)"
            );
            return;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let err = LocalContainerProvider::new()
            .provision(&spec(
                dir.path(),
                "m-noruntime-host",
                Some(minimal_contract()),
            ))
            .await
            .expect_err("a runtime-less host fails closed at provision");
        let msg = err.to_string();
        assert!(msg.contains("workspace.provider"), "{msg}");
        assert!(msg.contains("docker"), "{msg}");
        assert!(msg.contains("owner: operator"), "{msg}");
    }

    #[tokio::test]
    async fn provision_writes_compose_goes_up_and_reads_back_dynamic_ports() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = Arc::new(FakeRuntime::default());
        let provider = LocalContainerProvider::with_hooks(fake.hooks());
        let handle = provider
            .provision(&spec(dir.path(), "m-Fake1", Some(fake_contract())))
            .await
            .expect("provision");

        assert_eq!(provider.kind(), WorkspaceProviderKind::Container);
        assert_eq!(handle.cwd, dir.path());
        assert_eq!(
            handle.env.get("KRANZ_BASE_SHA").map(String::as_str),
            Some("deadbeefcafe")
        );
        assert_eq!(
            handle.detail.as_deref(),
            Some("compose project kranz-ws-m-fake1"),
            "the provisioned event's detail carries the compose project"
        );
        let workspace = handle.container.as_ref().expect("container state");
        assert_eq!(workspace.project, "kranz-ws-m-fake1");
        assert_eq!(
            workspace.assigned_ports,
            vec![("api".to_string(), 32768)],
            "the OS-assigned port is read back from the runtime"
        );
        // Exactly one dynamic service ⇒ the preview template got the
        // ASSIGNED port (never fabricated).
        assert_eq!(
            handle.previews,
            vec![PreviewPlaceholder {
                name: "app".to_string(),
                url_template: "http://localhost:32768/".to_string(),
            }]
        );

        // The compose file landed in the mission-owned runtime dir
        // (gitignored), named for the mission.
        let compose_file = dir
            .path()
            .join(".kranz/missions/m-Fake1/workspace/compose.json");
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&compose_file).expect("compose file written"))
                .expect("compose file is JSON");
        assert_eq!(doc["name"], "kranz-ws-m-fake1");
        assert_eq!(workspace.compose_file, compose_file);

        // The project went up; the declared health check was waited on
        // inside its service container.
        assert!(fake.called_with(&["up", "-d"]), "{:?}", fake.calls());
        assert!(fake.execed("api", "true"), "{:?}", fake.calls());
    }

    #[tokio::test]
    async fn readiness_execs_bootstrap_then_readiness_inside_the_container_network() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = Arc::new(FakeRuntime::default());
        let provider = LocalContainerProvider::with_hooks(fake.hooks());
        let handle = provider
            .provision(&spec(dir.path(), "m-fake2", Some(fake_contract())))
            .await
            .expect("provision");
        let mut progress = Progress::default();
        let outcome = provider
            .readiness(&handle, &mut progress.sink())
            .await
            .expect("readiness");

        assert!(matches!(outcome, ReadinessOutcome::Ready), "{outcome:?}");
        assert!(
            fake.execed(WORKSPACE_SERVICE, "echo boot > .boot-marker"),
            "bootstrap exec'd inside the workspace container: {:?}",
            fake.calls()
        );
        assert!(
            fake.execed(WORKSPACE_SERVICE, "test -f .boot-marker"),
            "readiness exec'd inside the workspace container: {:?}",
            fake.calls()
        );
        assert_eq!(
            progress.summaries(),
            vec![
                "workspace bootstrap: running 1 commands",
                "workspace bootstrap: 1/1 commands ok",
                "workspace readiness: running 1 checks",
                "workspace readiness: 1/1 checks ok",
            ],
            "the gate's decision lines are byte-identical to the host path"
        );
    }

    #[tokio::test]
    async fn readiness_failure_reports_the_gate_outcome() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = Arc::new(FakeRuntime {
            fail_exec_containing: Some("test -f".to_string()),
            ..FakeRuntime::default()
        });
        let provider = LocalContainerProvider::with_hooks(fake.hooks());
        let handle = provider
            .provision(&spec(dir.path(), "m-fake3", Some(fake_contract())))
            .await
            .expect("provision (health checks do not match the failure needle)");
        let mut progress = Progress::default();
        let outcome = provider
            .readiness(&handle, &mut progress.sink())
            .await
            .expect("readiness");

        let ReadinessOutcome::Failed { kind, failed } = outcome else {
            panic!("readiness failure must be Failed, got {outcome:?}");
        };
        assert_eq!(kind, "readiness check");
        assert_eq!(failed.code, Some(3));
        assert_eq!(
            progress.summaries(),
            vec![
                "workspace bootstrap: running 1 commands",
                "workspace bootstrap: 1/1 commands ok",
                "workspace readiness: running 1 checks",
                "workspace readiness: FAILED at check 1/1 — blocking mission (owner: repo-setup)",
            ]
        );
    }

    #[tokio::test]
    async fn teardown_keep_hibernate_and_destroy_semantics() {
        // Keep: nothing happens — the project stays live (previews keep
        // working); the provider makes NO further runtime calls.
        let dir = tempfile::tempdir().expect("tempdir");
        let fake = Arc::new(FakeRuntime::default());
        let provider = LocalContainerProvider::with_hooks(fake.hooks());
        let handle = provider
            .provision(&spec(dir.path(), "m-fake4", Some(fake_contract())))
            .await
            .expect("provision");
        let calls_before = fake.calls().len();
        provider
            .teardown(handle, TeardownMode::Keep)
            .await
            .expect("keep is a no-op");
        assert_eq!(
            fake.calls().len(),
            calls_before,
            "Keep leaves the project running: no runtime calls"
        );

        // Hibernate: compose stop (containers paused, project kept).
        let fake = Arc::new(FakeRuntime::default());
        let provider = LocalContainerProvider::with_hooks(fake.hooks());
        let handle = provider
            .provision(&spec(dir.path(), "m-fake5", Some(fake_contract())))
            .await
            .expect("provision");
        provider
            .teardown(handle, TeardownMode::Hibernate)
            .await
            .expect("hibernate");
        assert!(fake.called_with(&["stop"]), "{:?}", fake.calls());
        assert!(!fake.called_with(&["down", "-v"]), "{:?}", fake.calls());

        // Destroy: compose down -v (project + volumes removed), then the
        // contract's disk prune hint runs on the host (relative marker in
        // the prune cwd = the compose dir — the portable idiom).
        let fake = Arc::new(FakeRuntime::default());
        let provider = LocalContainerProvider::with_hooks(fake.hooks());
        let handle = provider
            .provision(&spec(dir.path(), "m-fake6", Some(fake_contract())))
            .await
            .expect("provision");
        provider
            .teardown(handle, TeardownMode::Destroy)
            .await
            .expect("destroy");
        assert!(fake.called_with(&["down", "-v"]), "{:?}", fake.calls());
        assert!(
            dir.path()
                .join(".kranz/missions/m-fake6/workspace/prune-marker.txt")
                .exists(),
            "the disk prune hint ran with the compose dir as cwd"
        );
    }

    #[tokio::test]
    async fn provision_without_a_contract_starts_no_containers_and_needs_no_runtime() {
        // Production hooks on a possibly runtime-less host: a contract-less
        // provision must never touch the runtime boundary (D-H).
        let dir = tempfile::tempdir().expect("tempdir");
        let provider = LocalContainerProvider::new();
        let handle = provider
            .provision(&spec(dir.path(), "m-nocontract", None))
            .await
            .expect("provision");
        assert!(handle.container.is_none());
        assert!(handle.detail.is_none());
        assert!(handle.previews.is_empty());

        let mut progress = Progress::default();
        let outcome = provider
            .readiness(&handle, &mut progress.sink())
            .await
            .expect("readiness");
        assert!(matches!(outcome, ReadinessOutcome::Ready));
        assert!(progress.0.is_empty(), "no contract ⇒ no gate lines");
        provider
            .teardown(handle, TeardownMode::Destroy)
            .await
            .expect("teardown with no containers is a no-op");
    }

    #[test]
    fn parse_compose_port_reads_the_assigned_host_port() {
        assert_eq!(parse_compose_port("0.0.0.0:32768\n"), Some(32768));
        assert_eq!(
            parse_compose_port("[::]:49153\n0.0.0.0:49153\n"),
            Some(49153)
        );
        assert_eq!(parse_compose_port(""), None);
        assert_eq!(parse_compose_port("Error: no such service\n"), None);
    }

    #[test]
    fn previews_substitute_only_a_single_actually_assigned_dynamic_port() {
        let previews = vec![PreviewSpec {
            name: "app".to_string(),
            url_template: "http://localhost:{port}/".to_string(),
        }];
        // Zero assigned ⇒ the template stays unfilled (never fabricated).
        assert_eq!(
            fill_previews(&previews, &[])[0].url_template,
            "http://localhost:{port}/"
        );
        // Exactly one ⇒ substituted with that assigned port.
        assert_eq!(
            fill_previews(&previews, &[("api".to_string(), 32768)])[0].url_template,
            "http://localhost:32768/"
        );
        // Several ⇒ ambiguous which service's port; stays unfilled.
        assert_eq!(
            fill_previews(
                &previews,
                &[("api".to_string(), 32768), ("web".to_string(), 32769)]
            )[0]
            .url_template,
            "http://localhost:{port}/"
        );
    }

    /// Runtime-gated smoke: two parallel provisions get distinct compose
    /// projects (distinct networks — no shared port namespace), readiness
    /// passes inside the container network, the bootstrap write lands on the
    /// HOST worktree through the mount, and Destroy removes both projects.
    /// Skips on hosts with no container runtime (this macOS dev host); CI
    /// ubuntu-latest has docker. CI runners are ephemeral, so a failed
    /// assertion mid-test may leave a project behind.
    #[tokio::test]
    async fn container_workspace_smoke_provisions_isolates_and_destroys() {
        let Some(runtime) = sandbox_container::detect() else {
            eprintln!(
                "no container runtime (docker/podman/nerdctl/container) on PATH; \
                 skipping container workspace smoke test"
            );
            return;
        };
        if runtime == ContainerRuntime::AppleContainer {
            eprintln!("the `container` runtime has no compose subcommand; skipping smoke test");
            return;
        }
        let compose_ok = std::process::Command::new(runtime.binary())
            .args(["compose", "version"])
            .stdin(std::process::Stdio::null())
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false);
        if !compose_ok {
            eprintln!(
                "`{} compose` unavailable; skipping smoke test",
                runtime.binary()
            );
            return;
        }

        let dir_a = tempfile::tempdir().expect("tempdir a");
        let dir_b = tempfile::tempdir().expect("tempdir b");
        let provider = LocalContainerProvider::new();
        let handle_a = provider
            .provision(&spec(dir_a.path(), "m-smoke-a", Some(fake_contract())))
            .await
            .expect("provision a");
        let handle_b = provider
            .provision(&spec(dir_b.path(), "m-smoke-b", Some(fake_contract())))
            .await
            .expect("provision b (parallel)");

        let project_a = handle_a.container.as_ref().unwrap().project.clone();
        let project_b = handle_b.container.as_ref().unwrap().project.clone();
        assert_ne!(
            project_a, project_b,
            "parallel missions get distinct projects (distinct networks)"
        );

        for handle in [&handle_a, &handle_b] {
            let mut progress = Progress::default();
            let outcome = provider
                .readiness(handle, &mut progress.sink())
                .await
                .expect("readiness");
            assert!(
                matches!(outcome, ReadinessOutcome::Ready),
                "readiness passes inside the container network: {outcome:?}"
            );
        }
        assert!(
            dir_a.path().join(".boot-marker").exists(),
            "bootstrap wrote through the worktree mount to the host"
        );

        let port_a = handle_a.container.as_ref().unwrap().assigned_ports[0].1;
        let port_b = handle_b.container.as_ref().unwrap().assigned_ports[0].1;
        assert!(port_a > 0 && port_b > 0, "OS-assigned ports read back");
        assert_ne!(port_a, port_b, "no shared port namespace");
        assert!(
            handle_a.previews[0]
                .url_template
                .contains(&port_a.to_string()),
            "the preview got the actually-assigned port"
        );

        provider
            .teardown(handle_a, TeardownMode::Destroy)
            .await
            .expect("destroy a");
        provider
            .teardown(handle_b, TeardownMode::Destroy)
            .await
            .expect("destroy b");
        for project in [&project_a, &project_b] {
            let output = std::process::Command::new(runtime.binary())
                .args(["compose", "-p", project, "ps", "-q"])
                .stdin(std::process::Stdio::null())
                .output()
                .expect("spawn compose ps");
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout).trim().is_empty(),
                "Destroy removes the project {project}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
