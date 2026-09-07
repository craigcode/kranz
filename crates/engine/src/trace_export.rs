//! Derived, event-log-regenerable export of validation-PASSED worker traces
//! in instruction-pair form (plan §M2). Mirrors the pattern of
//! `orchestrator::render_mission_report`: a pure function over the folded
//! [`MissionState`] with no second persisted source of truth. Calling
//! `export_validated_traces`/`to_jsonl` again over the same log always
//! yields the same bytes — there is nothing to regenerate FROM except the
//! event log itself.

use crate::events::Event;
use crate::types::{
    Feature, FeatureStatus, Milestone, MilestoneStatus, MissionState, Role, RunResult,
};
use serde::{Deserialize, Serialize};

/// One fine-tuning-ready training example derived from a validation-PASSED
/// worker run: the task it was given (instruction) and its accepted final
/// report (response), carrying model provenance for dataset filtering.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionPair {
    pub instruction: String,
    pub response: String,
    pub model: String,
    pub quant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_hash: Option<String>,
    pub mission_id: String,
    pub feature_id: String,
    pub run_id: String,
}

/// The feature a run belongs to, plus the status of the milestone that owns
/// it — `None` if the feature id isn't found in the current plan (e.g. a
/// stale/foreign id).
fn feature_and_milestone_status<'a>(
    state: &'a MissionState,
    feature_id: &str,
) -> Option<(&'a Feature, MilestoneStatus)> {
    state.mission.milestones.iter().find_map(|m: &Milestone| {
        m.features
            .iter()
            .find(|f| f.id == feature_id)
            .map(|f| (f, m.status))
    })
}

/// Derive the validation-passed instruction-pair dataset from folded mission
/// state. Selection is validation-PASSED and DERIVED, never stored: a run
/// qualifies iff it is a `Role::Worker` run whose feature reached
/// `FeatureStatus::Complete` inside a milestone that reached
/// `MilestoneStatus::Complete`, AND the run's own `result` is
/// `Some(RunResult::Pass)`. Runs on failed/skipped features, non-worker
/// (orchestrator/validator) runs, and failed respawn attempts that precede a
/// later passing run on the same now-Complete feature, are all excluded.
///
/// `events` is accepted (unused today) to keep the signature honest about
/// what the export is a function of — the event log — should a future
/// revision need raw event data the fold doesn't retain (e.g. renders event
/// timestamps); `state` alone is sufficient for the current instruction-pair
/// shape since it is itself `fold(events)`.
pub fn export_validated_traces(state: &MissionState, _events: &[Event]) -> Vec<InstructionPair> {
    let mut pairs = Vec::new();
    // state.runs is a BTreeMap, so this iterates in a stable, deterministic
    // (sorted-by-run-id) order — required for byte-identical regeneration.
    for run in state.runs.values() {
        if run.role != Role::Worker {
            continue;
        }
        let Some(feature_id) = &run.feature_id else {
            continue;
        };
        let Some((feature, milestone_status)) = feature_and_milestone_status(state, feature_id)
        else {
            continue;
        };
        if feature.status != FeatureStatus::Complete
            || milestone_status != MilestoneStatus::Complete
        {
            continue;
        }
        if run.result != Some(RunResult::Pass) {
            continue;
        }
        let Some(report) = &run.report else {
            continue;
        };

        let mut instruction = feature.spec.clone();
        if !feature.validation_criteria.is_empty() {
            instruction.push_str("\n\nValidation criteria:\n");
            for criterion in &feature.validation_criteria {
                instruction.push_str("- ");
                instruction.push_str(criterion);
                instruction.push('\n');
            }
        }

        let mut response = report.summary.clone();
        if !report.test_evidence.is_empty() {
            response.push_str("\n\nTest evidence:\n");
            response.push_str(&report.test_evidence);
        }
        if !report.commits.is_empty() {
            response.push_str("\n\nCommits:\n");
            response.push_str(&report.commits.join("\n"));
        }

        pairs.push(InstructionPair {
            instruction,
            response,
            model: run.model.clone(),
            quant: run.quant.clone(),
            weight_hash: run.weight_hash.clone(),
            mission_id: state.mission.id.clone(),
            feature_id: feature_id.clone(),
            run_id: run.id.clone(),
        });
    }
    pairs
}

