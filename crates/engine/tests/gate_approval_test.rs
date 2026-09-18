//! Approval uses the real stage driver and contained checker. No vendor CLI or
//! credential is involved. The dedicated evaluator CI job requires this proof.
#![cfg(any(target_os = "macos", target_os = "linux"))]

use kranz_engine::backend_mock::MockBackend;
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
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir(root.join("pack")).unwrap();
    std::fs::write(root.join("pack/pack.toml"), format!("[pack]\nname='approval-checker'\nschema=5\n[[evaluator]]\nname='approval-checker'\nimage='{IMAGE}'\nexecutable='/usr/local/bin/python3'\nargs=['-I','-S','/checker/checker.py','{mode}']\nfiles=['checker.py']\nstages=['plan-approval']\nevidence=['scope','check-receipt']\nkind='mechanical'\nenforcement='blocking'\n")).unwrap();
    let checker = include_str!("fixtures/gate-evaluator/checker.py").replace(
        "request = json.load(sys.stdin)",
        "request = json.load(sys.stdin)\nif mode == 'slow-pass': time.sleep(2)",
    );
    std::fs::write(root.join("pack/checker.py"), checker).unwrap();
    std::fs::write(root.join("source.txt"), "base\n").unwrap();
    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.name", "Fixture"]);
    git(root, &["config", "user.email", "fixture@example.invalid"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    git(root, &["config", "core.hooksPath", "/dev/null"]);
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "base with checker"]);
    // Mission admission remains disabled until every S5 stage is connected.
    // Exercise the existing public engine approval API directly here.
    let config = MissionConfig {
        pack_dir: Some("pack".into()),
        ..Default::default()
    };
    let engine =
        MissionEngine::create(Arc::new(MockBackend::new()), root, "test approval", config).unwrap();
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
