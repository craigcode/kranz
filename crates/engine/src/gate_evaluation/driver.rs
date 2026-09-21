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
        let mut emit = emit;
        let (record, retention) = begin(paths, &built, &mut emit)?;
        // Synchronous approval/merge callers may already run on Tokio.
        let outcome = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?
                        .block_on(run(paths, registration, &built.evidence, &retention))
                })
                .join()
                .unwrap_or_else(|_| Err(invalid("evaluator driver panicked")))
        });
        finish(record, outcome, consent, &mut emit)
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (paths, registration, built, consent, emit);
        Err(invalid(
            "external mission evaluators are unavailable on this platform",
        ))
    }
}

/// Async stage calls keep the runtime available while the checker is running.
pub(crate) async fn evaluate_async(
    paths: &MissionPaths,
    registration: &PinnedRegistration,
    built: BuiltInput,
    consent: Option<Consent>,
    mut emit: impl FnMut(EventKind) -> Result<Event>,
) -> Result<Record> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let (record, retention) = begin(paths, &built, &mut emit)?;
        let outcome = run(paths, registration, &built.evidence, &retention).await;
        finish(record, outcome, consent, &mut emit)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (paths, registration, built, consent, &mut emit);
        Err(invalid(
            "external mission evaluators are unavailable on this platform",
        ))
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn run(
    paths: &MissionPaths,
    registration: &PinnedRegistration,
    evidence: &super::evidence::FrozenEvidence,
    retention: &Retention,
) -> Result<Finished> {
    let client = docker(paths)?;
    let parent = attempt_parent(paths)?;
    // Preserve recovery files on errors, panic or dropped futures. Only
    // confirmed namespace cleanup permits removing this private directory.
    let attempt = tempfile::tempdir_in(parent)?.keep();
    let outcome = client
        .evaluate(
            registration,
            evidence,
            super::subprocess::RunOptions {
                attempt_parent: &attempt,
                retain_private_inputs: false,
            },
            &std::sync::atomic::AtomicBool::new(false),
        )
        .await
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
            artifacts.push(retention.write("result.json", &serde_json::to_vec(&accepted.result)?)?);
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
fn begin(
    paths: &MissionPaths,
    built: &BuiltInput,
    emit: &mut impl FnMut(EventKind) -> Result<Event>,
) -> Result<(Record, Retention)> {
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
        policy: built.policy.clone(),
        retained_inputs,
        permission_request_id: built.permission_request_id.clone(),
    };
    let event = emit(EventKind::GateEvaluationRequested {
        evaluation: Box::new(requested.clone()),
    })?;
    let record = Record::new(requested, event.ts, event.seq).map_err(invalid)?;
    Ok((record, retention))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn finish(
    mut record: Record,
    outcome: Result<Finished>,
    consent: Option<Consent>,
    emit: &mut impl FnMut(EventKind) -> Result<Event>,
) -> Result<Record> {
    let finished = outcome.unwrap_or_else(|error| Finished {
        attempt_id: record.requested.request.params.attempt_id.clone(),
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
        attempt_id: record.requested.request.params.attempt_id.clone(),
        binding: record.requested.request.params.binding.clone(),
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
    pub source_log_range: Option<LogRange>,
}

// All decisions are consumed together after the final subject recheck. Pin one
// deadline before the first evaluator: each checker keeps its two-minute cap,
// with three minutes left for setup, cleanup and the final authority recheck.
fn stage_deadline(input: &StageEvaluation<'_>) -> Result<chrono::DateTime<chrono::Utc>> {
    let count = input
        .registrations
        .iter()
        .filter(|r| r.declaration().stages.contains(&input.stage.stage()))
        .count();
    deadline_for_checks(chrono::Utc::now(), count)
}

fn deadline_for_checks(
    now: chrono::DateTime<chrono::Utc>,
    count: usize,
) -> Result<chrono::DateTime<chrono::Utc>> {
    let millis = u64::try_from(count.max(1))
        .ok()
        .and_then(|count| limits().wall_time_ms.checked_mul(count))
        .and_then(|millis| i64::try_from(millis).ok())
        .ok_or_else(|| invalid("stage time budget overflow"))?;
    let budget = chrono::Duration::try_milliseconds(millis)
        .and_then(|wall| wall.checked_add(&chrono::Duration::minutes(3)))
        .ok_or_else(|| invalid("stage time budget overflow"))?;
    now.checked_add_signed(budget)
        .ok_or_else(|| invalid("stage deadline overflow"))
}

pub(crate) fn evaluate_stage(
    input: StageEvaluation<'_>,
    mut emit: impl FnMut(EventKind) -> Result<Event>,
) -> Result<Vec<Record>> {
    let deadline = stage_deadline(&input)?;
    let mut records = Vec::new();
    for registration in input
        .registrations
        .iter()
        .filter(|r| r.declaration().stages.contains(&input.stage.stage()))
    {
        let built = build_registration(&input, registration, deadline)?;
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
            return Err(refusal(&record, registration));
        }
        records.push(record);
    }
    Ok(records)
}

fn build_registration(
    input: &StageEvaluation<'_>,
    registration: &PinnedRegistration,
    deadline: chrono::DateTime<chrono::Utc>,
) -> Result<BuiltInput> {
    super::input_builder::build(super::input_builder::BuildInput {
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
        source_log_range: input.source_log_range.clone(),
        deadline,
        limits: limits(),
    })
    .map_err(invalid)
}

pub(crate) async fn evaluate_stage_async(
    input: StageEvaluation<'_>,
    mut emit: impl FnMut(EventKind) -> Result<Event>,
) -> Result<Vec<Record>> {
    let deadline = stage_deadline(&input)?;
    let mut records = Vec::new();
    for registration in input
        .registrations
        .iter()
        .filter(|r| r.declaration().stages.contains(&input.stage.stage()))
    {
        let built = build_registration(&input, registration, deadline)?;
        let record = evaluate_async(
            input.paths,
            registration,
            built,
            input.consent.clone(),
            &mut emit,
        )
        .await?;
        if record
            .resolution
            .as_ref()
            .expect("driver resolved")
            .disposition
            != Disposition::Proceed
        {
            return Err(refusal(&record, registration));
        }
        records.push(record);
    }
    Ok(records)
}

fn refusal(record: &Record, registration: &PinnedRegistration) -> EngineError {
    let detail = match record.finished.as_ref().map(|f| &f.outcome) {
        Some(Outcome::Error { message }) => message.as_str(),
        Some(Outcome::Evaluated { result, .. }) => result.rationale.as_str(),
        None => "attempt did not finish",
    };
    invalid(crate::scrub::scrub_and_truncate(
        &format!(
            "{:?} {:?} by {}: {detail}; inspect gate evidence before retrying",
            record.requested.request.params.stage,
            record.resolution.as_ref().map(|r| r.disposition),
            registration.declaration().name.as_str()
        ),
        8192,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::evaluator::{Enforcement, Kind};
    use chrono::{Duration, SecondsFormat, Utc};

    #[test]
    fn gate_stage_deadline_keeps_early_decisions_consumable_after_three_full_checks() {
        let now = "2026-09-21T12:00:00Z"
            .parse::<chrono::DateTime<Utc>>()
            .unwrap();
        let deadline = deadline_for_checks(now, 3).unwrap();
        assert_eq!(
            deadline_for_checks(now, 1).unwrap(),
            now + Duration::minutes(5)
        );
        assert!(deadline_for_checks(now, usize::MAX).is_err());
        let policy = Policy {
            kind: Kind::Mechanical,
            enforcement: Enforcement::Blocking,
            mission_policy_digest: Digest::of(b"fixture policy"),
            mechanical_prerequisites_passed: true,
        };
        let mut records = Vec::new();
        for index in 0..3 {
            let mut request = Request::from_bytes(include_bytes!(
                "../../schemas/fixtures/gate-v1/final-gate/request.json"
            ))
            .unwrap();
            request.params.deadline = deadline.to_rfc3339_opts(SecondsFormat::Secs, true);
            request.params.limits = limits();
            request.params.binding.policy_digest = policy.digest();
            request.params.attempt_id = id("attempt");
            request.id = request.params.attempt_id.clone();
            let result = EvaluationResult {
                schema_version: 1,
                evaluation_id: request.params.evaluation_id.clone(),
                attempt_id: request.params.attempt_id.clone(),
                binding: request.params.binding.clone(),
                evidence_digest: request.params.evidence.digest.clone(),
                status: Status::Judged,
                verdict: Some(Verdict::Pass),
                rationale: "fixture".into(),
                artifacts: vec![],
                findings: None,
                confidence: None,
            };
            let mut record = Record::new(
                Requested {
                    request,
                    policy: policy.clone(),
                    retained_inputs: vec![],
                    permission_request_id: None,
                },
                now + Duration::minutes(index * 2),
                1,
            )
            .unwrap();
            let finished_at = now + Duration::minutes((index + 1) * 2);
            record
                .finish(
                    Finished {
                        attempt_id: result.attempt_id.clone(),
                        outcome: Outcome::Evaluated {
                            result: Box::new(result),
                            raw_stdout_digest: Digest::of(b"fixture"),
                        },
                        exit_code: Some(0),
                        cleanup_confirmed: true,
                        artifacts: vec![],
                    },
                    finished_at,
                )
                .unwrap();
            record
                .resolve(
                    Resolution {
                        id: id("resolution"),
                        attempt_id: record.requested.request.params.attempt_id.clone(),
                        binding: record.requested.request.params.binding.clone(),
                        disposition: Disposition::Proceed,
                        rationale: "fixture".into(),
                        consent: None,
                    },
                    finished_at,
                )
                .unwrap();
            records.push(record);
        }
        for mut record in records {
            let consumed = Consumed {
                attempt_id: record.requested.request.params.attempt_id.clone(),
                resolution_id: record.resolution.as_ref().unwrap().id.clone(),
                rechecked_binding: record.requested.request.params.binding.clone(),
                action: Action::AcceptDeliverable,
            };
            assert!(record.clone().consume(consumed.clone(), deadline).is_err());
            let mut changed = consumed.clone();
            changed.rechecked_binding.subject_digest = Digest::of(b"changed subject");
            assert!(record
                .clone()
                .consume(changed, now + Duration::minutes(6))
                .is_err());
            record
                .consume(consumed, now + Duration::minutes(6))
                .unwrap();
        }
    }
}
