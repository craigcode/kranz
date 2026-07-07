//! Integration tests for `kranz_engine::merge::merge_mission`.
//!
//! Mirrors the fixture style of `git_ops_test.rs`: throwaway temp repos with
//! git's global/system config masked, skipping cleanly when git is missing.

use kranz_engine::git_ops::GitRepo;
use kranz_engine::merge::{merge_mission, MergeReport};
use std::path::Path;
use std::process::Command;
use std::sync::Once;
use tempfile::TempDir;

static ENV_ISOLATION: Once = Once::new();

fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing =
            std::env::temp_dir().join(format!("kranz-merge-test-no-config-{}", std::process::id()));
        std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
        std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
        if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
            std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
        }
    });
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn setup() -> bool {
    isolate_git_env();
    if git_available() {
        true
    } else {
        eprintln!("skipping test: git is not on PATH");
        false
    }
}

fn raw_git(dir: &Path, args: &[&str]) -> String {
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

fn init_repo() -> (TempDir, GitRepo) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let init = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .expect("spawn git init");
    if !init.status.success() {
        raw_git(dir.path(), &["init"]);
        raw_git(dir.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);
    }
    let repo = GitRepo::open(dir.path()).expect("open freshly-initialized repo");
    (dir, repo)
}

fn init_repo_with_identity() -> (TempDir, GitRepo) {
    let (dir, repo) = init_repo();
    repo.ensure_identity().expect("ensure_identity");
    (dir, repo)
}

fn write(dir: &TempDir, name: &str, content: &str) {
    std::fs::write(dir.path().join(name), content).expect("write file");
}

/// Repo with identity and one seed commit on `main`; returns the seed sha.
fn seeded_repo() -> (TempDir, GitRepo, String) {
    let (dir, repo) = init_repo_with_identity();
    write(&dir, "README.md", "hello\n");
    let sha = repo
        .add_all_and_commit("initial commit")
        .expect("seed commit");
    (dir, repo, sha)
}

/// Creates `kranz/mission-x` off the seed sha with one commit touching `path`.
fn seed_mission_branch(dir: &TempDir, repo: &GitRepo, seed: &str, path: &str, content: &str) {
    repo.create_branch("kranz/mission-x", Some(seed)).unwrap();
    repo.checkout("kranz/mission-x").unwrap();
    let full = dir.path().join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, content).unwrap();
    repo.add_all_and_commit("mission work").unwrap();
    repo.checkout("main").unwrap();
}

fn passing_executor(_cmd: &str, _cwd: &Path) -> (bool, String) {
    (true, String::new())
}

#[test]
fn dirty_tracked_tree_is_refused_without_running_gates_or_touching_base() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    seed_mission_branch(&dir, &repo, &seed, "src/lib.rs", "fn a() {}\n");

    // Dirty a TRACKED file.
    write(&dir, "README.md", "dirty\n");

    let gate_calls = std::cell::RefCell::new(Vec::<String>::new());
    let report = merge_mission(&repo, "main", &seed, "kranz/mission-x", |cmd, _cwd| {
        gate_calls.borrow_mut().push(cmd.to_string());
        (true, String::new())
    })
    .unwrap();

    assert_eq!(report, MergeReport::RefusedDirtyTree);
    assert!(
        gate_calls.borrow().is_empty(),
        "no gate executor call expected, got {:?}",
        gate_calls.borrow()
    );
    assert_eq!(repo.head_sha().unwrap(), seed, "base tip must be unchanged");
    assert_eq!(repo.current_branch().unwrap(), "main");
}

#[test]
fn passing_gates_produce_a_no_ff_merge_commit_with_two_parents() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    seed_mission_branch(&dir, &repo, &seed, "src/lib.rs", "fn a() {}\n");

    let report = merge_mission(&repo, "main", &seed, "kranz/mission-x", passing_executor).unwrap();

    match report {
        MergeReport::Merged { commit } => {
            let base_tip = repo.head_sha().unwrap();
            assert_eq!(commit, base_tip);
            assert_eq!(repo.current_branch().unwrap(), "main");

            let parents = raw_git(dir.path(), &["rev-list", "--parents", "-n", "1", &base_tip]);
            let parent_count = parents.split_whitespace().count() - 1;
            assert_eq!(parent_count, 2, "expected a two-parent merge commit");

            assert!(
                repo.is_ancestor("kranz/mission-x", &base_tip).unwrap(),
                "mission tip must be an ancestor of the base tip"
            );
            assert_ne!(base_tip, seed, "base must have advanced");
        }
        other => panic!("expected Merged, got {other:?}"),
    }
}

#[test]
fn a_failing_gate_stops_before_the_merge_and_leaves_base_unchanged() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    seed_mission_branch(&dir, &repo, &seed, "src/lib.rs", "fn a() {}\n");

    let report = merge_mission(&repo, "main", &seed, "kranz/mission-x", |cmd, _cwd| {
        if cmd == "cargo fmt --all --check" {
            (false, "diff detected: src/lib.rs".to_string())
        } else {
            (true, String::new())
        }
    })
    .unwrap();

    match report {
        MergeReport::GateFailed { gate, output } => {
            assert_eq!(gate, "cargo fmt --all --check");
            assert_eq!(output, "diff detected: src/lib.rs");
        }
        other => panic!("expected GateFailed, got {other:?}"),
    }
    assert_eq!(repo.head_sha().unwrap(), seed, "base tip must be unchanged");
    assert_eq!(repo.current_branch().unwrap(), "main");
    let _ = dir;
}

