//! Docker-backed ACP lifetime proof. Configuration admission remains separate:
//! a transport fixture does not certify a vendor runtime or authentication.
//! The private, read-only host lease is checked by a non-dumpable guest PID 1.
//! Engine death therefore stops the namespace without relying on a Drop handler,
//! stdin EOF, a cooperative adapter, or a host process-group kill.

use crate::backend::SessionSpec;
use crate::error::{EngineError, Result};
use crate::gate_evaluation::subprocess::DockerEvaluator;
use crate::sandbox::{SandboxBackend, SandboxInputs};
use crate::sandbox_container::{ContainerRuntime, MountProof};
use std::collections::HashMap;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::task::JoinHandle;

const SUPERVISOR: &str = include_str!("acp_container/supervisor.py");
const GUEST_CONTROL: &str = "/kranz-owned-session";

fn error(message: impl std::fmt::Display) -> EngineError {
    EngineError::Backend(format!("ACP container: {message}"))
}

pub(crate) struct OwnedContainer {
    client: DockerEvaluator,
    name: String,
    owner: String,
    image: String,
    root: Option<tempfile::TempDir>,
    heartbeat: Option<JoinHandle<()>>,
    creation_started: bool,
    creation_finished: bool,
    removed: bool,
}

impl OwnedContainer {
    pub(crate) async fn prepare(
        spec: &SessionSpec,
        program: &Path,
        args: &[String],
    ) -> Result<(Self, tokio::process::Command)> {
        let resolved = spec
            .sandbox
            .as_ref()
            .ok_or_else(|| error("missing sandbox"))?;
        let container = resolved
            .container
            .as_ref()
            .ok_or_else(|| error("missing runtime/image"))?;
        if resolved.backend != SandboxBackend::Container
            || container.runtime != ContainerRuntime::Docker
            || resolved.inputs.enforce == crate::types::SandboxEnforce::Off
        {
            return Err(error(
                "only an enforced private Linux Docker namespace is supported",
            ));
        }
        if !program.is_absolute() || !pinned_image(&container.image) {
            return Err(error(
                "requires an absolute guest executable and immutable image digest",
            ));
        }
        if crate::sandbox::absolutize(&spec.cwd)
            != crate::sandbox::absolutize(&resolved.inputs.session_cwd)
        {
            return Err(error("session working directory differs from sandbox"));
        }
        let filtered_network = resolved.inputs.enforce == crate::types::SandboxEnforce::FsNet
            && !resolved.inputs.egress.is_empty();
        if filtered_network
            && (!container
                .network
                .as_deref()
                .is_some_and(|n| n.starts_with("kranz-egress-"))
                || spec
                    .env
                    .get(crate::egress_proxy::HTTPS_PROXY_ENV)
                    .map(String::as_str)
                    != Some("http://kranz-egress:3128"))
        {
            return Err(error(
                "filtered egress requires the engine-owned internal relay network",
            ));
        }
        crate::sandbox::validate_git_config_protection(&resolved.inputs, true)?;
        let docker = trusted_docker(&resolved.inputs)?;
        let client = DockerEvaluator::new(&docker).map_err(error)?;
        if filtered_network {
            let network = client
                .control(&[
                    "network".into(),
                    "inspect".into(),
                    container.network.clone().expect("checked filtered network"),
                ])
                .await
                .map_err(error)?;
            let metadata: serde_json::Value =
                crate::strict_json::parse(&network.stdout).map_err(error)?;
            let network = metadata
                .get(0)
                .filter(|_| network.code == Some(0))
                .ok_or_else(|| error("cannot inspect filtered network"))?;
            let owner = crate::container_egress::owner_identity_hash(std::process::id() as i32)
                .ok_or_else(|| error("cannot verify network owner identity"))?;
            if network["Internal"] != true
                || network["Labels"]["com.kranz.egress-boundary"] != "true"
                || network["Labels"]["com.kranz.owner-pid"]
                    .as_str()
                    .and_then(|pid| pid.parse::<u32>().ok())
                    != Some(std::process::id())
                || network["Labels"]["com.kranz.owner-token"] != owner
            {
                return Err(error(
                    "filtered network is not internal and owned by this engine",
                ));
            }
        }
        let image = client
            .control(&["image".into(), "inspect".into(), container.image.clone()])
            .await
            .map_err(error)?;
        if image.code != Some(0) {
            return Err(error(
                "pinned image must already be installed; no implicit pull",
            ));
        }
        let image: serde_json::Value = crate::strict_json::parse(&image.stdout).map_err(error)?;
        let image = image
            .get(0)
            .ok_or_else(|| error("missing image inspection"))?;
        if (image["Id"].as_str() != Some(&container.image)
            && !image["RepoDigests"]
                .as_array()
                .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(&container.image))))
            || image["Os"] != "linux"
            || image["Config"]["Volumes"]
                .as_object()
                .is_some_and(|v| !v.is_empty())
        {
            return Err(error(
                "image digest/platform/anonymous-volume policy is not satisfied",
            ));
        }
        let root = tempfile::Builder::new()
            .prefix("kranz-acp-owned-")
            .tempdir_in(crate::backend_claude::scratch_root_base())
            .map_err(error)?;
        let canonical = root.path().canonicalize().map_err(error)?;
        for writable in std::iter::once(&resolved.inputs.session_cwd)
            .chain(std::iter::once(&resolved.inputs.tmpdir))
            .chain(&resolved.inputs.extra_write)
        {
            if canonical.starts_with(crate::sandbox::absolutize(writable)) {
                return Err(error(
                    "host lease must be outside every worker-writable root",
                ));
            }
        }
        std::fs::create_dir_all(&resolved.inputs.tmpdir).map_err(error)?;
        let roots = crate::sandbox_container::declared_mount_roots(
            &resolved.inputs.session_cwd,
            &resolved.inputs.mission_dir,
            &resolved.inputs.extra_write,
        );
        let runtime = container.runtime;
        let pin = container.image.clone();
        let proof_root = canonical.clone();
        let proof = tokio::task::spawn_blocking(move || {
            let mut roots = roots;
            roots.push(proof_root);
            crate::sandbox_container::prove_mount_roots(runtime, &roots, &pin)
        })
        .await
        .map_err(error)?;
        if let MountProof::Failed(why) = proof {
            return Err(error(format!("host mount proof failed: {why}")));
        }
        let name = container
            .name
            .clone()
            .unwrap_or_else(|| format!("kranz-acp-{}", uuid::Uuid::new_v4().simple()));
        if !name.starts_with("kranz-")
            || name.len() > 128
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(error("invalid owned container name"));
        }
        let owner = uuid::Uuid::new_v4().simple().to_string();
        let mut env = HashMap::from([
            (
                "PATH".to_string(),
                "/usr/local/bin:/usr/bin:/bin".to_string(),
            ),
            ("LANG".to_string(), "C.UTF-8".to_string()),
        ]);
        env.extend(spec.env.clone());
        let scratch = crate::sandbox::absolutize(&resolved.inputs.tmpdir)
            .display()
            .to_string();
        env.insert("HOME".into(), scratch.clone());
        env.insert("TMPDIR".into(), scratch);
        let mut argv = vec![program.display().to_string()];
        argv.extend_from_slice(args);
        private_write(&canonical.join("supervisor.py"), SUPERVISOR.as_bytes())?;
        private_write(
            &canonical.join("launch.json"),
            &serde_json::to_vec(&serde_json::json!({
                "argv":argv, "cwd":crate::sandbox::absolutize(&spec.cwd), "env":env,
            }))?,
        )?;
        private_write(&canonical.join("lease"), &0_u64.to_be_bytes())?;
        private_write(
            &canonical.join("container.json"),
            &serde_json::to_vec(&serde_json::json!({
                "version":1, "name":name, "owner":owner, "image":container.image, "docker":docker,
                "ownerPid":std::process::id(), "supervisorSha256":crate::standards_waiver::sha256_hex(SUPERVISOR.as_bytes()),
                "ownerIdentity":crate::event_log::process_identity_token(std::process::id() as i32),
                "cleanupConfirmed":false,
            }))?,
        )?;
        let mut owned = Self {
            client,
            name: name.clone(),
            owner: owner.clone(),
            image: container.image.clone(),
            root: Some(root),
            heartbeat: None,
            creation_started: false,
            creation_finished: false,
            removed: false,
        };
        let mut container = container.clone();
        container.name = Some(name.clone());
        let mut create = crate::sandbox_container::container_run_args(
            &resolved.inputs,
            &container,
            Path::new("-I"),
            &[
                "-S".into(),
                "-u".into(),
                format!("{GUEST_CONTROL}/supervisor.py"),
                GUEST_CONTROL.into(),
            ],
            spec.env
                .get(crate::egress_proxy::HTTPS_PROXY_ENV)
                .map(String::as_str),
        );
        create[0] = "create".into();
        let image_index = create.len() - 6;
        create.splice(
            image_index..image_index,
            [
                "--entrypoint".into(),
                "/usr/local/bin/python3".into(),
                "--init=false".into(),
                "--no-healthcheck".into(),
                "--pull=never".into(),
                "--mount".into(),
                format!(
                    "type=bind,src={},dst={GUEST_CONTROL},readonly",
                    mount_path(&canonical)?
                ),
                "--label".into(),
                format!("com.kranz.acp-owner={owner}"),
            ],
        );
        owned.creation_started = true;
        let created = owned.client.control(&create).await.map_err(error)?;
        let id = std::str::from_utf8(&created.stdout)
            .unwrap_or_default()
            .trim();
        if created.code != Some(0) || id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(error("could not create owned namespace"));
        }
        owned.creation_finished = true;
        let mut lease = std::fs::OpenOptions::new()
            .write(true)
            .open(canonical.join("lease"))
            .map_err(error)?;
        owned.heartbeat = Some(tokio::spawn(async move {
            let mut sequence = 1_u64;
            loop {
                // Keep one inode: VM shared filesystems need not expose a host
                // rename atomically. A torn counter still proves a recent host
                // write, then expires normally if the owner stops midway.
                if lease.seek(SeekFrom::Start(0)).is_err()
                    || lease.write_all(&sequence.to_be_bytes()).is_err()
                {
                    break; // Failure to renew expires the namespace, never extends it.
                }
                sequence = sequence.wrapping_add(1);
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }));
        let command = owned.client.attached_command(&[
            "start".into(),
            "--attach".into(),
            "--interactive".into(),
            name,
        ]);
        Ok((owned, command))
    }

    pub(crate) fn receipt(&self) -> serde_json::Value {
        serde_json::json!({
            "backend":"docker", "image":self.image, "owner":self.owner,
            "supervisorSha256":crate::standards_waiver::sha256_hex(SUPERVISOR.as_bytes()),
            "leaseSeconds":5, "credentialSource":"caller-supplied-session-environment-and-scratch",
            "providerCompatibilityCertified":false,
        })
    }

    fn stop_lease(&mut self) {
        if let Some(task) = self.heartbeat.take() {
            task.abort();
        }
        if let Some(root) = &self.root {
            let _ = std::fs::remove_file(root.path().join("lease"));
        }
    }

    pub(crate) async fn remove(&mut self) -> Result<()> {
        self.stop_lease();
        if self.removed {
            return Ok(());
        }
        let result = remove(
            &self.client,
            &self.owner,
            self.creation_started,
            self.creation_finished,
        )
        .await;
        if result.is_ok() {
            self.removed = true;
            if let Some(root) = self.root.take() {
                root.close().map_err(error)?;
            }
        }
        result.map_err(|why| {
            error(format!(
                "{why}; retained recovery ledger: {}",
                self.root
                    .as_ref()
                    .map(|r| r.path().display().to_string())
                    .unwrap_or_default()
            ))
        })
    }
}

