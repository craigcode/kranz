//! Integration tests for `kranz_engine::git_ops::GitRepo`.
//!
//! Each test builds a throwaway repository inside a tempdir. Tests skip
//! cleanly (eprintln + return) when git is not on PATH. The host's
//! global/system git config is masked via GIT_CONFIG_GLOBAL/GIT_CONFIG_SYSTEM
//! so identity, signing, and hook settings never leak in and
//! `ensure_identity` behaves deterministically.

use kranz_engine::error::EngineError;
use kranz_engine::git_ops::{CommitInfo, GitRepo, MergeOutcome};
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
        let missing = std::env::temp_dir().join(format!(
            "kranz-git-ops-test-no-config-{}",
            std::process::id()
        ));
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
    let sha = repo
        .add_all_and_commit("initial commit")
        .expect("seed commit");
    (dir, repo, sha)
}

#[test]
fn open_fails_on_non_repo_dir() {
    if !setup() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let err = GitRepo::open(dir.path()).expect_err("open must fail on a plain directory");
    assert!(
        matches!(err, EngineError::Git(_)),
        "expected EngineError::Git, got: {err:?}"
    );
}

#[test]
fn head_sha_and_add_all_and_commit_advance_history() {
    if !setup() {
        return;
    }
    let (dir, repo, first) = seeded_repo();
    assert!(first.len() >= 40, "unexpected sha: {first}");
    assert!(
        first.chars().all(|c| c.is_ascii_hexdigit()),
        "unexpected sha: {first}"
    );
    assert_eq!(repo.head_sha().unwrap(), first);

    write(&dir, "a.txt", "one\n");
    let second = repo.add_all_and_commit("add a").unwrap();
    assert_ne!(first, second);
    assert_eq!(
        repo.head_sha().unwrap(),
        second,
        "returned sha must be the new head"
    );
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
fn rev_parse_matches_head_sha_for_current_branch() {
    if !setup() {
        return;
    }
    let (_dir, repo, first) = seeded_repo();
    assert_eq!(repo.rev_parse("main").unwrap(), first);
    assert_eq!(repo.rev_parse("main").unwrap(), repo.head_sha().unwrap());
}

#[test]
fn rev_parse_rejects_flag_shaped_ref_without_invoking_git() {
    if !setup() {
        return;
    }
    let (_dir, repo, _first) = seeded_repo();
    let err = repo
        .rev_parse("-somethingflagshaped")
        .expect_err("flag-shaped ref must be refused");
    match err {
        EngineError::Git(msg) => {
            assert!(
                msg.contains("refusing"),
                "expected refusal message, got: {msg}"
            );
        }
        other => panic!("expected EngineError::Git, got: {other:?}"),
    }
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
    let err = repo
        .checkout("does-not-exist")
        .expect_err("checkout must fail");
    assert!(
        matches!(err, EngineError::Git(_)),
        "expected EngineError::Git, got: {err:?}"
    );
}

#[test]
fn is_clean_counts_untracked_files() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();
    assert!(repo.is_clean().unwrap(), "fresh commit leaves a clean tree");

    write(&dir, "untracked.txt", "x\n");
    assert!(
        !repo.is_clean().unwrap(),
        "an untracked file must make the tree dirty"
    );

    repo.add_all_and_commit("track it").unwrap();
    assert!(repo.is_clean().unwrap());
}

