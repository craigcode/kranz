//! Integration tests for `kranz_engine::git_ops::GitRepo`.
//!
//! Each test builds a throwaway repository inside a tempdir. Tests skip
//! cleanly (eprintln + return) when git is not on PATH. The host's
//! global/system git config is masked via GIT_CONFIG_GLOBAL/GIT_CONFIG_SYSTEM
//! so identity, signing, and hook settings never leak in and
//! `ensure_identity` behaves deterministically.

use kranz_engine::error::EngineError;
use kranz_engine::git_ops::{CommitInfo, GitRepo};
use std::path::Path;
use std::process::Command;
use std::sync::Once;
use tempfile::TempDir;

static ENV_ISOLATION: Once = Once::new();

/// Point global/system git config at a file that never exists. Every test
/// calls this (via `setup`) before spawning any git process; the same values
/// are written exactly once for the whole test process.
fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing = std::env::temp_dir()
            .join(format!("kranz-git-ops-test-no-config-{}", std::process::id()));
        std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
        std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
        // Stop git from discovering a repository above the temp dir (some CI
        // hosts place TMPDIR inside a checkout). Canonicalized so git's
        // textual prefix comparison matches its resolved cwd.
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

/// Returns false (after logging a skip note) when git is missing.
fn setup() -> bool {
    isolate_git_env();
    if git_available() {
        true
    } else {
        eprintln!("skipping test: git is not on PATH");
        false
    }
}

/// Run git directly (test plumbing, independent of the code under test).
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

/// Fresh repo on branch `main`, no identity configured yet.
fn init_repo() -> (TempDir, GitRepo) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let init = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .expect("spawn git init");
    if !init.status.success() {
        // Older git without `init -b`: init, then point HEAD at main.
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

/// Repo with identity and one seed commit; returns the seed sha.
fn seeded_repo() -> (TempDir, GitRepo, String) {
    let (dir, repo) = init_repo_with_identity();
    write(&dir, "README.md", "hello\n");
    let sha = repo.add_all_and_commit("initial commit").expect("seed commit");
    (dir, repo, sha)
}

#[test]
fn open_fails_on_non_repo_dir() {
    if !setup() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let err = GitRepo::open(dir.path()).expect_err("open must fail on a plain directory");
    assert!(matches!(err, EngineError::Git(_)), "expected EngineError::Git, got: {err:?}");
}

#[test]
fn head_sha_and_add_all_and_commit_advance_history() {
    if !setup() {
        return;
    }
    let (dir, repo, first) = seeded_repo();
    assert!(first.len() >= 40, "unexpected sha: {first}");
    assert!(first.chars().all(|c| c.is_ascii_hexdigit()), "unexpected sha: {first}");
    assert_eq!(repo.head_sha().unwrap(), first);

    write(&dir, "a.txt", "one\n");
    let second = repo.add_all_and_commit("add a").unwrap();
    assert_ne!(first, second);
    assert_eq!(repo.head_sha().unwrap(), second, "returned sha must be the new head");
}

#[test]
fn current_branch_reports_main() {
    if !setup() {
        return;
    }
    let (_dir, repo, _) = seeded_repo();
    assert_eq!(repo.current_branch().unwrap(), "main");
}

#[test]
fn branch_create_checkout_roundtrip() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();

    assert!(repo.branch_exists("main").unwrap());
    assert!(!repo.branch_exists("kranz/mission-1").unwrap());

    repo.create_branch("kranz/mission-1", None).unwrap();
    assert!(repo.branch_exists("kranz/mission-1").unwrap());
    // create_branch must not switch branches by itself.
    assert_eq!(repo.current_branch().unwrap(), "main");

    repo.checkout("kranz/mission-1").unwrap();
    assert_eq!(repo.current_branch().unwrap(), "kranz/mission-1");
    write(&dir, "feature.txt", "work\n");
    let on_branch = repo.add_all_and_commit("feature work").unwrap();

    repo.checkout("main").unwrap();
    assert_eq!(repo.current_branch().unwrap(), "main");
    assert_eq!(repo.head_sha().unwrap(), seed, "main must be untouched");
    assert_ne!(on_branch, seed);
}

#[test]
fn create_branch_from_specific_commit() {
    if !setup() {
        return;
    }
    let (dir, repo, first) = seeded_repo();
    write(&dir, "later.txt", "later\n");
    let second = repo.add_all_and_commit("later commit").unwrap();

    repo.create_branch("from-first", Some(&first)).unwrap();
    repo.checkout("from-first").unwrap();
    assert_eq!(repo.head_sha().unwrap(), first);
    assert_ne!(repo.head_sha().unwrap(), second);
}

#[test]
fn checkout_of_missing_branch_errors() {
    if !setup() {
        return;
    }
    let (_dir, repo, _) = seeded_repo();
    let err = repo.checkout("does-not-exist").expect_err("checkout must fail");
    assert!(matches!(err, EngineError::Git(_)), "expected EngineError::Git, got: {err:?}");
}

#[test]
fn is_clean_counts_untracked_files() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();
    assert!(repo.is_clean().unwrap(), "fresh commit leaves a clean tree");

    write(&dir, "untracked.txt", "x\n");
    assert!(!repo.is_clean().unwrap(), "an untracked file must make the tree dirty");

    repo.add_all_and_commit("track it").unwrap();
    assert!(repo.is_clean().unwrap());
}