impl Drop for OwnedContainer {
    fn drop(&mut self) {
        self.stop_lease();
        let Some(root) = self.root.take() else {
            return;
        };
        if self.removed || !self.creation_started {
            return;
        }
        let client = self.client.clone();
        let name = self.name.clone();
        let owner = self.owner.clone();
        let path = root.keep();
        let recovery_path = path.clone();
        let finished = self.creation_finished;
        // The lease already stopped. This bounded cleanup is a second line;
        // engine SIGKILL skips Drop but still cannot renew the guest lease.
        if let Err(error) = std::thread::Builder::new().name("acp-container-cleanup".into()).spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
            if matches!(runtime, Ok(runtime) if runtime.block_on(remove(&client, &owner, true, finished)).is_ok()) {
                let _ = std::fs::remove_dir_all(path);
            } else {
                tracing::error!(container = %name, ledger = %path.display(), "ACP namespace cleanup is unconfirmed");
            }
        }) {
            tracing::error!(%error, ledger = %recovery_path.display(), "ACP cleanup thread unavailable; lease has expired");
        }
    }
}

async fn owned_ids(client: &DockerEvaluator, owner: &str) -> Result<Vec<String>> {
    let listing = client
        .control(&[
            "container".into(),
            "ls".into(),
            "--all".into(),
            "--no-trunc".into(),
            "--filter".into(),
            format!("label=com.kranz.acp-owner={owner}"),
            "--format".into(),
            "{{.ID}}".into(),
        ])
        .await
        .map_err(error)?;
    if listing.code != Some(0) {
        return Err(error("cannot inspect owned namespaces"));
    }
    let text = std::str::from_utf8(&listing.stdout).map_err(error)?;
    let ids: Vec<String> = text.split_whitespace().map(str::to_string).collect();
    if ids.len() > 1
        || ids
            .iter()
            .any(|id| id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(error("ambiguous owned namespace inventory"));
    }
    Ok(ids)
}