#[test]
fn no_change_commit_is_a_clear_git_error() {
    if !setup() {
        return;
    }
    let (_dir, repo, _) = seeded_repo();
    let err = repo
        .add_all_and_commit("nothing here")
        .expect_err("no-change commit must fail");
    match err {
        EngineError::Git(msg) => {
            assert!(
                msg.to_lowercase().contains("commit"),
                "error lacks commit context: {msg}"
            );
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
    assert!(
        !stat.contains("b.txt"),
        "diff stat must not include b.txt: {stat}"
    );

    // Empty path list is rejected up front.
    let err = repo
        .commit_paths(&[], "nothing")
        .expect_err("empty path list must fail");
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
        expected.push(CommitInfo {
            sha,
            subject: subject.to_string(),
        });
    }

    let listed = repo.commits_between(&base, "HEAD").unwrap();
    assert_eq!(
        listed, expected,
        "must list oldest first with matching shas/subjects"
    );

    assert!(
        repo.commits_between("HEAD", "HEAD").unwrap().is_empty(),
        "empty range"
    );
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
    assert!(
        tags.lines().any(|l| l.trim() == "kranz/m1"),
        "tag missing from: {tags}"
    );

    // Annotated tags are real tag objects (lightweight ones point at commits).
    let objtype = raw_git(
        dir.path(),
        &[
            "for-each-ref",
            "refs/tags/kranz/m1",
            "--format=%(objecttype)",
        ],
    );
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
    assert!(
        !probe.status.success(),
        "test precondition failed: user.name is already set"
    );

    repo.ensure_identity().unwrap();
    assert_eq!(
        raw_git(dir.path(), &["config", "--get", "user.name"]).trim(),
        "kranz"
    );
    assert_eq!(
        raw_git(dir.path(), &["config", "--get", "user.email"]).trim(),
        "kranz@localhost"
    );

    // And committing actually works now.
    write(&dir, "f.txt", "x\n");
    repo.add_all_and_commit("first commit with kranz identity")
        .unwrap();

    // Idempotent: calling again neither errors nor changes anything.
    repo.ensure_identity().unwrap();
    assert_eq!(
        raw_git(dir.path(), &["config", "--get", "user.name"]).trim(),
        "kranz"
    );
}

#[test]
fn has_remote_reports_presence() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();

    // No remotes configured on a fresh repo.
    assert!(
        !repo.has_remote("origin").unwrap(),
        "fresh repo has no origin"
    );

    // Add a fake remote (URL is never contacted — has_remote only reads config).
    raw_git(
        dir.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ],
    );
    assert!(
        repo.has_remote("origin").unwrap(),
        "origin must be visible after remote add"
    );
    assert!(
        !repo.has_remote("upstream").unwrap(),
        "unadded remote must report absent"
    );
}

#[test]
fn push_mission_branch_rejects_non_kranz_ref_without_network() {
    if !setup() {
        return;
    }
    let (_dir, repo, _) = seeded_repo();

    // main / arbitrary names / a bare branch are all refused up front. There is
    // no remote configured at all, so if the guard failed to short-circuit the
    // call would still fail — but the error message proves the guard fired
    // first (it names the ref and never mentions a missing remote).
    for bad in ["main", "master", "HEAD", "feature/x", "kranz", "kranzish"] {
        let err = repo
            .push_mission_branch("origin", bad)
            .expect_err("non-kranz ref must be refused");
        match err {
            EngineError::Git(msg) => {
                assert!(
                    msg.contains("refusing to push"),
                    "guard message expected for {bad:?}, got: {msg}"
                );
                assert!(
                    msg.contains(bad),
                    "message should name the rejected ref {bad:?}: {msg}"
                );
            }
            other => panic!("expected EngineError::Git for {bad:?}, got: {other:?}"),
        }
    }
}

#[test]
fn push_mission_branch_rejects_refspec_and_flag_smuggling() {
    if !setup() {
        return;
    }
    let (_dir, repo, _) = seeded_repo();

    // Even a kranz/* prefix must not carry a refspec, a flag, or whitespace
    // that could push a second ref or force. Refused before git runs.
    for bad in [
        "kranz/mission-1:main",
        "kranz/mission-1 --force",
        "kranz/ mission",
    ] {
        let err = repo
            .push_mission_branch("origin", bad)
            .expect_err("malformed kranz ref must be refused");
        assert!(
            matches!(err, EngineError::Git(_)),
            "expected EngineError::Git for {bad:?}, got: {err:?}"
        );
    }
    // A leading-dash ref is refused by the malformed guard (it isn't kranz/*
    // either, but this pins the flag-injection intent explicitly).
    let err = repo
        .push_mission_branch("origin", "--force")
        .expect_err("flag-shaped ref must be refused");
    assert!(matches!(err, EngineError::Git(_)));
}

