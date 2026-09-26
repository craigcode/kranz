use chrono::{Duration, Utc};
use kranz_engine::{
    event_log::EventLog,
    events::EventKind,
    gate_evaluation::{input_builder::*, lifecycle::*, protocol::*, snapshot::SourceSnapshot},
    git_ops::GitRepo,
    pack::evaluator::{Enforcement, PinnedRegistration},
    paths::MissionPaths,
    reducer,
    review_packet::{self, EvidenceStatus},
    types::{MissionConfig, Plan, WorkerIsolation},
};
use serde_json::json;
use std::path::Path;

fn id(s: &str) -> Id {
    Id::try_from(s.to_string()).unwrap()
}
fn git(root: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
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
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
struct Fixture {
    dir: tempfile::TempDir,
    paths: MissionPaths,
    log: EventLog,
    base: String,
}
impl Fixture {
    fn new() -> Self {
        Self::with_kind("judgment")
    }
    fn with_kind(kind: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("pack.toml"), format!("[pack]\nname = 'fixture'\nschema = 5\n[[evaluator]]\nname = 'reviewer'\nimage = 'python@sha256:{}'\nexecutable = '/usr/bin/python3'\nargs = []\nfiles = ['checker.py']\nstages = ['milestone-validation']\nevidence = ['scope']\nkind = '{kind}'\nenforcement = 'blocking'\n", "a".repeat(64))).unwrap();
        std::fs::write(root.join("checker.py"), "# fixture\n").unwrap();
        std::fs::write(root.join("source.rs"), "before\n").unwrap();
        std::fs::write(root.join(".gitignore"), ".kranz/missions/\n").unwrap();
        git(root, &["init", "-b", "mission"]);
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "base"]);
        let base = GitRepo::open(root).unwrap().head_sha().unwrap();
        std::fs::write(root.join("source.rs"), "after\n").unwrap();
        let paths = MissionPaths::new(root, "m-review");
        std::fs::create_dir_all(paths.mission_dir().join("runs")).unwrap();
        let mut log = EventLog::acquire(
            &paths,
            "m-review",
            std::time::Duration::ZERO,
            kranz_engine::event_log::LockForce::No,
        )
        .unwrap();
        let config = MissionConfig {
            pack_dir: Some(".".into()),
            worker_isolation: WorkerIsolation::Checkout,
            ..Default::default()
        };
        log.append(EventKind::MissionCreated {
            goal: "check the endpoint".into(),
            base_branch: "mission".into(),
            mission_branch: "mission".into(),
            config,
        })
        .unwrap();
        let plan: Plan = serde_json::from_value(json!({"goal":"check the endpoint", "touchSet":["source.rs"], "validationContract":[{"id":"behavior", "statement":"returns 200", "check":"command", "command":"cargo test endpoint"}], "milestones":[{"title":"endpoint", "features":[{"title":"health", "spec":"health endpoint", "validationCriteria":["returns 200"]}]}]})).unwrap();
        log.append(EventKind::PlanApproved {
            plan,
            base_sha: Some(base.clone()),
        })
        .unwrap();
        log.append(EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: base.clone(),
        })
        .unwrap();
        log.append(EventKind::MilestoneValidating {
            milestone_id: "ms-1".into(),
        })
        .unwrap();
        Self {
            dir,
            paths,
            log,
            base,
        }
    }
    fn packet(&self) -> review_packet::ReviewPacket {
        review_packet::compute_review_packet(self.dir.path(), "m-review").unwrap()
    }
    fn evaluate(&mut self, attempt: &str, assertions: u64, status: Status, error: bool) {
        let events = EventLog::read_events(&self.paths.events_file()).unwrap();
        let state = reducer::fold(&events).unwrap();
        let repo = GitRepo::open(self.dir.path()).unwrap();
        let registration = PinnedRegistration::at_ref(&repo, &self.base, "", "reviewer").unwrap();
        let snapshot = SourceSnapshot::capture(&repo, &self.base).unwrap();
        let plan = events
            .iter()
            .rev()
            .find_map(|e| match &e.kind {
                EventKind::PlanApproved { plan, .. } => Some(plan),
                _ => None,
            })
            .unwrap();
        let plan_bytes = serde_json::to_vec(plan).unwrap();
        let m = &state.mission;
        let policy = Digest::of(
            &serde_json::to_vec(&(
                &state.config,
                &m.base_sha,
                &m.command_grants,
                &m.deny_exceptions,
                &m.egress_grants,
                &m.touch_set,
                &m.standards_manifest,
            ))
            .unwrap(),
        );
        let environment = Digest::of(b"fixture environment");
        let required = [RequiredCheck {
            id: id("tests"),
            command: "cargo test endpoint".into(),
            require_assertions: true,
        }];
        let observed = [ObservedCheck {
            check_id: id("tests"),
            run_id: id("run-tests"),
            sequence: 1,
            checked_content: Digest::of(&serde_json::to_vec(&snapshot.identity).unwrap()),
            environment: environment.clone(),
            command: "cargo test endpoint".into(),
            exit_code: Some(0),
            assertions_executed: Some(assertions),
            output_summary: None,
        }];
        let built = build(BuildInput {
            mission_id: id("m-review"),
            evaluation_id: id(attempt),
            attempt_id: id(attempt),
            workspace_id: workspace_id(&self.dir.path().to_string_lossy()),
            plan_bytes: &plan_bytes,
            policy: Policy {
                kind: registration.declaration().kind,
                enforcement: Enforcement::Blocking,
                mission_policy_digest: policy,
                mechanical_prerequisites_passed: true,
            },
            registration: &registration,
            stage: StageInput::Milestone {
                milestone_id: id("ms-1"),
                plan_index: 0,
                snapshot: &snapshot,
            },
            checks: Checks {
                environment,
                required: &required,
                observed: &observed,
            },
            prior_findings: &[],
            diagnostics: &[],
            source_log_range: None,
            deadline: Utc::now() + Duration::minutes(5),
            limits: Limits {
                wall_time_ms: 30000,
                write_time_ms: 1000,
                max_frame_bytes: 1048576,
                max_stdout_bytes: 1048576,
                max_stderr_bytes: 1048576,
                max_artifact_bytes: 67108864,
                max_artifacts: 128,
            },
        })
        .unwrap();
        // This materialization mirrors driver retention. Only engine-selected
        // structured inputs are retained; the worker transcript is separate.
        let retain = |name: &str, bytes: &[u8]| {
            let path = format!("runs/gates/{attempt}/{name}");
            let file = self.paths.mission_dir().join(&path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, bytes).unwrap();
            RetainedArtifact {
                path: WirePath::try_from(path).unwrap(),
                raw_digest: Digest::of(bytes),
                retained_digest: Digest::of(bytes),
                retained_bytes: bytes.len() as u64,
                transformation: "fixture-unchanged".into(),
            }
        };
        let mut retained = vec![retain("manifest.json", built.evidence.manifest_bytes())];
        for (artifact, bytes) in built.evidence.inputs() {
            retained.push(retain(artifact.id.as_str(), bytes));
        }
        let requested = Requested {
            request: built.evidence.request().clone(),
            policy: built.policy,
            retained_inputs: retained,
            permission_request_id: None,
        };
        let params = &requested.request.params;
        let source = built
            .evidence
            .inputs()
            .find(|(a, _)| a.role == ArtifactRole::Source)
            .unwrap()
            .0;
        let outcome = if error {
            Outcome::Error {
                message: "checker unavailable".into(),
            }
        } else {
            Outcome::Evaluated {
                raw_stdout_digest: Digest::of(b"result"),
                result: Box::new(EvaluationResult {
                    schema_version: 1,
                    evaluation_id: params.evaluation_id.clone(),
                    attempt_id: params.attempt_id.clone(),
                    binding: params.binding.clone(),
                    evidence_digest: params.evidence.digest.clone(),
                    status,
                    verdict: (status == Status::Judged).then_some(Verdict::Pass),
                    rationale: "independent assessment".into(),
                    artifacts: vec![],
                    findings: Some(vec![Finding {
                        id: id("finding-1"),
                        severity: Severity::High,
                        summary: "Unresolved behavior edge case".into(),
                        evidence: vec![Anchor {
                            artifact_id: source.id.clone(),
                            digest: source.content.digest.clone(),
                            line_start: Some(1),
                            line_end: Some(1),
                        }],
                    }]),
                    confidence: None,
                }),
            }
        };
        let event = self
            .log
            .append(EventKind::GateEvaluationRequested {
                evaluation: Box::new(requested.clone()),
            })
            .unwrap();
        let finished = Finished {
            attempt_id: id(attempt),
            outcome,
            exit_code: Some(0),
            cleanup_confirmed: true,
            artifacts: vec![],
        };
        let finished_event = self
            .log
            .append(EventKind::GateEvaluationFinished {
                evaluation: Box::new(finished.clone()),
            })
            .unwrap();
        let mut record = Record::new(requested, event.ts, event.seq).unwrap();
        record.finish(finished, finished_event.ts).unwrap();
        self.log
            .append(EventKind::GateResolutionRecorded {
                resolution: Resolution {
                    id: id(&format!("resolution-{attempt}")),
                    attempt_id: id(attempt),
                    binding: record.requested.request.params.binding.clone(),
                    disposition: record.disposition(None).unwrap(),
                    rationale: "existing policy".into(),
                    consent: None,
                },
            })
            .unwrap();
    }
}

