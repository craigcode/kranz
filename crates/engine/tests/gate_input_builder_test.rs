use chrono::{Duration, Utc};
use kranz_engine::gate_evaluation::{
    input_builder::*, lifecycle::*, protocol::*, snapshot::SourceSnapshot,
};
use kranz_engine::git_ops::GitRepo;
use kranz_engine::pack::evaluator::{Enforcement, PinnedRegistration};
use serde_json::{json, Value};
use std::path::Path;

fn id(text: &str) -> Id {
    Id::try_from(text.to_string()).unwrap()
}
fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
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
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
struct Fixture {
    dir: tempfile::TempDir,
    repo: GitRepo,
    registration: PinnedRegistration,
    plan: Vec<u8>,
    snapshot: SourceSnapshot,
}
impl Fixture {
    fn new(kind: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("pack.toml"), format!(
            "[pack]\nname = 'fixture'\nschema = 5\n[[evaluator]]\nname = 'checker'\nimage = 'python@sha256:{}'\nexecutable = '/usr/bin/python3'\nargs = []\nfiles = ['checker.py']\nstages = ['plan-approval', 'command-permission', 'milestone-validation', 'final-gate', 'merge']\nevidence = ['scope']\nkind = '{kind}'\nenforcement = 'blocking'\n", "a".repeat(64))).unwrap();
        std::fs::write(root.join("checker.py"), "# synthetic checker\n").unwrap();
        std::fs::write(root.join("source.rs"), "before\n").unwrap();
        git(root, &["init", "-q"]);
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "fixture"]);
        let repo = GitRepo::open(root).unwrap();
        let registration = PinnedRegistration::at_ref(&repo, "HEAD", "", "checker").unwrap();
        std::fs::write(root.join("source.rs"), "after\n").unwrap();
        for path in [
            ".kranz/missions/m-1/runs/worker.jsonl",
            ".kranz/missions/m-1/review-packet.json",
            ".kranz/missions/m-1/report.md",
            ".codex/auth.json",
            ".kranz/serve.token",
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "PRIVATE-CANARY").unwrap();
        }
        let snapshot = SourceSnapshot::capture(&repo, "HEAD").unwrap();
        let plan = serde_json::to_vec_pretty(&json!({"goal":"check behavior", "touchSet":["source.rs"], "validationContract":[], "milestones":[{"title":"first", "features":[{"title":"feature", "spec":"implement behavior", "validationCriteria":["behavior holds"]}]}]})).unwrap();
        Self {
            dir,
            repo,
            registration,
            plan,
            snapshot,
        }
    }
    fn input(&self) -> BuildInput<'_> {
        BuildInput {
            mission_id: id("m-1"),
            evaluation_id: id("evaluation-1"),
            attempt_id: id("attempt-1"),
            workspace_id: id("workspace-1"),
            plan_bytes: &self.plan,
            policy: Policy {
                kind: self.registration.declaration().kind,
                enforcement: Enforcement::Blocking,
                mission_policy_digest: Digest::of(b"policy"),
                mechanical_prerequisites_passed: true,
            },
            registration: &self.registration,
            stage: StageInput::Plan {
                revision: 1,
                base_commit: self.commit(),
            },
            checks: Checks {
                environment: Digest::of(b"environment"),
                required: &[],
                observed: &[],
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
        }
    }
    fn commit(&self) -> GitObject {
        GitObject {
            algorithm: "sha1".into(),
            value: self.snapshot.identity.head.clone(),
        }
    }
    fn permission(&self) -> kranz_engine::live_permission::Request {
        use kranz_engine::live_permission as live;
        let now = Utc::now();
        let action = json!({"kind":"execute","command":"cargo test"});
        let options = vec![
            json!({"optionId":"yes","kind":"allow_once"}),
            json!({"optionId":"no","kind":"reject_once"}),
        ];
        live::Request::new(
            live::Proposal {
                id: "permission-1".into(),
                engine_session_id: "session-1".into(),
                peer_session_id: "peer-1".into(),
                peer_request_id: json!(17),
                tool_call_id: "tool-1".into(),
                action_digest: live::digest(&action).unwrap(),
                options_digest: live::digest(&options).unwrap(),
                action,
                options,
                observed_at: now,
                deadline: now + Duration::minutes(5),
                prohibition: None,
            },
            live::Binding {
                mission_id: "m-1".into(),
                run_id: "run-1".into(),
                workspace: self.dir.path().to_string_lossy().into(),
                plan_digest: Digest::of(&self.plan).as_str()[7..].into(),
                policy_digest: Digest::of(b"policy").as_str()[7..].into(),
            },
        )
        .unwrap()
    }
}
fn bytes(built: &BuiltInput, role: ArtifactRole) -> &[u8] {
    built
        .evidence
        .inputs()
        .find(|(a, _)| a.role == role)
        .unwrap()
        .1
}

