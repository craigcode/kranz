//! Engine-owned execution and retention. This module never approves a stage:
//! the caller must recheck its live subject before consuming a resolution.
use super::input_builder::BuiltInput;
use super::lifecycle::*;
use super::protocol::*;
use crate::error::{EngineError, Result};
use crate::events::{Event, EventKind};
use crate::pack::evaluator::PinnedRegistration;
use crate::paths::MissionPaths;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use {
    cap_fs_ext::OpenOptionsFollowExt,
    cap_primitives::fs::FollowSymlinks,
    cap_std::fs::{Dir, OpenOptions},
    std::io::Write,
};

pub(crate) fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::InvalidState(format!("external gate: {}", message.into()))
}

pub(crate) fn id(prefix: &str) -> Id {
    Id::try_from(format!("{prefix}-{}", uuid::Uuid::new_v4().simple()))
        .expect("host-minted gate IDs are valid")
}

pub(crate) fn limits() -> Limits {
    Limits {
        wall_time_ms: 120_000,
        write_time_ms: 1_000,
        max_frame_bytes: 1_048_576,
        max_stdout_bytes: 1_048_576,
        max_stderr_bytes: 1_048_576,
        max_artifact_bytes: 67_108_864,
        max_artifacts: 128,
    }
}

/// Resolve an installed host CLI, excluding repository-controlled PATH entries.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn docker(paths: &MissionPaths) -> Result<super::subprocess::DockerEvaluator> {
    let repo = paths.repo_root.canonicalize()?;
    let program = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|p| p.is_absolute())
        .filter_map(|p| p.join("docker").canonicalize().ok())
        .find(|p| p.is_file() && !p.starts_with(&repo))
        .ok_or_else(|| invalid("no trusted Docker CLI on the host PATH"))?;
    super::subprocess::DockerEvaluator::new(&program).map_err(invalid)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
struct Retention {
    dir: Dir,
    relative: String,
}
#[cfg(any(target_os = "macos", target_os = "linux"))]
impl Retention {
    fn new(paths: &MissionPaths, attempt: &Id) -> Result<Self> {
        let mission = paths.open_mission_dir_nofollow(false)?;
        let runs = crate::paths::open_real_subdir(&mission, "runs", &paths.runs_dir(), true)?;
        let gates =
            crate::paths::open_real_subdir(&runs, "gates", &paths.runs_dir().join("gates"), true)?;
        // No reusable path: a retry has a fresh ID, and an existing directory
        // is a collision/tamper signal rather than permission to overwrite.
        gates.create_dir(attempt.as_str())?;
        use cap_fs_ext::DirExt;
        let dir = gates.open_dir_nofollow(attempt.as_str())?;
        Ok(Self {
            dir,
            relative: format!("runs/gates/{}", attempt.as_str()),
        })
    }

    fn write(&self, name: &str, raw: &[u8]) -> Result<RetainedArtifact> {
        let (retained, transformation) = match std::str::from_utf8(raw) {
            Ok(text) => (
                crate::scrub::scrub(text).into_bytes(),
                super::artifacts::transformation(),
            ),
            Err(_) => (
                b"Binary input omitted from redacted audit retention.\n".to_vec(),
                "binary-omitted-v1".into(),
            ),
        };
        self.write_retained(name, Digest::of(raw), &retained, transformation)
    }

    fn write_retained(
        &self,
        name: &str,
        raw_digest: Digest,
        retained: &[u8],
        transformation: String,
    ) -> Result<RetainedArtifact> {
        // Every name is selected by the host, never an evaluator output path.
        if name.contains(['/', '\\']) || name == "." || name == ".." {
            return Err(invalid("invalid retention leaf"));
        }
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_fs_ext::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = self.dir.open_with(name, &options)?;
        file.write_all(retained)?;
        file.sync_all()?;
        Ok(RetainedArtifact {
            path: WirePath::try_from(format!("{}/{name}", self.relative)).map_err(invalid)?,
            raw_digest,
            retained_digest: Digest::of(retained),
            retained_bytes: retained.len() as u64,
            transformation,
        })
    }
}

