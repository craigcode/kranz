//! Approval uses the real stage driver and contained checker. No vendor CLI or
//! credential is involved. The dedicated evaluator CI job requires this proof.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use kranz_engine::backend_mock::{mock_init, mock_result_text, mock_text, MockBackend, MockScript};
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::events::EventKind;
use kranz_engine::gate_evaluation::{lifecycle::*, protocol::*};
use kranz_engine::git_ops::GitRepo;
use kranz_engine::live_permission::Actor;
use kranz_engine::orchestrator::MissionEngine;
use kranz_engine::types::{MissionConfig, MissionStatus, Plan};
use std::path::Path;
use std::sync::Arc;

const IMAGE: &str =
    "python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d";
fn git(root: &Path, args: &[&str]) -> String {
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
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn fixture(mode: &str) -> (tempfile::TempDir, MissionEngine) {
    fixture_with(
        mode,
        "plan-approval",
        Arc::new(MockBackend::new()),
        MissionConfig::default(),
    )
}
fn fixture_with(
    mode: &str,
    stages: &str,
    backend: Arc<MockBackend>,
    mut config: MissionConfig,
) -> (tempfile::TempDir, MissionEngine) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join("pack")).unwrap();
    std::fs::create_dir_all(root.join(".kranz")).unwrap();
    std::fs::write(
        root.join(".kranz/merge-gates.json"),
        r#"{"gates":[{"command":"test -f source.txt"}]}"#,
    )
    .unwrap();
    std::fs::write(root.join("pack/pack.toml"), format!("[pack]\nname='approval-checker'\nschema=5\n[[evaluator]]\nname='approval-checker'\nimage='{IMAGE}'\nexecutable='/usr/local/bin/python3'\nargs=['-I','-S','/checker/checker.py','{mode}']\nfiles=['checker.py']\nstages=['{stages}']\nevidence=['scope','check-receipt']\nkind='mechanical'\nenforcement='blocking'\n")).unwrap();
    let checker = include_str!("fixtures/gate-evaluator/checker.py").replace(
        "request = json.load(sys.stdin)",
        "request = json.load(sys.stdin)\nif mode == 'slow-pass' or mode == 'slow:' + request['params']['stage']: time.sleep(2)",
    ).replace("if mode == 'fail':", "if mode == 'fail' or mode == 'fail:' + request['params']['stage']:");
    std::fs::write(root.join("pack/checker.py"), checker).unwrap();
    std::fs::write(root.join("source.txt"), "base\n").unwrap();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.name", "Fixture"]);
    git(root, &["config", "user.email", "fixture@example.invalid"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "core.hooksPath", "/dev/null"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "base with checker"]);
    config.pack_dir = Some("pack".into());
    let engine = MissionEngine::create(backend, root, "test approval", config).unwrap();
    (dir, engine)
}
fn plan() -> Plan {
    serde_json::from_value(serde_json::json!({"goal":"test approval","touchSet":["source.txt"],"validationContract":[{"id":"a-1","statement":"future behavior is absent on base","check":"command","command":"false"}],"milestones":[{"title":"one","features":[{"title":"feature","spec":"change source","validationCriteria":["source changes"]}]}]})).unwrap()
}
fn enabled() -> bool {
    if std::env::var("KRANZ_GATE_CONTAINER_TESTS").as_deref() != Ok("1") {
        eprintln!("SKIP-EXTERNAL-EVALUATOR: set KRANZ_GATE_CONTAINER_TESTS=1 for approval proof");
        false
    } else {
        true
    }
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn gate_subprocess_v1_approval_records_real_checks_and_requires_fresh_consent() {
    if !enabled() {
        return;
    }
    for mode in ["pass", "fail", "escalate", "nonzero"] {
        let (dir, mut engine) = fixture(mode);
        let base = git(dir.path(), &["rev-parse", "HEAD"]);
        let result = engine.approve_plan_as(plan(), Actor::LocalMutationCapability);
        let record = engine
            .state()
            .gate_evaluations
            .values()
            .next()
            .unwrap_or_else(|| panic!("real request missing: {result:?}"));
        assert!(record.finished.is_some(), "{mode}: {result:?}");
        let resolution = record.resolution.as_ref().expect("engine resolution");
        assert_eq!(
            resolution.consent.as_ref().unwrap().actor,
            Actor::LocalMutationCapability
        );
        assert_eq!(
            git(dir.path(), &["rev-parse", "HEAD"]),
            base,
            "primary unchanged"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("source.txt")).unwrap(),
            "base\n"
        );
        if mode == "pass" {
            result.unwrap();
            assert!(record.consumed.is_some());
            assert_ne!(engine.state().mission.status, MissionStatus::Planning);
            let events = EventLog::read_events(&engine.paths().events_file()).unwrap();
            let consumed = events
                .iter()
                .position(|e| matches!(e.kind, EventKind::GateResolutionConsumed { .. }))
                .unwrap();
            let approved = events
                .iter()
                .position(|e| matches!(e.kind, EventKind::PlanApproved { .. }))
                .unwrap();
            assert!(consumed < approved);
            for artifact in record
                .requested
                .retained_inputs
                .iter()
                .chain(&record.finished.as_ref().unwrap().artifacts)
            {
                let bytes =
                    std::fs::read(engine.paths().mission_dir().join(artifact.path.as_str()))
                        .unwrap();
                assert_eq!(Digest::of(&bytes), artifact.retained_digest);
            }
        } else {
            assert!(result.is_err(), "{mode}");
            assert!(record.consumed.is_none());
            assert_eq!(engine.state().mission.status, MissionStatus::Planning);
            assert!(!GitRepo::open(dir.path())
                .unwrap()
                .branch_exists(&engine.state().mission.mission_branch)
                .unwrap());
            if mode == "nonzero" {
                assert!(matches!(
                    record.finished.as_ref().unwrap().outcome,
                    Outcome::Error { .. }
                ));
            }
            let id = engine.mission_id().to_string();
            drop(engine);
            let resumed =
                MissionEngine::resume(Arc::new(MockBackend::new()), dir.path(), &id, LockForce::No)
                    .unwrap();
            let record = resumed.state().gate_evaluations.values().next().unwrap();
            assert!(record.closed.is_some());
            assert!(record.consumed.is_none(), "resume never consumes");
        }
    }
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn gate_subprocess_v1_approval_refuses_base_drift_after_a_passing_checker() {
    if !enabled() {
        return;
    }
    let (dir, mut engine) = fixture("slow-pass");
    let root = dir.path().to_path_buf();
    let log = engine.paths().events_file();
    let mutation = std::thread::spawn(move || {
        for _ in 0..150 {
            if EventLog::read_events(&log).is_ok_and(|events| {
                events
                    .iter()
                    .any(|e| matches!(e.kind, EventKind::GateEvaluationRequested { .. }))
            }) {
                git(
                    &root,
                    &["commit", "--allow-empty", "-qm", "concurrent base advance"],
                );
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("approval did not persist its request before checker execution");
    });
    let error = engine.approve_plan(plan()).unwrap_err();
    mutation
        .join()
        .unwrap_or_else(|_| panic!("drift fixture did not start: {error}"));
    assert!(
        error.to_string().contains("changed while checks ran"),
        "{error}"
    );
    let record = engine.state().gate_evaluations.values().next().unwrap();
    assert_eq!(
        record.resolution.as_ref().unwrap().disposition,
        Disposition::Proceed
    );
    assert!(record.closed.as_deref().unwrap().contains("inputs changed"));
    assert!(record.consumed.is_none());
    assert_eq!(engine.state().mission.status, MissionStatus::Planning);
    assert!(!GitRepo::open(dir.path())
        .unwrap()
        .branch_exists(&engine.state().mission.mission_branch)
        .unwrap());
}

#[test]
fn gate_approval_policy_cannot_impersonate_operator_consent() {
    let (dir, mut engine) = fixture("pass");
    let base = git(dir.path(), &["rev-parse", "HEAD"]);
    let error = engine.approve_plan_as(plan(), Actor::Policy).unwrap_err();
    assert!(error.to_string().contains("policy is not plan consent"));
    assert!(engine.state().gate_evaluations.is_empty());
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), base);
    assert_eq!(engine.state().mission.status, MissionStatus::Planning);
}

fn governed_fixture(mode: &str) -> (tempfile::TempDir, MissionEngine) {
    use kranz_engine::types::WorkerIsolation;
    let complete = r#"{"decision":"complete","guidance":"","summary":"accepted worker"}"#;
    let backend = Arc::new(MockBackend::with_scripts(vec![
        MockScript::single_shot_json(&serde_json::json!({"result":"pass","summary":"changed source","filesTouched":["source.txt"],"testsAdded":[],"testEvidence":"command evidence belongs to engine","commits":[]}))
            .writes_file("source.txt", "changed\n").commits_all("deliver source change"),
        MockScript::streaming(vec![mock_init("orchestrator-fixture"), mock_result_text("ready")])
            .responding(vec![vec![mock_text(complete),mock_result_text(complete)],vec![mock_text("NONE"),mock_result_text("NONE")]]),
        MockScript::single_shot_json(&serde_json::json!({"findings":[],"summary":"independent fixture review"}))
            .with_session_id("independent-review-fixture"),
    ]));
    let cfg = MissionConfig {
        skip_functional: true,
        worker_isolation: WorkerIsolation::Worktree,
        validator_allow_uncontained_degrade: true,
        ..Default::default()
    };
    let (dir, mut engine) = fixture_with(
        mode,
        "plan-approval','milestone-validation','final-gate','merge",
        backend,
        cfg,
    );
    engine.seed_worker_auth_verdict_for_test(kranz_engine::auth_verify::AuthVerdict::Inconclusive);
    let mut approved = plan();
    approved.validation_contract[0].command = Some("test -f source.txt".into());
    engine.approve_plan(approved).unwrap();
    (dir, engine)
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_subprocess_v1_mission_stages_and_merge_bind_real_deliverables() {
    if !enabled() {
        return;
    }
    let (dir, mut engine) = governed_fixture("pass");
    let base = engine.state().mission.base_sha.clone().unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(180), engine.run())
        .await
        .expect("bounded mission");
    assert_eq!(
        result.unwrap(),
        MissionStatus::Complete,
        "{:?}",
        EventLog::read_events(&engine.paths().events_file())
            .unwrap()
            .iter()
            .filter(|e| matches!(e.kind, EventKind::MilestoneBlocked { .. }))
            .collect::<Vec<_>>()
    );
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), base);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("source.txt")).unwrap(),
        "base\n"
    );
    let mut stages: Vec<_> = engine
        .state()
        .gate_evaluations
        .values()
        .filter(|r| r.consumed.is_some())
        .map(|r| format!("{:?}", r.requested.request.params.stage))
        .collect();
    stages.sort();
    assert_eq!(stages, ["FinalGate", "MilestoneValidation", "PlanApproval"]);
    let state = engine.state().clone();
    let active_paths = engine.paths().clone();
    drop(engine);
    let root = dir.path().to_path_buf();
    let branch = state.mission.mission_branch;
    let merged = tokio::task::spawn_blocking(move || {
        let repo = GitRepo::open(&root).unwrap();
        kranz_engine::merge::merge_mission_with_external_evidence(
            &repo,
            "main",
            &base,
            &branch,
            None,
            None,
            &Default::default(),
            |cmd, cwd| kranz_engine::command_exec::run_bounded_gate_command(cwd, cmd),
            &active_paths,
            Actor::LocalMutationCapability,
        )
        .unwrap()
    })
    .await
    .unwrap();
    assert!(
        matches!(merged, kranz_engine::merge::MergeReport::Merged { .. }),
        "{merged:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("source.txt")).unwrap(),
        "changed\n"
    );
    let events = EventLog::read_events(
        &dir.path()
            .join(".kranz/missions")
            .join(&state.mission.id)
            .join("events.jsonl"),
    )
    .unwrap();
    let replay = kranz_engine::reducer::fold(&events).unwrap();
    let merge = replay
        .gate_evaluations
        .values()
        .find(|r| r.requested.request.params.stage == Stage::Merge)
        .unwrap();
    assert!(merge.consumed.is_some());
    assert_eq!(
        merge
            .resolution
            .as_ref()
            .unwrap()
            .consent
            .as_ref()
            .unwrap()
            .actor,
        Actor::LocalMutationCapability
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_subprocess_v1_stage_failure_never_completes_or_consumes() {
    if !enabled() {
        return;
    }
    for (mode, stage) in [
        ("fail:milestone-validation", Stage::MilestoneValidation),
        ("fail:final-gate", Stage::FinalGate),
    ] {
        let (dir, mut engine) = governed_fixture(mode);
        let base = engine.state().mission.base_sha.clone().unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(60), engine.run())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(status, MissionStatus::Blocked);
        let record = engine
            .state()
            .gate_evaluations
            .values()
            .find(|r| r.requested.request.params.stage == stage)
            .unwrap();
        assert!(record.consumed.is_none());
        assert_eq!(
            record.resolution.as_ref().unwrap().disposition,
            Disposition::Block
        );
        assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), base);
        if stage == Stage::FinalGate {
            let report =
                std::fs::read_to_string(engine.paths().mission_dir().join("report.md")).unwrap();
            assert!(report.contains("do not establish mission completion"));
        }
        assert!(!EventLog::read_events(&engine.paths().events_file())
            .unwrap()
            .iter()
            .any(|e| matches!(e.kind, EventKind::MissionCompleted {})));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_subprocess_v1_config_cannot_remove_approved_evaluators() {
    if !enabled() {
        return;
    }
    let (dir, mut engine) = governed_fixture("pass");
    let base = git(dir.path(), &["rev-parse", "HEAD"]);
    kranz_engine::control::enqueue(
        engine.paths(),
        &kranz_engine::types::ControlCommand::ConfigChange {
            patch: serde_json::json!({"packDir": null}),
        },
    )
    .unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(60), engine.run())
        .await
        .unwrap();
    assert_eq!(result.unwrap(), MissionStatus::Complete);
    assert_eq!(engine.state().config.pack_dir.as_deref(), Some("pack"));
    assert!(engine
        .state()
        .recent_decisions
        .iter()
        .any(|s| s.contains("not runtime-patchable")));
    let state = engine.state().clone();
    let paths = engine.paths().clone();
    drop(engine);
    // Even a sealed historical config change cannot remove approval authority
    // at merge. Runtime patch admission is an additional, separate guard.
    {
        let mut log = EventLog::acquire(
            &paths,
            &paths.mission_id,
            std::time::Duration::ZERO,
            LockForce::No,
        )
        .unwrap();
        log.append(EventKind::ConfigChanged {
            patch: serde_json::json!({"packDir": null}),
        })
        .unwrap();
    }
    let error = kranz_engine::merge::merge_mission_with_external_evidence(
        &GitRepo::open(dir.path()).unwrap(),
        "main",
        &base,
        &state.mission.mission_branch,
        None,
        None,
        &Default::default(),
        |_, _| panic!("a policy-drifted merge must not run commands"),
        &paths,
        Actor::LocalMutationCapability,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("evaluator pack changed after approval"),
        "{error}"
    );
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), base);
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_subprocess_v1_interrupted_stage_closes_without_replaying_effects() {
    if !enabled() {
        return;
    }
    let (dir, mut engine) = governed_fixture("slow:milestone-validation");
    let paths = engine.paths().clone();
    let pending = async {
        loop {
            if let Ok(events) = EventLog::read_events(&paths.events_file()) {
                if events.iter().any(|e| matches!(&e.kind, EventKind::GateEvaluationRequested { evaluation } if evaluation.request.params.stage == Stage::MilestoneValidation)) { break; }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    {
        let run = engine.run();
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => panic!("mission finished before interruption: {result:?}"),
            result = tokio::time::timeout(std::time::Duration::from_secs(60),pending) => result.unwrap(),
        }
    }
    drop(engine);
    let recovered = MissionEngine::resume(
        Arc::new(MockBackend::new()),
        dir.path(),
        &paths.mission_id,
        LockForce::No,
    )
    .unwrap();
    let record = recovered
        .state()
        .gate_evaluations
        .values()
        .find(|r| r.requested.request.params.stage == Stage::MilestoneValidation)
        .unwrap();
    assert!(record.closed.is_some());
    assert!(record.finished.is_none());
    assert!(record.consumed.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_subprocess_v1_merge_rejects_a_base_advance_after_pass() {
    if !enabled() {
        return;
    }
    let (dir, mut engine) = governed_fixture("slow:merge");
    assert_eq!(engine.run().await.unwrap(), MissionStatus::Complete);
    let state = engine.state().clone();
    let paths = engine.paths().clone();
    drop(engine);
    let root = dir.path().to_path_buf();
    let writer_root = root.clone();
    let writer_paths = paths.clone();
    let writer = std::thread::spawn(move || {
        let start = std::time::Instant::now();
        loop {
            if let Ok(events) = EventLog::read_events(&writer_paths.events_file()) {
                if events.iter().any(|e| matches!(&e.kind, EventKind::GateEvaluationRequested { evaluation } if evaluation.request.params.stage == Stage::Merge)) { break; }
            }
            assert!(
                start.elapsed().as_secs() < 60,
                "merge request never arrived"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        git(
            &writer_root,
            &[
                "commit",
                "--allow-empty",
                "-qm",
                "advance base during review",
            ],
        );
        git(&writer_root, &["rev-parse", "HEAD"])
    });
    let merge_paths = paths.clone();
    let report = tokio::task::spawn_blocking(move || {
        let repo = GitRepo::open(root).unwrap();
        kranz_engine::merge::merge_mission_with_external_evidence(
            &repo,
            "main",
            state.mission.base_sha.as_deref().unwrap(),
            &state.mission.mission_branch,
            None,
            None,
            &Default::default(),
            |cmd, cwd| kranz_engine::command_exec::run_bounded_gate_command(cwd, cmd),
            &merge_paths,
            Actor::LocalMutationCapability,
        )
        .unwrap()
    })
    .await
    .unwrap();
    let advanced = writer.join().unwrap();
    assert!(
        matches!(
            report,
            kranz_engine::merge::MergeReport::RefusedPreMerge { .. }
        ),
        "{report:?}"
    );
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), advanced);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("source.txt")).unwrap(),
        "base\n"
    );
    let replay =
        kranz_engine::reducer::fold(&EventLog::read_events(&paths.events_file()).unwrap()).unwrap();
    let record = replay
        .gate_evaluations
        .values()
        .find(|r| r.requested.request.params.stage == Stage::Merge)
        .unwrap();
    assert!(record.finished.is_some());
    assert!(record.consumed.is_none());
    assert!(record.closed.as_ref().unwrap().contains("changed"));
}

#[tokio::test(flavor = "multi_thread")]
async fn gate_subprocess_v1_revision_approval_uses_fresh_plan_consent() {
    if !enabled() {
        return;
    }
    let (dir, mut engine) = fixture("pass");
    engine.approve_plan(plan()).unwrap();
    let paths = engine.paths().clone();
    let branch = engine.state().mission.mission_branch.clone();
    let base = git(dir.path(), &["rev-parse", "HEAD"]);
    let mut revised = EventLog::read_events(&paths.events_file())
        .unwrap()
        .into_iter()
        .find_map(|e| match e.kind {
            EventKind::PlanApproved { plan, .. } => Some(plan),
            _ => None,
        })
        .unwrap();
    revised.milestones[0].features[0].spec = "approved revised implementation detail".into();
    drop(engine);
    {
        let mut log = EventLog::acquire(
            &paths,
            &paths.mission_id,
            std::time::Duration::ZERO,
            LockForce::No,
        )
        .unwrap();
        log.append(EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: git(dir.path(), &["rev-parse", &branch]),
        })
        .unwrap();
        log.append(EventKind::PlanRevisionProposed {
            revision: 1,
            plan: revised,
            instructions: "revise the pending work".into(),
        })
        .unwrap();
    }
    kranz_engine::control::enqueue(
        &paths,
        &kranz_engine::types::ControlCommand::ApproveRevision { revision: 1 },
    )
    .unwrap();
    let mut resumed = MissionEngine::resume(
        Arc::new(MockBackend::new()),
        dir.path(),
        &paths.mission_id,
        LockForce::No,
    )
    .unwrap();
    resumed.seed_worker_auth_verdict_for_test(kranz_engine::auth_verify::AuthVerdict::Inconclusive);
    // No worker script is supplied: this proof stops once the approved
    // revision has been applied and a new worker would be dispatched.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(30), resumed.run()).await;
    let events = EventLog::read_events(&paths.events_file()).unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, EventKind::PlanRevised { revision: 1, .. })),
        "{:?}",
        resumed.state().recent_decisions
    );
    let revisions: Vec<_> = resumed
        .state()
        .gate_evaluations
        .values()
        .filter(|r| {
            matches!(
                r.requested.request.params.subject,
                Subject::Plan { revision: 2, .. }
            )
        })
        .collect();
    assert_eq!(revisions.len(), 1);
    assert!(revisions[0].consumed.is_some());
    assert_eq!(
        revisions[0]
            .resolution
            .as_ref()
            .unwrap()
            .consent
            .as_ref()
            .unwrap()
            .actor,
        Actor::LocalRepositoryAuthority
    );
    assert_eq!(git(dir.path(), &["rev-parse", "HEAD"]), base);
}