#[test]
fn review_packet_answers_scope_change_receipts_judgment_and_pending_decision() {
    let mut f = Fixture::new();
    std::fs::write(
        f.dir.path().join("outside.rs"),
        "deliberate scope deviation\n",
    )
    .unwrap();
    f.evaluate("attempt-1", 3, Status::Escalate, false);
    let packet = f.packet();
    assert_eq!(packet.approval_seq, Some(2));
    assert_eq!(
        packet.approved_plan.as_ref().unwrap().validation_contract[0].id,
        "behavior"
    );
    let candidate = packet.candidate.as_ref().unwrap();
    assert_eq!(candidate.identity.base, f.base);
    assert_eq!(candidate.scope_deviations, ["outside.rs"]);
    let evaluation = &packet.evaluations[0];
    assert!(matches!(evaluation.status, EvidenceStatus::Escalated));
    assert!(matches!(
        evaluation.checks[0].status,
        EvidenceStatus::CurrentPass
    ));
    assert_eq!(
        evaluation.checks[0]
            .receipt
            .as_ref()
            .unwrap()
            .assertions_executed,
        Some(3)
    );
    assert_eq!(evaluation.findings[0].id.as_str(), "finding-1");
    assert_eq!(packet.decisions[0].title, "Gate attempt attempt-1");
    assert_eq!(packet.decisions[0].seq, evaluation.requested_seq);
    let markdown = review_packet::render_markdown(&packet);
    assert!(markdown.contains("Outside the approved touch set"));
    assert!(markdown.contains("Unresolved behavior edge case"));
    assert!(markdown.contains("event #2"));
}