/// Request is durable before the process starts. Result and resolution land
/// only after cleanup. No result is reused across calls or on replay.
pub(crate) fn evaluate(
    paths: &MissionPaths,
    registration: &PinnedRegistration,
    built: BuiltInput,
    consent: Option<Consent>,
    emit: impl FnMut(EventKind) -> Result<Event>,
) -> Result<Record> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        evaluate_with(paths, built, consent, emit, |evidence, retention| {
            let client = docker(paths)?;
            // The synchronous approval/merge APIs may already run on Tokio.
            // A scoped thread avoids a nested runtime on the caller's thread.
            std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        let runtime = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()?;
                        let parent = attempt_parent(paths)?;
                        // Preserve the private recovery ledger on every error or panic.
                        // Only confirmed namespace cleanup permits deleting it.
                        let attempt = tempfile::tempdir_in(parent)?.keep();
                        let outcome = runtime
                            .block_on(client.evaluate(
                                registration,
                                evidence,
                                super::subprocess::RunOptions {
                                    attempt_parent: &attempt,
                                    retain_private_inputs: false,
                                },
                                &std::sync::atomic::AtomicBool::new(false),
                            ))
                            .map_err(|error| {
                                invalid(format!(
                                    "{error}; private recovery directory: {}",
                                    attempt.display()
                                ))
                            })?;
                        let cleanup_confirmed = outcome.cleanup_confirmed;
                        if cleanup_confirmed {
                            std::fs::remove_dir_all(&attempt)?;
                        }
                        match outcome.evaluation {
                            Ok(accepted) => {
                                let mut artifacts = Vec::new();
                                for (index, artifact) in accepted.artifacts.iter().enumerate() {
                                    artifacts.push(retention.write_retained(
                                        &format!("output-{index}"),
                                        artifact.source.digest.clone(),
                                        &artifact.retained_bytes,
                                        artifact.transformation.clone(),
                                    )?);
                                }
                                artifacts.push(retention.write(
                                    "result.json",
                                    &serde_json::to_vec(&accepted.result)?,
                                )?);
                                Ok(Finished {
                                    attempt_id: evidence.request.params.attempt_id.clone(),
                                    outcome: Outcome::Evaluated {
                                        result: Box::new(accepted.result),
                                        raw_stdout_digest: accepted.raw_stdout_digest,
                                    },
                                    exit_code: Some(accepted.exit_code),
                                    cleanup_confirmed,
                                    artifacts,
                                })
                            }
                            Err(error) => Ok(Finished {
                                attempt_id: evidence.request.params.attempt_id.clone(),
                                outcome: Outcome::Error {
                                    message: crate::scrub::scrub_and_truncate(&error, 8192),
                                },
                                exit_code: None,
                                cleanup_confirmed,
                                artifacts: vec![],
                            }),
                        }
                    })
                    .join()
                    .map_err(|_| invalid("evaluator driver panicked"))?
            })
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (paths, registration, built, consent, emit);
        Err(invalid(
            "external mission evaluators are unavailable on this platform",
        ))
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn attempt_parent(paths: &MissionPaths) -> Result<std::path::PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| invalid("host HOME is unavailable"))?;
    let mut path = std::path::PathBuf::from(home).canonicalize()?;
    if path.starts_with(paths.repo_root.canonicalize()?) {
        return Err(invalid(
            "evaluator scratch home must be outside the repository",
        ));
    }
    let mut dir = Dir::open_ambient_dir(&path, cap_std::ambient_authority())?;
    for leaf in [".cache", "kranz", "evaluator-attempts"] {
        path.push(leaf);
        dir = crate::paths::open_real_subdir(&dir, leaf, &path, true)?;
    }
    Ok(path)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn evaluate_with(
    paths: &MissionPaths,
    built: BuiltInput,
    consent: Option<Consent>,
    mut emit: impl FnMut(EventKind) -> Result<Event>,
    run: impl FnOnce(&super::evidence::FrozenEvidence, &Retention) -> Result<Finished>,
) -> Result<Record> {
    let evidence = &built.evidence;
    let retention = Retention::new(paths, &evidence.request.params.attempt_id)?;
    let mut retained_inputs = vec![
        retention.write("request.json", &serde_json::to_vec(&evidence.request)?)?,
        retention.write("manifest.json", &evidence.manifest_bytes)?,
    ];
    for (id, bytes) in &evidence.inputs {
        retained_inputs.push(retention.write(id.as_str(), bytes)?);
    }
    let requested = Requested {
        request: evidence.request.clone(),
        policy: built.policy,
        retained_inputs,
        permission_request_id: built.permission_request_id,
    };
    let event = emit(EventKind::GateEvaluationRequested {
        evaluation: Box::new(requested.clone()),
    })?;
    let mut record = Record::new(requested, event.ts, event.seq).map_err(invalid)?;
    let finished = run(evidence, &retention).unwrap_or_else(|error| Finished {
        attempt_id: evidence.request.params.attempt_id.clone(),
        outcome: Outcome::Error {
            message: crate::scrub::scrub_and_truncate(&error.to_string(), 8192),
        },
        exit_code: None,
        cleanup_confirmed: false,
        artifacts: vec![],
    });
    let event = emit(EventKind::GateEvaluationFinished {
        evaluation: Box::new(finished.clone()),
    })?;
    record.finish(finished, event.ts).map_err(invalid)?;
    let disposition = record.disposition(consent.as_ref()).map_err(invalid)?;
    let resolution = Resolution {
        id: id("resolution"),
        attempt_id: evidence.request.params.attempt_id.clone(),
        binding: evidence.request.params.binding.clone(),
        disposition,
        rationale: "Engine applied pinned enforcement, prerequisites and stage consent.".into(),
        consent,
    };
    let event = emit(EventKind::GateResolutionRecorded {
        resolution: resolution.clone(),
    })?;
    record.resolve(resolution, event.ts).map_err(invalid)?;
    Ok(record)
}

