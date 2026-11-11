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
    prove_with_client(host_dir, image, None)
}

fn ensure_dir_mode_0700(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(path)
}

fn lexical_absolutize(path: &Path) -> PathBuf {
    use std::path::Component;
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

pub(super) fn prove_with_client(
    host_dir: &Path,
    image: &str,
    client: Option<DockerEvaluator>,
) -> MountProof {
    if let Err(reason) = super::validate_container_mount_path(host_dir) {
        return MountProof::Failed(reason);
    }
    // Declared mission/scratch parents may not exist until preflight. Create any
    // missing directory with mode 0700 rather than inheriting process umask.
    if let Err(error) = ensure_dir_mode_0700(host_dir) {
        return MountProof::Failed(format!(
            "could not create mount proof root {}: {error}",
            host_dir.display()
        ));
    }
    if let Err(reason) = super::validate_container_mount_path(host_dir) {
        return MountProof::Failed(reason);
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
                    let client = match client {
                        Some(client) => client,
                        None => DockerEvaluator::new(&docker_path(host_dir)?)?,
                    };
                    let scratch_base = super::scratch_root_base();
                    let _ = reap_dead_owner_ledgers(&scratch_base, &client, WALL).await;
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
    docker_path_from_env(host_dir, std::env::var_os("PATH").as_deref())
}

fn docker_path_from_env(
    host_dir: &Path,
    path_var: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let host_lex = lexical_absolutize(host_dir);
    let host_canon = host_dir.canonicalize().map_err(|e| e.to_string())?;
    for root in std::env::split_paths(&path_var.unwrap_or_default()).filter(|p| p.is_absolute()) {
        let root_lex = lexical_absolutize(&root);
        let entry = root.join("docker");
        // Check `root` (absolutized) against `host_dir` BEFORE `canonicalize()`
        // so a symlink inside the tested write root pointing outside cannot
        // hijack control-plane binary selection. If `host_dir` is an ancestor
        // such as `$HOME` and a benign PATH dir (`~/.cargo/bin`) contains no
        // `docker` entry at all, skip without traversing `canonicalize()`.
        if root_lex.starts_with(&host_lex) || root_lex.starts_with(&host_canon) {
            match std::fs::symlink_metadata(&entry) {
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                _ => {
                    return Err(
                        "mount proof Docker executable is inside the tested write root".into(),
                    );
                }
            }
        }
        if let Ok(candidate) = entry.canonicalize() {
            if candidate.starts_with(&host_canon) || candidate.starts_with(&host_lex) {
                return Err("mount proof Docker executable is inside the tested write root".into());
            }
            if let Ok(meta) = std::fs::metadata(&candidate) {
                if meta.is_file() && (meta.permissions().mode() & 0o111) != 0 {
                    return Ok(candidate);
                }
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
        super::validate_container_mount_path(root)?;
        let probe = tempfile::Builder::new()
            .prefix("kranz-mount-proof-")
            .tempdir_in(root)
            .map_err(|e| e.to_string())?;
        // Store recovery ledgers under the private container scratch base; this
        // directory is never mounted into the helper or worker container.
        let ledger_base = super::scratch_root_base();
        ensure_dir_mode_0700(&ledger_base).map_err(|e| {
            format!(
                "could not create private mount ledger base {}: {e}",
                ledger_base.display()
            )
        })?;
        let ledger = tempfile::Builder::new()
            .prefix("kranz-mount-ledger-")
            .tempdir_in(&ledger_base)
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
        super::validate_container_mount_path(probe)?;
        super::validate_container_mount_path(Path::new(super::MOUNT_PROOF_GUEST_DIR))?;
        // The probe sits inside a declared mount root, which a live worker of
        // another session may be able to write. `create_new` + `O_NOFOLLOW`
        // refuses a pre-planted `host.txt` (a symlink would otherwise send
        // these bytes, and a truncation, to its target).
        {
            use std::io::Write as _;
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
            }
            options
                .open(probe.join("host.txt"))
                .and_then(|mut file| file.write_all(host.as_ref()))
                .map_err(|e| format!("mount proof could not create its probe file: {e}"))?;
        }
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
        let owner_pid = std::process::id();
        let owner_identity = crate::event_log::process_identity_token(owner_pid as i32);
        let evidence = serde_json::json!({
            "schema":1, "ownerLabel":OWNER_LABEL, "owner":self.owner, "name":self.name,
            "hostPid":owner_pid, "ownerPid":owner_pid, "ownerIdentity":owner_identity,
            "docker":self.client.program(), "endpoint":self.client.pinned_endpoint(),
            "imageId":image_id,
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
        if !self.creation_started {
            if let Some(probe) = self.probe.take() {
                let _ = super::remove_owned_tree_nofollow(&probe.keep());
            }
            if let Some(ledger) = self.ledger.take() {
                let _ = super::remove_owned_tree_nofollow(&ledger.keep());
            }
            return;
        }
        if self.ledger.is_none() {
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
        if let Err(error) = std::thread::Builder::new()
            .name("mount-proof-cleanup".into())
            .spawn(move || {
                cleanup.creation_started = true;
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())
                    .and_then(|runtime| runtime.block_on(cleanup.cleanup()));
                if result.is_ok() {
                    let _ = super::remove_owned_tree_nofollow(&probe);
                    let _ = super::remove_owned_tree_nofollow(&ledger);
                } else {
                    tracing::error!(
                        ?result,
                        ledger = %ledger.display(),
                        "mount helper cleanup unconfirmed after cancellation"
                    );
                }
            })
        {
            tracing::error!(%error, ledger = %recovery.display(), "mount helper cleanup thread unavailable");
        }
    }
}

fn owner_pid_is_provably_dead(owner_pid: u32, owner_identity: Option<&str>) -> bool {
    let Ok(pid_i32) = i32::try_from(owner_pid) else {
        return false;
    };
    if pid_i32 <= 0 {
        return false;
    }
    if let Some(expected) = owner_identity.filter(|s| !s.trim().is_empty()) {
        match crate::event_log::process_identity_token(pid_i32) {
            Some(live) if live == expected => return false,
            Some(_) => return true,
            None => {}
        }
    }
    let rc = unsafe { libc::kill(pid_i32, 0) };
    rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// Owned by this user and not writable by group or other.
fn owned_private(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o022 == 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// A `unix://` endpoint whose socket exists and belongs to this user.
fn owned_local_socket(endpoint: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        endpoint
            .strip_prefix("unix://")
            .filter(|path| path.starts_with('/'))
            .and_then(|path| std::fs::metadata(path).ok())
            .is_some_and(|meta| {
                meta.file_type().is_socket() && meta.uid() == unsafe { libc::geteuid() }
            })
    }
    #[cfg(not(unix))]
    {
        let _ = endpoint;
        false
    }
}

pub(super) async fn reap_dead_owner_ledgers(
    scratch_base: &Path,
    client: &DockerEvaluator,
    min_age: Duration,
) -> usize {
    use std::io::Read;
    let Ok(entries) = std::fs::read_dir(scratch_base) else {
        return 0;
    };
    let expected_docker = client
        .program()
        .canonicalize()
        .unwrap_or_else(|_| client.program().to_path_buf());
    let mut reaped = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.starts_with("kranz-mount-ledger-")
            && !name_str.starts_with("kranz-mount-owner-")
        {
            continue;
        }
        let ledger_dir = entry.path();
        let Ok(dir_meta) = std::fs::symlink_metadata(&ledger_dir) else {
            continue;
        };
        if !dir_meta.file_type().is_dir() {
            continue;
        }
        // The scratch base may be a shared temp directory. A ledger is only
        // ours if this user created it and nobody else can write into it;
        // otherwise another local user could name a Docker endpoint and a
        // tree for us to act on (2026-11-10 audit CTR-3).
        if !owned_private(&dir_meta) {
            continue;
        }
        let record_path = ledger_dir.join("owner.json");
        let Ok(file) = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&record_path)
        else {
            continue;
        };
        let Ok(file_meta) = file.metadata() else {
            continue;
        };
        if !file_meta.is_file() || file_meta.len() > 64 * 1024 || !owned_private(&file_meta) {
            continue;
        }
        if !min_age.is_zero()
            && !file_meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age >= min_age)
        {
            continue;
        }
        let mut bytes = Vec::new();
        if file.take(64 * 1024 + 1).read_to_end(&mut bytes).is_err() || bytes.len() > 64 * 1024 {
            continue;
        }
        let Ok(record) = crate::strict_json::parse(&bytes) else {
            continue;
        };
        let Some(owner) = record.get("owner").and_then(|v| v.as_str()).filter(|s| {
            s.len() == 32
                && s.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        }) else {
            continue;
        };
        let owner_pid = record
            .get("ownerPid")
            .or_else(|| record.get("hostPid"))
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok())
            .filter(|pid| *pid > 0);
        let Some(owner_pid) = owner_pid else {
            continue;
        };
        let owner_identity = record.get("ownerIdentity").and_then(|v| v.as_str());
        if !owner_pid_is_provably_dead(owner_pid, owner_identity) {
            continue;
        }
        if let Some(recorded_docker) = record
            .get("docker")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
        {
            let recorded_canon = recorded_docker
                .canonicalize()
                .unwrap_or_else(|_| recorded_docker.clone());
            if recorded_canon != expected_docker && recorded_docker != client.program() {
                continue;
            }
        }
        let mut ledger_client = client.clone();
        match record.get("endpoint") {
            None | Some(serde_json::Value::Null) => {}
            // Only a local socket this user owns: never a TCP or SSH
            // endpoint taken from a file.
            Some(serde_json::Value::String(ep)) if owned_local_socket(ep) => {
                ledger_client.pin_local_endpoint(ep);
            }
            Some(_) => continue,
        }
        let Ok(ids) = owned_ids(&ledger_client, owner).await else {
            continue;
        };
        let mut rm_ok = true;
        for id in ids {
            if ledger_client
                .control(&["rm".into(), "--force".into(), id])
                .await
                .is_err()
            {
                rm_ok = false;
                break;
            }
        }
        if !rm_ok {
            continue;
        }
        let Ok(remaining) = owned_ids(&ledger_client, owner).await else {
            continue;
        };
        if !remaining.is_empty() {
            continue;
        }
        if let Some(probe_str) = record.get("probe").and_then(|v| v.as_str()) {
            let probe_path = Path::new(probe_str);
            if probe_path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("kranz-mount-proof-"))
                && std::fs::symlink_metadata(probe_path)
                    .is_ok_and(|meta| meta.file_type().is_dir() && owned_private(&meta))
            {
                let _ = super::remove_owned_tree_nofollow(probe_path);
            }
        }
        if super::remove_owned_tree_nofollow(&ledger_dir).is_ok() {
            reaped += 1;
        }
    }
    reaped
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

#[cfg(test)]
mod containment_remediation_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn mount_proof_creates_missing_host_dir_mode_0700_and_refuses_colon_paths() {
        let dir = tempfile::tempdir().unwrap();
        let colon_dir = dir.path().join("bad:mount");
        let colon_proof = prove_with_client(&colon_dir, "alpine:3", None);
        assert!(
            matches!(colon_proof, MountProof::Failed(ref why) if why.contains("forbidden delimiter")),
            "{colon_proof:?}"
        );
        assert!(
            !colon_dir.exists(),
            "colon path must be refused before creating host_dir"
        );

        let fake_bin = dir.path().join("fake-docker");
        std::fs::write(&fake_bin, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let client = DockerEvaluator::new(&fake_bin).unwrap();
        let missing = dir.path().join("missing-parent/missing-leaf");
        let _ = prove_with_client(&missing, "alpine@sha256:0000", Some(client));
        assert!(missing.is_dir());
        assert_eq!(
            std::fs::symlink_metadata(&missing)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::symlink_metadata(missing.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn mount_proof_docker_path_checks_root_before_canonicalize_and_skips_non_exec() {
        let dir = tempfile::tempdir().unwrap();
        let write_root = dir.path().join("worktree");
        let write_bin = write_root.join("bin");
        let trusted_bin = dir.path().join("trusted-bin");
        let non_exec_bin = dir.path().join("non-exec-bin");
        std::fs::create_dir_all(&write_bin).unwrap();
        std::fs::create_dir_all(&trusted_bin).unwrap();
        std::fs::create_dir_all(&non_exec_bin).unwrap();

        let real_docker = trusted_bin.join("docker");
        std::fs::write(&real_docker, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&real_docker, std::fs::Permissions::from_mode(0o755)).unwrap();

        // Non-executable candidate earlier on PATH must be skipped.
        let non_exec_docker = non_exec_bin.join("docker");
        std::fs::write(&non_exec_docker, "not executable").unwrap();
        std::fs::set_permissions(&non_exec_docker, std::fs::Permissions::from_mode(0o644)).unwrap();

        let path_ok = std::env::join_paths([&non_exec_bin, &trusted_bin]).unwrap();
        assert_eq!(
            docker_path_from_env(&write_root, Some(&path_ok)).unwrap(),
            real_docker.canonicalize().unwrap()
        );

        // Symlink inside `write_root` pointing to `real_docker` outside `write_root`
        // MUST be rejected BEFORE `canonicalize()`!
        std::os::unix::fs::symlink(&real_docker, write_bin.join("docker")).unwrap();
        let path_poisoned = std::env::join_paths([&write_bin, &trusted_bin]).unwrap();
        let err = docker_path_from_env(&write_root, Some(&path_poisoned)).unwrap_err();
        assert!(err.contains("inside the tested write root"), "{err}");
    }

    #[tokio::test]
    async fn mount_proof_reaps_dead_owner_ledger_and_locked_probe_dir() {
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("scratch");
        let host_root = dir.path().join("host");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::create_dir_all(&host_root).unwrap();

        let id = "d".repeat(64);
        let removed_marker = dir.path().join("reaped-container");
        let fake_bin = dir.path().join("fake-docker");
        std::fs::write(
            &fake_bin,
            format!(
                "#!/bin/sh\nif [ \"$1\" = container ] && [ \"$2\" = ls ]; then\n  [ -e '{}' ] || echo '{id}'\n  exit 0\nfi\nif [ \"$1\" = rm ] && [ \"$2\" = --force ] && [ \"$3\" = '{id}' ]; then\n  touch '{}'\n  exit 0\nfi\nexit 1\n",
                removed_marker.display(),
                removed_marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let client = DockerEvaluator::new(&fake_bin).unwrap();

        // Spawn a short-lived child to get a provably dead PID + mismatched/dead identity.
        let mut child = std::process::Command::new("/bin/sh")
            .env_clear()
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let dead_pid = child.id();
        child.wait().unwrap();

        let probe_dir = tempfile::Builder::new()
            .prefix("kranz-mount-proof-")
            .tempdir_in(&host_root)
            .unwrap()
            .keep();
        let locked_sub = probe_dir.join("guest-locked");
        std::fs::create_dir_all(&locked_sub).unwrap();
        std::fs::write(locked_sub.join("data"), "x").unwrap();
        std::fs::set_permissions(&locked_sub, std::fs::Permissions::from_mode(0o000)).unwrap();

        let dead_ledger = tempfile::Builder::new()
            .prefix("kranz-mount-ledger-")
            .tempdir_in(&scratch)
            .unwrap()
            .keep();
        let owner = "0123456789abcdef0123456789abcdef";
        std::fs::write(
            dead_ledger.join("owner.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "ownerLabel": OWNER_LABEL,
                "owner": owner,
                "ownerPid": dead_pid,
                "ownerIdentity": "dead-identity-token-never-matches",
                "docker": client.program(),
                "probe": probe_dir,
            }))
            .unwrap(),
        )
        .unwrap();

        // Live current-process ledger must NOT be reaped.
        let live_ledger = tempfile::Builder::new()
            .prefix("kranz-mount-ledger-")
            .tempdir_in(&scratch)
            .unwrap()
            .keep();
        let live_pid = std::process::id();
        std::fs::write(
            live_ledger.join("owner.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema": 1,
                "ownerLabel": OWNER_LABEL,
                "owner": "fedcba9876543210fedcba9876543210",
                "ownerPid": live_pid,
                "ownerIdentity": crate::event_log::process_identity_token(live_pid as i32),
                "docker": client.program(),
            }))
            .unwrap(),
        )
        .unwrap();

        // 2026-11-10 audit CTR-3: a dead-owner ledger is acted on only if
        // this user owns it privately and it names no remote endpoint. Both
        // of these look reapable otherwise and must be left untouched.
        let untrusted = |label: &str, endpoint: Option<&str>, mode: u32| {
            let ledger = tempfile::Builder::new()
                .prefix("kranz-mount-ledger-")
                .tempdir_in(&scratch)
                .unwrap()
                .keep();
            let mut record = serde_json::json!({
                "schema": 1,
                "ownerLabel": OWNER_LABEL,
                "owner": label,
                "ownerPid": dead_pid,
                "ownerIdentity": "dead-identity-token-never-matches",
                "docker": client.program(),
            });
            if let Some(endpoint) = endpoint {
                record["endpoint"] = endpoint.into();
            }
            std::fs::write(
                ledger.join("owner.json"),
                serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
            std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(mode)).unwrap();
            ledger
        };
        let shared_ledger = untrusted("11111111111111111111111111111111", None, 0o777);
        let remote_ledger = untrusted(
            "22222222222222222222222222222222",
            Some("tcp://203.0.113.7:2375"),
            0o700,
        );

        let reaped = reap_dead_owner_ledgers(&scratch, &client, Duration::ZERO).await;
        assert_eq!(reaped, 1);
        assert!(removed_marker.exists());
        assert!(!dead_ledger.exists());
        assert!(!probe_dir.exists());
        assert!(live_ledger.exists());
        assert!(
            shared_ledger.exists(),
            "a ledger others can write is not ours"
        );
        assert!(
            remote_ledger.exists(),
            "a remote endpoint is never taken from a ledger"
        );
        super::super::remove_owned_tree_nofollow(&shared_ledger).unwrap();
        super::super::remove_owned_tree_nofollow(&remote_ledger).unwrap();
        super::super::remove_owned_tree_nofollow(&live_ledger).unwrap();
    }
}