#[test]
fn push_mission_branch_attempts_push_and_surfaces_git_error() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();
    // A kranz/* branch clears the guard and reaches `git push`. Point origin at
    // a path that is not a repo so git fails locally *without* touching the
    // network (a file:// path to a non-repo errors before any transport).
    let bogus = dir.path().join("no-such-remote-repo");
    raw_git(
        dir.path(),
        &[
            "remote",
            "add",
            "origin",
            &format!("file://{}", bogus.display()),
        ],
    );
    repo.create_branch("kranz/mission-1", None).unwrap();

    let err = repo
        .push_mission_branch("origin", "kranz/mission-1")
        .expect_err("push to a bogus remote must fail");
    match err {
        EngineError::Git(msg) => {
            // The guard did NOT fire — this is git's own failure, surfaced.
            assert!(
                !msg.contains("refusing to push"),
                "kranz/* ref must pass the guard and reach git: {msg}"
            );
            assert!(
                msg.contains("push"),
                "error should carry the push command context: {msg}"
            );
        }
        other => panic!("expected EngineError::Git, got: {other:?}"),
    }
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
    assert_eq!(
        raw_git(dir.path(), &["config", "--get", "user.name"]).trim(),
        "Alice"
    );
    assert_eq!(
        raw_git(dir.path(), &["config", "--get", "user.email"]).trim(),
        "alice@example.com"
    );
}

// ---------------------------------------------------------------------------
// Worktrees (roadmap M3 parallel workers)
// ---------------------------------------------------------------------------

/// A worktree directory OUTSIDE the repo working tree (git worktrees must not
/// nest inside the primary tree, or the primary `git status` reports the
/// worktree dir as an untracked path). Lives under a dedicated tempdir whose
/// drop cleans it up. Mirrors the engine's use of the system temp dir.
fn worktree_dir(base: &TempDir, name: &str) -> std::path::PathBuf {
    base.path().join(format!("wt-{name}"))
}

/// add_worktree makes a worktree on a NEW branch off a specific sha; a commit
/// made in it advances that branch; the primary tree is untouched; and
/// merge_no_ff clean-merges the branch back into the primary branch.
#[test]
fn add_worktree_new_branch_then_clean_merge_back() {
    if !setup() {
        return;
    }
    let (_dir, repo, seed) = seeded_repo();
    let wt_base = tempfile::tempdir().unwrap();

    let wt = worktree_dir(&wt_base, "feat");
    repo.add_worktree(&wt, "kranz/wt/m/f-1", &seed).unwrap();

    // git worktree list now names our worktree path (canonicalized on macOS).
    let listed = repo.list_worktrees().unwrap();
    let wt_canon = std::fs::canonicalize(&wt).unwrap();
    assert!(
        listed.iter().any(|p| std::fs::canonicalize(p)
            .map(|c| c == wt_canon)
            .unwrap_or(false)),
        "worktree not in list: {listed:?}"
    );
    // The new branch exists and is checked out in the worktree at the seed sha.
    assert!(repo.branch_exists("kranz/wt/m/f-1").unwrap());
    assert_eq!(raw_git(&wt, &["rev-parse", "HEAD"]).trim(), seed);

    // Commit work IN the worktree (a GitRepo rooted there).
    let wt_repo = GitRepo::open(&wt).unwrap();
    std::fs::write(wt.join("feature.txt"), "worktree work\n").unwrap();
    let on_branch = wt_repo
        .add_all_and_commit("feature work in worktree")
        .unwrap();
    assert_ne!(on_branch, seed);
    // The primary tree (still on main) has not moved.
    assert_eq!(repo.head_sha().unwrap(), seed, "primary branch untouched");

    // Merge the branch into main via merge_no_ff → clean, and main now carries
    // the feature file through an explicit merge commit.
    assert_eq!(repo.current_branch().unwrap(), "main");
    let outcome = repo.merge_no_ff("kranz/wt/m/f-1").unwrap();
    assert_eq!(outcome, MergeOutcome::Clean);
    assert!(repo.is_clean().unwrap(), "clean tree after a clean merge");
    assert_ne!(repo.head_sha().unwrap(), seed, "main advanced by the merge");
    let merged = repo.commits_between(&seed, "HEAD").unwrap();
    assert!(
        merged
            .iter()
            .any(|c| c.subject.contains("feature work in worktree")),
        "the worktree commit is now on main: {merged:?}"
    );

    // remove_worktree tears it down; list no longer names it.
    repo.remove_worktree(&wt).unwrap();
    repo.prune_worktrees().unwrap();
    let after = repo.list_worktrees().unwrap();
    assert!(
        !after.iter().any(|p| std::fs::canonicalize(p)
            .map(|c| c == wt_canon)
            .unwrap_or(false)),
        "worktree still listed after remove: {after:?}"
    );
    // remove is idempotent: a second remove of a gone worktree is Ok.
    repo.remove_worktree(&wt)
        .expect("second remove tolerates absence");
}

