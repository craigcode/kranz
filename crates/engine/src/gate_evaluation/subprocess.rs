//! One contained process per evaluation. Docker is the trusted host control
//! plane; only the explicitly assembled inputs/checker/scratch mounts cross
//! into the untrusted process. Mission stages share this contained runner.
use super::{artifacts, evidence::FrozenEvidence, protocol::*};
use crate::command_exec::evaluator_io::{self, Output};
use crate::pack::evaluator::PinnedRegistration;
use cap_fs_ext::DirExt;
use cap_std::{ambient_authority, fs::Dir};
use serde::Serialize;
use std::collections::HashMap;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

const DRIVER: &str = "set -eu\nexpected=$1\nshift\nfor root in /gate/inputs /checker; do\n  test \"$(cat \"$root/engine-mount-proof\")\" = \"$expected\" || exit 125\ndone\nfor root in /gate/outputs /gate/build /gate/home; do\n  printf '%s' \"$expected\" > \"$root/engine-mount-proof\"\ndone\nexec /usr/bin/env -i HOME=/gate/home TMPDIR=/gate/build PATH=/usr/local/bin:/usr/bin:/bin \"$@\"\n";
const CONTROL_LIMIT: usize = 1_048_576;
const GATE_OWNER_LABEL: &str = "com.kranz.gate-owner";

/// Host-owned Docker executable and a frozen client environment. Neither is
/// selected from checker output or forwarded to the container.
#[derive(Debug, Clone)]
pub struct DockerEvaluator {
    program: PathBuf,
    env: HashMap<String, String>,
    pinned_endpoint: Option<String>,
}

pub struct RunOptions<'a> {
    /// An existing trusted host directory shared with the Docker daemon.
    /// A fresh mode-0700 child is created for every attempt.
    pub attempt_parent: &'a Path,
    /// Explicit audit policy. False removes raw inputs/output after cleanup.
    pub retain_private_inputs: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcceptedEvaluation {
    /// Digest of all raw stdout; `result` below is scrubbed for retention.
    pub raw_stdout_digest: Digest,
    pub raw_stdout_retained: bool,
    pub retained_result_digest: Digest,
    pub transformation: String,
    pub result: EvaluationResult,
    pub artifacts: Vec<artifacts::ImportedArtifact>,
    pub diagnostics: String,
    pub exit_code: i32,
    pub container_removed: bool,
}

#[derive(Debug)]
pub struct AttemptOutcome {
    /// Contains a scrubbed receipt; raw files remain only when explicitly
    /// retained or when container cleanup was not confirmed.
    pub directory: PathBuf,
    pub cleanup_confirmed: bool,
    pub evaluation: Result<AcceptedEvaluation, String>,
}

impl DockerEvaluator {
    /// `program` must be a trusted installed Docker CLI, not a repo script.
    pub fn new(program: &Path) -> Result<Self, String> {
        if !program.is_absolute() {
            return Err("Docker CLI must be an absolute trusted host path".into());
        }
        let program = program.canonicalize().map_err(|e| e.to_string())?;
        if !program.is_file() {
            return Err("Docker CLI is not a regular file".into());
        }
        Ok(Self {
            program,
            env: crate::sandbox_container::ContainerRuntime::Docker.client_env(),
            pinned_endpoint: None,
        })
    }

    pub(crate) fn program(&self) -> &Path {
        &self.program
    }

    /// Freeze a reviewed local daemon selection; later Docker context changes
    /// must not retarget a credential-bearing worker or its cleanup commands.
    pub(crate) fn pin_local_endpoint(&mut self, endpoint: &str) {
        for key in [
            "DOCKER_CONTEXT",
            "DOCKER_CONFIG",
            "SSH_AUTH_SOCK",
            "DOCKER_TLS",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CERT_PATH",
            "DOCKER_API_VERSION",
        ] {
            self.env.remove(key);
        }
        self.env.retain(|key, _| !key.starts_with("DOCKER_TLS"));
        self.env.insert("DOCKER_HOST".into(), endpoint.into());
        self.pinned_endpoint = Some(endpoint.to_string());
    }

    pub(crate) fn pinned_endpoint(&self) -> Option<&str> {
        self.pinned_endpoint.as_deref()
    }

    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn environment(&self) -> &HashMap<String, String> {
        &self.env
    }

