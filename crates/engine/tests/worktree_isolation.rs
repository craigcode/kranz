//! Tests for the `workerIsolation` mission-config key (M7 tier 1, feature f-1-1)
//! and the mission integration-worktree git primitive (feature f-1-2).
//!
//! f-1-1 only added the config surface; f-1-2 adds the git plumbing
//! (`GitRepo::add_worktree_checkout`) that a later milestone will use to
//! re-route mission-branch mutations through a dedicated worktree. Nothing
//! consumes either yet.

use kranz_engine::backend::{AgentBackend, PromptMode};
use kranz_engine::backend_mock::{mock_init, mock_result_text, mock_text, MockBackend, MockScript};
use kranz_engine::config::load_layers;
use kranz_engine::git_ops::GitRepo;
use kranz_engine::orchestrator::MissionEngine;
use kranz_engine::types::{
    MissionConfig, MissionStatus, Plan, PlanFeature, PlanMilestone, WorkerIsolation,
};
use serde_json::json;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use tokio::time::{timeout, Duration as TokioDuration};

fn write_layer(dir: &tempfile::TempDir, name: &str, contents: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).expect("write layer");
    path
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Fresh repo on `main` with identity + one seed commit; returns the seed sha.
fn seeded_repo() -> Option<(tempfile::TempDir, GitRepo, String)> {
    if !git_available() {
        eprintln!("skipping test: git is not on PATH");
        return None;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .expect("spawn git");
        assert!(out.status.success(), "git {args:?} failed: {out:?}");
    };
    if !Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        run(&["init"]);
        run(&["symbolic-ref", "HEAD", "refs/heads/main"]);
    }
    run(&["config", "user.name", "test"]);
    run(&["config", "user.email", "test@example.com"]);
    std::fs::write(dir.path().join("README.md"), "seed\n").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-m", "seed"]);
    let repo = GitRepo::open(dir.path()).expect("open repo");
    let sha = repo.rev_parse("HEAD").expect("seed sha");
    Some((dir, repo, sha))
}

/// Black-box coverage (public API only) for the mission integration-worktree
/// primitive's git plumbing: `add_worktree_checkout` puts an EXISTING branch
/// (created off the mission base, never checked out in the primary tree)
/// into a new worktree at that branch's tip, without moving the primary
/// checkout off `main`.
#[test]
fn add_worktree_checkout_mirrors_mission_worktree_setup() {
    let Some((_dir, repo, seed)) = seeded_repo() else {
        return;
    };

    // Simulate a mission branch created off the base sha, exactly as
    // `setup_mission_worktree` does — never checked out in the primary tree.
    let mission_branch = "kranz/mission-m-test";
    repo.create_branch(mission_branch, Some(&seed))
        .expect("create mission branch");
    assert_eq!(
        repo.current_branch().unwrap(),
        "main",
        "creating the mission branch must not check it out"
    );

    let wt_dir = tempfile::tempdir().expect("worktree tempdir");
    let wt_path = wt_dir.path().join("integration");
    repo.add_worktree_checkout(&wt_path, mission_branch)
        .expect("checkout mission branch into integration worktree");

    let wt_repo = GitRepo::open(&wt_path).expect("open integration worktree");
    assert_eq!(wt_repo.head_sha().unwrap(), seed);
    assert_eq!(wt_repo.current_branch().unwrap(), mission_branch);

    // The primary checkout never moved off main.
    assert_eq!(repo.current_branch().unwrap(), "main");

    repo.remove_worktree(&wt_path).expect("remove worktree");
    repo.prune_worktrees().expect("prune");
}