pub(crate) fn consume(
    record: &Record,
    binding: Binding,
    action: Action,
    mut emit: impl FnMut(EventKind) -> Result<Event>,
) -> Result<()> {
    let resolution = record
        .resolution
        .as_ref()
        .ok_or_else(|| invalid("missing resolution"))?;
    let consumption = Consumed {
        attempt_id: record.requested.request.params.attempt_id.clone(),
        resolution_id: resolution.id.clone(),
        rechecked_binding: binding,
        action,
    };
    let mut probe = record.clone();
    probe
        .consume(consumption.clone(), chrono::Utc::now())
        .map_err(invalid)?;
    emit(EventKind::GateResolutionConsumed { consumption })?;
    Ok(())
}

pub(crate) struct StageEvaluation<'a> {
    pub paths: &'a MissionPaths,
    pub registrations: &'a [PinnedRegistration],
    pub plan_bytes: &'a [u8],
    pub mission_policy_digest: Digest,
    pub stage: super::input_builder::StageInput<'a>,
    pub checks: super::input_builder::Checks<'a>,
    pub diagnostics: &'a [crate::gate::GateReport],
    pub prior_findings: &'a [&'a Record],
    pub consent: Option<Consent>,
}

pub(crate) fn evaluate_stage(
    input: StageEvaluation<'_>,
    mut emit: impl FnMut(EventKind) -> Result<Event>,
) -> Result<Vec<Record>> {
    use super::input_builder::{build, BuildInput};
    let mut records = Vec::new();
    for registration in input
        .registrations
        .iter()
        .filter(|r| r.declaration().stages.contains(&input.stage.stage()))
    {
        let built = build(BuildInput {
            mission_id: Id::try_from(input.paths.mission_id.clone()).map_err(invalid)?,
            evaluation_id: id("evaluation"),
            attempt_id: id("attempt"),
            workspace_id: workspace_id(&input.paths.repo_root.to_string_lossy()),
            plan_bytes: input.plan_bytes,
            policy: Policy {
                kind: registration.declaration().kind,
                enforcement: registration.declaration().enforcement,
                mission_policy_digest: input.mission_policy_digest.clone(),
                mechanical_prerequisites_passed: true,
            },
            registration,
            stage: input.stage.clone(),
            checks: input.checks.clone(),
            diagnostics: input.diagnostics,
            prior_findings: input.prior_findings,
            source_log_range: None,
            deadline: chrono::Utc::now() + chrono::Duration::minutes(5),
            limits: limits(),
        })
        .map_err(invalid)?;
        let record = evaluate(
            input.paths,
            registration,
            built,
            input.consent.clone(),
            &mut emit,
        )?;
        let disposition = record
            .resolution
            .as_ref()
            .expect("driver resolved")
            .disposition;
        if disposition != Disposition::Proceed {
            return Err(invalid(format!(
                "{:?} {:?} by {}; inspect gate evidence before retrying",
                input.stage.stage(),
                disposition,
                registration.declaration().name.as_str()
            )));
        }
        records.push(record);
    }
    Ok(records)
}
