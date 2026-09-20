//! Own the daemon-side mount helper, separately from the Docker client process.
//! Recovery intent precedes creation. A guest PID-1 watchdog bounds execution
//! after engine death without reading the filesystem that is being tested.

use super::{container_host_path, unshared_path_reason, ContainerRuntime, MountProof};
use crate::gate_evaluation::subprocess::DockerEvaluator;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const WALL: Duration = Duration::from_secs(90);
const OWNER_LABEL: &str = "com.kranz.mount-proof-owner";
// The parent shell only waits for children. Filesystem I/O happens in the
// payload child, so even a blocked mount cannot block delivery of the trap.
// Exiting PID 1 kills the entire private Linux PID namespace.
const WATCHDOG: &str = r#"
[ "$$" = 1 ] || exit 125
trap 'exit 124' USR1
parent=$$
( sleep "$1"; kill -USR1 "$parent" ) &
guard=$!
/bin/sh -c "$2" &
payload=$!
wait "$payload"
result=$?
kill "$guard" 2>/dev/null || :
exit "$result"
"#;

pub(super) fn prove(host_dir: &Path, image: &str) -> MountProof {
    // Declared mission/scratch parents may not exist until preflight. Preserve
    // the old probe's create_dir_all behavior before canonicalizing the root.
    if let Err(error) = std::fs::create_dir_all(host_dir) {
        return MountProof::Failed(format!(
            "could not create mount proof root {}: {error}",
            host_dir.display()
        ));
    }
    // Resolution also runs inside Tokio and spawn_blocking. Keep the bounded
    // control runtime on its own thread; dropping a caller does not detach an
    // unowned daemon command or cancel its cleanup.
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?;
                runtime.block_on(async {
                    let client = DockerEvaluator::new(&docker_path(host_dir)?)?;
                    let mut helper = Helper::new(client, host_dir)?;
                    Ok(helper.prove(image, WALL, 85, &AtomicBool::new(false)).await)
                })
            })
            .join()
            .unwrap_or_else(|_| Err("mount proof control thread panicked".into()))
            .unwrap_or_else(MountProof::Failed)
    })
}

fn docker_path(host_dir: &Path) -> Result<PathBuf, String> {
    let host_dir = host_dir.canonicalize().map_err(|e| e.to_string())?;
    for root in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|p| p.is_absolute())
    {
        if let Ok(path) = root.join("docker").canonicalize() {
            if path.is_file() {
                if path.starts_with(&host_dir) {
                    return Err(
                        "mount proof Docker executable is inside the tested write root".into(),
                    );
                }
                return Ok(path);
            }
        }
    }
    Err("trusted installed Docker executable unavailable".into())
}

struct Helper {
    client: DockerEvaluator,
    owner: String,
    name: String,
    root: PathBuf,
    probe: Option<tempfile::TempDir>,
    ledger: Option<tempfile::TempDir>,
    creation_started: bool,
    creation_settled: bool,
    id: Option<String>,
}

