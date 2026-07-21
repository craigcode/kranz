//! Tests for the `workerIsolation` mission-config key (M7 tier 1, feature f-1-1)
//! and the mission integration-worktree mode it enables (feature f-1-2).
//!
//! The config surface (f-1-1) and the git plumbing
//! (`GitRepo::add_worktree_checkout`, f-1-2) are fully wired: in worktree mode
//! `run()` routes mission-branch mutations through a dedicated integration
//! worktree (`setup_mission_worktree`/`teardown_mission_worktree` in
//! orchestrator.rs), worker/validator sessions run with that worktree as cwd,
//! `approve_plan` writes through it, and the primary checkout stays
//! byte-untouched for the whole mission.

use kranz_engine::auth_verify::AuthVerdict;
use kranz_engine::backend::{AgentBackend, PromptMode};
use kranz_engine::backend_mock::{
    mock_init, mock_result_error, mock_result_text, mock_text, MockBackend, MockScript,
};
use kranz_engine::config::load_layers;
use kranz_engine::git_ops::GitRepo;
use kranz_engine::orchestrator::MissionEngine;
use kranz_engine::types::{
    Assertion, AssertionCheck, MissionConfig, MissionStatus, Plan, PlanFeature, PlanMilestone,
    WorkerIsolation,
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

/// Native-shell assertion for the final-gate command environment. Contract
/// commands run through `sh` on Unix and `cmd` on Windows, so the variable
/// syntax must match the executor while asserting the same value everywhere.
/// Compare in the shell instead of redirecting to a capture file: cmd's
/// redirection parsing is sensitive to quoting and previously sent the test
/// into the failure-conversion dialogue on Windows, where its success-only
/// mock script correctly had no reply queued.
fn gate_base_sha_assertion_command(expected: &str) -> String {
    #[cfg(unix)]
    {
        format!("test \"$KRANZ_BASE_SHA\" = '{expected}'")
    }
    #[cfg(windows)]
    {
        format!("if \"%KRANZ_BASE_SHA%\"==\"{expected}\" (exit /b 0) else (exit /b 1)")
    }
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
fn worker_isolation_config_defaults_to_worktree() {
    assert_eq!(
        MissionConfig::default().worker_isolation,
        WorkerIsolation::Worktree
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let layer = write_layer(&dir, "config.json", r#"{"maxRespawns":3}"#);

    let cfg = load_layers(&[layer]).expect("load layers");
    assert_eq!(cfg.worker_isolation, WorkerIsolation::Worktree);
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
    assert_eq!(value["workerIsolation"], "worktree");
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
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec![],
    }
}

/// Worker script: completed single-shot run whose final text is a passing
/// WorkerReport. Writes a unique file into the session cwd so the worker
/// leaves a dirty tree behind (§4.4), which the engine checkpoints as a
/// real, non-meta commit on the mission branch. The path is unique per
/// worker (atomic counter) so worktree-mode's per-feature branch merges
/// never collide on the same path.
fn worker_pass() -> MockScript {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = format!("delivered-{n}.txt");
    MockScript::single_shot_json(&json!({
        "result": "pass",
        "summary": "implemented and tested",
        "filesTouched": [path],
        "testsAdded": [],
        "testEvidence": "all green",
        "commits": []
    }))
    .writes_file(&path, "delivered by the mock worker\n")
}

fn judgement_complete() -> String {
    json!({ "decision": "complete", "guidance": "", "summary": "worker judged: complete" })
        .to_string()
}

/// Dirty-tree-turn reply (§4.4): the worker left uncommitted changes; commit
/// them as-is so they land on the mission branch.
fn dirty_tree_commit_as_is() -> String {
    json!({ "action": "commit-as-is", "note": "worker delivered files" }).to_string()
}

/// Orchestrator streaming script: seed, then the dirty-tree turn (the
/// sequential worker's file write), then one judgement turn, then the
/// (empty-contract) capture turn replying NONE.
fn orch_script_complete_no_lesson() -> MockScript {
    MockScript::streaming(vec![mock_init("orch-session"), mock_result_text("ready")]).responding(
        vec![
            vec![
                mock_text(&dirty_tree_commit_as_is()),
                mock_result_text(&dirty_tree_commit_as_is()),
            ],
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
        worker_isolation: WorkerIsolation::Checkout,
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
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    engine.approve_plan(one_feature_plan()).unwrap();
    // `approve_plan` (f-2-3) never checks out the mission branch in the
    // primary tree in worktree mode, so the primary is already on `main`
    // here; this is just belt-and-suspenders (a no-op checkout of the
    // branch already checked out).
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
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
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

// -----------------------------------------------------------------------
// f-2-2: validators, parallel merge-back, milestone tags, and the final
// gate routed through the integration worktree in worktree mode.
// -----------------------------------------------------------------------

fn validator_findings_empty() -> MockScript {
    MockScript::single_shot_json(&json!({
        "findings": [],
        "summary": "clean"
    }))
}

/// In worktree mode, a spawned validator's `SessionSpec.cwd` is the mission
/// integration worktree (not `paths.repo_root`) — mirrors
/// `worker_session_cwd_is_worktree` but for `run_validator_in`.
#[tokio::test(flavor = "multi_thread")]
async fn validator_session_cwd_is_worktree() {
    let Some((_dir, root)) = mission_init_repo() else {
        return;
    };

    let mut cfg = worktree_cfg();
    cfg.skip_scrutiny = false; // only scrutiny runs; functional stays skipped

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script_complete_no_lesson(),
        validator_findings_empty(),
    ]));
    let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
    let mut engine = MissionEngine::create(backend_dyn, &root, GOAL, cfg).expect("create engine");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    engine.approve_plan(one_feature_plan()).unwrap();
    raw_git(&root, &["checkout", "main"]);

    let status = timeout(TokioDuration::from_secs(60), engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let specs = backend.started_specs();
    let validator_spec = specs
        .iter()
        .find(|s| matches!(s.prompt, PromptMode::SingleShot(ref t) if t.contains("Validate milestone")))
        .expect("a validator spec was started");

    assert_ne!(
        validator_spec.cwd, root,
        "worktree mode must not spawn the validator in the primary repo root"
    );
    assert!(
        validator_spec
            .cwd
            .to_string_lossy()
            .contains("_integration"),
        "validator cwd should be the mission integration worktree path: {:?}",
        validator_spec.cwd
    );

    // The primary checkout never left its starting branch across the run.
    let branch_after = raw_git(&root, &["branch", "--show-current"])
        .trim()
        .to_string();
    assert_eq!(
        branch_after, "main",
        "worktree mode must never check out the mission branch in the primary tree"
    );
}

/// In worktree mode, `KRANZ_BASE_SHA` reaches the worker session env, the
/// validator session env, and the final-gate contract-command env, all
/// equal to the pinned base sha — proving the shared `contract_env`
/// constructor is fed the same base sha regardless of which tree the
/// command/session actually runs in.
#[tokio::test(flavor = "multi_thread")]
async fn base_sha_reaches_sessions_in_worktree_mode() {
    let Some((_dir, root)) = mission_init_repo() else {
        return;
    };
    let expected_base_sha = GitRepo::open(&root)
        .expect("open repo")
        .head_sha()
        .expect("seed sha");

    let mut cfg = worktree_cfg();
    cfg.skip_scrutiny = false; // only scrutiny runs; functional stays skipped

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script_complete_no_lesson(),
        validator_findings_empty(),
    ]));
    let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
    let mut engine = MissionEngine::create(backend_dyn, &root, GOAL, cfg).expect("create engine");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);

    let mut plan = one_feature_plan();
    plan.validation_contract.push(Assertion {
        id: "assert-base-sha".to_string(),
        statement: "the final gate command env carries KRANZ_BASE_SHA".to_string(),
        check: AssertionCheck::Command,
        command: Some(gate_base_sha_assertion_command(&expected_base_sha)),
    });
    engine.approve_plan(plan).unwrap();
    raw_git(&root, &["checkout", "main"]);

    let base_sha = engine
        .state()
        .mission
        .base_sha
        .clone()
        .expect("mission must pin a base sha at approval");
    assert_eq!(base_sha, expected_base_sha);

    let status = timeout(TokioDuration::from_secs(60), engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(
        status,
        MissionStatus::Complete,
        "completion proves the final-gate equality command observed the pinned base sha"
    );

    let specs = backend.started_specs();
    let worker_spec = specs
        .iter()
        .find(|s| matches!(s.prompt, PromptMode::SingleShot(ref t) if t.contains("Implement feature")))
        .expect("a worker spec was started");
    assert_eq!(
        worker_spec.env.get("KRANZ_BASE_SHA"),
        Some(&base_sha),
        "worker session env must carry KRANZ_BASE_SHA"
    );

    let validator_spec = specs
        .iter()
        .find(|s| matches!(s.prompt, PromptMode::SingleShot(ref t) if t.contains("Validate milestone")))
        .expect("a validator spec was started");
    assert_eq!(
        validator_spec.env.get("KRANZ_BASE_SHA"),
        Some(&base_sha),
        "validator session env must carry KRANZ_BASE_SHA"
    );
}

// -----------------------------------------------------------------------
// f-2-3: committed artifacts + approval route to the worktree; the primary
// checkout stays byte-untouched across an entire mission in worktree mode.
// -----------------------------------------------------------------------

/// End-to-end (approval through completion): in worktree mode the primary
/// checkout's branch, HEAD sha, and tracked-tree status are byte-identical
/// before and after the whole mission — proving neither `approve_plan` nor
/// `write_mission_report` (nor anything else `run()` does) ever checks out
/// or commits in the primary tree.
#[tokio::test(flavor = "multi_thread")]
async fn primary_checkout_untouched_in_worktree_mode() {
    let Some((_dir, root)) = mission_init_repo() else {
        return;
    };
    let repo = GitRepo::open(&root).expect("open repo");

    let branch_before = repo.current_branch().unwrap();
    let head_before = repo.head_sha().unwrap();
    let status_before = raw_git(&root, &["status", "--porcelain", "--untracked-files=no"]);

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script_complete_no_lesson(),
    ]));
    let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
    let mut engine =
        MissionEngine::create(backend_dyn, &root, GOAL, worktree_cfg()).expect("create engine");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    engine.approve_plan(one_feature_plan()).unwrap();

    let status = timeout(TokioDuration::from_secs(60), engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let branch_after = repo.current_branch().unwrap();
    let head_after = repo.head_sha().unwrap();
    let status_after = raw_git(&root, &["status", "--porcelain", "--untracked-files=no"]);

    assert_eq!(
        branch_before, branch_after,
        "primary checkout must never change branches across the whole mission"
    );
    assert_eq!(
        head_before, head_after,
        "primary HEAD sha must be unchanged across the whole mission"
    );
    assert_eq!(
        status_before, status_after,
        "primary tracked-tree status must be byte-identical across the whole mission"
    );
}

