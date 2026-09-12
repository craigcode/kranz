//! Opt-in evidence that an approved command distinguishes a valid implementation
//! from a particular defect. Controls are advisory; an execution failure is not
//! evidence of rejection. The same command runs twice with a read-only checkout
//! and writable scratch, and must report a nonzero number of behavioral checks.

use crate::error::{EngineError, Result};
use crate::gate::{ArtefactRef, GateKind, GateOutcome, GateReport};
use crate::git_ops::GitRepo;
use crate::paths::MissionPaths;
use crate::types::{Assertion, AssertionCheck, MissionConfig, SandboxEnforce, SandboxProvider};
use cap_std::fs::Dir;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_FILE_BYTES: usize = 64 * 1024;
const MAX_TOTAL_BYTES: usize = 512 * 1024;
const MAX_CONTROLS: usize = 8;
const TOTAL_BUDGET: Duration = Duration::from_secs(300);
static CONTROL_EXECUTIONS: Mutex<()> = Mutex::new(());

/// Dropping the awaiting mission operation stops subsequent control launches.
/// A running bounded case retains ownership until it exits and is cleaned up.
#[derive(Default)]
pub(crate) struct CancellationGuard(Arc<AtomicBool>);

impl CancellationGuard {
    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        self.0.clone()
    }
}

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn check_budget(deadline: Instant, cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(invalid("control evaluation cancelled"));
    }
    if Instant::now() >= deadline {
        return Err(invalid("control evaluation budget exhausted"));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ControlSpec {
    pub checker_files: Vec<ControlFile>,
    pub valid_files: Vec<ControlFile>,
    pub defective_files: Vec<ControlFile>,
    pub expected_failure: String,
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,
}

fn default_timeout() -> u64 {
    60
}

fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::InvalidState(format!("negative control: {}", message.into()))
}

fn file_paths(files: &[ControlFile]) -> Result<BTreeSet<String>> {
    if files.is_empty() || files.len() > 16 {
        return Err(invalid("each file group must contain 1–16 files"));
    }
    let mut paths = BTreeSet::new();
    for file in files {
        let path = &file.path;
        if path.is_empty()
            || path.len() > 240
            || path.contains(['\\', ':', '\0'])
            || path.split('/').any(|part| {
                part.is_empty()
                    || matches!(part, "." | "..")
                    || part.eq_ignore_ascii_case(".git")
                    || part.eq_ignore_ascii_case(".kranz")
                    || part.ends_with(['.', ' '])
            })
            || file.content.len() > MAX_FILE_BYTES
            || !paths.insert(path.to_lowercase())
        {
            return Err(invalid(format!(
                "unsafe, duplicate, or oversized file {path:?}"
            )));
        }
    }
    Ok(paths)
}