#[test]
fn review_packet_invalidates_pass_after_dirty_edit_and_exposes_previous_review_binding() {
    let mut f = Fixture::new();
    f.evaluate("attempt-1", 3, Status::Judged, false);
    let packet = f.packet();
    assert!(
        matches!(packet.evaluations[0].status, EvidenceStatus::CurrentPass),
        "{:#?}",
        packet.evaluations[0]
    );
    std::fs::write(
        f.dir.path().join("source.rs"),
        "later edit without a commit\n",
    )
    .unwrap();
    let packet = f.packet();
    assert!(matches!(
        packet.evaluations[0].status,
        EvidenceStatus::Historical
    ));
    assert_eq!(
        packet.evaluations[0]
            .paths_changed_after_review
            .as_ref()
            .unwrap(),
        &["source.rs"]
    );
    f.evaluate("attempt-2", 4, Status::Judged, false);
    let packet = f.packet();
    assert!(matches!(
        packet.evaluations[1].status,
        EvidenceStatus::CurrentPass
    ));
    assert_eq!(
        packet.evaluations[1].preceding_attempt.as_deref(),
        Some("attempt-1")
    );
    assert!(packet.evaluations[1]
        .changes_since_review
        .contains("subject changed"));
}

#[test]
fn review_packet_missing_tampered_zero_assertions_error_and_expiry_never_pass() {
    for mode in ["missing", "tampered", "zero", "error", "expired"] {
        let mut f = Fixture::with_kind(if mode == "zero" {
            "mechanical"
        } else {
            "judgment"
        });
        f.evaluate(
            "attempt-1",
            if mode == "zero" { 0 } else { 3 },
            Status::Judged,
            mode == "error",
        );
        let file = f
            .paths
            .mission_dir()
            .join("runs/gates/attempt-1/check-requirements");
        if mode == "missing" {
            std::fs::remove_file(&file).unwrap();
        }
        if mode == "tampered" {
            std::fs::write(&file, "tampered").unwrap();
        }
        let packet = if mode == "expired" {
            let events = EventLog::read_events(&f.paths.events_file()).unwrap();
            let state = reducer::fold(&events).unwrap();
            review_packet::project(
                &f.paths.mission_dir(),
                &state,
                &events,
                Some(&GitRepo::open(f.dir.path()).unwrap()),
                Utc::now() + Duration::hours(1),
            )
            .unwrap()
        } else {
            f.packet()
        };
        assert!(
            !matches!(packet.evaluations[0].status, EvidenceStatus::CurrentPass),
            "{mode}"
        );
    }
}