/// Regression guard: with `.kranz/missions/index.md` TRACKED and committed
/// on the default branch (a repo with merged missions), a worktree-mode
/// mission must not dirty any tracked file in the PRIMARY checkout.
/// `approve_plan` once wrote the missions catalog into the primary runtime
/// dir, leaving a tracked ` M .kranz/missions/index.md` behind that tripped
/// the worktree-mode cleanliness sweep — the catalog is committed on the
/// mission branch only, never rewritten on the primary.
#[tokio::test(flavor = "multi_thread")]
async fn tracked_missions_index_not_dirtied_in_primary_in_worktree_mode() {
    let Some((_dir, root)) = mission_init_repo() else {
        return;
    };
    // Track the missions catalog on main, as in a repo with merged missions.
    let index_rel = ".kranz/missions/index.md";
    let index_path = root.join(index_rel);
    std::fs::create_dir_all(index_path.parent().unwrap()).unwrap();
    let seeded_index = "# Missions\n\n- 2026-01-01 — earlier mission ([plan](m-old/plan.md))\n";
    std::fs::write(&index_path, seeded_index).unwrap();
    raw_git(&root, &["add", index_rel]);
    raw_git(&root, &["commit", "-m", "track missions catalog"]);

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script_complete_no_lesson(),
    ]));
    let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
    let mut engine =
        MissionEngine::create(backend_dyn, &root, GOAL, worktree_cfg()).expect("create engine");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    engine.approve_plan(one_feature_plan()).unwrap();

    // The regression fired at approval time, so pin approve_plan itself
    // before running the rest of the mission.
    assert_eq!(
        raw_git(&root, &["diff", "--name-only", "HEAD"]).trim(),
        "",
        "approve_plan must not modify tracked files in the primary checkout"
    );

    let status = timeout(TokioDuration::from_secs(60), engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    // No tracked file in the primary checkout was modified by the whole
    // mission (untracked runtime twins like plan.json are expected and fine).
    assert_eq!(
        raw_git(&root, &["diff", "--name-only", "HEAD"]).trim(),
        "",
        "the mission must not modify tracked files in the primary checkout"
    );
    assert_eq!(
        std::fs::read_to_string(&index_path).unwrap(),
        seeded_index,
        "the tracked missions catalog in the primary checkout must be byte-identical"
    );
}