async fn remove(
    client: &DockerEvaluator,
    owner: &str,
    started: bool,
    finished: bool,
) -> Result<()> {
    if !started {
        return Ok(());
    }
    // An immutable random owner label prevents create/name collisions from
    // deleting someone else's container. Delete the inspected ID, never a name
    // that could be rebound between inspection and removal.
    let ids = owned_ids(client, owner).await?;
    if ids.is_empty() && !finished {
        return Err(error(
            "container creation was interrupted; daemon outcome remains uncertain",
        ));
    }
    for id in ids {
        let _ = client
            .control(&["rm".into(), "--force".into(), id])
            .await
            .map_err(error)?;
    }
    // `--rm` can start asynchronous deletion before our explicit rm arrives.
    // A successful rm request or an "already in progress" response is not an
    // absence receipt. Poll within the existing five-second control budget.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if owned_ids(client, owner).await?.is_empty() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| error("daemon removal could not be confirmed within deadline"))?
}

fn pinned_image(image: &str) -> bool {
    let digest = image
        .strip_prefix("sha256:")
        .or_else(|| image.rsplit_once("@sha256:").map(|(_, d)| d));
    digest.is_some_and(|d| {
        d.len() == 64
            && d.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn mount_path(path: &Path) -> Result<&str> {
    path.to_str()
        .filter(|s| !s.contains([',', '\n', '\r']))
        .ok_or_else(|| error("invalid host mount path"))
}

fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(error)?;
    file.write_all(bytes).map_err(error)
}

fn trusted_docker(inputs: &SandboxInputs) -> Result<PathBuf> {
    for root in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|p| p.is_absolute())
    {
        if let Ok(candidate) = root.join("docker").canonicalize() {
            if !candidate.is_file() {
                continue;
            }
            if std::iter::once(&inputs.session_cwd)
                .chain(std::iter::once(&inputs.tmpdir))
                .chain(&inputs.extra_write)
                .any(|root| candidate.starts_with(crate::sandbox::absolutize(root)))
            {
                return Err(error("Docker control executable is worker-writable"));
            }
            return Ok(candidate);
        }
    }
    Err(error("trusted Docker executable unavailable"))
}

#[cfg(test)]
mod tests;
