//! Non-bypassable per-host egress for Docker worker sessions.
//!
//! A worker with `provider: container`, `enforce: fs+net`, and a non-empty
//! allowlist joins a unique Docker `--internal` network. The only peer is a
//! trusted relay that also joins Docker's ordinary bridge. The relay injects
//! a random bearer token into CONNECT and forwards it to the host-side
//! [`crate::egress_proxy::EgressProxy`]; the worker receives only the relay
//! URL, never the token. Removing proxy variables therefore removes the only
//! usable route instead of reopening unrestricted NAT.
//!
//! The boundary owns its network, relay, stopped credential loader, private
//! credential volume, transient host staging directory, and proxy. Docker's
//! local copy API moves the files into the volume without a host bind; the
//! loader and host copies are deleted before a worker starts, and only the
//! relay later mounts the volume, read-only.
//! Explicit shutdown verifies teardown. `Drop` repeats bounded best-effort
//! teardown on backend/session errors. Every start also reaps resources whose
//! recorded owner PID + immutable process identity is no longer live, closing
//! the kill-9/crash-recovery gap without touching a live sibling run.

use crate::backend::SessionSpec;
use crate::egress_proxy::{
    EgressDenial, EgressProxy, HTTPS_PROXY_ENV, HTTP_PROXY_ENV, NO_PROXY_ENV, NO_PROXY_VALUE,
};
use crate::error::{EngineError, Result};
use crate::paths::MissionPaths;
use crate::sandbox::SandboxBackend;
use crate::sandbox_container::ContainerRuntime;
use crate::types::SandboxEnforce;
use sha2::{Digest, Sha256};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

const RELAY_HOST: &str = "kranz-egress";
const RELAY_PORT: u16 = 3128;
const RESOURCE_LABEL: &str = "com.kranz.egress-boundary";
const OWNER_PID_LABEL: &str = "com.kranz.owner-pid";
const OWNER_TOKEN_LABEL: &str = "com.kranz.owner-token";
const BOUNDARY_ID_LABEL: &str = "com.kranz.boundary-id";
const CREDENTIAL_DIR_LABEL: &str = "com.kranz.credential-dir";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Pinned multi-architecture manifest. The relay is trusted enforcement code,
/// not a worker-selected image, so its bytes must not float with a tag.
const RELAY_IMAGE: &str =
    "python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d";

const RELAY_SCRIPT: &str = r#"import asyncio
import sys

HOST = sys.argv[1]
PORT = int(sys.argv[2])
AUTHORITY = open(sys.argv[3], "rb").read().strip()
MAX_HEAD = 8192

async def pump(reader, writer):
    try:
        while True:
            chunk = await reader.read(65536)
            if not chunk:
                break
            writer.write(chunk)
            await writer.drain()
    finally:
        try:
            writer.write_eof()
        except (AttributeError, OSError):
            pass

async def relay(client_reader, client_writer):
    upstream_writer = None
    try:
        head = await asyncio.wait_for(client_reader.readuntil(b"\r\n\r\n"), 10)
        if len(head) > MAX_HEAD:
            raise ValueError("CONNECT head exceeds 8 KiB")
        upstream_reader, upstream_writer = await asyncio.open_connection(HOST, PORT)
        authenticated = head[:-2] + b"Proxy-Authorization: Bearer " + AUTHORITY + b"\r\n\r\n"
        upstream_writer.write(authenticated)
        await upstream_writer.drain()
        tasks = [
            asyncio.create_task(pump(client_reader, upstream_writer)),
            asyncio.create_task(pump(upstream_reader, client_writer)),
        ]
        await asyncio.gather(*tasks, return_exceptions=True)
    except Exception as exc:
        print(f"relay connection failed: {exc}", file=sys.stderr, flush=True)
    finally:
        if upstream_writer is not None:
            upstream_writer.close()
            await upstream_writer.wait_closed()
        client_writer.close()
        await client_writer.wait_closed()

async def main():
    server = await asyncio.start_server(relay, "0.0.0.0", 3128)
    print("READY", flush=True)
    async with server:
        await server.serve_forever()

asyncio.run(main())
"#;

/// All resources for one live container egress boundary.
pub struct ContainerEgressBoundary {
    runtime: ContainerRuntime,
    network: String,
    relay: String,
    loader: String,
    worker: String,
    credential_volume: String,
    credential_dir: PathBuf,
    proxy: Option<EgressProxy>,
    active: bool,
}