/// After the mission, the mission branch tip carries the committed plan.json
/// and report.md (and the engine's approval/report commits), while the
/// primary HEAD is unchanged from before the mission — and human-readable
/// plan.md/report.md twins are readable via the primary runtime dir.
#[tokio::test(flavor = "multi_thread")]
async fn mission_branch_carries_deliverables_in_worktree_mode() {
    let Some((_dir, root)) = mission_init_repo() else {
        return;
    };
    let repo = GitRepo::open(&root).expect("open repo");
    let head_before = repo.head_sha().unwrap();

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script_complete_no_lesson(),
    ]));
    let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
    let mut engine =
        MissionEngine::create(backend_dyn, &root, GOAL, worktree_cfg()).expect("create engine");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    let mission_branch = engine.state().mission.mission_branch.clone();
    let mission_id = engine.mission_id().to_string();
    engine.approve_plan(one_feature_plan()).unwrap();

    let status = timeout(TokioDuration::from_secs(60), engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    // Primary HEAD/branch unchanged from before the mission.
    assert_eq!(repo.head_sha().unwrap(), head_before);
    assert_eq!(repo.current_branch().unwrap(), "main");

    // The mission branch tip carries the committed deliverables.
    let plan_json = raw_git(
        &root,
        &[
            "show",
            &format!("{mission_branch}:.kranz/missions/{mission_id}/plan.json"),
        ],
    );
    assert!(
        !plan_json.trim().is_empty(),
        "plan.json must be committed on the mission branch"
    );
    let report_md = raw_git(
        &root,
        &[
            "show",
            &format!("{mission_branch}:.kranz/missions/{mission_id}/report.md"),
        ],
    );
    assert!(
        !report_md.trim().is_empty(),
        "report.md must be committed on the mission branch"
    );

    // The engine's own approval + report commits (this feature's git-side
    // mutations) landed on the mission branch, never on the primary tree.
    let log = raw_git(&root, &["log", "--format=%s", &mission_branch]);
    assert!(
        log.contains(&format!("approved plan for {mission_id}")),
        "mission branch log missing the plan-approval commit: {log}"
    );
    assert!(
        log.contains(&format!("mission report for {mission_id}")),
        "mission branch log missing the mission-report commit: {log}"
    );

    // Deliverables stay readable: untracked twins in the primary runtime
    // dir (never committed there — canonical copies are on the mission branch).
    let mission_dir = root.join(".kranz/missions").join(&mission_id);
    assert!(
        mission_dir.join("plan.json").is_file(),
        "plan.json twin must be readable in the primary runtime dir"
    );
    assert!(
        mission_dir.join("plan.md").is_file(),
        "plan.md twin must be readable in the primary runtime dir"
    );
    assert!(
        mission_dir.join("report.md").is_file(),
        "report.md twin must be readable in the primary runtime dir"
    );
}

/// A single-milestone, single-feature plan whose validation contract has one
/// command assertion that already passes on the untouched base (`true`) and
/// one that correctly fails there (`false`).
fn one_feature_plan_with_contract() -> Plan {
    Plan {
        validation_contract: vec![
            Assertion {
                id: "a-1".to_string(),
                statement: "vacuous assertion".to_string(),
                check: AssertionCheck::Command,
                command: Some("true".to_string()),
            },
            Assertion {
                id: "a-2".to_string(),
                statement: "not-yet-landed assertion".to_string(),
                check: AssertionCheck::Command,
                command: Some("false".to_string()),
            },
        ],
        ..one_feature_plan()
    }
}