#[test]
fn review_packet_old_logs_are_explicit_and_never_read_worker_transcripts() {
    let f = Fixture::new();
    std::fs::write(
        f.paths.mission_dir().join("runs/worker.jsonl"),
        "HUMAN-ONLY-WORKER-REASONING",
    )
    .unwrap();
    let before = std::fs::read(f.paths.events_file()).unwrap();
    let packet = f.packet();
    assert!(packet.evaluations.is_empty());
    assert!(packet
        .unknowns
        .iter()
        .any(|s| s.contains("No source-bound")));
    assert!(!serde_json::to_string(&packet)
        .unwrap()
        .contains("HUMAN-ONLY"));
    assert_eq!(before, std::fs::read(f.paths.events_file()).unwrap());
    let events = EventLog::read_events(&f.paths.events_file()).unwrap();
    let state = reducer::fold(&events[..1]).unwrap();
    let planning = review_packet::project(
        &f.paths.mission_dir(),
        &state,
        &events[..1],
        None,
        Utc::now(),
    )
    .unwrap();
    assert!(planning.approved_plan.is_none());
    assert!(planning.candidate.is_none());
    assert!(planning
        .unknowns
        .iter()
        .any(|s| s.contains("Approved scope")));
}

#[cfg(unix)]
#[test]
fn review_packet_retention_links_cannot_resolve_to_host_files() {
    let mut f = Fixture::new();
    f.evaluate("attempt-1", 3, Status::Judged, false);
    let file = f
        .paths
        .mission_dir()
        .join("runs/gates/attempt-1/check-requirements");
    let outside = f.dir.path().join("outside.json");
    std::fs::rename(&file, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &file).unwrap();
    let packet = f.packet();
    assert!(matches!(
        packet.evaluations[0].status,
        EvidenceStatus::Unavailable
    ));
}

#[test]
fn review_packet_removed_checkout_keeps_committed_diff_without_claiming_freshness() {
    let mut f = Fixture::new();
    f.evaluate("attempt-1", 3, Status::Judged, false);
    git(f.dir.path(), &["add", "source.rs"]);
    git(f.dir.path(), &["commit", "-qm", "deliver"]);
    let head = GitRepo::open(f.dir.path()).unwrap().head_sha().unwrap();
    git(f.dir.path(), &["checkout", "-b", "other", &f.base]);
    let packet = f.packet();
    assert!(packet.candidate.is_none());
    let committed = packet.committed_change.unwrap();
    assert_eq!(committed.base, f.base);
    assert_eq!(committed.head, head);
    assert!(committed.diff_stat.contains("source.rs"));
    assert!(matches!(
        packet.evaluations[0].status,
        EvidenceStatus::Historical
    ));
}

#[test]
fn review_packet_native_block_names_the_exact_latest_decision_event() {
    let mut f = Fixture::new();
    let event = f
        .log
        .append(EventKind::MilestoneBlocked {
            milestone_id: "ms-1".into(),
            reason: "Required checker could not run".into(),
            block_context: None,
        })
        .unwrap();
    let packet = f.packet();
    let block = packet
        .decisions
        .iter()
        .find(|d| d.title == "Blocked milestone ms-1")
        .unwrap();
    assert_eq!(block.seq, event.seq);
    assert!(block.detail.contains("Required checker could not run"));
}

#[test]
fn review_packet_never_emits_terminal_controls_or_invisible_unicode() {
    let f = Fixture::new();
    let mut packet = f.packet();
    packet.mission_id = "m-\u{1b}c\r\u{009b}2J\u{202e}\u{200b}".into();
    let markdown = review_packet::render_markdown(&packet);
    assert!(!markdown.chars().any(kranz_engine::presentation::ambiguous));
    assert!(markdown.contains("u001b") && markdown.contains("u202e"));
}
