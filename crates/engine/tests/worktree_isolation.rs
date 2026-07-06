//! Tests for the `workerIsolation` mission-config key (M7 tier 1, feature f-1-1)
//! and the mission integration-worktree git primitive (feature f-1-2).
//!
//! f-1-1 only added the config surface; f-1-2 adds the git plumbing
//! (`GitRepo::add_worktree_checkout`) that a later milestone will use to
//! re-route mission-branch mutations through a dedicated worktree. Nothing
//! consumes either yet.

use kranz_engine::config::load_layers;
use kranz_engine::git_ops::GitRepo;
use kranz_engine::types::{MissionConfig, WorkerIsolation};
use std::path::PathBuf;
use std::process::Command;

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