#[test]
fn no_change_commit_is_a_clear_git_error() {
    if !setup() {
        return;
    }
    let (_dir, repo, _) = seeded_repo();
    let err = repo.add_all_and_commit("nothing here").expect_err("no-change commit must fail");
    match err {
        EngineError::Git(msg) => {
            assert!(msg.to_lowercase().contains("commit"), "error lacks commit context: {msg}");
        }
        other => panic!("expected EngineError::Git, got: {other:?}"),
    }
}

#[test]
fn commit_paths_commits_only_named_paths() {
    if !setup() {
        return;
    }
    let (dir, repo, base) = seeded_repo();
    write(&dir, "a.txt", "aaa\n");
    write(&dir, "b.txt", "bbb\n");

    let sha = repo.commit_paths(&[Path::new("a.txt")], "only a").unwrap();
    assert_eq!(repo.head_sha().unwrap(), sha);
    assert!(!repo.is_clean().unwrap(), "b.txt must still be uncommitted");

    let stat = repo.diff_stat(&base, &sha).unwrap();
    assert!(stat.contains("a.txt"), "diff stat missing a.txt: {stat}");
    assert!(!stat.contains("b.txt"), "diff stat must not include b.txt: {stat}");

    // Empty path list is rejected up front.
    let err = repo.commit_paths(&[], "nothing").expect_err("empty path list must fail");
    assert!(matches!(err, EngineError::Git(_)));
}

#[test]
fn commits_between_is_oldest_first() {
    if !setup() {
        return;
    }
    let (dir, repo, base) = seeded_repo();
    let mut expected = Vec::new();
    for (file, subject) in [
        ("one.txt", "first change"),
        ("two.txt", "second change"),
        ("three.txt", "third change"),
    ] {
        write(&dir, file, subject);
        let sha = repo.add_all_and_commit(subject).unwrap();
        expected.push(CommitInfo { sha, subject: subject.to_string() });
    }

    let listed = repo.commits_between(&base, "HEAD").unwrap();
    assert_eq!(listed, expected, "must list oldest first with matching shas/subjects");

    assert!(repo.commits_between("HEAD", "HEAD").unwrap().is_empty(), "empty range");
}

#[test]
fn diff_stat_and_diff_full_show_changes() {
    if !setup() {
        return;
    }
    let (dir, repo, base) = seeded_repo();
    write(&dir, "README.md", "hello\nnew line\n");
    let sha = repo.add_all_and_commit("extend readme").unwrap();

    let stat = repo.diff_stat(&base, &sha).unwrap();
    assert!(stat.contains("README.md"), "stat: {stat}");
    assert!(stat.contains("1 file changed"), "stat: {stat}");

    let full = repo.diff_full(&base, &sha).unwrap();
    assert!(full.contains("README.md"), "diff: {full}");
    assert!(full.contains("+new line"), "diff: {full}");
}

#[test]
fn tag_creates_annotated_tag() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();
    repo.tag("kranz/m1", "milestone 1 complete").unwrap();

    let tags = raw_git(dir.path(), &["tag", "--list"]);
    assert!(tags.lines().any(|l| l.trim() == "kranz/m1"), "tag missing from: {tags}");

    // Annotated tags are real tag objects (lightweight ones point at commits).
    let objtype =
        raw_git(dir.path(), &["for-each-ref", "refs/tags/kranz/m1", "--format=%(objecttype)"]);
    assert_eq!(objtype.trim(), "tag");
}

#[test]
fn ensure_identity_sets_local_identity_when_missing() {
    if !setup() {
        return;
    }
    let (dir, repo) = init_repo();

    // Precondition: global/system config is masked, so no identity resolves.
    let probe = Command::new("git")
        .args(["config", "--get", "user.name"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(!probe.status.success(), "test precondition failed: user.name is already set");

    repo.ensure_identity().unwrap();
    assert_eq!(raw_git(dir.path(), &["config", "--get", "user.name"]).trim(), "kranz");
    assert_eq!(raw_git(dir.path(), &["config", "--get", "user.email"]).trim(), "kranz@localhost");

    // And committing actually works now.
    write(&dir, "f.txt", "x\n");
    repo.add_all_and_commit("first commit with kranz identity").unwrap();

    // Idempotent: calling again neither errors nor changes anything.
    repo.ensure_identity().unwrap();
    assert_eq!(raw_git(dir.path(), &["config", "--get", "user.name"]).trim(), "kranz");
}

#[test]
fn ensure_identity_keeps_existing_identity() {
    if !setup() {
        return;
    }
    let (dir, repo) = init_repo();
    raw_git(dir.path(), &["config", "user.name", "Alice"]);
    raw_git(dir.path(), &["config", "user.email", "alice@example.com"]);

    repo.ensure_identity().unwrap();
    assert_eq!(raw_git(dir.path(), &["config", "--get", "user.name"]).trim(), "Alice");
    assert_eq!(
        raw_git(dir.path(), &["config", "--get", "user.email"]).trim(),
        "alice@example.com"
    );
}