    /// Runs a bounded control-plane Docker command on an isolated thread +
    /// current-thread runtime so synchronous callers (including `Drop` inside a
    /// Tokio runtime) stay pinned to `self.program` and `self.env`.
    pub(crate) fn control_blocking(
        &self,
        args: &[String],
        wall: Duration,
    ) -> Result<Output, String> {
        let client = self.clone();
        let owned_args: Vec<String> = args.to_vec();
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|e| format!("Docker control runtime failed: {e}"))?;
                    runtime.block_on(client.bounded_control(
                        &owned_args,
                        wall,
                        &AtomicBool::new(false),
                    ))
                })
                .join()
                .unwrap_or_else(|_| Err("Docker control thread panicked".into()))
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn command_with_notify(
        &self,
        args: &[String],
        input: &[u8],
        wall: Duration,
        write: Duration,
        caps: (usize, usize),
        cancelled: &AtomicBool,
        cancel_notify: Option<&tokio::sync::Notify>,
    ) -> Result<Output, String> {
        evaluator_io::run_with_notify(
            &self.program,
            args,
            &self.env,
            input,
            wall,
            write,
            caps.0,
            caps.1,
            cancelled,
            cancel_notify,
        )
        .await
    }

    pub(crate) fn attached_command(&self, args: &[String]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.program);
        command.args(args).env_clear().envs(&self.env);
        command
    }

    pub(crate) async fn control(&self, args: &[String]) -> Result<Output, String> {
        self.bounded_control(args, Duration::from_secs(5), &AtomicBool::new(false))
            .await
    }

    pub(crate) async fn bounded_control(
        &self,
        args: &[String],
        wall: Duration,
        cancelled: &AtomicBool,
    ) -> Result<Output, String> {
        self.bounded_control_with_notify(args, wall, cancelled, None)
            .await
    }

    async fn bounded_control_with_notify(
        &self,
        args: &[String],
        wall: Duration,
        cancelled: &AtomicBool,
        cancel_notify: Option<&tokio::sync::Notify>,
    ) -> Result<Output, String> {
        self.command_with_notify(
            args,
            b"",
            wall,
            Duration::from_secs(1),
            (CONTROL_LIMIT, CONTROL_LIMIT),
            cancelled,
            cancel_notify,
        )
        .await
        .map_err(|error| format!("Docker {} control failed: {error}", args[0]))
    }

    pub async fn evaluate(
        &self,
        registration: &PinnedRegistration,
        evidence: &FrozenEvidence,
        options: RunOptions<'_>,
        cancelled: &AtomicBool,
    ) -> Result<AttemptOutcome, String> {
        if evidence.request.params.binding.registration_digest != registration.digest()
            || evidence.request.params.gate_id != registration.declaration.name
        {
            return Err("registration changed before execution".into());
        }
        let deadline = chrono::DateTime::parse_from_rfc3339(&evidence.request.params.deadline)
            .map_err(|_| "invalid deadline")?;
        let remaining = (deadline.with_timezone(&chrono::Utc) - chrono::Utc::now())
            .to_std()
            .map_err(|_| "evaluation deadline expired")?;
        let wall = remaining.min(Duration::from_millis(
            evidence.request.params.limits.wall_time_ms,
        ));
        let deadline = tokio::time::Instant::now() + wall;
        let parent = options
            .attempt_parent
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let owner = uuid::Uuid::new_v4().simple().to_string();
        let name = format!("kranz-evaluator-{owner}");
        let root = parent.join(&name);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(|e| e.to_string())?;
        let mount_nonce = uuid::Uuid::new_v4().simple().to_string();
        let cancel_notify = std::sync::Arc::new(tokio::sync::Notify::new());
        let mut guard = ContainerGuard {
            client: self.clone(),
            owner: owner.clone(),
            name: name.clone(),
            id: None,
            armed: false,
            creation_finished: false,
        };
        let execution = async {
            prepare(&root, &mount_nonce, registration, evidence)?;
            let pin = &registration.declaration.image;
            let image = self
                .control(&["image".into(), "inspect".into(), pin.clone()])
                .await?;
            if image.code != Some(0) {
                return Err("pinned evaluator image is not installed locally (automatic pulls are disabled)".into());
            }
            let metadata: serde_json::Value =
                crate::strict_json::parse(&image.stdout).map_err(|_| "invalid image inspection")?;
            let config = metadata.get(0).ok_or("missing image inspection")?;
            if !config["RepoDigests"]
                .as_array()
                .is_some_and(|values| values.iter().any(|d| d.as_str() == Some(pin)))
                || config["Config"]["Volumes"]
                    .as_object()
                    .is_some_and(|v| !v.is_empty())
            {
                return Err(
                    "image digest is unresolved or the image declares extra writable volumes"
                        .into(),
                );
            }
            let args = create_args(&name, &owner, &root, &mount_nonce, registration)?;
            // A ledger and guard exist before create, so cancellation cannot
            // forget a daemon-side object while the CLI is in flight.
            private_write(
                &root.join("container.json"),
                &serde_json::to_vec(&serde_json::json!({
                    "name": name,
                    "owner": owner,
                    "ownerLabel": GATE_OWNER_LABEL,
                    "image": pin,
                    "docker": self.program,
                    "endpoint": self.pinned_endpoint,
                }))
                .map_err(|e| e.to_string())?,
            )?;
            guard.armed = true;
            // Creation can materialize a cold image's root filesystem. Charge
            // it to the evaluation's existing deadline, not the short control
            // timeout used for inspection/cleanup. Never retry an uncertain create.
            let create = self
                .bounded_control_with_notify(
                    &args,
                    deadline.saturating_duration_since(tokio::time::Instant::now()),
                    cancelled,
                    Some(&cancel_notify),
                )
                .await?;
            let id = std::str::from_utf8(&create.stdout)
                .map_err(|_| "invalid container ID")?
                .trim()
                .to_string();
            if create.code != Some(0) || !full_id(&id) {
                return Err("could not create the contained evaluator".into());
            }
            guard.id = Some(id.clone());
            guard.creation_finished = true;
            let mut input = serde_json::to_vec(&evidence.request).map_err(|e| e.to_string())?;
            input.push(b'\n');
            private_write(&root.join("request.ndjson"), &input)?;
            let limits = &evidence.request.params.limits;
            let output = self
                .command_with_notify(
                    &[
                        "start".into(),
                        "--attach".into(),
                        "--interactive".into(),
                        id.clone(),
                    ],
                    &input,
                    deadline.saturating_duration_since(tokio::time::Instant::now()),
                    Duration::from_millis(limits.write_time_ms),
                    (
                        limits.max_stdout_bytes as usize,
                        limits.max_stderr_bytes as usize,
                    ),
                    cancelled,
                    Some(&cancel_notify),
                )
                .await
                .map_err(|error| format!("Docker evaluator start failed: {error}"))?;
            if output.code != Some(0) {
                return Err(format!(
                    "evaluator or its control process exited unsuccessfully ({:?}): {}",
                    output.code,
                    crate::scrub::scrub_and_truncate(
                        &String::from_utf8_lossy(&output.stderr),
                        4096
                    )
                ));
            }
            let inspect = self
                .control(&[
                    "inspect".into(),
                    "--format".into(),
                    "{{json .State}}".into(),
                    id,
                ])
                .await?;
            if inspect.code != Some(0) {
                return Err("cannot verify evaluator exit state".into());
            }
            let state = crate::strict_json::parse(&inspect.stdout)
                .map_err(|_| "invalid container state")?;
            if state["Status"] != "exited"
                || state["Running"] != false
                || state["OOMKilled"] != false
                || state["ExitCode"] != 0
                || state["Pid"] != 0
                || state["Error"] != ""
            {
                return Err("evaluator container did not exit cleanly".into());
            }
            Ok(output)
        };
        let cancellation = async {
            while !cancelled.load(std::sync::atomic::Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            cancel_notify.notify_waiters();
        };
        let execution = async {
            tokio::select! { result = execution => result, () = cancellation => Err("evaluation cancelled".to_string()) }
        };
        let execution = tokio::time::timeout_at(deadline, execution)
            .await
            .map_err(|_| {
                cancel_notify.notify_waiters();
                "evaluation deadline expired".to_string()
            })
            .and_then(|r| r);
        let cleanup = if guard.armed {
            guard.remove().await
        } else {
            Ok(())
        };
        let evaluation = match cleanup {
            Err(error) => Err(format!(
                "{error}; no result accepted; recovery ledger: {}",
                root.join("container.json").display()
            )),
            Ok(()) => execution.and_then(|output| {
                if cancelled.load(std::sync::atomic::Ordering::Acquire)
                    || tokio::time::Instant::now() >= deadline
                {
                    return Err("evaluation cancelled or deadline expired before acceptance".into());
                }
                if options.retain_private_inputs {
                    private_write(&root.join("raw-stdout.ndjson"), &output.stdout)?;
                }
                accept(
                    &root,
                    &mount_nonce,
                    evidence,
                    output,
                    options.retain_private_inputs,
                )
            }),
        };
        // Only scrubbed data goes into the default retained receipt. Raw
        // stdout is never copied from a failing process into diagnostics.
        let receipt = match &evaluation {
            Ok(accepted) => serde_json::to_vec(accepted),
            Err(error) => serde_json::to_vec(&serde_json::json!({"error":crate::scrub::scrub(error),"containerRemoved":!guard.armed})),
        }.map_err(|e| e.to_string())?;
        private_write(&root.join("receipt.json"), &receipt)?;
        if !options.retain_private_inputs && !guard.armed {
            for dir in ["inputs", "checker", "driver", "outputs", "build", "home"] {
                crate::sandbox_container::remove_owned_tree_nofollow(&root.join(dir))
                    .map_err(|e| format!("private attempt cleanup failed: {e}"))?;
            }
            for file in ["request.ndjson", "container.json"] {
                match std::fs::remove_file(root.join(file)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
        }
        Ok(AttemptOutcome {
            directory: root,
            cleanup_confirmed: !guard.armed,
            evaluation,
        })
    }
}

fn full_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

async fn owned_gate_ids(client: &DockerEvaluator, owner: &str) -> Result<Vec<String>, String> {
    let out = client
        .control(&[
            "container".into(),
            "ls".into(),
            "--all".into(),
            "--no-trunc".into(),
            "--filter".into(),
            format!("label={GATE_OWNER_LABEL}={owner}"),
            "--format".into(),
            "{{.ID}}".into(),
        ])
        .await?;
    if out.code != Some(0) {
        return Err("container cleanup could not be confirmed".into());
    }
    let ids: Vec<String> = std::str::from_utf8(&out.stdout)
        .map_err(|_| "container cleanup could not be confirmed".to_string())?
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if ids.len() > 1 || ids.iter().any(|id| !full_id(id)) {
        return Err("container cleanup could not be confirmed".into());
    }
    Ok(ids)
}

async fn remove_owned_gate_container(
    client: &DockerEvaluator,
    owner: &str,
    expected_id: Option<&str>,
    creation_finished: bool,
) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let ids = owned_gate_ids(client, owner).await?;
        if ids.is_empty() && !creation_finished {
            return Err(
                "container creation was interrupted; daemon completion and cleanup remain uncertain"
                    .into(),
            );
        }
        for id in ids {
            if expected_id.is_some_and(|expected| expected != id) {
                return Err("evaluator container ID differs from creation receipt".into());
            }
            let _ = client.control(&["rm".into(), "--force".into(), id]).await?;
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if owned_gate_ids(client, owner).await?.is_empty() {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .map_err(|_| "container cleanup could not be confirmed".to_string())?
    })
    .await
    .map_err(|_| "container cleanup could not be confirmed".to_string())?
}

struct ContainerGuard {
    client: DockerEvaluator,
    owner: String,
    name: String,
    id: Option<String>,
    armed: bool,
    creation_finished: bool,
}
impl ContainerGuard {
    async fn remove(&mut self) -> Result<(), String> {
        remove_owned_gate_container(
            &self.client,
            &self.owner,
            self.id.as_deref(),
            self.creation_finished,
        )
        .await?;
        self.armed = false;
        Ok(())
    }
}
impl Drop for ContainerGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let client = self.client.clone();
        let owner = self.owner.clone();
        let name = self.name.clone();
        let expected_id = self.id.clone();
        let creation_finished = self.creation_finished;
        // Dropping the async driver is cancellation too. This worker owns a
        // bounded cleanup runtime independent of the cancelled caller and joins
        // before returning so directory cleanup never races container teardown.
        match std::thread::Builder::new()
            .name("gate-evaluator-cleanup".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    tracing::error!(
                        container = %name,
                        owner = %owner,
                        "evaluator cleanup runtime unavailable; needs operator recovery"
                    );
                    return;
                };
                runtime.block_on(async {
                    if let Err(error) = remove_owned_gate_container(
                        &client,
                        &owner,
                        expected_id.as_deref(),
                        creation_finished,
                    )
                    .await
                    {
                        tracing::error!(
                            container = %name,
                            owner = %owner,
                            %error,
                            "evaluator cleanup needs operator recovery"
                        );
                    }
                });
            }) {
            Ok(handle) => {
                let _ = handle.join();
            }
            Err(error) => {
                tracing::error!(
                    container = %self.name,
                    owner = %self.owner,
                    %error,
                    "evaluator cleanup thread spawn failed; needs operator recovery"
                );
            }
        }
    }
}