#[test]
fn changed_paths_and_dashboard_touched_detect_dashboard_diffs() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    seed_mission_branch(
        &dir,
        &repo,
        &seed,
        "apps/dashboard/src/App.tsx",
        "export default {}\n",
    );

    let paths = repo.changed_paths(&seed, "kranz/mission-x").unwrap();
    assert_eq!(paths, vec!["apps/dashboard/src/App.tsx".to_string()]);
    assert!(repo.dashboard_touched(&seed, "kranz/mission-x").unwrap());

    // A second mission branch that never touches apps/dashboard/.
    repo.create_branch("kranz/mission-y", Some(&seed)).unwrap();
    repo.checkout("kranz/mission-y").unwrap();
    write(&dir, "src_lib.rs", "fn b() {}\n");
    repo.add_all_and_commit("engine-only work").unwrap();
    repo.checkout("main").unwrap();

    assert!(!repo.dashboard_touched(&seed, "kranz/mission-y").unwrap());
}

#[test]
fn no_merge_mission_outcome_ever_pushes() {
    if !setup() {
        return;
    }
    // No remote is configured at all; if any code path attempted a push it
    // would fail loudly (no remote named "origin" exists), so a passing
    // Merged/RefusedDirtyTree/GateFailed outcome here is itself proof no
    // push was attempted.
    let (dir, repo, seed) = seeded_repo();
    assert!(!repo.has_remote("origin").unwrap());

    seed_mission_branch(&dir, &repo, &seed, "src/lib.rs", "fn a() {}\n");
    let merged = merge_mission(&repo, "main", &seed, "kranz/mission-x", passing_executor).unwrap();
    assert!(matches!(merged, MergeReport::Merged { .. }));
    assert!(
        !repo.has_remote("origin").unwrap(),
        "still no remote after a merge"
    );
}

/// Commits `plan.md` and `report.md` under `.kranz/missions/<id>/` on a new
/// mission branch off `seed`, mirroring the canonical tracked paths the
/// engine writes on the mission branch. Leaves `main` checked out.
fn seed_mission_branch_with_twins(
    dir: &TempDir,
    repo: &GitRepo,
    seed: &str,
    plan: &str,
    report: &str,
) -> (String, String) {
    let plan_path = ".kranz/missions/m-x/plan.md".to_string();
    let report_path = ".kranz/missions/m-x/report.md".to_string();
    repo.create_branch("kranz/mission-x", Some(seed)).unwrap();
    repo.checkout("kranz/mission-x").unwrap();
    for (path, content) in [(&plan_path, plan), (&report_path, report)] {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, content).unwrap();
    }
    repo.add_all_and_commit("mission deliverables").unwrap();
    repo.checkout("main").unwrap();
    (plan_path, report_path)
}

#[test]
fn preview_twin_byte_identical_untracked_files_let_merge_land_cleanly() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    let plan_content = "# Plan\ncanonical\n";
    let report_content = "# Report\ncanonical\n";
    let (plan_path, report_path) =
        seed_mission_branch_with_twins(&dir, &repo, &seed, plan_content, report_content);

    // Untracked human-readable twins in the primary tree, byte-identical to
    // what the mission branch will bring in.
    for (path, content) in [(&plan_path, plan_content), (&report_path, report_content)] {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, content).unwrap();
    }
    // Confirm they really are untracked before the merge.
    assert!(repo.is_untracked(&plan_path).unwrap());
    assert!(repo.is_untracked(&report_path).unwrap());

    let report = merge_mission(&repo, "main", &seed, "kranz/mission-x", passing_executor).unwrap();
    assert!(
        matches!(report, MergeReport::Merged { .. }),
        "expected Merged, got {report:?}"
    );

    assert_eq!(
        std::fs::read_to_string(dir.path().join(&plan_path)).unwrap(),
        plan_content
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join(&report_path)).unwrap(),
        report_content
    );
}

#[test]
fn preview_twin_divergent_untracked_file_blocks_merge_without_abort_wrapper() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    let plan_content = "# Plan\ncanonical\n";
    let report_content = "# Report\ncanonical\n";
    let (plan_path, report_path) =
        seed_mission_branch_with_twins(&dir, &repo, &seed, plan_content, report_content);

    // plan.md twin is identical, but report.md twin DIVERGES from the
    // incoming canonical content — must NOT be silently removed.
    let divergent_report = "# Report\nSTALE OPERATOR-VISIBLE COPY\n";
    let full_plan = dir.path().join(&plan_path);
    std::fs::create_dir_all(full_plan.parent().unwrap()).unwrap();
    std::fs::write(&full_plan, plan_content).unwrap();
    let full_report = dir.path().join(&report_path);
    std::fs::create_dir_all(full_report.parent().unwrap()).unwrap();
    std::fs::write(&full_report, divergent_report).unwrap();

    let report = merge_mission(&repo, "main", &seed, "kranz/mission-x", passing_executor).unwrap();

    match &report {
        MergeReport::Merged { .. } => panic!("must not merge over a divergent untracked file"),
        MergeReport::RefusedPreMerge { detail } => {
            assert!(
                detail.contains("would be overwritten by merge"),
                "detail must carry git's verbatim refusal: {detail}"
            );
            assert!(
                !detail.contains("abort also failed"),
                "pre-MERGE_HEAD refusal must never show the abort-wrapper text: {detail}"
            );
        }
        other => panic!("expected RefusedPreMerge, got {other:?}"),
    }

    // The divergent file must be left in place, untouched.
    assert_eq!(
        std::fs::read_to_string(&full_report).unwrap(),
        divergent_report
    );
    assert_eq!(repo.head_sha().unwrap(), seed, "base tip must be unchanged");
}