/// Two branches that change the SAME file differently: the first merges clean,
/// the second conflicts. merge_no_ff reports the conflict, names the file, and
/// LEAVES THE TREE CLEAN (git merge --abort ran) — porcelain empty afterwards.
#[test]
fn conflicting_merge_reports_conflict_and_leaves_tree_clean() {
    if !setup() {
        return;
    }
    let (dir, repo, _seed) = seeded_repo();
    let wt_base = tempfile::tempdir().unwrap();
    // A shared file both branches will edit incompatibly.
    std::fs::write(dir.path().join("shared.txt"), "base\n").unwrap();
    let base = repo.add_all_and_commit("add shared file").unwrap();

    // Branch A (in a worktree) rewrites shared.txt.
    let wt_a = worktree_dir(&wt_base, "a");
    repo.add_worktree(&wt_a, "kranz/wt/m/a", &base).unwrap();
    let a_repo = GitRepo::open(&wt_a).unwrap();
    std::fs::write(wt_a.join("shared.txt"), "A's version\n").unwrap();
    a_repo.add_all_and_commit("A edits shared").unwrap();

    // Branch B (in another worktree) rewrites the SAME line differently.
    let wt_b = worktree_dir(&wt_base, "b");
    repo.add_worktree(&wt_b, "kranz/wt/m/b", &base).unwrap();
    let b_repo = GitRepo::open(&wt_b).unwrap();
    std::fs::write(wt_b.join("shared.txt"), "B's version\n").unwrap();
    b_repo.add_all_and_commit("B edits shared").unwrap();

    // First merge (A) is clean.
    assert_eq!(
        repo.merge_no_ff("kranz/wt/m/a").unwrap(),
        MergeOutcome::Clean
    );
    assert!(repo.is_clean().unwrap());

    // Second merge (B) conflicts on shared.txt; merge_no_ff aborts it.
    match repo.merge_no_ff("kranz/wt/m/b").unwrap() {
        MergeOutcome::Conflict { files } => {
            assert!(
                files.iter().any(|f| f.contains("shared.txt")),
                "conflict must name shared.txt: {files:?}"
            );
        }
        MergeOutcome::Clean => panic!("B must conflict against A's change"),
    }
    // The crucial post-condition: the tree is CLEAN (aborted), not mid-merge.
    assert!(
        repo.is_clean().unwrap(),
        "merge --abort must leave a clean tree: {}",
        raw_git(dir.path(), &["status", "--porcelain"])
    );
    // And MERGE_HEAD is gone (no merge in progress).
    let merge_head = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "MERGE_HEAD"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        !merge_head.status.success(),
        "no merge should be in progress after abort"
    );
    // A's change survived; B's was rolled back.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("shared.txt")).unwrap(),
        "A's version\n"
    );

    // Cleanup.
    repo.remove_worktree(&wt_a).unwrap();
    repo.remove_worktree(&wt_b).unwrap();
    repo.prune_worktrees().unwrap();
}