#[test]
fn gate_inputs_all_five_stages_bind_exact_bytes_and_keep_private_history_out() {
    let f = Fixture::new("mechanical");
    let permission = f.permission();
    let features = vec![FeatureReceipt {
        feature_id: id("f-1"),
        worker_run_id: id("worker-1"),
        candidate_commit: f.commit(),
        independent_validation_attempt: id("review-1"),
    }];
    for stage in [
        StageInput::Plan {
            revision: 1,
            base_commit: f.commit(),
        },
        StageInput::Invocation(&permission),
        StageInput::Milestone {
            milestone_id: id("ms-1"),
            plan_index: 0,
            snapshot: &f.snapshot,
        },
        StageInput::Deliverable {
            snapshot: &f.snapshot,
            features: &features,
        },
        StageInput::Integration {
            snapshot: &f.snapshot,
            live_base_commit: f.commit(),
            candidate_commit: f.commit(),
            integration_tree: f.commit(),
        },
    ] {
        let mut input = f.input();
        if matches!(stage, StageInput::Invocation(_)) {
            input.workspace_id = workspace_id(&permission.binding.workspace);
            input.deadline = permission.proposal.deadline;
        }
        input.stage = stage;
        let built = build(input).unwrap();
        assert_eq!(bytes(&built, ArtifactRole::Plan), f.plan);
        for (artifact, data) in built.evidence.inputs() {
            assert_eq!(artifact.content.digest, Digest::of(data));
            assert!(!String::from_utf8_lossy(data).contains("PRIVATE-CANARY"));
            assert_ne!(artifact.producer.kind, ProducerKind::Worker);
        }
        assert_eq!(
            built.evidence.request().params.binding.policy_digest,
            built.policy.digest()
        );
        assert_eq!(
            built.permission_request_id.is_some(),
            built.evidence.request().params.stage == Stage::CommandPermission
        );
    }
}

#[test]
fn gate_inputs_required_receipts_reject_zero_missing_stale_and_later_failures() {
    let f = Fixture::new("mechanical");
    let required = [RequiredCheck {
        id: id("check-1"),
        command: "cargo test".into(),
        require_assertions: true,
    }];
    let good = ObservedCheck {
        check_id: id("check-1"),
        run_id: id("run-1"),
        sequence: 7,
        checked_content: Digest::of(&f.plan),
        environment: Digest::of(b"environment"),
        command: "cargo test".into(),
        exit_code: Some(0),
        assertions_executed: Some(3),
        output_summary: None,
    };
    let mut alternatives = vec![vec![], vec![good.clone()]];
    for mode in 0..6 {
        let mut receipt = good.clone();
        match mode {
            0 => receipt.assertions_executed = Some(0),
            1 => receipt.assertions_executed = None,
            2 => receipt.checked_content = Digest::of(b"earlier bytes"),
            3 => receipt.environment = Digest::of(b"different environment"),
            4 => receipt.command = "true".into(),
            _ => receipt.exit_code = Some(1),
        }
        alternatives.push(vec![receipt]);
    }
    let mut later = good.clone();
    later.sequence += 1;
    later.exit_code = Some(1);
    alternatives.push(vec![good.clone(), later]);
    for (i, observed) in alternatives.iter().enumerate() {
        let mut input = f.input();
        input.checks.required = &required;
        input.checks.observed = observed;
        assert_eq!(
            build(input).unwrap().policy.mechanical_prerequisites_passed,
            i == 1
        );
    }
    let judgment = Fixture::new("judgment");
    let mut input = judgment.input();
    input.checks.required = &required;
    assert!(build(input).err().unwrap().contains("nonvacuous"));
    let duplicated = [good.clone(), good];
    let mut input = f.input();
    input.checks.observed = &duplicated;
    assert!(build(input).err().unwrap().contains("duplicate observed"));
}

#[test]
fn gate_inputs_invocation_cannot_change_workspace_policy_or_offered_action() {
    let f = Fixture::new("mechanical");
    let permission = f.permission();
    for mode in 0..4 {
        let mut changed = permission.clone();
        let mut input = f.input();
        input.deadline = permission.proposal.deadline;
        input.workspace_id = workspace_id(&permission.binding.workspace);
        match mode {
            0 => input.workspace_id = id("foreign-workspace"),
            1 => input.policy.mission_policy_digest = Digest::of(b"changed policy"),
            2 => changed.proposal.action = json!({"command":"different"}),
            _ => input.deadline += Duration::seconds(1),
        }
        input.stage = StageInput::Invocation(&changed);
        assert!(build(input).is_err());
    }
}