pub fn validate(assertions: &[Assertion]) -> Result<()> {
    let selected: Vec<_> = assertions
        .iter()
        .filter(|a| a.negative_control.is_some())
        .collect();
    if selected.len() > MAX_CONTROLS {
        return Err(invalid("at most eight assertions may carry controls"));
    }
    for assertion in selected {
        let spec = assertion
            .negative_control
            .as_ref()
            .expect("selected control");
        if assertion.check != AssertionCheck::Command
            || assertion
                .command
                .as_ref()
                .is_none_or(|command| command.trim().is_empty())
        {
            return Err(invalid("controls require a nonempty command assertion"));
        }
        if !(1..=180).contains(&spec.timeout_seconds)
            || spec.expected_failure.trim().is_empty()
            || spec.expected_failure.len() > 128
        {
            return Err(invalid(
                "timeout must be 1–180 seconds and expectedFailure must name a defect",
            ));
        }
        let checker = file_paths(&spec.checker_files)?;
        let valid = file_paths(&spec.valid_files)?;
        let defective = file_paths(&spec.defective_files)?;
        let exact_paths = |files: &[ControlFile]| {
            files
                .iter()
                .map(|file| file.path.clone())
                .collect::<BTreeSet<_>>()
        };
        if valid != defective
            || exact_paths(&spec.valid_files) != exact_paths(&spec.defective_files)
            || !checker.is_disjoint(&valid)
        {
            return Err(invalid(
                "valid/defective paths must match and exclude checking inputs",
            ));
        }
        let paths: Vec<_> = checker.union(&valid).collect();
        if paths.iter().any(|a| {
            paths
                .iter()
                .any(|b| a != b && b.starts_with(&format!("{a}/")))
        }) {
            return Err(invalid("file paths must not contain one another"));
        }
        if !spec.valid_files.iter().any(|valid| {
            spec.defective_files
                .iter()
                .any(|defect| defect.path == valid.path && defect.content != valid.content)
        }) {
            return Err(invalid(
                "the defective control must change at least one file",
            ));
        }
        if spec
            .checker_files
            .iter()
            .chain(&spec.valid_files)
            .chain(&spec.defective_files)
            .map(|file| file.content.len())
            .sum::<usize>()
            > MAX_TOTAL_BYTES
        {
            return Err(invalid("control contents exceed 512 KiB"));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ControlStatus {
    Verified,
    NotRejected,
    Inconclusive,
}

/// Written by the approved checking adapter to KRANZ_CONTROL_RESULT. A failed
/// build, missing test, or shell error without this receipt cannot masquerade
/// as a test finding the intended defect. This is scoped checker evidence,
/// not an independent attestation that an arbitrary checker is truthful.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CheckReceipt {
    pub checks_run: u64,
    pub outcome: CheckOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckOutcome {
    Passed,
    Failed,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseEvidence {
    pub exit_code: Option<i32>,
    pub receipt: Option<CheckReceipt>,
    pub output_tail: String,
    pub elapsed_ms: u128,
    pub environment_names: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlEvidence {
    pub version: u32,
    pub assertion_id: String,
    pub assertion_sha256: String,
    pub checker_sha256: String,
    pub control_sha256: String,
    pub source_revision: String,
    pub recorded_at: String,
    pub platform: String,
    pub containment: String,
    pub environment_names: Vec<String>,
    pub status: ControlStatus,
    pub detail: String,
    pub valid: Option<CaseEvidence>,
    pub defective: Option<CaseEvidence>,
}

fn identity(value: &impl Serialize) -> String {
    crate::standards_waiver::sha256_hex(&serde_json::to_vec(value).expect("serializable control"))
}

fn classify(valid: &CaseEvidence, defective: &CaseEvidence, expected: &str) -> ControlStatus {
    let passed = |case: &CaseEvidence| {
        case.exit_code == Some(0)
            && case.receipt.as_ref().is_some_and(|r| {
                r.checks_run > 0 && r.outcome == CheckOutcome::Passed && r.failure_id.is_none()
            })
    };
    if !passed(valid) {
        return ControlStatus::Inconclusive;
    }
    if passed(defective) {
        return ControlStatus::NotRejected;
    }
    if defective.exit_code.is_some_and(|code| code != 0)
        && defective.receipt.as_ref().is_some_and(|r| {
            r.checks_run > 0
                && r.outcome == CheckOutcome::Failed
                && r.failure_id.as_deref() == Some(expected)
        })
    {
        ControlStatus::Verified
    } else {
        ControlStatus::Inconclusive
    }
}

struct ScratchRoot(PathBuf);
impl ScratchRoot {
    fn create() -> Result<Self> {
        let path = std::env::temp_dir().join(format!("kranz-controls-{}", uuid::Uuid::new_v4()));
        #[allow(unused_mut)] // Unix permissions require the mutable builder.
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(&path)?;
        Ok(Self(std::fs::canonicalize(path)?))
    }
}
impl Drop for ScratchRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn parent_under(root: &Path, path: &str, create: bool) -> Result<(Dir, String)> {
    let mut dir = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    let mut walked = root.to_path_buf();
    let parts: Vec<_> = path.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        walked.push(part);
        dir = crate::paths::open_real_subdir(&dir, part, &walked, create)?;
    }
    Ok((dir, parts[parts.len() - 1].to_string()))
}

fn check_inputs(root: &Path, files: &[ControlFile]) -> Result<()> {
    for file in files {
        let (dir, name) = parent_under(root, &file.path, false)?;
        if crate::paths::read_regular_file_under(&dir, Path::new(&name), MAX_FILE_BYTES as u64)?
            != file.content
        {
            return Err(invalid(format!(
                "approved checking input {} changed or is unavailable",
                file.path
            )));
        }
    }
    Ok(())
}

fn apply_files(root: &Path, files: &[ControlFile]) -> Result<()> {
    for file in files {
        let (dir, name) = parent_under(root, &file.path, true)?;
        match dir.symlink_metadata(&name) {
            Ok(metadata) if metadata.is_file() => dir.remove_file(&name)?,
            Ok(_) => {
                return Err(invalid(format!(
                    "fixture {} is not a regular file",
                    file.path
                )))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        // Fresh inode: a tracked symlink or hard link can never redirect writes.
        dir.open_with(
            &name,
            cap_std::fs::OpenOptions::new().write(true).create_new(true),
        )?
        .write_all(file.content.as_bytes())?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_case(
    repo: &GitRepo,
    paths: &MissionPaths,
    revision: &str,
    assertion: &Assertion,
    files: &[ControlFile],
    config: &MissionConfig,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<CaseEvidence> {
    check_budget(deadline, cancelled)?;
    if config.worker.sandbox.provider != SandboxProvider::Process || cfg!(target_os = "windows") {
        return Err(invalid(
            "read-only control snapshots currently require native macOS/Linux process containment",
        ));
    }
    let spec = assertion
        .negative_control
        .as_ref()
        .expect("selected control");
    let root = ScratchRoot::create()?;
    let snapshot = root.0.join("checkout");
    let _worktree = crate::orchestrator::ApprovalLintWorktree::create(repo, &snapshot, revision)?;
    check_inputs(&snapshot, &spec.checker_files)?;
    apply_files(&snapshot, files)?;
    let scratch = root.0.join("scratch");
    let profiles = root.0.join("profiles");
    std::fs::create_dir(&scratch)?;
    std::fs::create_dir(&profiles)?;
    let mut policy = config.worker.sandbox.clone();
    // The writable root is deliberately scratch, while the command's actual
    // cwd is the sibling read-only checkout. Container/LPAC layouts have no
    // verified mount/read contract for this split yet; refuse, never degrade.
    if policy.enforce == SandboxEnforce::Off {
        policy.enforce = SandboxEnforce::Fs;
    }
    policy.extra_write.clear();
    let sandbox = crate::command_exec::resolve_gate_sandbox(
        &policy,
        &scratch,
        &paths.mission_dir(),
        &scratch,
        &profiles,
    )?
    .sandbox;
    if sandbox.enforce() == SandboxEnforce::Off {
        return Err(invalid("control containment unavailable"));
    }
    let mut env =
        crate::contract_lint::lint_env(&scratch, Some(revision), &config.contract_env_passthrough);
    env.insert(
        "CARGO_TARGET_DIR".into(),
        scratch.join("target").display().to_string(),
    );
    env.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
    env.insert(
        "KRANZ_CONTROL_SCRATCH".into(),
        scratch.display().to_string(),
    );
    env.insert(
        "KRANZ_CONTROL_RESULT".into(),
        scratch.join("result.json").display().to_string(),
    );
    let mut environment_names: Vec<_> = env.keys().cloned().collect();
    environment_names.sort();
    let timeout = deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(spec.timeout_seconds));
    if timeout.is_zero() {
        return Err(invalid("control evaluation budget exhausted"));
    }
    check_budget(deadline, cancelled)?;
    let start = Instant::now();
    let (exit_code, output) = crate::command_exec::run_shell_command_sandboxed_blocking(
        &snapshot,
        assertion.command.as_deref().expect("validated command"),
        timeout,
        &env,
        &sandbox,
    );
    let dir = Dir::open_ambient_dir(&scratch, cap_std::ambient_authority())?;
    let receipt = crate::paths::read_regular_file_under(
        &dir,
        Path::new("result.json"),
        MAX_FILE_BYTES as u64,
    )
    .ok()
    .and_then(|bytes| serde_json::from_str::<CheckReceipt>(&bytes).ok());
    Ok(CaseEvidence {
        exit_code,
        receipt,
        output_tail: crate::scrub::scrub_and_truncate(&output, 4096),
        elapsed_ms: start.elapsed().as_millis(),
        environment_names,
    })
}

fn persist(paths: &MissionPaths, evidence: &ControlEvidence) -> Result<String> {
    let mission = paths.open_mission_dir_nofollow(false)?;
    let runs = crate::paths::open_real_subdir(&mission, "runs", &paths.runs_dir(), true)?;
    let name = format!("control-{}.json", uuid::Uuid::new_v4());
    let mut value = serde_json::to_value(evidence)?;
    crate::scrub::scrub_json_value(&mut value, "control-evidence");
    let mut file = runs.open_with(
        &name,
        cap_std::fs::OpenOptions::new().write(true).create_new(true),
    )?;
    file.write_all(&serde_json::to_vec_pretty(&value)?)?;
    file.sync_all()?;
    Ok(crate::gate_results::file_artefact_ref(&format!(
        "runs/{name}"
    )))
}

/// Run selected controls afresh at this immutable revision. Receipt files are
/// unique per evaluation and referenced by the existing gate.result event.
/// Failure to run or retain evidence stays visibly inconclusive and advisory.
pub fn evaluate(
    repo: &GitRepo,
    paths: &MissionPaths,
    revision: &str,
    assertions: &[Assertion],
    config: &MissionConfig,
) -> Vec<GateReport> {
    evaluate_cancellable(
        repo,
        paths,
        revision,
        assertions,
        config,
        &AtomicBool::new(false),
    )
}

pub(crate) fn evaluate_cancellable(
    repo: &GitRepo,
    paths: &MissionPaths,
    revision: &str,
    assertions: &[Assertion],
    config: &MissionConfig,
    cancelled: &AtomicBool,
) -> Vec<GateReport> {
    let admission = match CONTROL_EXECUTIONS.try_lock() {
        Ok(permit) => Some(permit),
        Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    };
    let deadline = Instant::now() + TOTAL_BUDGET;
    let mut reports = Vec::new();
    for assertion in assertions {
        let Some(spec) = assertion.negative_control.as_ref() else {
            continue;
        };
        let mut evidence = ControlEvidence {
            version: 1,
            assertion_id: assertion.id.clone(),
            assertion_sha256: identity(assertion),
            checker_sha256: identity(&spec.checker_files),
            control_sha256: identity(spec),
            source_revision: revision.to_string(),
            recorded_at: chrono::Utc::now().to_rfc3339(),
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            containment: format!(
                "{}:{}; read-only checkout; scratch-only writes",
                config.worker.sandbox.provider.as_str(),
                if config.worker.sandbox.enforce == SandboxEnforce::Off {
                    "fs"
                } else {
                    config.worker.sandbox.enforce.as_str()
                }
            ),
            environment_names: Vec::new(),
            status: ControlStatus::Inconclusive,
            detail: String::new(),
            valid: None,
            defective: None,
        };
        let result = (|| -> Result<()> {
            if admission.is_none() {
                return Err(invalid("control evaluator busy; retry to collect evidence"));
            }
            check_budget(deadline, cancelled)?;
            validate(assertions)?;
            if !repo.is_clean_tracked_strict()? {
                return Err(invalid("source has tracked changes or hidden index flags"));
            }
            check_inputs(repo.root(), &spec.checker_files)?;
            evidence.valid = Some(run_case(
                repo,
                paths,
                revision,
                assertion,
                &spec.valid_files,
                config,
                deadline,
                cancelled,
            )?);
            evidence.environment_names = evidence.valid.as_ref().unwrap().environment_names.clone();
            evidence.defective = Some(run_case(
                repo,
                paths,
                revision,
                assertion,
                &spec.defective_files,
                config,
                deadline,
                cancelled,
            )?);
            evidence.status = classify(
                evidence.valid.as_ref().unwrap(),
                evidence.defective.as_ref().unwrap(),
                &spec.expected_failure,
            );
            Ok(())
        })();
        evidence.detail = match result {
            Err(error) => format!("INCONCLUSIVE: {error}"),
            Ok(()) => match evidence.status {
                ControlStatus::Verified => "VERIFIED: valid control passed; defective control failed with the expected behavioral finding".into(),
                ControlStatus::NotRejected => "NOT REJECTED: both controls passed; this check did not detect the selected defect".into(),
                ControlStatus::Inconclusive => "INCONCLUSIVE: execution did not establish both a valid pass and rejection of the intended defect; inspect case receipts and output".into(),
            },
        };
        let artefact = match persist(paths, &evidence) {
            Ok(reference) => {
                ArtefactRef::new(reference).with_detail(format!("{} (advisory)", evidence.detail))
            }
            Err(error) => {
                evidence.status = ControlStatus::Inconclusive;
                ArtefactRef::new("negative-control evidence unavailable").with_detail(format!(
                    "INCONCLUSIVE: could not persist control evidence: {error} (advisory)"
                ))
            }
        };
        reports.push(GateReport {
            name: format!("negative-control:{}", assertion.id),
            kind: GateKind::Deterministic,
            outcome: if evidence.status == ControlStatus::Verified {
                GateOutcome::pass(artefact)
            } else {
                GateOutcome::fail(artefact)
            },
        });
    }
    reports
}

#[cfg(test)]
#[path = "contract_controls_tests.rs"]
mod tests;