/// merge_no_ff of a branch with no new commits (the branch == HEAD) is a clean
/// no-op — the mock-worker parallel path relies on this: workers that touch no
/// files leave their per-feature branch at the milestone-start sha.
#[test]
fn merge_no_ff_of_up_to_date_branch_is_clean() {
    if !setup() {
        return;
    }
    let (_dir, repo, seed) = seeded_repo();
    let wt_base = tempfile::tempdir().unwrap();
    let wt = worktree_dir(&wt_base, "noop");
    // Worktree/branch off the seed sha, no commits made in it.
    repo.add_worktree(&wt, "kranz/wt/m/noop", &seed).unwrap();

    let outcome = repo.merge_no_ff("kranz/wt/m/noop").unwrap();
    assert_eq!(outcome, MergeOutcome::Clean, "an up-to-date merge is clean");
    assert!(repo.is_clean().unwrap());
    assert_eq!(
        repo.head_sha().unwrap(),
        seed,
        "HEAD unchanged by a no-op merge"
    );

    repo.remove_worktree(&wt).unwrap();
    repo.prune_worktrees().unwrap();
}

/// add_worktree refuses flag-shaped arguments before spawning git.
#[test]
fn add_worktree_rejects_flag_shaped_arguments() {
    if !setup() {
        return;
    }
    let (_dir, repo, seed) = seeded_repo();
    let wt_base = tempfile::tempdir().unwrap();
    let wt = worktree_dir(&wt_base, "x");
    let err = repo
        .add_worktree(&wt, "--force", &seed)
        .expect_err("flag-shaped branch must be refused");
    assert!(
        matches!(err, EngineError::Git(_)),
        "expected EngineError::Git, got: {err:?}"
    );
    // No worktree was created.
    assert!(
        !wt.exists(),
        "no worktree dir should exist after a refused add"
    );
}

// ---------------------------------------------------------------------------
// add_worktree_checkout (M7 tier 1 mission integration worktree primitive)
// ---------------------------------------------------------------------------

/// add_worktree_checkout puts an EXISTING branch into a new worktree whose
/// HEAD equals that branch's tip.
#[test]
fn add_worktree_checkout_existing_branch_at_its_tip() {
    if !setup() {
        return;
    }
    let (_dir, repo, seed) = seeded_repo();
    repo.create_branch("feature-x", Some(&seed)).unwrap();
    // Advance feature-x one commit past the seed, via its own worktree, so
    // "main" (checked out in the primary tree) is never touched twice.
    let advance_base = tempfile::tempdir().unwrap();
    let advance_wt = worktree_dir(&advance_base, "advance");
    repo.add_worktree_checkout(&advance_wt, "feature-x").unwrap();
    let advance_repo = GitRepo::open(&advance_wt).unwrap();
    std::fs::write(advance_wt.join("on-branch.txt"), "branch work\n").unwrap();
    let tip = advance_repo.add_all_and_commit("advance feature-x").unwrap();
    assert_ne!(seed, tip);
    repo.remove_worktree(&advance_wt).unwrap();
    repo.prune_worktrees().unwrap();

    let wt_base = tempfile::tempdir().unwrap();
    let wt = worktree_dir(&wt_base, "checkout-existing");
    repo.add_worktree_checkout(&wt, "feature-x").unwrap();

    let wt_repo = GitRepo::open(&wt).expect("open worktree as repo");
    assert_eq!(wt_repo.head_sha().unwrap(), tip);
    assert_eq!(wt_repo.current_branch().unwrap(), "feature-x");

    // No new branch was created; the primary checkout is untouched.
    assert_eq!(repo.current_branch().unwrap(), "main");

    repo.remove_worktree(&wt).unwrap();
    repo.prune_worktrees().unwrap();
}