#[test]
fn worker_isolation_config_defaults_to_checkout() {
    assert_eq!(
        MissionConfig::default().worker_isolation,
        WorkerIsolation::Checkout
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let layer = write_layer(&dir, "config.json", r#"{"maxRespawns":3}"#);

    let cfg = load_layers(&[layer]).expect("load layers");
    assert_eq!(cfg.worker_isolation, WorkerIsolation::Checkout);
    assert_eq!(cfg.max_respawns, 3);
}

#[test]
fn worker_isolation_config_parses_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let layer = write_layer(&dir, "config.json", r#"{"workerIsolation":"worktree"}"#);

    let cfg = load_layers(&[layer]).expect("load layers");
    assert_eq!(cfg.worker_isolation, WorkerIsolation::Worktree);
}

#[test]
fn worker_isolation_config_serializes_camel_case() {
    let value = serde_json::to_value(MissionConfig::default()).expect("serialize");
    assert_eq!(value["workerIsolation"], "checkout");
}

#[test]
fn worker_isolation_config_rejects_unknown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let layer = write_layer(&dir, "config.json", r#"{"workerIsolation":"sandbox"}"#);

    let result = load_layers(&[layer]);
    assert!(result.is_err());
}

// -----------------------------------------------------------------------
// f-2-1: routing the sequential worker + run() loop through the mission
// integration worktree in worktree mode.
// -----------------------------------------------------------------------

const GOAL: &str = "ship the demo feature";

fn raw_git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A single-milestone, single-feature plan with no validation contract, so
/// the mission completes right after the one judgement turn (mirrors
/// `mission_test.rs`'s minimal happy-path shape).
fn one_feature_plan() -> Plan {
    Plan {
        goal: GOAL.to_string(),
        validation_contract: vec![],
        milestones: vec![PlanMilestone {
            title: "M1".to_string(),
            features: vec![PlanFeature {
                title: "feature 1".to_string(),
                spec: "build part 1".to_string(),
                validation_criteria: vec!["part 1 works".to_string()],
            }],
        }],
        command_grants: vec![],
    }
}

fn worker_pass() -> MockScript {
    MockScript::single_shot_json(&json!({
        "result": "pass",
        "summary": "implemented and tested",
        "filesTouched": [],
        "testsAdded": [],
        "testEvidence": "all green",
        "commits": []
    }))
}

fn judgement_complete() -> String {
    json!({ "decision": "complete", "guidance": "", "summary": "worker judged: complete" })
        .to_string()
}

/// Orchestrator streaming script: seed, then one judgement turn, then the
/// (empty-contract) capture turn replying NONE.
fn orch_script_complete_no_lesson() -> MockScript {
    MockScript::streaming(vec![mock_init("orch-session"), mock_result_text("ready")]).responding(
        vec![
            vec![
                mock_text(&judgement_complete()),
                mock_result_text(&judgement_complete()),
            ],
            vec![mock_text("NONE"), mock_result_text("NONE")],
        ],
    )
}

fn worktree_cfg() -> MissionConfig {
    MissionConfig {
        skip_scrutiny: true,
        skip_functional: true,
        worker_isolation: WorkerIsolation::Worktree,
        ..MissionConfig::default()
    }
}

fn checkout_cfg() -> MissionConfig {
    MissionConfig {
        skip_scrutiny: true,
        skip_functional: true,
        ..MissionConfig::default()
    }
}

/// Fresh git repo, seeded, on `main`, root canonicalized.
fn mission_init_repo() -> Option<(tempfile::TempDir, PathBuf)> {
    if !git_available() {
        eprintln!("skipping test: git is not on PATH");
        return None;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let init = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .expect("spawn git init");
    if !init.status.success() {
        raw_git(dir.path(), &["init"]);
        raw_git(dir.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);
    }
    raw_git(dir.path(), &["config", "user.name", "test"]);
    raw_git(dir.path(), &["config", "user.email", "test@example.com"]);
    std::fs::write(dir.path().join("README.md"), "seed\n").unwrap();
    raw_git(dir.path(), &["add", "-A"]);
    raw_git(dir.path(), &["commit", "-m", "seed"]);
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize repo root");
    Some((dir, root))
}

/// In worktree mode, the sequential worker's spawned `SessionSpec.cwd` is the
/// mission integration worktree (NOT `paths.repo_root`), and the primary
/// checkout's branch is unchanged across the whole mission — proving
/// `run()` never checks out the mission branch in the primary tree.
#[tokio::test(flavor = "multi_thread")]
async fn worker_session_cwd_is_worktree() {
    let Some((_dir, root)) = mission_init_repo() else {
        return;
    };

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script_complete_no_lesson(),
    ]));
    let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
    let mut engine =
        MissionEngine::create(backend_dyn, &root, GOAL, worktree_cfg()).expect("create engine");
    engine.approve_plan(one_feature_plan()).unwrap();
    // `approve_plan`'s primary-tree checkout + artifact commit is out of
    // scope for f-2-1 (later features route it through the integration
    // worktree too). Model the dispatcher boundary that already restores
    // the operator's checkout between `kranz draft` and `kranz work`
    // (see `run()`'s doc comment) so `run()` starts from a primary tree
    // that isn't sitting on the mission branch — exactly the real-world
    // precondition `setup_mission_worktree` needs.
    raw_git(&root, &["checkout", "main"]);
    let branch_before = raw_git(&root, &["branch", "--show-current"])
        .trim()
        .to_string();
    assert_eq!(branch_before, "main");

    let status = timeout(TokioDuration::from_secs(60), engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let specs = backend.started_specs();
    let worker_spec = specs
        .iter()
        .find(|s| matches!(s.prompt, PromptMode::SingleShot(ref t) if t.contains("Implement feature")))
        .expect("a worker spec was started");

    assert_ne!(
        worker_spec.cwd, root,
        "worktree mode must not spawn the worker in the primary repo root"
    );
    assert!(
        worker_spec
            .cwd
            .to_string_lossy()
            .contains(&engine.mission_id().to_string()),
        "worker cwd should be the mission's integration worktree: {:?}",
        worker_spec.cwd
    );
    assert!(
        worker_spec.cwd.to_string_lossy().contains("_integration"),
        "worker cwd should be the mission integration worktree path: {:?}",
        worker_spec.cwd
    );

    // The primary checkout never left its starting branch across the run.
    let branch_after = raw_git(&root, &["branch", "--show-current"])
        .trim()
        .to_string();
    assert_eq!(
        branch_before, branch_after,
        "worktree mode must never check out the mission branch in the primary tree"
    );
    assert_eq!(branch_after, "main");
}

/// In checkout mode (default), the sequential path is unchanged: the worker
/// spawns with cwd = `paths.repo_root`, and the primary checkout IS on the
/// mission branch after the run (legacy behavior preserved byte-for-byte).
#[tokio::test(flavor = "multi_thread")]
async fn checkout_mode_runs_worker_in_primary_root() {
    let Some((_dir, root)) = mission_init_repo() else {
        return;
    };

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script_complete_no_lesson(),
    ]));
    let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
    let mut engine =
        MissionEngine::create(backend_dyn, &root, GOAL, checkout_cfg()).expect("create engine");
    let mission_branch = engine.state().mission.mission_branch.clone();
    engine.approve_plan(one_feature_plan()).unwrap();

    let status = timeout(TokioDuration::from_secs(60), engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let specs = backend.started_specs();
    let worker_spec = specs
        .iter()
        .find(|s| matches!(s.prompt, PromptMode::SingleShot(ref t) if t.contains("Implement feature")))
        .expect("a worker spec was started");
    assert_eq!(
        worker_spec.cwd, root,
        "checkout mode must spawn the worker in the primary repo root"
    );

    let branch_after = raw_git(&root, &["branch", "--show-current"])
        .trim()
        .to_string();
    assert_eq!(
        branch_after, mission_branch,
        "checkout mode must leave the primary checkout on the mission branch"
    );
}