/// Render instruction pairs as JSONL: one compact JSON object per line,
/// each terminated by `\n`. Pure function of its input — no I/O, no
/// persisted dataset file — so it is byte-identical across repeated calls
/// on the same pairs.
pub fn to_jsonl(pairs: &[InstructionPair]) -> String {
    let mut out = String::new();
    for pair in pairs {
        out.push_str(&serde_json::to_string(pair).expect("InstructionPair always serializes"));
        out.push('\n');
    }
    out
}

/// Write one export payload to an operator-named `--out` path: parent chain
/// pinned no-follow, a symlinked (or otherwise non-regular) destination
/// REFUSED, bytes landed through a sibling temp file and a rename.
///
/// Shared by `kranz export-traces --out` and `kranz export-corpus --out`,
/// which both used a bare `std::fs::write`. That truncates THROUGH a
/// symlink, so an agent could plant `corpus.jsonl` → `~/.ssh/authorized_keys`
/// (or a git hook) in the repo root and have the operator's own export
/// overwrite it with partly agent-authored JSONL. Refusing the link is the
/// point; the atomic rename is the same discipline the queue and ticket
/// writers already use.
///
/// Lives here rather than in `paths` only because both exports are the
/// callers; it is a candidate to move next to the other no-follow helpers.
pub fn write_export_output(path: &std::path::Path, bytes: &[u8]) -> crate::error::Result<()> {
    use cap_fs_ext::OpenOptionsFollowExt as _;
    use cap_primitives::fs::FollowSymlinks;
    use std::io::Write as _;
    use std::path::PathBuf;

    let file_name = path.file_name().ok_or_else(|| {
        crate::error::EngineError::InvalidState(format!(
            "export output path {} has no file name",
            path.display()
        ))
    })?;
    // A bare `corpus.jsonl` has an EMPTY parent, which no path helper can
    // canonicalize; anchor it at the cwd first.
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let anchored = parent.join(file_name);
    let (dir, name) = crate::paths::open_parent_nofollow(&anchored)?;

    let refusal = || {
        crate::error::EngineError::InvalidState(format!(
            "refusing to write export output through a symlink or non-regular file: {}",
            path.display()
        ))
    };
    match dir.symlink_metadata(&name) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => return Err(refusal()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let tmp = format!(
        ".{}.{}.kranz-export.tmp",
        name.to_string_lossy(),
        std::process::id()
    );
    let write = (|| -> crate::error::Result<()> {
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        let mut file = dir.open_with(&tmp, &options)?;
        file.write_all(bytes)?;
        file.sync_data()?;
        drop(file);
        // POSIX rename replaces the destination NAME, so a link swapped in
        // after the check is replaced, never written through. Windows needs
        // the destination gone first.
        match dir.rename(&tmp, &dir, &name) {
            Ok(()) => Ok(()),
            Err(error) if cfg!(windows) => {
                match dir.symlink_metadata(&name) {
                    Ok(metadata) if metadata.file_type().is_file() => {
                        dir.remove_file(&name)?;
                    }
                    Ok(_) => return Err(refusal()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(error.into()),
                    Err(e) => return Err(e.into()),
                }
                dir.rename(&tmp, &dir, &name)?;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    })();
    if write.is_err() {
        let _ = dir.remove_file(&tmp);
    }
    write
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reducer::fold;
    use crate::types::*;
    use chrono::{DateTime, TimeZone, Utc};

    const MISSION: &str = "m-1";

    fn base_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
    }

    fn ev(seq: u64, kind: crate::events::EventKind) -> Event {
        Event {
            seq,
            ts: base_ts() + chrono::Duration::seconds(seq as i64),
            mission_id: MISSION.to_string(),
            kind,
        }
    }

    fn plan_feature(title: &str) -> PlanFeature {
        PlanFeature {
            title: title.to_string(),
            spec: format!("spec for {title}"),
            validation_criteria: vec![format!("{title} works")],
        }
    }

    /// One milestone, two features: f-1-1 (will pass) and f-1-2 (will fail).
    fn plan() -> Plan {
        Plan {
            goal: "build the thing".to_string(),
            validation_contract: vec![],
            milestones: vec![PlanMilestone {
                title: "milestone one".to_string(),
                features: vec![plan_feature("alpha"), plan_feature("beta")],
            }],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
            standards_manifest: None,
            reviewer_independence: None,
        }
    }

    fn spawn(run_id: &str, feature_id: &str) -> crate::events::EventKind {
        crate::events::EventKind::WorkerSpawned {
            backend: None,
            run_id: run_id.to_string(),
            role: Role::Worker,
            feature_id: Some(feature_id.to_string()),
            milestone_id: None,
            candidate: None,
            executor_route: None,
            sdk_session_id: format!("sess-{run_id}"),
            model: "sonnet".to_string(),
            quant: "n/a".to_string(),
            weight_hash: None,
            prompt_hash: "deadbeef".to_string(),
            transcript_path: format!("runs/{run_id}.jsonl"),
        }
    }

    fn completed_with_report(
        run_id: &str,
        result: RunResult,
        summary: &str,
    ) -> crate::events::EventKind {
        crate::events::EventKind::WorkerCompleted {
            run_id: run_id.to_string(),
            result,
            tokens: TokenUsage::default(),
            cost_usd: None,
            report: Some(WorkerReport {
                result,
                summary: summary.to_string(),
                files_touched: vec![],
                tests_added: vec![],
                test_evidence: "cargo test: ok".to_string(),
                dependencies_added: vec![],
                known_gaps: vec![],
                commits: vec!["deadbeef commit".to_string()],
                commands_run: vec![],
                escalation: None,
                questions: None,
            }),
        }
    }

    /// Fixture: one Complete milestone with a passed feature (f-1-1, worker
    /// run r-pass) and a failed feature (f-1-2, worker run r-fail).
    fn fixture_events() -> Vec<Event> {
        vec![
            ev(
                1,
                crate::events::EventKind::MissionCreated {
                    goal: "build the thing".to_string(),
                    base_branch: "main".to_string(),
                    mission_branch: format!("kranz/mission-{MISSION}"),
                    config: MissionConfig::default(),
                },
            ),
            ev(
                2,
                crate::events::EventKind::PlanApproved {
                    plan: plan(),
                    base_sha: None,
                },
            ),
            ev(
                3,
                crate::events::EventKind::MilestoneStarted {
                    milestone_id: "ms-1".to_string(),
                    start_sha: "abc123".to_string(),
                },
            ),
            ev(
                4,
                crate::events::EventKind::FeatureStarted {
                    feature_id: "f-1-1".to_string(),
                },
            ),
            ev(5, spawn("r-pass", "f-1-1")),
            ev(
                6,
                completed_with_report("r-pass", RunResult::Pass, "did the alpha thing"),
            ),
            ev(
                7,
                crate::events::EventKind::FeatureCompleted {
                    feature_id: "f-1-1".to_string(),
                    commits: vec!["deadbeef".to_string()],
                },
            ),
            ev(
                8,
                crate::events::EventKind::FeatureStarted {
                    feature_id: "f-1-2".to_string(),
                },
            ),
            ev(9, spawn("r-fail", "f-1-2")),
            ev(
                10,
                completed_with_report("r-fail", RunResult::Fail, "could not do the beta thing"),
            ),
            ev(
                11,
                crate::events::EventKind::FeatureFailed {
                    feature_id: "f-1-2".to_string(),
                    reason: "gave up".to_string(),
                    commits: Vec::new(),
                },
            ),
            ev(
                12,
                crate::events::EventKind::MilestoneCompleted {
                    milestone_id: "ms-1".to_string(),
                    tag: None,
                },
            ),
        ]
    }

    #[test]
    fn passed_only() {
        let events = fixture_events();
        let state = fold(&events).unwrap();

        let pairs = export_validated_traces(&state, &events);

        assert_eq!(
            pairs.len(),
            1,
            "expected exactly one passed trace: {pairs:?}"
        );
        assert_eq!(pairs[0].run_id, "r-pass");
        assert_eq!(pairs[0].feature_id, "f-1-1");
        assert!(pairs.iter().all(|p| p.run_id != "r-fail"));
    }

    #[test]
    fn passed_only_excludes_failed_respawn_attempt() {
        // Single feature reaching Complete via TWO worker runs: a first
        // attempt that fails (report present) and a respawned second
        // attempt that passes. Only the passing run's pair may be exported.
        let events = vec![
            ev(
                1,
                crate::events::EventKind::MissionCreated {
                    goal: "build the thing".to_string(),
                    base_branch: "main".to_string(),
                    mission_branch: format!("kranz/mission-{MISSION}"),
                    config: MissionConfig::default(),
                },
            ),
            ev(
                2,
                crate::events::EventKind::PlanApproved {
                    plan: plan(),
                    base_sha: None,
                },
            ),
            ev(
                3,
                crate::events::EventKind::MilestoneStarted {
                    milestone_id: "ms-1".to_string(),
                    start_sha: "abc123".to_string(),
                },
            ),
            ev(
                4,
                crate::events::EventKind::FeatureStarted {
                    feature_id: "f-1-1".to_string(),
                },
            ),
            ev(5, spawn("r-attempt-1", "f-1-1")),
            ev(
                6,
                completed_with_report("r-attempt-1", RunResult::Fail, "first attempt failed"),
            ),
            ev(7, spawn("r-attempt-2", "f-1-1")),
            ev(
                8,
                completed_with_report("r-attempt-2", RunResult::Pass, "respawn succeeded"),
            ),
            ev(
                9,
                crate::events::EventKind::FeatureCompleted {
                    feature_id: "f-1-1".to_string(),
                    commits: vec!["deadbeef".to_string()],
                },
            ),
            ev(
                10,
                crate::events::EventKind::MilestoneCompleted {
                    milestone_id: "ms-1".to_string(),
                    tag: None,
                },
            ),
        ];
        let state = fold(&events).unwrap();

        let pairs = export_validated_traces(&state, &events);

        assert_eq!(
            pairs.len(),
            1,
            "expected exactly one passed trace, not the failed respawn attempt: {pairs:?}"
        );
        assert_eq!(pairs[0].run_id, "r-attempt-2");
        assert!(pairs.iter().all(|p| p.run_id != "r-attempt-1"));
    }

    #[test]
    fn regenerable() {
        let events = fixture_events();
        let state = fold(&events).unwrap();

        let first = to_jsonl(&export_validated_traces(&state, &events));
        let second = to_jsonl(&export_validated_traces(&state, &events));

        assert_eq!(first, second, "export must be byte-identical across calls");
        assert!(!first.is_empty());

        // Independently regenerated from the same log — a second fold, not
        // a cached/reused value — still matches byte-for-byte.
        let restate = fold(&events).unwrap();
        let third = to_jsonl(&export_validated_traces(&restate, &events));
        assert_eq!(first, third);
    }

    #[test]
    fn instruction_pair() {
        let events = fixture_events();
        let state = fold(&events).unwrap();
        let jsonl = to_jsonl(&export_validated_traces(&state, &events));

        let lines: Vec<&str> = jsonl.lines().collect();
        assert_eq!(lines.len(), 1);
        for line in lines {
            let value: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("line is not valid JSON: {e}: {line}"));
            assert!(value["instruction"].is_string());
            assert!(value["response"].is_string());
            assert_eq!(value["model"], "sonnet");
            assert_eq!(value["quant"], "n/a");
            assert!(value.get("weightHash").is_none());
        }
    }

    /// Audit M2: `--out` used a bare `std::fs::write`, which truncates
    /// through a symlink an agent can plant at a plausible output path.
    #[cfg(unix)]
    #[test]
    fn export_output_refuses_to_write_through_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("authorized_keys");
        std::fs::write(&target, "ssh-ed25519 REAL\n").unwrap();
        let out = tmp.path().join("corpus.jsonl");
        std::os::unix::fs::symlink(&target, &out).unwrap();

        let error = write_export_output(&out, b"{}\n").unwrap_err();
        assert!(
            error.to_string().contains("refusing"),
            "unexpected error: {error}"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "ssh-ed25519 REAL\n",
            "the symlink target must be untouched"
        );
    }

    #[test]
    fn export_output_writes_and_replaces_a_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("nested").join("corpus.jsonl");
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();

        write_export_output(&out, b"first\n").unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "first\n");
        write_export_output(&out, b"second\n").unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "second\n");
        // No temp file left behind.
        let leftovers: Vec<_> = std::fs::read_dir(out.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains("kranz-export"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }
}
