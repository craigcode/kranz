//! Actual revision pairs reuse the control runner. These are advisory,
//! source-bound observations, never independent attestations or consent.
use super::*;
use crate::events::{Event, EventKind};
use crate::gate_evaluation::{protocol::Digest, snapshot::Identity};
use std::collections::{BTreeMap, HashMap};

const PREFIX: &str = "baseline-candidate-v1:";
const MAX_EVIDENCE_BYTES: u64 = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "outcome",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ExpectedOutcome {
    Passed,
    Failed {
        failure_id: String,
    },
    Diagnostic {
        failure_id: String,
        diagnostic: String,
    },
}

impl ExpectedOutcome {
    fn validate(&self) -> Result<()> {
        let (failure, diagnostic) = match self {
            Self::Passed => return Ok(()),
            Self::Failed { failure_id } => (failure_id, None),
            Self::Diagnostic {
                failure_id,
                diagnostic,
            } => (failure_id, Some(diagnostic)),
        };
        if failure.trim().is_empty()
            || failure.len() > 128
            || diagnostic.is_some_and(|d| d.trim().is_empty() || d.len() > 1024)
        {
            return Err(invalid(
                "pair expectations need a named failure and a bounded exact diagnostic",
            ));
        }
        Ok(())
    }

    pub(super) fn matches(&self, case: &CaseEvidence) -> bool {
        let Some(receipt) = &case.receipt else {
            return false;
        };
        if receipt.checks_run == 0 {
            return false;
        }
        match self {
            Self::Passed => {
                case.exit_code == Some(0)
                    && receipt.outcome == CheckOutcome::Passed
                    && receipt.failure_id.is_none()
                    && receipt.diagnostic.is_none()
            }
            Self::Failed { failure_id } => {
                case.exit_code.is_some_and(|c| c != 0)
                    && receipt.outcome == CheckOutcome::Failed
                    && receipt.failure_id.as_ref() == Some(failure_id)
                    && receipt.diagnostic.is_none()
            }
            Self::Diagnostic {
                failure_id,
                diagnostic,
            } => {
                case.exit_code.is_some_and(|c| c != 0)
                    && receipt.outcome == CheckOutcome::Failed
                    && receipt.failure_id.as_ref() == Some(failure_id)
                    && receipt.diagnostic.as_ref() == Some(diagnostic)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PairSpec {
    pub baseline_revision: String,
    pub expected_baseline: ExpectedOutcome,
    pub expected_candidate: ExpectedOutcome,
    pub environment_label: String,
    pub overlay_checker_on_baseline: bool,
}

impl PairSpec {
    pub(super) fn validate(&self) -> Result<()> {
        if !matches!(self.baseline_revision.len(), 40 | 64)
            || !self
                .baseline_revision
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || self.environment_label.trim().is_empty()
            || self.environment_label.len() > 256
        {
            return Err(invalid(
                "baselinePair requires a full lowercase commit ID and a bounded environment label",
            ));
        }
        self.expected_baseline.validate()?;
        self.expected_candidate.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseBinding {
    /// Captured before any fixture or checker overlay.
    pub source: Identity,
    pub environment: Digest,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairEvidence {
    pub spec: PairSpec,
    pub status: ControlStatus,
    pub detail: String,
    pub baseline: Option<CaseEvidence>,
    pub candidate: Option<CaseEvidence>,
    pub checker_overlay_sha256: Option<String>,
}

impl PairEvidence {
    pub(super) fn pending(spec: &PairSpec) -> Self {
        Self {
            spec: spec.clone(),
            status: ControlStatus::Inconclusive,
            detail: "INCONCLUSIVE: pair has not run".into(),
            baseline: None,
            candidate: None,
            checker_overlay_sha256: None,
        }
    }
}

pub(super) fn case_environment(
    config: &MissionConfig,
    scratch: &Path,
    revision: &str,
) -> HashMap<String, String> {
    let env =
        crate::contract_lint::lint_env(scratch, Some(revision), &config.contract_env_passthrough);
    case_environment_values(env, scratch)
}

fn case_environment_values(
    mut env: HashMap<String, String>,
    scratch: &Path,
) -> HashMap<String, String> {
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
    env
}

pub(super) fn environment_identity(
    config: &MissionConfig,
    scratch: &Path,
    env: &HashMap<String, String>,
) -> Digest {
    let scratch = scratch.to_string_lossy();
    let normalized: BTreeMap<_, _> = env
        .iter()
        .map(|(k, v)| {
            (
                k,
                if k == "KRANZ_BASE_SHA" {
                    "<separately-bound-revision>".into()
                } else if k == "CARGO_HOME" {
                    "<per-case-credential-free-cargo-cache>".into()
                } else {
                    v.replace(scratch.as_ref(), "<per-case-scratch>")
                },
            )
        })
        .collect();
    let mut policy = config.worker.sandbox.clone();
    if policy.enforce == SandboxEnforce::Off {
        policy.enforce = SandboxEnforce::Fs;
    }
    policy.extra_write.clear();
    Digest::of(
        &serde_json::to_vec(&(
            std::env::consts::OS,
            std::env::consts::ARCH,
            policy,
            crate::agent_env::contract_cargo_cache_source(),
            normalized,
        ))
        .expect("environment identity"),
    )
}

fn current_environment(config: &MissionConfig) -> Digest {
    // No filesystem mutation or child execution is needed to reconstruct the
    // configuration. Tool binaries, caches and remote services are not pinned.
    let scratch = Path::new("/kranz-pair-observation-scratch");
    let env = crate::agent_env::contract_command_env_preview(
        scratch,
        Some("current"),
        &config.contract_env_passthrough,
    );
    let env = crate::contract_lint::with_git_hooks_disabled(env);
    let mut env = case_environment_values(env, scratch);
    if config.worker.sandbox.enforce == SandboxEnforce::FsNet {
        env.insert("CARGO_NET_OFFLINE".into(), "true".into());
    }
    environment_identity(config, scratch, &env)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn evaluate_pair(
    repo: &GitRepo,
    paths: &MissionPaths,
    revision: &str,
    assertion: &Assertion,
    config: &MissionConfig,
    deadline: Instant,
    cancelled: &AtomicBool,
    evidence: &mut ControlEvidence,
) -> Result<()> {
    let spec = assertion
        .negative_control
        .as_ref()
        .unwrap()
        .baseline_pair
        .as_ref()
        .unwrap();
    let pair = evidence.baseline_pair.as_mut().unwrap();
    if repo.rev_parse(&format!("{}^{{commit}}", spec.baseline_revision))? != spec.baseline_revision
    {
        return Err(invalid("baseline revision is not an exact commit"));
    }
    pair.checker_overlay_sha256 = spec
        .overlay_checker_on_baseline
        .then(|| evidence.checker_sha256.clone());
    pair.baseline = Some(run_case(
        repo,
        paths,
        &spec.baseline_revision,
        assertion,
        &[],
        spec.overlay_checker_on_baseline,
        config,
        deadline,
        cancelled,
    )?);
    pair.candidate = Some(run_case(
        repo,
        paths,
        revision,
        assertion,
        &[],
        false,
        config,
        deadline,
        cancelled,
    )?);
    let baseline = pair.baseline.as_ref().unwrap();
    let candidate = pair.candidate.as_ref().unwrap();
    let environments: Option<Vec<_>> = [
        evidence.valid.as_ref(),
        evidence.defective.as_ref(),
        Some(baseline),
        Some(candidate),
    ]
    .into_iter()
    .map(|case| case?.binding.as_ref().map(|b| &b.environment))
    .collect();
    let comparable =
        environments.is_some_and(|envs| envs.windows(2).all(|pair| pair[0] == pair[1]));
    let same_candidate = candidate.binding.as_ref().is_some_and(|observed| {
        [evidence.valid.as_ref(), evidence.defective.as_ref()]
            .into_iter()
            .all(|case| {
                case.and_then(|c| c.binding.as_ref())
                    .is_some_and(|binding| binding.source == observed.source)
            })
    });
    if comparable
        && same_candidate
        && spec.expected_baseline.matches(baseline)
        && spec.expected_candidate.matches(candidate)
    {
        pair.status = ControlStatus::Verified;
        pair.detail = "VERIFIED: approved expectations matched at both revisions after valid/defective controls passed. Environment configuration matches; host toolchain, cache and service state are not attested. Advisory observation only.".into();
    } else {
        pair.detail = "INCONCLUSIVE: source identities, expectations or environment configuration differ; inspect the actual receipts. Setup errors, missing receipts and zero checks do not establish the intended behavior.".into();
    }
    Ok(())
}

/// A narrow dossier carried to gates and human views. It contains no worker
/// transcript or command output. The retained artifact carries bounded output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub binding: Option<CaseBinding>,
    pub exit_code: Option<i32>,
    pub receipt: Option<CheckReceipt>,
    pub environment_names: Vec<String>,
}
impl From<&CaseEvidence> for Observation {
    fn from(case: &CaseEvidence) -> Self {
        Self {
            binding: case.binding.clone(),
            exit_code: case.exit_code,
            receipt: case.receipt.clone(),
            environment_names: case.environment_names.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub assertion_id: String,
    pub assertion_sha256: String,
    pub checker_sha256: String,
    pub control_sha256: String,
    pub recorded_at: String,
    pub spec: PairSpec,
    pub status: ControlStatus,
    pub detail: String,
    pub baseline: Option<Observation>,
    pub candidate: Option<Observation>,
    pub checker_overlay_sha256: Option<String>,
}
impl Summary {
    fn from_evidence(evidence: &ControlEvidence) -> Option<Self> {
        if evidence.version != 2 {
            return None;
        }
        let pair = evidence.baseline_pair.as_ref()?;
        Some(Self {
            assertion_id: evidence.assertion_id.clone(),
            assertion_sha256: evidence.assertion_sha256.clone(),
            checker_sha256: evidence.checker_sha256.clone(),
            control_sha256: evidence.control_sha256.clone(),
            recorded_at: evidence.recorded_at.clone(),
            spec: pair.spec.clone(),
            status: pair.status,
            detail: pair.detail.clone(),
            baseline: pair.baseline.as_ref().map(Observation::from),
            candidate: pair.candidate.as_ref().map(Observation::from),
            checker_overlay_sha256: pair.checker_overlay_sha256.clone(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Descriptor {
    pub digest: Digest,
    pub bytes: u64,
    pub summary: Summary,
}

pub fn descriptor(detail: Option<&str>) -> Option<Descriptor> {
    let text = detail?.strip_prefix(PREFIX)?;
    if text.len() > MAX_EVIDENCE_BYTES as usize {
        return None;
    }
    let descriptor: Descriptor = serde_json::from_str(text).ok()?;
    (descriptor.bytes <= MAX_EVIDENCE_BYTES).then_some(descriptor)
}

pub(super) fn report(
    evidence: &ControlEvidence,
    retained: &Result<(String, Vec<u8>)>,
) -> GateReport {
    let (artefact, pass) = match retained.as_ref().ok().and_then(|(reference, bytes)| {
        let retained: ControlEvidence = serde_json::from_slice(bytes).ok()?;
        Some((reference, bytes, Summary::from_evidence(&retained)?))
    }) {
        Some((reference, bytes, summary)) => {
            let pass = summary.status == ControlStatus::Verified;
            let descriptor = Descriptor {
                digest: Digest::of(bytes),
                bytes: bytes.len() as u64,
                summary,
            };
            (
                ArtefactRef::new(reference).with_detail(format!(
                    "{PREFIX}{}",
                    serde_json::to_string(&descriptor).expect("pair descriptor")
                )),
                pass,
            )
        }
        None => (
            ArtefactRef::new("baseline/candidate evidence unavailable")
                .with_detail("INCONCLUSIVE: pair evidence could not be retained (advisory)"),
            false,
        ),
    };
    GateReport {
        name: format!("baseline-candidate:{}", evidence.assertion_id),
        kind: GateKind::Deterministic,
        outcome: if pass {
            GateOutcome::pass(artefact)
        } else {
            GateOutcome::fail(artefact)
        },
    }
}

pub(crate) fn retained_bytes(
    mission_dir: &Path,
    reference: &str,
    descriptor: &Descriptor,
) -> Option<Vec<u8>> {
    let path = reference.strip_prefix(crate::gate_results::FILE_REF_SCHEME)?;
    crate::gate_evaluation::protocol::WirePath::try_from(path.to_string()).ok()?;
    let file = crate::paths::open_read_nofollow(&mission_dir.join(path)).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if file.metadata().ok()?.nlink() != 1 {
            return None;
        }
    }
    let bytes = crate::paths::read_regular_file_bounded(file, MAX_EVIDENCE_BYTES).ok()?;
    (bytes.len() as u64 == descriptor.bytes
        && Digest::of(bytes.as_bytes()) == descriptor.digest
        && serde_json::from_str::<ControlEvidence>(&bytes)
            .ok()
            .and_then(|e| Summary::from_evidence(&e))
            .is_some_and(|summary| identity(&summary) == identity(&descriptor.summary)))
    .then(|| bytes.into_bytes())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    pub seq: u64,
    pub reference: String,
    pub available: bool,
    pub source_and_config_match: bool,
    pub detail: String,
    pub summary: Summary,
}

/// Availability is digest checked; freshness is conservative. Matching source
/// and configuration does not prove that external services or tools stood still.
pub fn reviews(
    mission_dir: &Path,
    events: &[Event],
    repo: Option<&GitRepo>,
    assertions: &[Assertion],
    config: &MissionConfig,
) -> Vec<Review> {
    reviews_with_budget(
        mission_dir,
        events,
        repo,
        assertions,
        config,
        &mut (128 * 1024 * 1024),
    )
}

pub(crate) fn reviews_with_budget(
    mission_dir: &Path,
    events: &[Event],
    repo: Option<&GitRepo>,
    assertions: &[Assertion],
    config: &MissionConfig,
    budget: &mut usize,
) -> Vec<Review> {
    let environment = current_environment(config);
    let mut snapshots = BTreeMap::new();
    let mut reviews = Vec::new();
    for event in events.iter().rev() {
        let EventKind::GateResult {
            gate,
            artefact_ref,
            artefact_detail,
            ..
        } = &event.kind
        else {
            continue;
        };
        if !gate.starts_with("baseline-candidate:") {
            continue;
        }
        let Some(descriptor) = descriptor(artefact_detail.as_deref()) else {
            continue;
        };
        if gate != &format!("baseline-candidate:{}", descriptor.summary.assertion_id) {
            continue;
        }
        let length = descriptor.bytes as usize;
        let available =
            *budget >= length && retained_bytes(mission_dir, artefact_ref, &descriptor).is_some();
        *budget = budget.saturating_sub(length);
        let summary = descriptor.summary;
        let may_capture = snapshots.len() < 32;
        let source = snapshots
            .entry(summary.spec.baseline_revision.clone())
            .or_insert_with(|| {
                if available && may_capture && summary.spec.validate().is_ok() {
                    repo.and_then(|repo| {
                        crate::gate_evaluation::snapshot::SourceSnapshot::capture(
                            repo,
                            &summary.spec.baseline_revision,
                        )
                        .ok()
                    })
                    .map(|s| s.identity)
                } else {
                    None
                }
            });
        let source_and_config_match = available
            && assertions.iter().any(|a| {
                a.id == summary.assertion_id
                    && identity(a) == summary.assertion_sha256
                    && a.negative_control
                        .as_ref()
                        .zip(repo)
                        .is_some_and(|(spec, repo)| {
                            check_inputs(repo.root(), &spec.checker_files).is_ok()
                        })
            })
            && summary
                .candidate
                .as_ref()
                .and_then(|c| c.binding.as_ref())
                .is_some_and(|binding| {
                    source.as_ref() == Some(&binding.source) && binding.environment == environment
                });
        let detail = if !available {
            "UNAVAILABLE: retained pair evidence is missing or its digest differs."
        } else if source_and_config_match {
            "Source and environment configuration match this recorded observation. Toolchain, cache and service state remain unqualified; this is not permission to reuse a gate decision."
        } else {
            "HISTORICAL: source, approved check, or environment configuration changed or cannot be established."
        };
        reviews.push(Review {
            seq: event.seq,
            reference: artefact_ref.clone(),
            available,
            source_and_config_match,
            detail: detail.into(),
            summary,
        });
    }
    reviews.reverse();
    reviews
}

pub(crate) fn diagnostics(
    mission_dir: &Path,
    events: &[Event],
    repo: &GitRepo,
    assertions: &[Assertion],
    config: &MissionConfig,
) -> Vec<GateReport> {
    let reviews = reviews(mission_dir, events, Some(repo), assertions, config);
    // A newer missing/malformed artifact must not fall back to an older pass.
    let latest_event: BTreeMap<_, _> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::GateResult { gate, .. } if gate.starts_with("baseline-candidate:") => {
                Some((gate.as_str(), event.seq))
            }
            _ => None,
        })
        .collect();
    let mut latest = BTreeMap::new();
    for review in reviews {
        latest.insert(review.summary.assertion_id.clone(), review);
    }
    latest
        .into_values()
        .filter(|r| {
            r.source_and_config_match
                && latest_event
                    .get(format!("baseline-candidate:{}", r.summary.assertion_id).as_str())
                    == Some(&r.seq)
        })
        .map(|review| {
            let event = events
                .iter()
                .find(|event| event.seq == review.seq)
                .expect("review event");
            let EventKind::GateResult {
                artefact_detail, ..
            } = &event.kind
            else {
                unreachable!()
            };
            let artefact = ArtefactRef::new(review.reference)
                .with_detail(artefact_detail.clone().expect("descriptor"));
            GateReport {
                name: format!("baseline-candidate:{}", review.summary.assertion_id),
                kind: GateKind::Deterministic,
                outcome: if review.summary.status == ControlStatus::Verified {
                    GateOutcome::pass(artefact)
                } else {
                    GateOutcome::fail(artefact)
                },
            }
        })
        .collect()
}