/// add_worktree_checkout refuses a flag-shaped branch before spawning git.
#[test]
fn add_worktree_checkout_rejects_flag_shaped_branch() {
    if !setup() {
        return;
    }
    let (_dir, repo, _seed) = seeded_repo();
    let wt_base = tempfile::tempdir().unwrap();
    let wt = worktree_dir(&wt_base, "flag");
    let err = repo
        .add_worktree_checkout(&wt, "--force")
        .expect_err("flag-shaped branch must be refused");
    assert!(
        matches!(err, EngineError::Git(_)),
        "expected EngineError::Git, got: {err:?}"
    );
    assert!(
        !wt.exists(),
        "no worktree dir should exist after a refused add"
    );
}

/// A branch already checked out in one worktree cannot be checked out again
/// in a second worktree (git's own rule) — add_worktree_checkout surfaces
/// that as an EngineError::Git rather than silently succeeding.
#[test]
fn add_worktree_checkout_rejects_branch_already_checked_out_elsewhere() {
    if !setup() {
        return;
    }
    let (_dir, repo, seed) = seeded_repo();
    repo.create_branch("kranz/mission-x", Some(&seed)).unwrap();

    let wt_base = tempfile::tempdir().unwrap();
    let wt_a = worktree_dir(&wt_base, "first");
    repo.add_worktree_checkout(&wt_a, "kranz/mission-x")
        .unwrap();

    let wt_b = worktree_dir(&wt_base, "second");
    let err = repo
        .add_worktree_checkout(&wt_b, "kranz/mission-x")
        .expect_err("git must refuse checking out the same branch twice");
    assert!(
        matches!(err, EngineError::Git(_)),
        "expected EngineError::Git, got: {err:?}"
    );

    repo.remove_worktree(&wt_a).unwrap();
    repo.prune_worktrees().unwrap();
}

// ---------------------------------------------------------------------------
// is_ancestor
// ---------------------------------------------------------------------------

#[test]
fn is_ancestor_true_for_earlier_commit_on_same_branch() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    write(&dir, "second.txt", "more\n");
    let second = repo.add_all_and_commit("second commit").unwrap();

    assert!(
        repo.is_ancestor(&seed, &second).unwrap(),
        "seed commit is an ancestor of a later commit on the same branch"
    );
}

#[test]
fn is_ancestor_false_for_later_commit_checked_against_earlier() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    write(&dir, "second.txt", "more\n");
    let second = repo.add_all_and_commit("second commit").unwrap();

    assert!(
        !repo.is_ancestor(&second, &seed).unwrap(),
        "a later commit is not an ancestor of an earlier one"
    );
}

#[test]
fn is_ancestor_true_for_a_commit_and_itself() {
    if !setup() {
        return;
    }
    let (_dir, repo, seed) = seeded_repo();
    assert!(
        repo.is_ancestor(&seed, &seed).unwrap(),
        "a commit is an ancestor of itself"
    );
}

#[test]
fn is_ancestor_errs_on_non_resolving_ref() {
    if !setup() {
        return;
    }
    let (_dir, repo, seed) = seeded_repo();

    // A well-formed but non-existent sha (not flag-shaped) makes `git
    // merge-base --is-ancestor` exit 128 — neither 0 nor 1 — which must
    // surface as a genuine EngineError::Git, not Ok(true)/Ok(false).
    let bogus = "0000000000000000000000000000000000000000";
    let err = repo
        .is_ancestor(bogus, &seed)
        .expect_err("non-resolving ref must be a git error, not Ok");
    assert!(
        matches!(err, EngineError::Git(_)),
        "expected EngineError::Git, got: {err:?}"
    );
}

#[test]
fn is_ancestor_rejects_flag_shaped_refs() {
    if !setup() {
        return;
    }
    let (_dir, repo, seed) = seeded_repo();

    let err = repo
        .is_ancestor("--force", &seed)
        .expect_err("flag-shaped ancestor ref must be refused");
    assert!(matches!(err, EngineError::Git(_)));

    let err = repo
        .is_ancestor(&seed, "--force")
        .expect_err("flag-shaped descendant ref must be refused");
    assert!(matches!(err, EngineError::Git(_)));
}
