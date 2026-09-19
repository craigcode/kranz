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

/// Host-owned Docker executable and a frozen client environment. Neither is
/// selected from checker output or forwarded to the container.
#[derive(Clone)]
pub struct DockerEvaluator {
    program: PathBuf,
    env: HashMap<String, String>,
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
        })
    }
    async fn command(
        &self,
        args: &[String],
        input: &[u8],
        wall: Duration,
        write: Duration,
        caps: (usize, usize),
        cancelled: &AtomicBool,
    ) -> Result<Output, String> {
        evaluator_io::run(
            &self.program,
            args,
            &self.env,
            input,
            wall,
            write,
            caps.0,
            caps.1,
            cancelled,
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
        self.command(
            args,
            b"",
            wall,
            Duration::from_secs(1),
            (CONTROL_LIMIT, CONTROL_LIMIT),
            cancelled,
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
        let name = format!("kranz-evaluator-{}", uuid::Uuid::new_v4().simple());
        let root = parent.join(&name);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .map_err(|e| e.to_string())?;
        let mount_nonce = uuid::Uuid::new_v4().simple().to_string();
        let mut guard = ContainerGuard {
            client: self.clone(),
            name: name.clone(),
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
            let args = create_args(&name, &root, &mount_nonce, registration)?;
            // A ledger and guard exist before create, so cancellation cannot
            // forget a daemon-side object while the CLI is in flight.
            private_write(
                &root.join("container.json"),
                &serde_json::to_vec(
                    &serde_json::json!({"name":name,"image":pin,"docker":self.program}),
                )
                .map_err(|e| e.to_string())?,
            )?;
            guard.armed = true;
            let create = self.control(&args).await?;
            let id = std::str::from_utf8(&create.stdout)
                .map_err(|_| "invalid container ID")?
                .trim();
            if create.code != Some(0)
                || id.len() != 64
                || !id.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err("could not create the contained evaluator".into());
            }
            guard.creation_finished = true;
            let mut input = serde_json::to_vec(&evidence.request).map_err(|e| e.to_string())?;
            input.push(b'\n');
            private_write(&root.join("request.ndjson"), &input)?;
            let limits = &evidence.request.params.limits;
            let output = self
                .command(
                    &[
                        "start".into(),
                        "--attach".into(),
                        "--interactive".into(),
                        name.clone(),
                    ],
                    &input,
                    deadline.saturating_duration_since(tokio::time::Instant::now()),
                    Duration::from_millis(limits.write_time_ms),
                    (
                        limits.max_stdout_bytes as usize,
                        limits.max_stderr_bytes as usize,
                    ),
                    cancelled,
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
                    name.clone(),
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
        };
        let execution = async {
            tokio::select! { result = execution => result, () = cancellation => Err("evaluation cancelled".to_string()) }
        };
        let execution = tokio::time::timeout_at(deadline, execution)
            .await
            .map_err(|_| "evaluation deadline expired".to_string())
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
                std::fs::remove_dir_all(root.join(dir))
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

struct ContainerGuard {
    client: DockerEvaluator,
    name: String,
    armed: bool,
    creation_finished: bool,
}
impl ContainerGuard {
    async fn remove(&mut self) -> Result<(), String> {
        let removed = self
            .client
            .control(&["rm".into(), "--force".into(), self.name.clone()])
            .await?;
        // Removal may report absent after an interrupted create. A successful
        // daemon listing, not a failed inspect, proves absence in both cases.
        let listing = self
            .client
            .control(&[
                "container".into(),
                "ls".into(),
                "--all".into(),
                "--filter".into(),
                format!("name=^/{}$", self.name),
                "--format".into(),
                "{{.ID}}".into(),
            ])
            .await?;
        if listing.code != Some(0) || !listing.stdout.iter().all(u8::is_ascii_whitespace) {
            return Err("container cleanup could not be confirmed".into());
        }
        if !self.creation_finished && removed.code != Some(0) {
            return Err("container creation was interrupted; daemon completion and cleanup remain uncertain".into());
        }
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
        let name = self.name.clone();
        // Dropping the async driver is cancellation too. This worker owns a
        // bounded cleanup runtime independent of the cancelled caller.
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async {
                let result = client
                    .control(&["rm".into(), "--force".into(), name.clone()])
                    .await;
                if !matches!(result, Ok(output) if output.code == Some(0)) {
                    tracing::error!(container = %name, "evaluator cleanup needs operator recovery");
                }
            });
        });
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
    root: &Path,
    mount_nonce: &str,
    registration: &PinnedRegistration,
) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = [
        "create",
        "--pull=never",
        "--interactive",
        "--name",
        name,
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
            .filter(|s| !s.contains([',', '\n', '\r']))
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