#[test]
fn gate_inputs_snapshot_limits_and_drift_cannot_be_hidden_by_a_stable_head() {
    let f = Fixture::new("mechanical");
    let observed = [ObservedCheck {
        check_id: id("check-1"),
        run_id: id("run-1"),
        sequence: 1,
        checked_content: Digest::of(&serde_json::to_vec(&f.snapshot.identity).unwrap()),
        environment: Digest::of(b"environment"),
        command: "cargo test".into(),
        exit_code: Some(0),
        assertions_executed: Some(3),
        output_summary: None,
    }];
    let required = [RequiredCheck {
        id: id("check-1"),
        command: "cargo test".into(),
        require_assertions: true,
    }];
    std::fs::write(f.dir.path().join("source.rs"), "later edit\n").unwrap();
    let changed = SourceSnapshot::capture(&f.repo, "HEAD").unwrap();
    assert_eq!(changed.identity.head, f.snapshot.identity.head);
    let mut input = f.input();
    input.checks.observed = &observed;
    input.checks.required = &required;
    input.stage = StageInput::Milestone {
        milestone_id: id("ms-1"),
        plan_index: 0,
        snapshot: &changed,
    };
    assert!(!build(input).unwrap().policy.mechanical_prerequisites_passed);
    for n in 0..1000 {
        std::fs::write(f.dir.path().join(format!("input-{n}")), "x").unwrap();
    }
    let large = SourceSnapshot::capture(&f.repo, "HEAD").unwrap();
    let mut input = f.input();
    input.stage = StageInput::Milestone {
        milestone_id: id("ms-1"),
        plan_index: 0,
        snapshot: &large,
    };
    assert!(build(input).err().unwrap().contains("evidence limits"));
}

#[test]
fn gate_inputs_prior_findings_preserve_old_bindings_without_worker_reports() {
    let f = Fixture::new("judgment");
    let built = build(f.input()).unwrap();
    let request = built.evidence.request().clone();
    let now = Utc::now();
    let mut record = Record::new(
        Requested {
            request: request.clone(),
            policy: built.policy,
            retained_inputs: vec![],
            permission_request_id: None,
        },
        now,
        1,
    )
    .unwrap();
    record
        .finish(
            Finished {
                attempt_id: request.params.attempt_id.clone(),
                outcome: Outcome::Evaluated {
                    raw_stdout_digest: Digest::of(b"raw result"),
                    result: Box::new(EvaluationResult {
                        schema_version: 1,
                        evaluation_id: request.params.evaluation_id,
                        attempt_id: request.params.attempt_id,
                        binding: request.params.binding.clone(),
                        evidence_digest: request.params.evidence.digest.clone(),
                        status: Status::Judged,
                        verdict: Some(Verdict::Fail),
                        rationale: "not forwarded as worker context".into(),
                        artifacts: vec![],
                        findings: Some(vec![Finding {
                            id: id("finding-1"),
                            severity: Severity::High,
                            summary: "check behavior".into(),
                            evidence: vec![Anchor {
                                artifact_id: id("plan"),
                                digest: Digest::of(&f.plan),
                                line_start: Some(1),
                                line_end: Some(1),
                            }],
                        }]),
                        confidence: None,
                    }),
                },
                exit_code: Some(0),
                cleanup_confirmed: true,
                artifacts: vec![],
            },
            now,
        )
        .unwrap();
    let prior = [&record];
    let mut input = f.input();
    input.prior_findings = &prior;
    input.attempt_id = id("attempt-2");
    let built = build(input).unwrap();
    let prior: Value = serde_json::from_slice(bytes(&built, ArtifactRole::PriorFinding)).unwrap();
    assert_eq!(
        prior["binding"],
        serde_json::to_value(request.params.binding).unwrap()
    );
    assert!(prior.get("rationale").is_none());
    record.requested.request.params.mission_id = id("foreign-mission");
    let prior = [&record];
    let mut input = f.input();
    input.prior_findings = &prior;
    assert!(build(input).err().unwrap().contains("this mission"));
}

#[test]
fn gate_inputs_identical_retry_material_stays_byte_identical_and_registration_cannot_drift() {
    let f = Fixture::new("mechanical");
    let first = build(f.input()).unwrap();
    let second = build(f.input()).unwrap();
    assert_eq!(
        first.evidence.manifest_bytes(),
        second.evidence.manifest_bytes()
    );
    let mut input = f.input();
    input.policy.enforcement = Enforcement::Advisory;
    assert!(build(input)
        .err()
        .unwrap()
        .contains("approved registration"));
    let mut input = f.input();
    input.stage = StageInput::Deliverable {
        snapshot: &f.snapshot,
        features: &[],
    };
    assert!(build(input)
        .err()
        .unwrap()
        .contains("accepted feature receipts"));
}

#[test]
fn gate_input_builder_retains_advisory_diagnostics_without_inventing_command_receipts() {
    use kranz_engine::gate::{ArtefactRef, GateKind, GateOutcome, GateReport};
    let fixture = Fixture::new("judgment");
    let diagnostics = [GateReport {
        name: "passes-on-base".into(),
        kind: GateKind::Deterministic,
        outcome: GateOutcome::fail(
            ArtefactRef::new("approval lint").with_detail("advisory suspect"),
        ),
    }];
    let mut input = fixture.input();
    input.diagnostics = &diagnostics;
    let built = build(input).unwrap();
    assert!(built.policy.mechanical_prerequisites_passed);
    let (diagnostic, bytes) = built
        .evidence
        .inputs()
        .find(|(a, _)| a.id.as_str() == "diagnostic-0")
        .unwrap();
    let value: Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(value["verdict"], "fail");
    assert!(value.get("exitCode").is_none());
    assert_eq!(diagnostic.content.digest, Digest::of(bytes));
}