impl std::fmt::Debug for ContainerEgressBoundary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContainerEgressBoundary")
            .field("network", &self.network)
            .field("relay", &self.relay)
            .field("loader", &self.loader)
            .field("worker", &self.worker)
            .field("credential_volume", &self.credential_volume)
            .field("credential_dir", &self.credential_dir)
            .finish_non_exhaustive()
    }
}

impl ContainerEgressBoundary {
    /// Provision a fully ready boundary and only then mutate the session spec.
    /// Any setup failure drops the partial boundary and removes its resources.
    pub async fn start(spec: &mut SessionSpec, paths: &MissionPaths) -> Result<Self> {
        let (runtime, allowlist) = {
            let sandbox = spec.sandbox.as_ref().ok_or_else(|| {
                EngineError::Backend("container egress requested without a sandbox".to_string())
            })?;
            if sandbox.backend != SandboxBackend::Container
                || sandbox.inputs.enforce != SandboxEnforce::FsNet
                || sandbox.inputs.egress.is_empty()
            {
                return Err(EngineError::Backend(
                    "container egress boundary requested for an incompatible session".to_string(),
                ));
            }
            let container = sandbox.container.as_ref().ok_or_else(|| {
                EngineError::Backend("container sandbox is missing runtime/image".to_string())
            })?;
            (
                container.runtime,
                crate::sandbox::effective_egress(&sandbox.inputs.egress),
            )
        };
        if runtime != ContainerRuntime::Docker {
            return Err(EngineError::Backend(format!(
                "container per-host egress requires Docker's internal-network boundary; {} is not supported",
                runtime.binary()
            )));
        }

        recover_stale_boundaries(runtime)?;
        let owner_pid = std::process::id().to_string();
        let owner_identity = owner_identity_hash(std::process::id() as i32).ok_or_else(|| {
            EngineError::Backend(
                "cannot obtain an immutable process identity for container egress cleanup"
                    .to_string(),
            )
        })?;

        let id = uuid::Uuid::new_v4().simple().to_string();
        let network = format!("kranz-egress-{id}");
        let relay = format!("kranz-egress-relay-{id}");
        let loader = format!("kranz-egress-loader-{id}");
        let worker = format!("kranz-egress-worker-{id}");
        let credential_volume = format!("kranz-egress-secret-{id}");
        let credential_dir = credential_root()?.join(format!("kranz-container-egress-{id}"));
        let relay_authority = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let proxy = EgressProxy::start_authenticated_bound(
            SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            allowlist,
            paths.egress_denials_file(),
            relay_authority.clone(),
        )
        .await?;
        let boundary = Self {
            runtime,
            network,
            relay,
            loader,
            worker,
            credential_volume,
            credential_dir,
            proxy: Some(proxy),
            active: true,
        };

        let labels = [
            (RESOURCE_LABEL, "true".to_string()),
            (OWNER_PID_LABEL, owner_pid),
            (OWNER_TOKEN_LABEL, owner_identity),
            (BOUNDARY_ID_LABEL, id),
            (
                CREDENTIAL_DIR_LABEL,
                boundary.credential_dir.display().to_string(),
            ),
        ];
        let mut create = vec![
            "network".to_string(),
            "create".to_string(),
            "--internal".to_string(),
        ];
        push_labels(&mut create, &labels);
        create.push(boundary.network.clone());
        docker_checked(runtime, &create, "create internal egress network")?;

        let mut create_volume = vec!["volume".to_string(), "create".to_string()];
        push_labels(&mut create_volume, &labels);
        create_volume.push(boundary.credential_volume.clone());
        docker_checked(
            runtime,
            &create_volume,
            "create private relay credential volume",
        )?;

        create_secure_credential_dir(&boundary.credential_dir)?;
        write_private(
            &boundary.credential_dir.join("authority"),
            relay_authority.as_bytes(),
        )?;
        write_private(
            &boundary.credential_dir.join("relay.py"),
            RELAY_SCRIPT.as_bytes(),
        )?;
        let credential_owner = credential_owner(&boundary.credential_dir)?;

        // Attach the private volume to a STOPPED, networkless loader and use
        // Docker's local copy API. No image code runs and the 0700/0600 host
        // source is never bind-mounted, avoiding both user-namespace mapping
        // failures and overlap with an unusually broad worker mount.
        let mut create_loader = vec![
            "create".to_string(),
            "--name".to_string(),
            boundary.loader.clone(),
            "--network".to_string(),
            "none".to_string(),
            "--read-only".to_string(),
            "--user".to_string(),
            credential_owner.clone(),
            "--cap-drop".to_string(),
            "ALL".to_string(),
            "--security-opt".to_string(),
            "no-new-privileges".to_string(),
            "--mount".to_string(),
            format!(
                "type=volume,src={},dst=/opt/kranz",
                boundary.credential_volume
            ),
        ];
        push_labels(&mut create_loader, &labels);
        create_loader.extend([
            RELAY_IMAGE.to_string(),
            "sh".to_string(),
            "-c".to_string(),
            "true".to_string(),
        ]);
        docker_checked(runtime, &create_loader, "create stopped credential loader")?;
        let credential_source = format!("{}/.", canonical_display(&boundary.credential_dir)?);
        let credential_target = format!("{}:/opt/kranz", boundary.loader);
        docker_checked(
            runtime,
            &["cp".to_string(), credential_source, credential_target],
            "copy private enforcement files into the relay volume",
        )?;
        docker_checked(
            runtime,
            &["rm".to_string(), boundary.loader.clone()],
            "remove stopped credential loader before worker spawn",
        )?;
        std::fs::remove_dir_all(&boundary.credential_dir).map_err(|error| {
            EngineError::Backend(format!(
                "remove copied container egress credentials {} before worker spawn: {error}",
                boundary.credential_dir.display()
            ))
        })?;

        // Create the relay stopped so both networks are attached before its
        // listener can accept a worker connection.
        let mut create_relay = vec![
            "create".to_string(),
            "--name".to_string(),
            boundary.relay.clone(),
            "--network".to_string(),
            boundary.network.clone(),
            "--network-alias".to_string(),
            RELAY_HOST.to_string(),
            "--add-host".to_string(),
            "host.docker.internal:host-gateway".to_string(),
            "--read-only".to_string(),
            "--user".to_string(),
            credential_owner,
            "--cap-drop".to_string(),
            "ALL".to_string(),
            "--security-opt".to_string(),
            "no-new-privileges".to_string(),
            "--pids-limit".to_string(),
            "64".to_string(),
            "--tmpfs".to_string(),
            "/tmp:rw,noexec,nosuid,size=1m".to_string(),
            "-e".to_string(),
            "PYTHONDONTWRITEBYTECODE=1".to_string(),
            "--mount".to_string(),
            format!(
                "type=volume,src={},dst=/opt/kranz,readonly",
                boundary.credential_volume
            ),
        ];
        push_labels(&mut create_relay, &labels);
        create_relay.extend([
            RELAY_IMAGE.to_string(),
            "python".to_string(),
            "/opt/kranz/relay.py".to_string(),
            "host.docker.internal".to_string(),
            boundary.proxy_port().to_string(),
            "/opt/kranz/authority".to_string(),
        ]);
        docker_checked(runtime, &create_relay, "create trusted egress relay")?;
        if let Err(error) = docker_checked(
            runtime,
            &[
                "network".to_string(),
                "connect".to_string(),
                "bridge".to_string(),
                boundary.relay.clone(),
            ],
            "attach trusted egress relay to the external bridge",
        ) {
            let logs = docker_output(runtime, &["logs".to_string(), boundary.relay.clone()])
                .map(|output| output_text(&output))
                .unwrap_or_else(|log_error| format!("logs unavailable: {log_error}"));
            return Err(EngineError::Backend(format!("{error}; relay logs: {logs}")));
        }
        docker_checked(
            runtime,
            &["start".to_string(), boundary.relay.clone()],
            "start trusted egress relay",
        )?;
        boundary.wait_ready()?;

        let sandbox = spec.sandbox.as_mut().expect("validated sandbox");
        let container = sandbox
            .container
            .as_mut()
            .expect("validated container spec");
        container.network = Some(boundary.network.clone());
        container.name = Some(boundary.worker.clone());
        let url = format!("http://{RELAY_HOST}:{RELAY_PORT}");
        spec.env.insert(HTTPS_PROXY_ENV.to_string(), url.clone());
        spec.env.insert(HTTP_PROXY_ENV.to_string(), url);
        spec.env
            .insert(NO_PROXY_ENV.to_string(), NO_PROXY_VALUE.to_string());

        Ok(boundary)
    }