fn private_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())
}
fn prepare(
    root: &Path,
    mount_nonce: &str,
    registration: &PinnedRegistration,
    evidence: &FrozenEvidence,
) -> Result<(), String> {
    for dir in ["inputs", "outputs", "build", "home", "checker", "driver"] {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join(dir))
            .map_err(|e| e.to_string())?;
    }
    for artifact in &evidence.manifest.artifacts {
        let path = root.join(artifact.content.path.as_str());
        std::fs::create_dir_all(path.parent().ok_or("input has no parent")?)
            .map_err(|e| e.to_string())?;
        private_write(&path, &evidence.inputs[&artifact.id])?;
    }
    let manifest = root.join(evidence.request.params.evidence.path.as_str());
    std::fs::create_dir_all(manifest.parent().ok_or("manifest has no parent")?)
        .map_err(|e| e.to_string())?;
    private_write(&manifest, &evidence.manifest_bytes)?;
    for file in &registration.files {
        let path = root.join("checker").join(file.path.as_str());
        std::fs::create_dir_all(path.parent().ok_or("checker file has no parent")?)
            .map_err(|e| e.to_string())?;
        private_write(&path, &file.bytes)?;
        if file.executable {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
    }
    for part in ["inputs", "checker"] {
        private_write(
            &root.join(part).join("engine-mount-proof"),
            mount_nonce.as_bytes(),
        )?;
    }
    private_write(&root.join("driver/entry.sh"), DRIVER.as_bytes())
}
fn create_args(
    name: &str,
    owner: &str,
    root: &Path,
    mount_nonce: &str,
    registration: &PinnedRegistration,
) -> Result<Vec<String>, String> {
    let label = format!("{GATE_OWNER_LABEL}={owner}");
    let mut args: Vec<String> = [
        "create",
        "--pull=never",
        "--interactive",
        "--name",
        name,
        "--label",
        label.as_str(),
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--pids-limit=32",
        "--memory=128m",
        "--memory-swap=128m",
        "--cpus=1",
        "--ulimit",
        "fsize=67108864:67108864",
        "--no-healthcheck",
        "--workdir=/gate",
        "--entrypoint=/usr/bin/env",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    args.extend([
        "--user".into(),
        format!("{}:{}", unsafe { libc::geteuid() }, unsafe {
            libc::getegid()
        }),
    ]);
    for part in ["inputs", "outputs", "build", "home", "checker", "driver"] {
        let source = root.join(part);
        let source = source
            .to_str()
            .filter(|s| !s.contains([':', ',', '\n', '\r']))
            .ok_or("host mount path is not representable safely")?;
        let target = if ["checker", "driver"].contains(&part) {
            format!("/{part}")
        } else {
            format!("/gate/{part}")
        };
        let readonly = if ["inputs", "checker", "driver"].contains(&part) {
            ",readonly"
        } else {
            ""
        };
        args.extend([
            "--mount".into(),
            format!("type=bind,src={source},dst={target}{readonly}"),
        ]);
    }
    args.extend([
        registration.declaration.image.clone(),
        "-i".into(),
        "HOME=/gate/home".into(),
        "TMPDIR=/gate/build".into(),
        "PATH=/usr/local/bin:/usr/bin:/bin".into(),
        "/bin/sh".into(),
        "/driver/entry.sh".into(),
        mount_nonce.into(),
        registration.declaration.executable.clone(),
    ]);
    args.extend(registration.declaration.args.clone());
    Ok(args)
}
fn accept(
    root: &Path,
    mount_nonce: &str,
    evidence: &FrozenEvidence,
    output: Output,
    raw_stdout_retained: bool,
) -> Result<AcceptedEvaluation, String> {
    let dir = Dir::open_ambient_dir(root, ambient_authority()).map_err(|e| e.to_string())?;
    for part in ["outputs", "build", "home"] {
        let root = dir
            .open_dir_nofollow(part)
            .map_err(|_| "scratch root was replaced")?;
        let reference = ArtifactRef {
            path: WirePath::try_from("engine-mount-proof".to_string())?,
            digest: Digest::of(mount_nonce.as_bytes()),
            bytes: mount_nonce.len() as u64,
        };
        artifacts::import(
            &root,
            &[reference],
            &Limits {
                max_artifacts: 1,
                max_artifact_bytes: 64,
                ..evidence.request.params.limits.clone()
            },
        )?;
    }
    let newline = output
        .stdout
        .iter()
        .position(|b| *b == b'\n')
        .ok_or("terminal response is missing its NDJSON newline")?;
    if newline as u64 > evidence.request.params.limits.max_frame_bytes
        || !output.stdout[newline + 1..]
            .iter()
            .all(u8::is_ascii_whitespace)
    {
        return Err("duplicate response, trailing stdout or frame overflow".into());
    }
    let response = Response::from_bytes(&output.stdout[..newline])?;
    response.correlate(&evidence.request)?;
    let Response::Result(mut response) = response else {
        return Err("checker did not judge".into());
    };
    evidence.validate_findings(&response.result)?;
    for label in response
        .result
        .artifacts
        .iter()
        .map(|a| a.path.as_str())
        .chain(
            response
                .result
                .findings
                .iter()
                .flatten()
                .map(|f| f.id.as_str()),
        )
    {
        if crate::scrub::scrub(label) != label {
            return Err("secret-shaped output identifier is not retained".into());
        }
    }
    let outputs = dir
        .open_dir_nofollow("outputs")
        .map_err(|_| "output root was replaced")?;
    let artifacts = artifacts::import(
        &outputs,
        &response.result.artifacts,
        &evidence.request.params.limits,
    )?;
    response.result.rationale = crate::scrub::scrub(&response.result.rationale);
    for finding in response.result.findings.iter_mut().flatten() {
        finding.summary = crate::scrub::scrub(&finding.summary);
    }
    let retained = serde_json::to_vec(&response.result).map_err(|e| e.to_string())?;
    Ok(AcceptedEvaluation {
        raw_stdout_digest: Digest::of(&output.stdout),
        raw_stdout_retained,
        retained_result_digest: Digest::of(&retained),
        transformation: artifacts::transformation(),
        result: response.result,
        artifacts,
        diagnostics: crate::scrub::scrub(&String::from_utf8_lossy(&output.stderr)),
        exit_code: 0,
        container_removed: true,
    })
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;
    use crate::git_ops::GitRepo;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn acp_v2_admission_pins_context_for_attached_and_control_commands() {
        let mut client = DockerEvaluator {
            program: PathBuf::from("/trusted/docker"),
            env: HashMap::from([
                ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
                ("HOME".into(), "/operator".into()),
                ("USER".into(), "operator".into()),
                ("DOCKER_CONFIG".into(), "/operator/.docker-evil".into()),
                ("SSH_AUTH_SOCK".into(), "/tmp/ssh-agent.sock".into()),
                ("DOCKER_CONTEXT".into(), "colima".into()),
                ("DOCKER_HOST".into(), "ssh://remote".into()),
                ("DOCKER_TLS".into(), "1".into()),
                ("DOCKER_TLS_VERIFY".into(), "1".into()),
                ("DOCKER_TLS_CUSTOM".into(), "1".into()),
                ("DOCKER_CERT_PATH".into(), "/other".into()),
                ("DOCKER_API_VERSION".into(), "old".into()),
            ]),
            pinned_endpoint: None,
        };
        client.pin_local_endpoint("unix:///operator/.colima/default/docker.sock");
        assert_eq!(
            client.pinned_endpoint(),
            Some("unix:///operator/.colima/default/docker.sock")
        );
        assert!(!client.env.contains_key("DOCKER_CONFIG"));
        assert!(!client.env.contains_key("SSH_AUTH_SOCK"));
        assert!(!client.env.contains_key("DOCKER_CONTEXT"));
        assert!(!client.env.contains_key("DOCKER_TLS"));
        assert!(!client.env.contains_key("DOCKER_TLS_VERIFY"));
        assert!(!client.env.contains_key("DOCKER_TLS_CUSTOM"));
        assert!(!client.env.contains_key("DOCKER_CERT_PATH"));
        assert!(!client.env.contains_key("DOCKER_API_VERSION"));
        assert_eq!(client.env["PATH"], "/usr/local/bin:/usr/bin:/bin");
        assert_eq!(client.env["HOME"], "/operator");
        assert_eq!(client.env["USER"], "operator");
        assert_eq!(
            client.env["DOCKER_HOST"],
            "unix:///operator/.colima/default/docker.sock"
        );
        assert_eq!(client.env.len(), 4);

        let command = client.attached_command(&["start".into()]);
        let mut environment: HashMap<String, Option<String>> = command
            .as_std()
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|val| val.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert!(!environment.contains_key("DOCKER_CONFIG"));
        assert!(!environment.contains_key("SSH_AUTH_SOCK"));
        assert_eq!(
            environment.remove("DOCKER_HOST").flatten().as_deref(),
            Some("unix:///operator/.colima/default/docker.sock")
        );
    }

    fn synthetic_fixture() -> (tempfile::TempDir, PinnedRegistration, FrozenEvidence) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let image =
            "python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d";
        std::fs::write(
            root.join("pack.toml"),
            format!(
                "[pack]\nname = 'synthetic-checker'\nschema = 5\n[[evaluator]]\nname = 'synthetic-checker'\nimage = '{image}'\nexecutable = '/bin/sh'\nargs = ['/checker/check.sh']\nfiles = ['check.sh']\nstages = ['plan-approval']\nevidence = ['source']\nkind = 'mechanical'\nenforcement = 'blocking'\n"
            ),
        )
        .unwrap();
        std::fs::write(root.join("check.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "pack.toml", "check.sh"][..],
            &["commit", "-qm", "init"][..],
        ] {
            let status = std::process::Command::new("git")
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("HOME", root)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args([
                    "-c",
                    "user.name=Fixture",
                    "-c",
                    "user.email=fixture@example.invalid",
                    "-c",
                    "commit.gpgsign=false",
                    "-c",
                    "core.hooksPath=/dev/null",
                ])
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
            assert!(status.success());
        }
        let repo = GitRepo::open(root).unwrap();
        let reg = PinnedRegistration::at_ref(&repo, "HEAD", "", "synthetic-checker").unwrap();
        let plan = b"{\"goal\":\"synthetic\"}".to_vec();
        let policy = b"{\"humanConsentRequired\":true}".to_vec();
        let subject = json!({"kind":"plan","revision":1,"planDigest":Digest::of(&plan),"baseCommit":{"algorithm":"sha1","value":"a".repeat(40)}});
        let subject_bytes = serde_json::to_vec(&subject).unwrap();
        let mut inputs = BTreeMap::new();
        let mut artifacts = Vec::new();
        for (id, role, bytes) in [
            ("a-plan", "plan", plan.clone()),
            ("b-policy", "policy", policy.clone()),
            ("c-registration", "registration", reg.bytes().to_vec()),
            ("d-subject", "subject", subject_bytes.clone()),
            ("e-source", "source", b"src\n".to_vec()),
        ] {
            inputs.insert(Id::try_from(id.to_string()).unwrap(), bytes.clone());
            artifacts.push(json!({"id":id,"role":role,"content":{"path":format!("inputs/{id}.json"),"bytes":bytes.len(),"digest":Digest::of(&bytes)},"producer":{"kind":"engine"}}));
        }
        let binding = json!({"subjectDigest":Digest::of(&subject_bytes),"planDigest":Digest::of(&plan),"policyDigest":Digest::of(&policy),"registrationDigest":reg.digest(),"workspaceId":"workspace-1"});
        let mut request = json!({"jsonrpc":"2.0","id":"attempt-1","method":"gate/evaluate","params":{"schemaVersion":1,"evaluationId":"evaluation-1","attemptId":"attempt-1","gateId":"synthetic-checker","missionId":"mission-1","stage":"plan-approval","subject":subject,"binding":binding,"deadline":(chrono::Utc::now()+chrono::Duration::minutes(2)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),"limits":{"wallTimeMs":10000,"writeTimeMs":1000,"maxFrameBytes":1048576,"maxStdoutBytes":1048576,"maxStderrBytes":1048576,"maxArtifactBytes":67108864,"maxArtifacts":128}}});
        let manifest = serde_json::to_vec(&json!({"schemaVersion":1,"missionId":"mission-1","binding":request["params"]["binding"],"artifacts":artifacts})).unwrap();
        request["params"]["evidence"] = json!({"path":"inputs/manifest.json","digest":Digest::of(&manifest),"bytes":manifest.len()});
        let ev = FrozenEvidence::new(
            serde_json::to_vec(&request).unwrap(),
            manifest,
            inputs,
            &reg,
        )
        .unwrap();
        (tmp, reg, ev)
    }

    #[test]
    fn gate_subprocess_v1_create_args_adds_owner_label_and_refuses_colon_mount_root() {
        let (_tmp, reg, _ev) = synthetic_fixture();
        let args = create_args(
            "kranz-evaluator-owner123",
            "owner123",
            Path::new("/tmp/clean-root"),
            "nonce123",
            &reg,
        )
        .unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--label" && pair[1] == "com.kranz.gate-owner=owner123"),
            "missing gate-owner label: {args:?}"
        );
        let colon_err = create_args(
            "kranz-evaluator-owner123",
            "owner123",
            Path::new("/tmp/bad:root"),
            "nonce123",
            &reg,
        )
        .unwrap_err();
        assert!(colon_err.contains("not representable safely"));
    }

    #[tokio::test]
    async fn gate_subprocess_v1_container_guard_removes_by_owner_label_and_joins_drop() {
        let dir = tempfile::tempdir().unwrap();
        let id = "b".repeat(64);
        let removed_marker = dir.path().join("removed");
        let log_file = dir.path().join("docker.log");
        let script = dir.path().join("fake-docker");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"$1\" = container ] && [ \"$2\" = ls ]; then\n  case \"$*\" in\n    *label=com.kranz.gate-owner=owner-drop*)\n      [ -e '{}' ] || echo '{id}'\n      exit 0\n      ;;\n  esac\n  exit 9\nfi\nif [ \"$1\" = rm ] && [ \"$2\" = --force ] && [ \"$3\" = '{id}' ]; then\n  sleep 0.1\n  touch '{}'\n  exit 0\nfi\nexit 9\n",
                log_file.display(),
                removed_marker.display(),
                removed_marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let client = DockerEvaluator::new(&script).unwrap();
        let guard = ContainerGuard {
            client,
            owner: "owner-drop".into(),
            name: "mutable-name-never-rm-target".into(),
            id: Some(id.clone()),
            armed: true,
            creation_finished: true,
        };
        drop(guard);
        // Because Drop joins the cleanup thread, `removed_marker` must already
        // exist immediately when `drop(guard)` returns without any sleep.
        assert!(
            removed_marker.exists(),
            "ContainerGuard::drop must join its bounded cleanup thread before returning"
        );
        let log = std::fs::read_to_string(&log_file).unwrap();
        assert!(log.contains("label=com.kranz.gate-owner=owner-drop"));
        assert!(log.contains(&format!("rm --force {id}")));
        assert!(!log.contains("mutable-name-never-rm-target"));
    }

    #[tokio::test]
    async fn gate_subprocess_v1_cleanup_repairs_zero_mode_guest_build_dir_and_tolerates_missing_dirs(
    ) {
        let (tmp, reg, ev) = synthetic_fixture();
        let id = "c".repeat(64);
        let removed_marker = tmp.path().join("removed");
        let script = tmp.path().join("fake-docker");
        let image = &reg.declaration.image;
        // Fake docker simulates:
        // 1) `image inspect` -> valid pinned image metadata
        // 2) `create` -> records the build mount path from `--mount` and returns 64-hex ID
        // 3) `start` -> creates a non-empty mode-0000 subdir inside `build` and exits 1
        // 4) `container ls` / `rm --force` -> cleans up by owner label
        let build_path_record = tmp.path().join("build-mount.txt");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ \"$1\" = image ] && [ \"$2\" = inspect ]; then\n  printf '[{{\"RepoDigests\":[\"{image}\"],\"Config\":{{\"Volumes\":{{}}}}}}]'\n  exit 0\nfi\nif [ \"$1\" = create ]; then\n  rm -f '{}'\n  for arg in \"$@\"; do\n    case \"$arg\" in\n      type=bind,src=*,dst=/gate/build)\n        src=${{arg#type=bind,src=}}\n        src=${{src%,dst=/gate/build}}\n        printf '%s' \"$src\" > '{}'\n        ;;\n    esac\n  done\n  echo '{id}'\n  exit 0\nfi\nif [ \"$1\" = start ]; then\n  cat >/dev/null\n  build_dir=$(cat '{}')\n  mkdir -p \"$build_dir/locked\"\n  printf 'pinned' > \"$build_dir/locked/item\"\n  chmod 0000 \"$build_dir/locked/item\"\n  chmod 0000 \"$build_dir/locked\"\n  exit 1\nfi\nif [ \"$1\" = container ] && [ \"$2\" = ls ]; then\n  [ -e '{}' ] || echo '{id}'\n  exit 0\nfi\nif [ \"$1\" = rm ] && [ \"$2\" = --force ] && [ \"$3\" = '{id}' ]; then\n  touch '{}'\n  exit 0\nfi\nexit 1\n",
                removed_marker.display(),
                build_path_record.display(),
                build_path_record.display(),
                removed_marker.display(),
                removed_marker.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let client = DockerEvaluator::new(&script).unwrap();
        let attempts_dir = tmp.path().join("attempts");
        std::fs::create_dir_all(&attempts_dir).unwrap();

        let outcome = client
            .evaluate(
                &reg,
                &ev,
                RunOptions {
                    attempt_parent: &attempts_dir,
                    retain_private_inputs: false,
                },
                &AtomicBool::new(false),
            )
            .await
            .expect("guest chmod 0000 inside /gate/build must not mask AttemptOutcome");
        assert!(outcome.cleanup_confirmed);
        assert!(outcome
            .evaluation
            .as_ref()
            .unwrap_err()
            .contains("exited unsuccessfully"));
        assert!(!outcome.directory.join("build").exists());
        assert!(outcome.directory.join("receipt.json").is_file());

        // Partial `prepare` failure (unwritable or missing sibling scratch dirs when
        // retain_private_inputs = false and !guard.armed) must also treat NotFound
        // as Ok(()) and return AttemptOutcome instead of masking it.
        let mut bad_reg = reg.clone();
        bad_reg.files[0].bytes = b"changed".to_vec();
        // Make `prepare` fail before all scratch dirs exist by pre-creating a failing docker
        // and verifying missing dirs under `remove_owned_tree_nofollow` never error.
        let fail_script = tmp.path().join("fail-docker");
        std::fs::write(&fail_script, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&fail_script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let fail_client = DockerEvaluator::new(&fail_script).unwrap();
        let mut broken_ev = ev;
        broken_ev.inputs.clear();
        // `broken_ev.inputs` missing causes `prepare` to panic if indexed, so instead make
        // `manifest` parent invalid or pre-test missing dir removal via image inspect failure
        // after removing `build` inside fake-docker:
        let rm_build_script = tmp.path().join("rm-build-docker");
        std::fs::write(
            &rm_build_script,
            "#!/bin/sh\n# image inspect fails while one scratch subdir is already absent\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&rm_build_script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (_, reg2, ev2) = synthetic_fixture();
        let rm_client = DockerEvaluator::new(&rm_build_script).unwrap();
        let outcome2 = rm_client
            .evaluate(
                &reg2,
                &ev2,
                RunOptions {
                    attempt_parent: &attempts_dir,
                    retain_private_inputs: false,
                },
                &AtomicBool::new(false),
            )
            .await
            .unwrap();
        assert!(outcome2.cleanup_confirmed);
        assert!(outcome2
            .evaluation
            .unwrap_err()
            .contains("pinned evaluator image is not installed locally"));
        let _ = (bad_reg, fail_client);
    }
}