/// finding a2 / a6 / f-1-2: in worktree mode, `approve_plan` still lints the
/// contract's command assertions against the untouched base tree and commits
/// the '## Contract lint' section in plan.md on the mission branch (and its
/// untracked primary twin) — not just in the non-worktree/checkout path.
#[tokio::test(flavor = "multi_thread")]
async fn approval_lint_covers_worktree_mode() {
    let Some((_dir, root)) = mission_init_repo() else {
        return;
    };
    let backend = Arc::new(MockBackend::new());
    let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
    let mut engine =
        MissionEngine::create(backend_dyn, &root, GOAL, worktree_cfg()).expect("create engine");
    let mission_branch = engine.state().mission.mission_branch.clone();
    let mission_id = engine.mission_id().to_string();

    engine
        .approve_plan(one_feature_plan_with_contract())
        .unwrap();

    let committed_md = raw_git(
        &root,
        &[
            "show",
            &format!("{mission_branch}:.kranz/missions/{mission_id}/plan.md"),
        ],
    );
    assert!(committed_md.contains("## Contract lint"), "{committed_md}");
    assert!(
        committed_md
            .contains("author-bug suspects (already pass / no verdict on the untouched base)"),
        "{committed_md}"
    );
    assert!(committed_md.contains("[a-1] true"), "{committed_md}");
    assert!(
        committed_md.contains("base-expected-to-fail (benign): [a-2] false"),
        "{committed_md}"
    );

    let primary_twin = root
        .join(".kranz/missions")
        .join(&mission_id)
        .join("plan.md");
    let twin_md = std::fs::read_to_string(&primary_twin).expect("primary plan.md twin readable");
    assert!(twin_md.contains("## Contract lint"), "{twin_md}");
    assert!(twin_md.contains("[a-1] true"), "{twin_md}");
}

// -----------------------------------------------------------------------
// f-3-1: end-to-end guarantees across a MULTI-feature/MULTI-milestone
// mission that exercises BOTH the sequential and parallel-batch paths,
// leak-free cleanup, and the checkout-mode regression guard.
// -----------------------------------------------------------------------

/// Two milestones: M1 has two plan-origin features (the parallel-batch
/// candidate pair), M2 has one (always sequential — `try_parallel_batch`
/// short-circuits below 2 candidates). Feature ids follow the reducer's
/// `f-<milestone-number>-<feature-number>` scheme: f-1-1, f-1-2, f-2-1.
fn two_milestone_plan() -> Plan {
    Plan {
        goal: GOAL.to_string(),
        validation_contract: vec![],
        milestones: vec![
            PlanMilestone {
                title: "M1".to_string(),
                features: vec![
                    PlanFeature {
                        title: "feature 1".to_string(),
                        spec: "build part 1".to_string(),
                        validation_criteria: vec!["part 1 works".to_string()],
                    },
                    PlanFeature {
                        title: "feature 2".to_string(),
                        spec: "build part 2".to_string(),
                        validation_criteria: vec!["part 2 works".to_string()],
                    },
                ],
            },
            PlanMilestone {
                title: "M2".to_string(),
                features: vec![PlanFeature {
                    title: "feature 3".to_string(),
                    spec: "build part 3".to_string(),
                    validation_criteria: vec!["part 3 works".to_string()],
                }],
            },
        ],
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec![],
    }
}

/// Parallelization-decision reply (roadmap M3): the listed feature ids are
/// independent and merge in the given order.
fn parallel_plan(ids: &[&str]) -> String {
    json!({
        "independent": ids,
        "mergeOrder": ids,
        "summary": format!("{} features are independent", ids.len())
    })
    .to_string()
}

/// General streaming orchestrator script: one entry per engine turn after
/// the seed (mirrors `orch_script` in mission_test.rs / soak_test.rs).
fn orch_multi_script(replies: Vec<String>) -> MockScript {
    MockScript::streaming(vec![mock_init("orch-session"), mock_result_text("ready")]).responding(
        replies
            .iter()
            .map(|reply| vec![mock_text(reply), mock_result_text(reply)])
            .collect(),
    )
}

/// Scripts for the two-milestone mission: orchestrator (parallel-plan for
/// M1, one judgement turn per feature, then the capture-lesson turn), then
/// one single-shot worker script per feature (3 total — 2 parallel, 1
/// sequential; the mock backend pops these FIFO regardless of which feature
/// binds to which, so all three being identical `worker_pass()` scripts is
/// sufficient).
fn two_milestone_scripts() -> Vec<MockScript> {
    vec![
        orch_multi_script(vec![
            parallel_plan(&["f-1-1", "f-1-2"]),
            judgement_complete(),
            judgement_complete(),
            dirty_tree_commit_as_is(),
            judgement_complete(),
            "NONE".to_string(),
        ]),
        worker_pass(),
        worker_pass(),
        worker_pass(),
    ]
}