    pub fn proxy_port(&self) -> u16 {
        self.proxy.as_ref().expect("live proxy").port()
    }

    /// Stop the authenticated proxy and verify that every Docker/filesystem
    /// resource has gone away. A cleanup failure fails the run visibly; Drop
    /// retries once more as a best-effort fallback.
    pub async fn shutdown(mut self) -> Result<Vec<EgressDenial>> {
        let denials = match self.proxy.take() {
            Some(proxy) => proxy.shutdown().await,
            None => Vec::new(),
        };
        self.cleanup_resources()?;
        self.active = false;
        Ok(denials)
    }

    fn wait_ready(&self) -> Result<()> {
        let start = Instant::now();
        while start.elapsed() < READY_TIMEOUT {
            let output = docker_output(self.runtime, &["logs".to_string(), self.relay.clone()])?;
            if output.status.success()
                && String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .any(|line| line.trim() == "READY")
            {
                return Ok(());
            }
            let state = docker_output(
                self.runtime,
                &[
                    "inspect".to_string(),
                    "--format".to_string(),
                    "{{.State.Running}}".to_string(),
                    self.relay.clone(),
                ],
            )?;
            if !state.status.success() || String::from_utf8_lossy(&state.stdout).trim() != "true" {
                return Err(EngineError::Backend(format!(
                    "trusted container egress relay {} exited before readiness: {}",
                    self.relay,
                    output_text(&output)
                )));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(EngineError::Backend(format!(
            "trusted container egress relay {} did not become ready within {}s",
            self.relay,
            READY_TIMEOUT.as_secs()
        )))
    }

    fn cleanup_resources(&mut self) -> Result<()> {
        let mut failures = Vec::new();
        if let Err(error) = docker_remove_if_present(
            self.runtime,
            &["rm".to_string(), "-f".to_string(), self.worker.clone()],
        ) {
            failures.push(error.to_string());
        }
        if let Err(error) = docker_remove_if_present(
            self.runtime,
            &["rm".to_string(), "-f".to_string(), self.relay.clone()],
        ) {
            failures.push(error.to_string());
        }
        if let Err(error) = docker_remove_if_present(
            self.runtime,
            &["rm".to_string(), "-f".to_string(), self.loader.clone()],
        ) {
            failures.push(error.to_string());
        }
        if let Err(error) = docker_remove_if_present(
            self.runtime,
            &[
                "volume".to_string(),
                "rm".to_string(),
                self.credential_volume.clone(),
            ],
        ) {
            failures.push(error.to_string());
        }
        if let Err(error) = docker_remove_if_present(
            self.runtime,
            &[
                "network".to_string(),
                "rm".to_string(),
                self.network.clone(),
            ],
        ) {
            failures.push(error.to_string());
        }
        if let Err(error) = std::fs::remove_dir_all(&self.credential_dir) {
            if error.kind() != std::io::ErrorKind::NotFound {
                failures.push(format!(
                    "remove credential dir {}: {error}",
                    self.credential_dir.display()
                ));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(EngineError::Backend(format!(
                "container egress teardown incomplete: {}",
                failures.join("; ")
            )))
        }
    }

    #[cfg(test)]
    fn resource_names(&self) -> (&str, &str, &str, &str, &str, &Path) {
        (
            &self.network,
            &self.relay,
            &self.loader,
            &self.worker,
            &self.credential_volume,
            &self.credential_dir,
        )
    }
}

impl Drop for ContainerEgressBoundary {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        self.proxy.take();
        if let Err(error) = self.cleanup_resources() {
            tracing::warn!(error = %error, "best-effort container egress teardown incomplete");
        }
    }
}

fn push_labels(args: &mut Vec<String>, labels: &[(&str, String)]) {
    for (key, value) in labels {
        args.push("--label".to_string());
        args.push(format!("{key}={value}"));
    }
}

fn create_secure_credential_dir(path: &Path) -> Result<()> {
    let root = credential_root()?;
    std::fs::create_dir_all(&root).map_err(|error| {
        EngineError::Backend(format!(
            "create container egress credential root {}: {error}",
            root.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::create_dir(path).map_err(|error| {
        EngineError::Backend(format!(
            "create container egress credential dir {}: {error}",
            path.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn credential_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| {
        EngineError::Backend(
            "HOME is required for the container egress credential root".to_string(),
        )
    })?;
    Ok(PathBuf::from(home).join(".kranz").join("container-egress"))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        EngineError::Backend(format!("create private file {}: {error}", path.display()))
    })?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn canonical_display(path: &Path) -> Result<String> {
    path.canonicalize()
        .map(|path| path.display().to_string())
        .map_err(|error| EngineError::Backend(format!("canonicalize {}: {error}", path.display())))
}

#[cfg(unix)]
fn credential_owner(path: &Path) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).map_err(|error| {
        EngineError::Backend(format!(
            "inspect container egress credential owner {}: {error}",
            path.display()
        ))
    })?;
    Ok(format!("{}:{}", metadata.uid(), metadata.gid()))
}

#[cfg(not(unix))]
fn credential_owner(_path: &Path) -> Result<String> {
    Err(EngineError::Backend(
        "container per-host egress requires a Unix credential owner mapping".to_string(),
    ))
}

/// The `--user <uid>:<gid>` a worker or gate container runs as: the OWNER of
/// the path it bind-mounts read-write, derived by exactly the same
/// [`credential_owner`] the relay applies to its credential dir (2026-09-01
/// adversarial audit, MED-2 — the worker container got none of the relay's
/// hardening). `None` off unix, and `None` when the path cannot be stat'd:
/// the argv builder is infallible by design, and a missing `--user` leaves
/// the pre-audit posture rather than failing a mission on a stat.
pub(crate) fn mount_owner(path: &Path) -> Option<String> {
    credential_owner(path).ok()
}

fn docker_output(runtime: ContainerRuntime, args: &[String]) -> Result<Output> {
    crate::command_exec::run_with_timeout(Path::new(runtime.binary()), args, COMMAND_TIMEOUT)
        .ok_or_else(|| {
            EngineError::Backend(format!(
                "{} {} failed to spawn or timed out",
                runtime.binary(),
                args.join(" ")
            ))
        })
}

fn docker_checked(runtime: ContainerRuntime, args: &[String], action: &str) -> Result<Output> {
    let output = docker_output(runtime, args)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(EngineError::Backend(format!(
            "failed to {action}: {}",
            output_text(&output)
        )))
    }
}

fn docker_remove_if_present(runtime: ContainerRuntime, args: &[String]) -> Result<()> {
    let output = docker_output(runtime, args)?;
    if output.status.success() {
        return Ok(());
    }
    let message = output_text(&output);
    let lower = message.to_ascii_lowercase();
    if lower.contains("no such") || lower.contains("not found") {
        Ok(())
    } else {
        Err(EngineError::Backend(format!(
            "{} {}: {message}",
            runtime.binary(),
            args.join(" ")
        )))
    }
}

fn output_text(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    crate::command_exec::last_chars_local(text.trim(), 1200)
}

fn owner_identity_hash(pid: i32) -> Option<String> {
    crate::event_log::process_identity_token(pid).map(|identity| {
        let digest = Sha256::digest(identity.as_bytes());
        digest.iter().map(|byte| format!("{byte:02x}")).collect()
    })
}

fn recover_stale_boundaries(runtime: ContainerRuntime) -> Result<()> {
    let list = docker_checked(
        runtime,
        &[
            "network".to_string(),
            "ls".to_string(),
            "--filter".to_string(),
            format!("label={RESOURCE_LABEL}=true"),
            "--format".to_string(),
            "{{.Name}}".to_string(),
        ],
        "list stale container egress networks",
    )?;
    for network in String::from_utf8_lossy(&list.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let inspect = docker_checked(
            runtime,
            &[
                "network".to_string(),
                "inspect".to_string(),
                "--format".to_string(),
                format!(
                    "{{{{index .Labels \"{OWNER_PID_LABEL}\"}}}}|{{{{index .Labels \"{OWNER_TOKEN_LABEL}\"}}}}|{{{{index .Labels \"{BOUNDARY_ID_LABEL}\"}}}}|{{{{index .Labels \"{CREDENTIAL_DIR_LABEL}\"}}}}"
                ),
                network.to_string(),
            ],
            "inspect container egress owner",
        )?;
        let fields = String::from_utf8_lossy(&inspect.stdout);
        let mut fields = fields.trim().splitn(4, '|');
        let pid = fields.next().and_then(|value| value.parse::<i32>().ok());
        let recorded_identity = fields.next().unwrap_or_default();
        let id = fields.next().unwrap_or_default();
        let credential_dir = fields.next().unwrap_or_default();
        let live = pid
            .and_then(owner_identity_hash)
            .is_some_and(|current| current == recorded_identity);
        if live {
            continue;
        }
        if !id.is_empty() {
            docker_remove_if_present(
                runtime,
                &[
                    "rm".to_string(),
                    "-f".to_string(),
                    format!("kranz-egress-worker-{id}"),
                ],
            )?;
            docker_remove_if_present(
                runtime,
                &[
                    "rm".to_string(),
                    "-f".to_string(),
                    format!("kranz-egress-relay-{id}"),
                ],
            )?;
            docker_remove_if_present(
                runtime,
                &[
                    "rm".to_string(),
                    "-f".to_string(),
                    format!("kranz-egress-loader-{id}"),
                ],
            )?;
        }
        docker_remove_if_present(
            runtime,
            &["network".to_string(), "rm".to_string(), network.to_string()],
        )?;
        if !id.is_empty() {
            docker_remove_if_present(
                runtime,
                &[
                    "volume".to_string(),
                    "rm".to_string(),
                    format!("kranz-egress-secret-{id}"),
                ],
            )?;
        }
        remove_recovered_credential_dir(credential_dir)?;
    }
    Ok(())
}

fn remove_recovered_credential_dir(raw: &str) -> Result<()> {
    if raw.is_empty() {
        return Ok(());
    }
    let path = PathBuf::from(raw);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let root = credential_root()?.canonicalize().map_err(EngineError::Io)?;
    let parent = path
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        .unwrap_or_default();
    if parent != root || !file_name.starts_with("kranz-container-egress-") {
        return Err(EngineError::Backend(format!(
            "refusing to remove malformed recovered credential path {raw:?}"
        )));
    }
    match std::fs::remove_dir_all(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(EngineError::Backend(format!(
            "remove recovered credential dir {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{PromptMode, SessionSpec};
    use crate::sandbox::{ResolvedSandbox, SandboxInputs};
    use std::collections::HashMap;

    fn session_spec(root: &Path, allowed_port: u16) -> SessionSpec {
        SessionSpec {
            cwd: root.to_path_buf(),
            prompt: PromptMode::SingleShot("test".to_string()),
            append_system_prompt: None,
            model: "mock".to_string(),
            effort: "medium".to_string(),
            session_id: "container-egress-live-proof".to_string(),
            resume: None,
            permission_mode: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            tools: Vec::new(),
            writable: true,
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: HashMap::new(),
            sandbox: Some(ResolvedSandbox {
                backend: SandboxBackend::Container,
                inputs: SandboxInputs {
                    enforce: SandboxEnforce::FsNet,
                    session_cwd: root.to_path_buf(),
                    mission_dir: root.to_path_buf(),
                    tmpdir: root.to_path_buf(),
                    extra_write: Vec::new(),
                    egress: vec![format!("127.0.0.1:{allowed_port}")],
                    validator_read_deny_roots: Vec::new(),
                },
                container: Some(crate::sandbox_container::ContainerSpec {
                    runtime: ContainerRuntime::Docker,
                    image: crate::sandbox_container::DEFAULT_IMAGE.to_string(),
                    network: None,
                    name: None,
                }),
            }),
            hook_status: None,
        }
    }

    fn docker_available() -> bool {
        crate::sandbox_container::detect() == Some(ContainerRuntime::Docker)
            && docker_output(ContainerRuntime::Docker, &["info".to_string()])
                .is_ok_and(|output| output.status.success())
    }

    fn inspect_exists(kind: &str, name: &str) -> bool {
        docker_output(
            ContainerRuntime::Docker,
            &[kind.to_string(), "inspect".to_string(), name.to_string()],
        )
        .is_ok_and(|output| output.status.success())
    }

    /// Linux live proof for the ticket contract: the allowed CONNECT tunnels,
    /// a denied CONNECT yields this run's structured record, removing proxy
    /// variables cannot recover a direct route, and explicit + Drop teardown
    /// remove the network, relay, and credential. CI invokes this exact
    /// ignored test with an anti-vacuity assertion.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires a live Docker daemon and pulled pinned relay image"]
    async fn container_per_host_egress_live_proof() {
        assert!(
            docker_available(),
            "Docker daemon unavailable for required live proof"
        );
        docker_checked(
            ContainerRuntime::Docker,
            &["pull".to_string(), RELAY_IMAGE.to_string()],
            "pull pinned relay image",
        )
        .unwrap();
        docker_checked(
            ContainerRuntime::Docker,
            &[
                "pull".to_string(),
                crate::sandbox_container::DEFAULT_IMAGE.to_string(),
            ],
            "pull worker proof image",
        )
        .unwrap();

        let allowed = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let allowed_port = allowed.local_addr().unwrap().port();
        let allowed_task = tokio::spawn(async move {
            let (mut stream, _) = allowed.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut request = [0_u8; 4];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"ping");
            stream.write_all(b"pong").await.unwrap();
        });

        let root = tempfile::tempdir().unwrap();
        let paths = MissionPaths::new(root.path(), "m-container-egress-proof");
        let mut spec = session_spec(root.path(), allowed_port);
        let boundary = ContainerEgressBoundary::start(&mut spec, &paths)
            .await
            .unwrap();
        let (network, relay, loader, worker, credential_volume, credential_dir) =
            boundary.resource_names();
        let network = network.to_string();
        let relay = relay.to_string();
        let loader = loader.to_string();
        let worker = worker.to_string();
        let credential_volume = credential_volume.to_string();
        let credential_dir = credential_dir.to_path_buf();
        assert!(
            !credential_dir.exists(),
            "host credential copies must be gone before any worker starts"
        );

        let allowed_request = format!(
            "printf 'CONNECT 127.0.0.1:{allowed_port} HTTP/1.1\\r\\nHost: 127.0.0.1:{allowed_port}\\r\\n\\r\\nping' | nc -w 5 {RELAY_HOST} {RELAY_PORT}"
        );
        let allowed_output = docker_checked(
            ContainerRuntime::Docker,
            &[
                "run".to_string(),
                "--rm".to_string(),
                "--network".to_string(),
                network.clone(),
                crate::sandbox_container::DEFAULT_IMAGE.to_string(),
                "sh".to_string(),
                "-c".to_string(),
                allowed_request,
            ],
            "run allowed CONNECT through relay",
        )
        .unwrap();
        let allowed_text = String::from_utf8_lossy(&allowed_output.stdout);
        assert!(
            allowed_text.contains("200 Connection Established"),
            "{allowed_text}"
        );
        assert!(allowed_text.contains("pong"), "{allowed_text}");
        allowed_task.await.unwrap();

        let denied_output = docker_checked(
            ContainerRuntime::Docker,
            &[
                "run".to_string(),
                "--rm".to_string(),
                "--network".to_string(),
                network.clone(),
                crate::sandbox_container::DEFAULT_IMAGE.to_string(),
                "sh".to_string(),
                "-c".to_string(),
                format!(
                    "printf 'CONNECT denied.invalid:443 HTTP/1.1\\r\\nHost: denied.invalid:443\\r\\n\\r\\n' | nc -w 5 {RELAY_HOST} {RELAY_PORT}"
                ),
            ],
            "run denied CONNECT through relay",
        )
        .unwrap();
        assert!(
            String::from_utf8_lossy(&denied_output.stdout).contains("403 Forbidden"),
            "{}",
            String::from_utf8_lossy(&denied_output.stdout)
        );

        // A peer on the external bridge is reachable from an ordinary
        // control container but not from the worker's internal-only network.
        let target = format!("kranz-egress-target-{}", uuid::Uuid::new_v4().simple());
        docker_checked(
            ContainerRuntime::Docker,
            &[
                "run".to_string(),
                "-d".to_string(),
                "--rm".to_string(),
                "--name".to_string(),
                target.clone(),
                crate::sandbox_container::DEFAULT_IMAGE.to_string(),
                "sh".to_string(),
                "-c".to_string(),
                "while true; do echo external | nc -l -p 18080; done".to_string(),
            ],
            "start external direct-socket target",
        )
        .unwrap();
        let target_ip = docker_checked(
            ContainerRuntime::Docker,
            &[
                "inspect".to_string(),
                "--format".to_string(),
                "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}".to_string(),
                target.clone(),
            ],
            "inspect direct-socket target",
        )
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap();
        let control = docker_checked(
            ContainerRuntime::Docker,
            &[
                "run".to_string(),
                "--rm".to_string(),
                crate::sandbox_container::DEFAULT_IMAGE.to_string(),
                "nc".to_string(),
                "-w".to_string(),
                "3".to_string(),
                target_ip.clone(),
                "18080".to_string(),
            ],
            "prove external target is live from the ordinary bridge",
        )
        .unwrap();
        assert!(String::from_utf8_lossy(&control.stdout).contains("external"));
        let bypass = docker_output(
            ContainerRuntime::Docker,
            &[
                "run".to_string(),
                "--rm".to_string(),
                "--network".to_string(),
                network.clone(),
                crate::sandbox_container::DEFAULT_IMAGE.to_string(),
                "nc".to_string(),
                "-w".to_string(),
                "3".to_string(),
                target_ip,
                "18080".to_string(),
            ],
        )
        .unwrap();
        assert!(
            !bypass.status.success(),
            "internal-only worker bypassed relay: {}",
            String::from_utf8_lossy(&bypass.stdout)
        );
        docker_remove_if_present(
            ContainerRuntime::Docker,
            &["rm".to_string(), "-f".to_string(), target],
        )
        .unwrap();

        let denials = boundary.shutdown().await.unwrap();
        assert_eq!(
            denials,
            vec![EgressDenial {
                host: "denied.invalid".to_string(),
                port: 443,
            }]
        );
        assert!(!inspect_exists("network", &network));
        assert!(!inspect_exists("container", &relay));
        assert!(!inspect_exists("container", &loader));
        assert!(!inspect_exists("container", &worker));
        assert!(!inspect_exists("volume", &credential_volume));
        assert!(!credential_dir.exists());

        // Dropping without explicit shutdown models backend failure, timeout,
        // and cancellation error paths; the same owned resources disappear.
        let mut dropped_spec = session_spec(root.path(), allowed_port);
        let dropped = ContainerEgressBoundary::start(&mut dropped_spec, &paths)
            .await
            .unwrap();
        let (
            dropped_network,
            dropped_relay,
            dropped_loader,
            dropped_worker,
            dropped_volume,
            dropped_dir,
        ) = dropped.resource_names();
        let dropped_network = dropped_network.to_string();
        let dropped_relay = dropped_relay.to_string();
        let dropped_loader = dropped_loader.to_string();
        let dropped_worker = dropped_worker.to_string();
        let dropped_volume = dropped_volume.to_string();
        let dropped_dir = dropped_dir.to_path_buf();
        docker_checked(
            ContainerRuntime::Docker,
            &[
                "run".to_string(),
                "-d".to_string(),
                "--rm".to_string(),
                "--name".to_string(),
                dropped_worker.clone(),
                "--network".to_string(),
                dropped_network.clone(),
                crate::sandbox_container::DEFAULT_IMAGE.to_string(),
                "sleep".to_string(),
                "600".to_string(),
            ],
            "seed daemon-owned worker for timeout teardown",
        )
        .unwrap();
        drop(dropped);
        assert!(!inspect_exists("network", &dropped_network));
        assert!(!inspect_exists("container", &dropped_relay));
        assert!(!inspect_exists("container", &dropped_loader));
        assert!(!inspect_exists("container", &dropped_worker));
        assert!(!inspect_exists("volume", &dropped_volume));
        assert!(!dropped_dir.exists());

        // A kill-9 cannot run Drop. Seed the exact labeled shape under a
        // definitely-dead owner identity, then prove the next start's
        // recovery removes its relay, internal network, and credential dir.
        let stale_id = uuid::Uuid::new_v4().simple().to_string();
        let stale_network = format!("kranz-egress-{stale_id}");
        let stale_relay = format!("kranz-egress-relay-{stale_id}");
        let stale_loader = format!("kranz-egress-loader-{stale_id}");
        let stale_volume = format!("kranz-egress-secret-{stale_id}");
        let stale_dir = credential_root()
            .unwrap()
            .join(format!("kranz-container-egress-{stale_id}"));
        create_secure_credential_dir(&stale_dir).unwrap();
        write_private(&stale_dir.join("authority"), b"stale").unwrap();
        let stale_labels = [
            (RESOURCE_LABEL, "true".to_string()),
            (OWNER_PID_LABEL, i32::MAX.to_string()),
            (OWNER_TOKEN_LABEL, "dead-owner".to_string()),
            (BOUNDARY_ID_LABEL, stale_id),
            (CREDENTIAL_DIR_LABEL, stale_dir.display().to_string()),
        ];
        let mut create = vec![
            "network".to_string(),
            "create".to_string(),
            "--internal".to_string(),
        ];
        push_labels(&mut create, &stale_labels);
        create.push(stale_network.clone());
        docker_checked(
            ContainerRuntime::Docker,
            &create,
            "seed stale internal network",
        )
        .unwrap();
        let mut create_volume = vec!["volume".to_string(), "create".to_string()];
        push_labels(&mut create_volume, &stale_labels);
        create_volume.push(stale_volume.clone());
        docker_checked(
            ContainerRuntime::Docker,
            &create_volume,
            "seed stale credential volume",
        )
        .unwrap();
        let mut run = vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            stale_relay.clone(),
            "--network".to_string(),
            stale_network.clone(),
        ];
        push_labels(&mut run, &stale_labels);
        run.extend([
            crate::sandbox_container::DEFAULT_IMAGE.to_string(),
            "sleep".to_string(),
            "600".to_string(),
        ]);
        docker_checked(ContainerRuntime::Docker, &run, "seed stale relay").unwrap();
        let mut create_loader = vec![
            "create".to_string(),
            "--name".to_string(),
            stale_loader.clone(),
            "--network".to_string(),
            "none".to_string(),
            "--mount".to_string(),
            format!("type=volume,src={stale_volume},dst=/opt/kranz"),
        ];
        push_labels(&mut create_loader, &stale_labels);
        create_loader.extend([
            crate::sandbox_container::DEFAULT_IMAGE.to_string(),
            "true".to_string(),
        ]);
        docker_checked(
            ContainerRuntime::Docker,
            &create_loader,
            "seed stale credential loader",
        )
        .unwrap();
        recover_stale_boundaries(ContainerRuntime::Docker).unwrap();
        assert!(!inspect_exists("network", &stale_network));
        assert!(!inspect_exists("container", &stale_relay));
        assert!(!inspect_exists("container", &stale_loader));
        assert!(!inspect_exists("volume", &stale_volume));
        assert!(!stale_dir.exists());
    }
}