impl Helper {
    fn new(client: DockerEvaluator, root: &Path) -> Result<Self, String> {
        let probe = tempfile::Builder::new()
            .prefix("kranz-mount-proof-")
            .tempdir_in(root)
            .map_err(|e| e.to_string())?;
        // This directory is never mounted. A broken tested mount must not
        // make the daemon ownership record disappear or become guest-writable.
        let ledger = tempfile::Builder::new()
            .prefix("kranz-mount-owner-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let owner = uuid::Uuid::new_v4().simple().to_string();
        Ok(Self {
            client,
            name: format!("kranz-mount-{owner}"),
            owner,
            root: root.to_path_buf(),
            probe: Some(probe),
            ledger: Some(ledger),
            creation_started: false,
            creation_settled: false,
            id: None,
        })
    }

    async fn prove(
        &mut self,
        image: &str,
        wall: Duration,
        guest_seconds: u32,
        cancelled: &AtomicBool,
    ) -> MountProof {
        let host = uuid::Uuid::new_v4().simple().to_string();
        let guest = uuid::Uuid::new_v4().simple().to_string();
        let result = self
            .round_trip(image, wall, guest_seconds, cancelled, &host, &guest)
            .await;
        let cleanup = self.cleanup().await;
        if let Err(why) = cleanup {
            let ledger = self.retain();
            return MountProof::Failed(format!(
                "{}; mount helper cleanup UNCONFIRMED: {why}; retained recovery ledger: {}",
                result
                    .err()
                    .unwrap_or_else(|| "sentinel round trip succeeded".into()),
                ledger.display()
            ));
        }
        self.creation_started = false;
        match result {
            Ok(()) => MountProof::Proven,
            Err(why) => MountProof::Failed(format!("{why}; mount helper absence confirmed")),
        }
    }

    async fn round_trip(
        &mut self,
        image: &str,
        wall: Duration,
        guest_seconds: u32,
        cancelled: &AtomicBool,
        host: &str,
        guest: &str,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + wall;
        let remaining = || {
            deadline
                .checked_duration_since(tokio::time::Instant::now())
                .filter(|duration| !duration.is_zero())
                .ok_or_else(|| "mount proof deadline expired before control spawn".to_string())
        };
        let inspect = ["image".into(), "inspect".into(), image.into()];
        let mut inspected = self
            .client
            .bounded_control(
                &inspect,
                remaining()?.min(Duration::from_secs(5)),
                cancelled,
            )
            .await?;
        if inspected.code != Some(0) {
            if image.starts_with("sha256:") || image.contains("@sha256:") {
                return Err("pinned mount proof image must already be installed".into());
            }
            // Preserve the existing default-image pull behavior, within the
            // same proof deadline and frozen host client environment.
            let pulled = self
                .client
                .bounded_control(&["pull".into(), image.into()], remaining()?, cancelled)
                .await?;
            if pulled.code != Some(0) {
                return Err("mount proof image is unavailable".into());
            }
            inspected = self
                .client
                .bounded_control(
                    &inspect,
                    remaining()?.min(Duration::from_secs(5)),
                    cancelled,
                )
                .await?;
            if inspected.code != Some(0) {
                return Err("cannot inspect the mount proof image after pull".into());
            }
        }
        let metadata: serde_json::Value =
            crate::strict_json::parse(&inspected.stdout).map_err(|e| e.to_string())?;
        let metadata = metadata
            .get(0)
            .ok_or("missing mount proof image metadata")?;
        let image_id = metadata["Id"]
            .as_str()
            .ok_or("missing mount proof image ID")?;
        if !image_id.strip_prefix("sha256:").is_some_and(full_id)
            || metadata["Os"] != "linux"
            || metadata["Config"]["Volumes"]
                .as_object()
                .is_some_and(|v| !v.is_empty())
        {
            return Err("mount proof requires a Linux image without anonymous volumes".into());
        }
        let probe = self.probe.as_ref().expect("owned probe").path();
        std::fs::write(probe.join("host.txt"), host).map_err(|e| e.to_string())?;
        let payload = super::mount_proof_script(guest);
        // Build a create/start pair so successful creation has an immutable ID
        // before attach can time out. No image ENTRYPOINT or network is used.
        let mut args = vec![
            "create".into(),
            "--rm".into(),
            "--pull=never".into(),
            "--name".into(),
            self.name.clone(),
            "--label".into(),
            format!("{OWNER_LABEL}={}", self.owner),
            "--user".into(),
            crate::container_egress::mount_owner(probe)
                .ok_or("cannot identify mount probe owner")?,
            "--no-healthcheck".into(),
            "--workdir=/".into(),
            "--pids-limit=32".into(),
            "--network=none".into(),
            "--read-only".into(),
            "--cap-drop=ALL".into(),
            "--security-opt=no-new-privileges".into(),
            "--entrypoint=/bin/sh".into(),
            "--env=PATH=/usr/bin:/bin".into(),
            "--env=ENV=".into(),
            "--env=BASH_ENV=".into(),
            "-v".into(),
            format!(
                "{}:{}",
                container_host_path(probe),
                super::MOUNT_PROOF_GUEST_DIR
            ),
        ];
        // Docker client configuration can inject proxy credentials even when
        // the host process environment is cleared. Override those defaults.
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
            args.extend(["--env".into(), format!("{key}=")]);
        }
        args.extend([
            image_id.into(),
            "-c".into(),
            WATCHDOG.into(),
            "mount-proof".into(),
            guest_seconds.to_string(),
            payload,
        ]);
        let evidence = serde_json::json!({
            "schema":1, "ownerLabel":OWNER_LABEL, "owner":self.owner, "name":self.name,
            "hostPid":std::process::id(), "imageId":image_id,
            "probe":probe, "creation":"intent-recorded-before-spawn",
            "cleanupConfirmed":false, "guestDeadlineSeconds":guest_seconds,
            "recovery":"Using the same Docker endpoint, list all containers with this owner label, inspect their full IDs and label, remove only those IDs, then verify an empty successful inventory. Interrupted create can finish late: an empty early inventory is not confirmation. Never delete by name."
        });
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(
                self.ledger
                    .as_ref()
                    .expect("owned ledger")
                    .path()
                    .join("owner.json"),
            )
            .map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(&evidence).map_err(|e| e.to_string())?)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        let create_wall = remaining()?;
        self.creation_started = true;
        let created = self
            .client
            .bounded_control(&args, create_wall, cancelled)
            .await?;
        if created.code != Some(0) {
            // A failed create may have partially succeeded. Inventory must
            // find our identity before this can be called a settled outcome.
            return Err(format!(
                "mount helper creation failed: {}",
                String::from_utf8_lossy(&created.stderr).trim()
            ));
        }
        let id = std::str::from_utf8(&created.stdout)
            .map_err(|e| e.to_string())?
            .trim();
        if !full_id(id) {
            return Err("mount helper creation returned an invalid ID".into());
        }
        self.id = Some(id.into());
        self.creation_settled = true;
        let output = self
            .client
            .bounded_control(
                &["start".into(), "--attach".into(), id.into()],
                remaining()?,
                cancelled,
            )
            .await?;
        if output.code != Some(0) {
            return Err(format!(
                "mount helper exited {:?}: {}",
                output.code,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        if String::from_utf8_lossy(&output.stdout).trim() != host {
            return Err(unshared_path_reason(
                ContainerRuntime::Docker,
                &self.root,
                "the host sentinel was not visible inside the container",
            ));
        }
        if std::fs::read_to_string(probe.join("guest.txt"))
            .ok()
            .as_deref()
            .map(str::trim)
            != Some(guest)
        {
            return Err(unshared_path_reason(
                ContainerRuntime::Docker,
                &self.root,
                "the container's write did not reach the host",
            ));
        }
        Ok(())
    }

    async fn cleanup(&self) -> Result<(), String> {
        if !self.creation_started {
            return Ok(());
        }
        tokio::time::timeout(Duration::from_secs(15), async {
            let ids = owned_ids(&self.client, &self.owner).await?;
            if ids.is_empty() && !self.creation_settled {
                return Err(
                    "creation was interrupted or failed; late daemon creation cannot be excluded"
                        .into(),
                );
            }
            for id in ids {
                if self.id.as_ref().is_some_and(|expected| expected != &id) {
                    return Err("mount helper ID differs from creation receipt".into());
                }
                // The daemon inventory supplies full IDs and immutable labels.
                // A same-name replacement never becomes a deletion target.
                let _ = self
                    .client
                    .control(&["rm".into(), "--force".into(), id])
                    .await?;
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if owned_ids(&self.client, &self.owner).await?.is_empty() {
                        return Ok(());
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .map_err(|_| "mount helper absence deadline expired".to_string())?
        })
        .await
        .map_err(|_| "mount helper cleanup deadline expired".to_string())?
    }

    fn retain(&mut self) -> PathBuf {
        if let Some(probe) = self.probe.take() {
            let _ = probe.keep();
        }
        self.ledger.take().expect("owned recovery ledger").keep()
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        if !self.creation_started || self.ledger.is_none() {
            return;
        }
        let mut cleanup = Self {
            client: self.client.clone(),
            owner: self.owner.clone(),
            name: self.name.clone(),
            root: self.root.clone(),
            probe: self.probe.take(),
            ledger: self.ledger.take(),
            creation_started: true,
            creation_settled: self.creation_settled,
            id: self.id.clone(),
        };
        // Keep evidence even if thread creation fails. The guest watchdog is
        // independent of this best-effort host cleanup and of engine survival.
        let ledger = cleanup.ledger.take().expect("owned ledger").keep();
        let probe = cleanup.probe.take().expect("owned probe").keep();
        cleanup.creation_started = false;
        let recovery = ledger.clone();
        if let Err(error) = std::thread::Builder::new().name("mount-proof-cleanup".into()).spawn(move || {
            cleanup.creation_started = true;
            let result = tokio::runtime::Builder::new_current_thread().enable_all().build()
                .map_err(|e| e.to_string()).and_then(|runtime| runtime.block_on(cleanup.cleanup()));
            if result.is_ok() {
                let _ = std::fs::remove_dir_all(probe);
                let _ = std::fs::remove_dir_all(ledger);
            } else {
                tracing::error!(?result, ledger = %ledger.display(), "mount helper cleanup unconfirmed after cancellation");
            }
        }) {
            tracing::error!(%error, ledger = %recovery.display(), "mount helper cleanup thread unavailable");
        }
    }
}

fn full_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

async fn owned_ids(client: &DockerEvaluator, owner: &str) -> Result<Vec<String>, String> {
    let out = client
        .control(&[
            "container".into(),
            "ls".into(),
            "--all".into(),
            "--no-trunc".into(),
            "--filter".into(),
            format!("label={OWNER_LABEL}={owner}"),
            "--format".into(),
            "{{.ID}}".into(),
        ])
        .await?;
    if out.code != Some(0) {
        return Err("mount helper inventory failed".into());
    }
    let ids: Vec<String> = std::str::from_utf8(&out.stdout)
        .map_err(|e| e.to_string())?
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if ids.len() > 1 || ids.iter().any(|id| !full_id(id)) {
        return Err("ambiguous mount helper inventory".into());
    }
    Ok(ids)
}

#[cfg(test)]
mod tests;