/// `worktrees_removed_at_mission_end_in_worktree_mode`: after a completed
/// worktree-mode mission that runs BOTH the sequential path (M2's single
/// feature) and the parallel-batch path (M1's two independent features,
/// `maxParallelWorkers=2`), `list_worktrees()` shows only the primary
/// working tree — the integration worktree and every per-feature worktree
/// are gone, `prune_worktrees` leaves no dangling admin records, and nothing
/// leaked into the temp dir.
#[tokio::test(flavor = "multi_thread")]
async fn worktrees_removed_at_mission_end_in_worktree_mode() {
    let Some((_dir, root)) = mission_init_repo() else {
        return;
    };
    let repo = GitRepo::open(&root).expect("open repo");

    let backend = Arc::new(MockBackend::with_scripts(two_milestone_scripts()));
    let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
    let mut cfg = worktree_cfg();
    cfg.max_parallel_workers = 2;
    let mut engine = MissionEngine::create(backend_dyn, &root, GOAL, cfg).expect("create engine");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    let mission_id = engine.mission_id().to_string();
    engine.approve_plan(two_milestone_plan()).unwrap();

    let status = timeout(TokioDuration::from_secs(90), engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    // Positively prove the parallel-batch path actually engaged for M1
    // BEFORE asserting cleanup: at least one worker spawned in a per-feature
    // parallel worktree (distinct from the "_integration" tree) named after
    // one of M1's independent features. Without this, the cleanup assertions
    // below would pass vacuously even if the parallel path silently ran
    // sequentially instead.
    let specs = backend.started_specs();
    let parallel_worker_cwd = specs
        .iter()
        .filter(|s| matches!(s.prompt, PromptMode::SingleShot(ref t) if t.contains("Implement feature")))
        .find(|s| {
            let cwd = s.cwd.to_string_lossy();
            (cwd.contains(&format!("-{mission_id}-f-1-1"))
                || cwd.contains(&format!("-{mission_id}-f-1-2")))
                && !cwd.contains("_integration")
        });
    assert!(
        parallel_worker_cwd.is_some(),
        "expected at least one M1 worker to run in a per-feature parallel worktree \
         (*-{mission_id}-f-1-1 or -f-1-2), proving the parallel-batch path engaged: {:?}",
        specs.iter().map(|s| &s.cwd).collect::<Vec<_>>()
    );

    // Only the primary working tree remains registered.
    let worktrees = repo.list_worktrees().unwrap();
    assert_eq!(
        worktrees.len(),
        1,
        "only the primary worktree remains: {worktrees:?}"
    );

    // `prune_worktrees` is a no-op (idempotent) and leaves no dangling admin
    // records under `.git/worktrees`.
    repo.prune_worktrees().expect("prune");
    let worktrees_after_prune = repo.list_worktrees().unwrap();
    assert_eq!(worktrees_after_prune.len(), 1);
    let admin_dir = root.join(".git").join("worktrees");
    if admin_dir.is_dir() {
        let leftover: Vec<_> = std::fs::read_dir(&admin_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(
            leftover.is_empty(),
            "dangling worktree admin records remain: {leftover:?}"
        );
    }

    // Per-feature worktree branches were cleaned up.
    for fid in ["f-1-1", "f-1-2"] {
        assert!(
            !repo
                .branch_exists(&format!("kranz/wt/{mission_id}/{fid}"))
                .unwrap_or(false),
            "per-feature worktree branch {fid} must be deleted"
        );
    }

    // No leaked worktree dir (parallel OR integration) for this mission.
    let leak_marker = format!("-{mission_id}-");
    for entry in std::fs::read_dir(std::env::temp_dir()).unwrap().flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(
            !(name.starts_with("kranz-wt-") && name.contains(&leak_marker)),
            "a worktree dir leaked into temp: {name}"
        );
    }
}

/// `checkout_mode_matches_legacy_sequential`: the SAME multi-feature/
/// multi-milestone mission (including the parallel-batch path — which
/// always uses its own per-feature worktrees, regardless of
/// `workerIsolation`, per roadmap M3) run in checkout mode (default)
/// preserves legacy invariants: the mission branch is checked out in the
/// primary tree for the whole run, the SEQUENTIAL feature's worker spawns
/// with cwd = repo_root, and the primary tree ends on the mission branch.
#[tokio::test(flavor = "multi_thread")]
async fn checkout_mode_matches_legacy_sequential() {
    let Some((_dir, root)) = mission_init_repo() else {
        return;
    };

    let mut cfg = checkout_cfg();
    cfg.max_parallel_workers = 2;

    let backend = Arc::new(MockBackend::with_scripts(two_milestone_scripts()));
    let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
    let mut engine = MissionEngine::create(backend_dyn, &root, GOAL, cfg).expect("create engine");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    let mission_branch = engine.state().mission.mission_branch.clone();
    engine.approve_plan(two_milestone_plan()).unwrap();

    // Legacy invariant: `approve_plan` checks the mission branch out in the
    // primary tree BEFORE `run()` even starts, and nothing in checkout mode
    // ever moves it off that branch again.
    let branch_right_after_approval = raw_git(&root, &["branch", "--show-current"])
        .trim()
        .to_string();
    assert_eq!(branch_right_after_approval, mission_branch);

    let status = timeout(TokioDuration::from_secs(90), engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let specs = backend.started_specs();
    let worker_specs: Vec<_> = specs
        .iter()
        .filter(|s| matches!(s.prompt, PromptMode::SingleShot(ref t) if t.contains("Implement feature")))
        .collect();
    assert_eq!(worker_specs.len(), 3, "three worker sessions ran");
    // M2's single feature cannot start until M1 (the parallel batch) fully
    // completes, so the LAST worker spec started is the sequential one.
    let sequential_worker = worker_specs.last().expect("a sequential worker ran");
    assert_eq!(
        sequential_worker.cwd, root,
        "checkout mode must still spawn the sequential feature's worker in the primary repo root"
    );

    let branch_after = raw_git(&root, &["branch", "--show-current"])
        .trim()
        .to_string();
    assert_eq!(
        branch_after, mission_branch,
        "checkout mode must leave the primary checkout on the mission branch"
    );
}

/// End-to-end (approval through re-plan): in worktree mode
/// `approve_revised_plan` — the branch f-3-1 added that routes
/// revised-plan.md through `setup_mission_worktree` → commit →
/// `teardown_mission_worktree` — leaves the primary checkout's branch, HEAD
/// sha, and tracked porcelain status byte-identical, commits revised-plan.md
/// on the mission branch (never in the primary tree's HEAD), and leaks no
/// worktree. This exercises the revision leg of a1's "approval through
/// completion" guarantee in worktree mode, which was previously only
/// exercised in checkout mode (`mission_test.rs`'s `approve_revised_plan`
/// tests all use the default `WorkerIsolation::Checkout`).
#[tokio::test(flavor = "multi_thread")]
async fn approve_revised_plan_untouched_primary_in_worktree_mode() {
    let Some((_dir, root)) = mission_init_repo() else {
        return;
    };
    let repo = GitRepo::open(&root).expect("open repo");

    // Revised plan the orchestrator proposes: same single milestone "M1",
    // "feature 1" kept, a new "extra feature" added.
    let revised_json = json!({
        "goal": GOAL,
        "validationContract": [],
        "milestones": [{
            "title": "M1",
            "features": [
                { "title": "feature 1", "spec": "build part 1", "validationCriteria": ["part 1 works"] },
                { "title": "extra feature", "spec": "build the newly-needed part", "validationCriteria": ["extra works"] }
            ]
        }]
    })
    .to_string();

    let backend = Arc::new(MockBackend::with_scripts(vec![orch_multi_script(vec![
        revised_json,
    ])]));
    let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
    let mut engine =
        MissionEngine::create(backend_dyn, &root, GOAL, worktree_cfg()).expect("create engine");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    let mission_branch = engine.state().mission.mission_branch.clone();
    let mission_id = engine.mission_id().to_string();
    engine.approve_plan(one_feature_plan()).unwrap();

    // Requesting the revision spawns the orchestrator (one `WorkerSpawned`
    // event), which flips the mission from `Approved` to `Running` — the
    // state `approve_revised_plan` requires. Neither step touches the
    // primary tree in worktree mode.
    let request = timeout(TokioDuration::from_secs(60), engine.request_revised_plan())
        .await
        .expect("request_revised_plan must not hang")
        .expect("scripted plan JSON is not a backend error");
    let plan = match request {
        kranz_engine::orchestrator::PlanRequest::Ready(plan) => plan,
        kranz_engine::orchestrator::PlanRequest::NotReady(text) => {
            panic!("scripted revised plan must parse: {text}")
        }
    };

    // Snapshot the primary checkout right before the call under test.
    let branch_before = repo.current_branch().unwrap();
    let head_before = repo.head_sha().unwrap();
    let status_before = raw_git(&root, &["status", "--porcelain", "--untracked-files=no"]);

    engine
        .approve_revised_plan(plan)
        .expect("apply the revised plan");

    // The primary checkout is untouched by the revision.
    assert_eq!(
        repo.current_branch().unwrap(),
        branch_before,
        "approve_revised_plan must never change the primary checkout's branch"
    );
    assert_eq!(
        repo.head_sha().unwrap(),
        head_before,
        "approve_revised_plan must never move the primary HEAD"
    );
    assert_eq!(
        raw_git(&root, &["status", "--porcelain", "--untracked-files=no"]),
        status_before,
        "approve_revised_plan must never dirty the primary tracked tree"
    );

    // revised-plan.md is committed on the mission branch...
    let revised_md_path = format!(".kranz/missions/{mission_id}/revised-plan.md");
    let committed = raw_git(
        &root,
        &["show", &format!("{mission_branch}:{revised_md_path}")],
    );
    assert!(
        !committed.trim().is_empty(),
        "revised-plan.md must be committed on the mission branch"
    );

    // ...but is NOT committed in the primary tree's HEAD (it may exist only
    // as an untracked twin on disk).
    let show_on_primary_head = Command::new("git")
        .args(["show", &format!("{branch_before}:{revised_md_path}")])
        .current_dir(&root)
        .output()
        .expect("spawn git show");
    assert!(
        !show_on_primary_head.status.success(),
        "revised-plan.md must not be committed on the primary tree's HEAD ({branch_before})"
    );

    // No integration worktree leaked.
    let worktrees = repo.list_worktrees().unwrap();
    assert_eq!(
        worktrees.len(),
        1,
        "only the primary worktree remains: {worktrees:?}"
    );
    let leak_marker = format!("-{mission_id}-");
    for entry in std::fs::read_dir(std::env::temp_dir()).unwrap().flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(
            !(name.starts_with("kranz-wt-") && name.contains(&leak_marker)),
            "a worktree dir leaked into temp: {name}"
        );
    }
}

/// Strengthens a1/a6/a7 to a MULTI-feature, MULTI-milestone worktree-mode
/// mission that exercises the parallel-batch path (M1, `maxParallelWorkers=2`)
/// as well as the sequential path (M2): the primary checkout stays
/// byte-untouched for the whole run (a1), the mission branch tip carries the
/// engine's deliverable commits and BOTH milestone tags (a6), and
/// `KRANZ_BASE_SHA` reaches every worker session env and the final-gate
/// contract-command env (a7).
#[tokio::test(flavor = "multi_thread")]
async fn multi_milestone_worktree_mode_preserves_a1_a6_a7() {
    let Some((_dir, root)) = mission_init_repo() else {
        return;
    };
    let repo = GitRepo::open(&root).expect("open repo");
    let branch_before = repo.current_branch().unwrap();
    let head_before = repo.head_sha().unwrap();
    let status_before = raw_git(&root, &["status", "--porcelain", "--untracked-files=no"]);

    let expected_base_sha = repo.head_sha().expect("seed sha");

    let mut cfg = worktree_cfg();
    cfg.max_parallel_workers = 2;

    let backend = Arc::new(MockBackend::with_scripts(two_milestone_scripts()));
    let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
    let mut engine = MissionEngine::create(backend_dyn, &root, GOAL, cfg).expect("create engine");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    let mission_branch = engine.state().mission.mission_branch.clone();
    let mission_id = engine.mission_id().to_string();

    let mut plan = two_milestone_plan();
    plan.validation_contract.push(Assertion {
        id: "assert-base-sha".to_string(),
        statement: "the final gate command env carries KRANZ_BASE_SHA".to_string(),
        check: AssertionCheck::Command,
        command: Some(gate_base_sha_assertion_command(&expected_base_sha)),
    });
    engine.approve_plan(plan).unwrap();

    let base_sha = engine
        .state()
        .mission
        .base_sha
        .clone()
        .expect("mission must pin a base sha at approval");
    assert_eq!(base_sha, expected_base_sha);

    let status = timeout(TokioDuration::from_secs(90), engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(
        status,
        MissionStatus::Complete,
        "completion proves the final-gate equality command observed the pinned base sha"
    );

    // a1: primary checkout byte-untouched across the whole multi-feature,
    // multi-milestone, parallel+sequential mission.
    assert_eq!(
        repo.current_branch().unwrap(),
        branch_before,
        "primary checkout must never change branches across the whole mission"
    );
    assert_eq!(
        repo.head_sha().unwrap(),
        head_before,
        "primary HEAD sha must be unchanged across the whole mission"
    );
    assert_eq!(
        raw_git(&root, &["status", "--porcelain", "--untracked-files=no"]),
        status_before,
        "primary tracked-tree status must be byte-identical across the whole mission"
    );

    // a6: the engine's own commits and both milestone tags landed on the
    // mission branch.
    let log = raw_git(&root, &["log", "--format=%s", &mission_branch]);
    assert!(
        log.contains(&format!("approved plan for {mission_id}")),
        "mission branch log missing the plan-approval commit: {log}"
    );
    assert!(
        log.contains(&format!("mission report for {mission_id}")),
        "mission branch log missing the mission-report commit: {log}"
    );
    let tags = raw_git(&root, &["tag", "--list", &format!("kranz/{mission_id}/*")]);
    assert!(
        tags.contains(&format!("kranz/{mission_id}/ms-1")),
        "M1's milestone tag missing: {tags}"
    );
    assert!(
        tags.contains(&format!("kranz/{mission_id}/ms-2")),
        "M2's milestone tag missing: {tags}"
    );

    // a7: KRANZ_BASE_SHA reached every worker session env...
    let specs = backend.started_specs();
    let worker_specs: Vec<_> = specs
        .iter()
        .filter(|s| matches!(s.prompt, PromptMode::SingleShot(ref t) if t.contains("Implement feature")))
        .collect();
    assert_eq!(worker_specs.len(), 3, "three worker sessions ran");
    for spec in &worker_specs {
        assert_eq!(
            spec.env.get("KRANZ_BASE_SHA"),
            Some(&base_sha),
            "every worker session env must carry KRANZ_BASE_SHA"
        );
    }
    // No worktrees leaked either.
    let worktrees = repo.list_worktrees().unwrap();
    assert_eq!(
        worktrees.len(),
        1,
        "only the primary worktree remains: {worktrees:?}"
    );
}

// -----------------------------------------------------------------------
// f-2-1: wiring the real auth preflight into both spawn paths, cached once
// per mission.
// -----------------------------------------------------------------------

/// A completed single-shot preflight probe session whose reply authenticates
/// (mirrors [`crate::auth_verify`]'s own `verify_worker_auth_normal_reply_is_authenticated`
/// fixture): init → assistant text "ack" → a non-error result.
fn preflight_authenticated_script() -> MockScript {
    MockScript::single_shot("ack")
}

/// A completed single-shot preflight probe session carrying the "Not logged
/// in" auth-failure signature, which `verify_worker_auth` classifies as
/// `Unauthenticated` regardless of cost/activity.
fn preflight_unauthenticated_script() -> MockScript {
    MockScript {
        events: vec![
            mock_init("preflight-session"),
            mock_result_error("Not logged in"),
        ],
        ..Default::default()
    }
}

/// Every `SessionSpec` in `specs` whose prompt is the auth-preflight probe
/// (see `crate::auth_verify::probe_spec`): a single-shot "Reply with the
/// single word: ack." prompt, distinguishable from every worker/orchestrator
/// prompt in this test suite.
fn preflight_probe_specs(
    specs: &[kranz_engine::backend::SessionSpec],
) -> Vec<&kranz_engine::backend::SessionSpec> {
    specs
        .iter()
        .filter(|s| {
            matches!(&s.prompt, PromptMode::SingleShot(t) if t.contains("Reply with the single word: ack"))
        })
        .collect()
}

fn worker_specs(
    specs: &[kranz_engine::backend::SessionSpec],
) -> Vec<&kranz_engine::backend::SessionSpec> {
    specs
        .iter()
        .filter(
            |s| matches!(&s.prompt, PromptMode::SingleShot(t) if t.contains("Implement feature")),
        )
        .collect()
}

/// `worker_auth_preflight_cached_once_per_mission`: a multi-feature,
/// multi-milestone mission (M1 parallel-batch, two workers; M2 sequential,
/// one worker — three workers total, exercising both spawn paths) drives the
/// auth preflight exactly once, and every worker shares that one decision.
/// An `Authenticated` preflight verdict means every one of the three worker
/// specs relocates `HOME`/`CLAUDE_CONFIG_DIR` — proving the decision was
/// reused, not recomputed (and silently flipping) per worker.
#[tokio::test(flavor = "multi_thread")]
async fn worker_auth_preflight_cached_once_per_mission() {
    let Some((_dir, root)) = mission_init_repo() else {
        return;
    };

    let mut scripts = vec![
        // orchestrator's own long-lived streaming session, exactly as
        // `two_milestone_scripts()` builds it.
        orch_multi_script(vec![
            parallel_plan(&["f-1-1", "f-1-2"]),
            judgement_complete(),
            judgement_complete(),
            dirty_tree_commit_as_is(),
            judgement_complete(),
            "NONE".to_string(),
        ]),
        // The ONE preflight probe: consumed by whichever spawn path runs
        // first (M1's parallel batch), before any worker session starts.
        preflight_authenticated_script(),
    ];
    scripts.extend((0..3).map(|_| worker_pass()));

    let backend = Arc::new(MockBackend::with_scripts(scripts));
    let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
    let mut cfg = worktree_cfg();
    cfg.max_parallel_workers = 2;
    let mut engine = MissionEngine::create(backend_dyn, &root, GOAL, cfg).expect("create engine");
    engine.approve_plan(two_milestone_plan()).unwrap();

    let status = timeout(TokioDuration::from_secs(90), engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let specs = backend.started_specs();

    let probes = preflight_probe_specs(&specs);
    assert_eq!(
        probes.len(),
        1,
        "the auth preflight must be driven exactly once per mission, regardless of \
         how many workers spawn: {:?}",
        specs.iter().map(|s| &s.prompt).collect::<Vec<_>>()
    );

    let workers = worker_specs(&specs);
    assert_eq!(
        workers.len(),
        3,
        "expected three worker sessions (two parallel + one sequential)"
    );
    for spec in &workers {
        assert!(
            spec.env.contains_key("HOME") && spec.env.contains_key("CLAUDE_CONFIG_DIR"),
            "every worker must share the single Authenticated decision and relocate HOME: {:?}",
            spec.env
        );
    }
}

/// `worker_auth_both_spawn_paths_gated`: both the live sequential spawn path
/// (`run_worker`/`run_worker_in`, M2's feature) and the buffered
/// parallel-batch spawn path (`run_worker_in_buffered`, M1's two features)
/// obtain their HOME relocate-vs-inherit decision from the SAME cached
/// preflight verdict, and relocate only when that verdict is `Authenticated`.
///
/// Runs the same multi-feature, multi-milestone mission twice: once with a
/// preflight that reports "Not logged in" (`Unauthenticated`) and once with a
/// preflight that authenticates. In the failure case every worker spec — on
/// both spawn paths — must omit `HOME`/`CLAUDE_CONFIG_DIR` entirely (the loud
/// fail-safe applying uniformly, mission m-165b6f); in the success case every
/// worker spec on both paths must carry the relocated pair.
#[tokio::test(flavor = "multi_thread")]
async fn worker_auth_both_spawn_paths_gated() {
    async fn run_mission_and_collect_worker_env_flags(preflight: MockScript) -> Vec<(bool, bool)> {
        let Some((_dir, root)) = mission_init_repo() else {
            return Vec::new();
        };

        let mut scripts = vec![
            orch_multi_script(vec![
                parallel_plan(&["f-1-1", "f-1-2"]),
                judgement_complete(),
                judgement_complete(),
                dirty_tree_commit_as_is(),
                judgement_complete(),
                "NONE".to_string(),
            ]),
            preflight,
        ];
        scripts.extend((0..3).map(|_| worker_pass()));

        let backend = Arc::new(MockBackend::with_scripts(scripts));
        let backend_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend) as Arc<dyn AgentBackend>;
        let mut cfg = worktree_cfg();
        cfg.max_parallel_workers = 2;
        let mut engine =
            MissionEngine::create(backend_dyn, &root, GOAL, cfg).expect("create engine");
        engine.approve_plan(two_milestone_plan()).unwrap();

        let status = timeout(TokioDuration::from_secs(90), engine.run())
            .await
            .expect("run must not hang")
            .unwrap();
        assert_eq!(status, MissionStatus::Complete);

        let specs = backend.started_specs();
        assert_eq!(
            preflight_probe_specs(&specs).len(),
            1,
            "exactly one preflight session per mission"
        );
        let workers = worker_specs(&specs);
        assert_eq!(
            workers.len(),
            3,
            "two parallel-batch + one sequential worker"
        );
        workers
            .iter()
            .map(|s| {
                (
                    s.env.contains_key("HOME"),
                    s.env.contains_key("CLAUDE_CONFIG_DIR"),
                )
            })
            .collect()
    }

    // Fail-safe: an Unauthenticated preflight means every worker on both
    // spawn paths inherits the real HOME — no HOME/CLAUDE_CONFIG_DIR key at
    // all, uniformly.
    let unauthenticated_flags =
        run_mission_and_collect_worker_env_flags(preflight_unauthenticated_script()).await;
    if unauthenticated_flags.is_empty() {
        return; // git unavailable; mission_init_repo already logged why.
    }
    for (has_home, has_config_dir) in &unauthenticated_flags {
        assert!(
            !has_home && !has_config_dir,
            "an Unauthenticated cached verdict must gate OFF relocation on every spawn path: \
             HOME present={has_home}, CLAUDE_CONFIG_DIR present={has_config_dir}"
        );
    }

    // Success: an Authenticated preflight means every worker on both spawn
    // paths relocates.
    let authenticated_flags =
        run_mission_and_collect_worker_env_flags(preflight_authenticated_script()).await;
    for (has_home, has_config_dir) in &authenticated_flags {
        assert!(
            *has_home && *has_config_dir,
            "an Authenticated cached verdict must gate ON relocation on every spawn path: \
             HOME present={has_home}, CLAUDE_CONFIG_DIR present={has_config_dir}"
        );
    }
}
