//! Integration tests for `kranz_engine::git_ops::GitRepo`.
//!
//! Each test builds a throwaway repository inside a tempdir. Tests skip
//! cleanly (eprintln + return) when git is not on PATH. The host's
//! global/system git config is masked via GIT_CONFIG_GLOBAL/GIT_CONFIG_SYSTEM
//! so identity, signing, and hook settings never leak in and
//! `ensure_identity` behaves deterministically.

use kranz_engine::error::EngineError;
use kranz_engine::git_ops::{CheckpointOutcome, CommitInfo, GitRepo, MergeOutcome};
use std::path::{Path, PathBuf};
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
        kranz_engine::test_capability::skip(
            kranz_engine::test_capability::capability::GIT,
            "git is not on PATH",
        );
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
fn commit_that_added_reports_the_introducing_commit_or_none() {
    if !setup() {
        return;
    }
    let (dir, repo, _seed) = seeded_repo();
    std::fs::create_dir_all(dir.path().join(".kranz/lessons")).unwrap();
    write(&dir, ".kranz/lessons/m-x.md", "LESSON\n");
    raw_git(dir.path(), &["add", ".kranz/lessons/m-x.md"]);
    raw_git(
        dir.path(),
        &[
            "commit",
            "-m",
            "[kranz] mission report for m-x\n\nKranz-Mission: m-x\nKranz-Cost-USD: 0.0000",
        ],
    );

    let add = repo
        .commit_that_added(".kranz/lessons/m-x.md")
        .unwrap()
        .expect("the introducing commit is found");
    assert!(
        add.subject.starts_with("[kranz] mission report for m-x"),
        "subject: {}",
        add.subject
    );
    assert!(
        add.body.contains("Kranz-Mission: m-x"),
        "body must carry the trailer block: {}",
        add.body
    );
    assert_eq!(add.sha.len(), 40, "full sha expected: {}", add.sha);

    // A path never added under version control resolves to None.
    assert!(repo
        .commit_that_added(".kranz/lessons/m-absent.md")
        .unwrap()
        .is_none());

    // Flag-shaped path is refused, not passed to git.
    assert!(repo.commit_that_added("--all").is_err());
}

#[test]
fn commit_paths_is_idempotent_when_nothing_staged() {
    if !setup() {
        return;
    }
    let (dir, repo, _base) = seeded_repo();
    write(&dir, "a.txt", "aaa\n");
    let first = repo.commit_paths(&[Path::new("a.txt")], "add a").unwrap();

    // Re-committing byte-identical content (a crash-replayed re-approval) must
    // be a no-op that returns the unchanged head, not an empty-commit error
    // that would wedge the caller.
    write(&dir, "a.txt", "aaa\n");
    let second = repo
        .commit_paths(&[Path::new("a.txt")], "add a (replay)")
        .expect("re-committing identical content must not error");
    assert_eq!(first, second, "head must not advance on a no-op re-commit");
}

/// A staged rename (`git mv a b`) reports BOTH sides: the destination and
/// the source. Dropping the source made `commit_dirty_paths` commit only the
/// destination, leaving the staged `D a` behind — the engine reported the
/// tree resolved while it was still dirty.
#[test]
fn dirty_paths_reports_both_sides_of_a_staged_rename() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();
    write(&dir, "a.txt", "content\n");
    repo.add_all_and_commit("add a.txt").unwrap();

    raw_git(dir.path(), &["mv", "a.txt", "b.txt"]);

    let dirty = repo.dirty_paths().unwrap();
    assert!(
        dirty.contains(&PathBuf::from("b.txt")),
        "rename destination missing from dirty set: {dirty:?}"
    );
    assert!(
        dirty.contains(&PathBuf::from("a.txt")),
        "rename SOURCE missing from dirty set: {dirty:?}"
    );
}

/// The checkpoint path end-to-end: after a worker's staged `git mv`,
/// `commit_dirty_paths` consumes the WHOLE rename — porcelain status is
/// empty afterwards, and the committed tree carries the new path only.
#[test]
fn commit_dirty_paths_commits_a_staged_rename_leaving_a_clean_tree() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();
    write(&dir, "a.txt", "content\n");
    let base = repo.add_all_and_commit("add a.txt").unwrap();

    raw_git(dir.path(), &["mv", "a.txt", "b.txt"]);

    let outcome = repo.commit_dirty_paths("checkpoint the rename").unwrap();
    let CheckpointOutcome::Committed(sha) = outcome else {
        panic!("expected a committed checkpoint, got: {outcome:?}");
    };
    assert_ne!(sha, base, "the rename must land in a real commit");
    assert_eq!(repo.head_sha().unwrap(), sha);
    let status = raw_git(dir.path(), &["status", "--porcelain"]);
    assert!(
        status.trim().is_empty(),
        "no staged `D a.txt` may be left behind: {status}"
    );
    let tree = raw_git(dir.path(), &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(tree.lines().any(|l| l == "b.txt"), "tree: {tree}");
    assert!(!tree.lines().any(|l| l == "a.txt"), "tree: {tree}");
}

/// Same as above with a SPACE in the rename source — the porcelain `-z`
/// oldpath record is unquoted, so the space must survive parsing intact.
#[test]
fn commit_dirty_paths_commits_a_staged_rename_with_space_in_source() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();
    write(&dir, "old name.txt", "content\n");
    repo.add_all_and_commit("add file with a space").unwrap();

    raw_git(dir.path(), &["mv", "old name.txt", "new.txt"]);

    let dirty = repo.dirty_paths().unwrap();
    assert!(
        dirty.contains(&PathBuf::from("new.txt")),
        "rename destination missing from dirty set: {dirty:?}"
    );
    assert!(
        dirty.contains(&PathBuf::from("old name.txt")),
        "spaced rename source missing from dirty set: {dirty:?}"
    );

    let outcome = repo
        .commit_dirty_paths("checkpoint the spaced rename")
        .unwrap();
    assert!(
        matches!(outcome, CheckpointOutcome::Committed(_)),
        "expected a committed checkpoint, got: {outcome:?}"
    );
    let status = raw_git(dir.path(), &["status", "--porcelain"]);
    assert!(
        status.trim().is_empty(),
        "no staged deletion may be left behind: {status}"
    );
}

/// Guard for the add-step filter the rename fix introduced: a plain UNSTAGED
/// working-tree deletion (gone from disk, still in the index) must still be
/// staged and committed by `commit_dirty_paths`.
#[test]
fn commit_dirty_paths_still_stages_an_unstaged_deletion() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();
    write(&dir, "doomed.txt", "content\n");
    repo.add_all_and_commit("add doomed.txt").unwrap();

    std::fs::remove_file(dir.path().join("doomed.txt")).unwrap();

    let outcome = repo.commit_dirty_paths("checkpoint the deletion").unwrap();
    assert!(
        matches!(outcome, CheckpointOutcome::Committed(_)),
        "expected a committed checkpoint, got: {outcome:?}"
    );
    let status = raw_git(dir.path(), &["status", "--porcelain"]);
    assert!(
        status.trim().is_empty(),
        "the deletion must be committed: {status}"
    );
    let tree = raw_git(dir.path(), &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(!tree.lines().any(|l| l == "doomed.txt"), "tree: {tree}");
}

#[test]
fn commit_paths_blocks_unwaived_secret_findings_before_staging() {
    if !setup() {
        return;
    }
    let (dir, repo, base) = seeded_repo();
    let secret = "sk-ant-api03-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    write(&dir, "leak.txt", &format!("ANTHROPIC_API_KEY={secret}\n"));

    let err = repo
        .commit_paths(&[Path::new("leak.txt")], "leak")
        .expect_err("secret finding must block the engine commit");
    match err {
        EngineError::Git(msg) => {
            assert!(
                msg.contains("secret scan blocked engine commit"),
                "missing scan context: {msg}"
            );
            assert!(
                msg.contains(".kranz/secret-allowlist"),
                "missing waiver guidance: {msg}"
            );
            assert!(msg.contains("anthropic-api-key"), "missing rule id: {msg}");
            assert!(
                !msg.contains(secret),
                "secret value leaked through error: {msg}"
            );
        }
        other => panic!("expected EngineError::Git, got: {other:?}"),
    }
    assert_eq!(repo.head_sha().unwrap(), base, "commit must not advance");
    assert!(
        raw_git(dir.path(), &["diff", "--cached", "--name-only"])
            .trim()
            .is_empty(),
        "blocked commit must not stage the secret file"
    );
}

/// The CHECKPOINT path surfaces the same scan block as a typed outcome, not
/// an error: a mission-loop caller must be able to record the refusal and
/// keep going (erroring the run would wedge the mission — the tree is still
/// dirty on resume, so it re-hits the identical refusal forever).
#[test]
fn commit_dirty_paths_surfaces_secret_scan_refusal_as_outcome() {
    if !setup() {
        return;
    }
    let (dir, repo, base) = seeded_repo();
    let secret = "sk-ant-api03-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    write(&dir, "leak.txt", &format!("ANTHROPIC_API_KEY={secret}\n"));

    let outcome = repo
        .commit_dirty_paths("checkpoint the leak")
        .expect("a scan refusal is an outcome, not a git error");
    match outcome {
        CheckpointOutcome::RefusedBySecretScan { detail } => {
            assert!(
                detail.contains("secret scan blocked engine commit"),
                "missing scan context: {detail}"
            );
            assert!(
                detail.contains(".kranz/secret-allowlist"),
                "missing waiver guidance: {detail}"
            );
            assert!(
                detail.contains("anthropic-api-key"),
                "missing rule id: {detail}"
            );
            assert!(
                !detail.contains(secret),
                "secret value leaked through the refusal detail: {detail}"
            );
        }
        other => panic!("expected RefusedBySecretScan, got: {other:?}"),
    }
    assert_eq!(repo.head_sha().unwrap(), base, "commit must not advance");
    assert!(
        raw_git(dir.path(), &["diff", "--cached", "--name-only"])
            .trim()
            .is_empty(),
        "refused checkpoint must not stage the secret file"
    );
}

/// The checkpoint scans the mission's ADDED lines, not the full dirty files
/// (m-0f1abd was refused twice by unchanged base content): a base-shaped
/// pattern in an unchanged region must never refuse, while a secret in an
/// added line must.
#[test]
fn checkpoint_scan_judges_added_lines_not_base_content() {
    if !setup() {
        return;
    }
    let (dir, repo, _seed) = seeded_repo();
    // Base content carries a secret-shaped pattern in an UNCHANGED region.
    let base_pattern = "sk-ant-api03-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    write(
        &dir,
        "src.txt",
        &format!("fn main() {{}}\n// ANTHROPIC_API_KEY={base_pattern}\n"),
    );
    repo.add_all_and_commit("base with a pattern").unwrap();

    // Case 1: an unrelated added line — the base pattern must NOT refuse.
    write(
        &dir,
        "src.txt",
        &format!("fn helper() {{}}\n// ANTHROPIC_API_KEY={base_pattern}\n"),
    );
    let outcome = repo
        .commit_dirty_paths("checkpoint an innocent edit")
        .expect("checkpoint ran");
    match outcome {
        CheckpointOutcome::Committed(_) => {}
        other => panic!("base-region pattern must not refuse a clean added diff, got: {other:?}"),
    }

    // Case 2: an added line carries a NEW secret — refused for that line.
    let after_case1 = repo.head_sha().unwrap();
    let added_secret = "sk-ant-api03-cccccccccccccccccccccccccccccccccccccccc";
    write(
        &dir,
        "src.txt",
        &format!(
            "fn main() {{}}\n// ANTHROPIC_API_KEY={base_pattern}\nlet k = \"{added_secret}\";\n"
        ),
    );
    let outcome = repo
        .commit_dirty_paths("checkpoint the added secret")
        .expect("checkpoint ran");
    match outcome {
        CheckpointOutcome::RefusedBySecretScan { detail } => {
            assert!(detail.contains("anthropic-api-key"), "{detail}");
            assert!(
                !detail.contains(added_secret),
                "secret value leaked through the refusal: {detail}"
            );
        }
        other => panic!("added-line secret must refuse, got: {other:?}"),
    }
    assert_eq!(
        repo.head_sha().unwrap(),
        after_case1,
        "refused checkpoint must not advance"
    );
}

/// Untracked NEW files still scan full-file: `git diff HEAD` never sees
/// them, and their whole content is added lines.
#[test]
fn checkpoint_scan_still_covers_new_untracked_files() {
    if !setup() {
        return;
    }
    let (dir, repo, _base) = seeded_repo();
    let secret = "sk-ant-api03-dddddddddddddddddddddddddddddddddddddddd";
    write(&dir, "newfile.txt", &format!("token={secret}\n"));

    let outcome = repo
        .commit_dirty_paths("checkpoint the new file")
        .expect("checkpoint ran");
    match outcome {
        CheckpointOutcome::RefusedBySecretScan { .. } => {}
        other => panic!("a new untracked file with a secret must refuse, got: {other:?}"),
    }
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
        other => panic!("B must conflict against A's change, got {other:?}"),
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
    repo.add_worktree_checkout(&advance_wt, "feature-x")
        .unwrap();
    let advance_repo = GitRepo::open(&advance_wt).unwrap();
    std::fs::write(advance_wt.join("on-branch.txt"), "branch work\n").unwrap();
    let tip = advance_repo
        .add_all_and_commit("advance feature-x")
        .unwrap();
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

/// merge_no_ff refuses BEFORE any merge starts (an untracked file at a path
/// the branch would bring in, differing from the incoming content) — no
/// MERGE_HEAD is ever created, so no `git merge --abort` is attempted, and
/// the outcome carries git's verbatim refusal rather than the "abort also
/// failed" wrapper.
#[test]
fn merge_no_ff_pre_merge_head_refusal_attempts_no_abort() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    let wt_base = tempfile::tempdir().unwrap();

    // Branch that commits tracked.txt.
    let wt = worktree_dir(&wt_base, "twin");
    repo.add_worktree(&wt, "kranz/wt/m/twin", &seed).unwrap();
    let wt_repo = GitRepo::open(&wt).unwrap();
    std::fs::write(wt.join("tracked.txt"), "incoming\n").unwrap();
    wt_repo.add_all_and_commit("add tracked.txt").unwrap();

    // In the primary tree, leave an UNTRACKED file at the same path that
    // DIFFERS from the incoming content — git must refuse before starting.
    write(&dir, "tracked.txt", "local divergent copy\n");

    let outcome = repo.merge_no_ff("kranz/wt/m/twin").unwrap();
    match outcome {
        MergeOutcome::RefusedPreMerge { detail } => {
            assert!(
                detail.contains("would be overwritten by merge"),
                "expected git's verbatim refusal, got: {detail}"
            );
            assert!(
                !detail.contains("abort also failed"),
                "pre-MERGE_HEAD refusal must not show the abort wrapper: {detail}"
            );
        }
        other => panic!("expected RefusedPreMerge, got {other:?}"),
    }
    // No merge was ever started, so MERGE_HEAD must still be absent.
    assert!(
        !repo
            .root()
            .join(".git/MERGE_HEAD")
            .try_exists()
            .unwrap_or(false),
        "MERGE_HEAD must not exist after a pre-merge refusal"
    );
    // The divergent untracked file must be untouched.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
        "local divergent copy\n"
    );
}

// ---------------------------------------------------------------------------
// Audit 2026-09-01 F-10 / F-11: engine-side git and the NETWORK path.
//
// `kranz exec --push` runs `push_mission_branch` on the tree the worker just
// wrote, with the CLI's full ambient environment and whatever credential the
// remote is authenticated with. Two things have to hold at once there:
//
//  - the repository's own config is the scope a worker can write, so a
//    `credential.helper`, a `core.sshCommand`, an `insteadOf` rewrite or a
//    transport hook appearing in it is an ATTACK SIGNAL and the push is
//    refused, naming every offending key;
//  - the OPERATOR's `~/.gitconfig` is a different scope and stays in force,
//    because nulling it is what breaks an https push (no credential helper),
//    an `insteadOf` convention, and a corporate `http.proxy`.
// ---------------------------------------------------------------------------

/// A bare repository to push at, wired up as `origin`. A real remote, no
/// network: `git push` runs the whole push path against it.
fn bare_remote(dir: &TempDir) -> PathBuf {
    let bare = dir.path().join("remote.git");
    std::fs::create_dir(&bare).expect("create bare dir");
    raw_git(&bare, &["init", "--bare", "-q"]);
    raw_git(
        dir.path(),
        &["remote", "add", "origin", &bare.display().to_string()],
    );
    bare
}

/// A path the payload would create if it ever ran, and the shell one-liner
/// that creates it. Unix-only: the payload is a `/bin/sh` command.
#[cfg(unix)]
fn sentinel_payload(dir: &TempDir) -> (PathBuf, String) {
    let sentinel = dir.path().join("credential-helper-fired");
    let payload = format!("!sh -c 'touch \"{}\"'", sentinel.display());
    (sentinel, payload)
}

/// Make git actually consult its credential helpers, with `extra` prepended
/// to the argv. The command's own exit status is irrelevant (there is no
/// terminal to prompt at); what matters is whether a helper ran.
#[cfg(unix)]
fn fire_credential_helper(root: &Path, extra: &[&str]) {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = Command::new("git")
        .args(extra)
        .args(["credential", "fill"])
        .current_dir(root)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn git credential fill");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"protocol=https\nhost=example.invalid\n\n")
        .expect("write credential request");
    let _ = child.wait();
}

/// A worker-planted `credential.helper` in the repository's own config
/// refuses the push, names the key, and never runs the payload.
#[cfg(unix)]
#[test]
fn push_refuses_a_repo_local_credential_helper_and_never_runs_it() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();
    let bare = bare_remote(&dir);
    repo.create_branch("kranz/mission-1", None).unwrap();
    let (sentinel, payload) = sentinel_payload(&dir);
    raw_git(dir.path(), &["config", "credential.helper", &payload]);

    // Fixture proof: the payload is LIVE. Without this the assertion below
    // would also pass on a repo where a helper simply never fires.
    fire_credential_helper(dir.path(), &[]);
    assert!(
        sentinel.exists(),
        "fixture: an unguarded git must run the planted credential helper"
    );
    std::fs::remove_file(&sentinel).unwrap();
    // And the `-c credential.helper=` reset the hardened segment carries for
    // LOCAL operations really does clear the list, planted helper included.
    fire_credential_helper(dir.path(), &["-c", "credential.helper="]);
    assert!(
        !sentinel.exists(),
        "an empty credential.helper must reset the helper list"
    );

    let err = repo
        .push_mission_branch("origin", "kranz/mission-1")
        .expect_err("a planted credential helper must refuse the push");
    match err {
        EngineError::Git(msg) => {
            assert!(
                msg.contains("credential.helper"),
                "the refusal must name the offending key: {msg}"
            );
            assert!(
                msg.contains("attack signal"),
                "the refusal must say why it is a refusal and not a fix-up: {msg}"
            );
        }
        other => panic!("expected EngineError::Git, got {other:?}"),
    }
    assert!(
        !sentinel.exists(),
        "the planted credential helper must never execute"
    );
    // Nothing reached the remote either.
    assert!(
        raw_git(&bare, &["for-each-ref", "--format=%(refname)"])
            .trim()
            .is_empty(),
        "a refused push must not have delivered the branch"
    );
}

/// Every armed key is named in one refusal, not just the first — an operator
/// who removes the one the message named must not have to re-run to discover
/// the next. `ls-remote` is pre-flighted the same way `push` is.
#[test]
fn the_refusal_names_every_armed_key_and_covers_ls_remote() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();
    bare_remote(&dir);
    repo.create_branch("kranz/mission-1", None).unwrap();
    for (key, value) in [
        ("core.sshCommand", "sh -c evil --"),
        ("core.askPass", "/tmp/evil-askpass"),
        ("core.gitProxy", "/tmp/evil-proxy"),
        ("http.proxy", "http://attacker.invalid:8080"),
        ("remote.origin.receivepack", "/tmp/evil-receive"),
        ("url.ext::sh -c evil %S.insteadOf", "https://"),
        ("protocol.allow", "always"),
    ] {
        raw_git(dir.path(), &["config", key, value]);
    }

    let EngineError::Git(msg) = repo
        .push_mission_branch("origin", "kranz/mission-1")
        .expect_err("an armed repo config must refuse the push")
    else {
        panic!("expected EngineError::Git");
    };
    for expected in [
        "core.sshcommand",
        "core.askpass",
        "core.gitproxy",
        "http.proxy",
        "remote.origin.receivepack",
        "insteadof",
        "protocol.allow",
    ] {
        assert!(
            msg.contains(expected),
            "the refusal must name {expected}: {msg}"
        );
    }
    // Values can carry secrets (an http.proxy with a password, a credential
    // username) and the refusal goes to mission logs: keys only.
    assert!(
        !msg.contains("attacker.invalid"),
        "the refusal must name keys, never their values: {msg}"
    );

    // The same pre-flight guards the read-only network probe.
    assert!(
        repo.remote_has_branch("origin", "kranz/mission-1").is_err(),
        "ls-remote contacts a remote too and must be pre-flighted"
    );
}

/// The control case: a clean repository config pushes normally. Without this
/// the refusal above could pass on a push path that never worked.
#[test]
fn a_clean_repository_config_pushes_to_a_bare_remote() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();
    let bare = bare_remote(&dir);
    repo.create_branch("kranz/mission-1", None).unwrap();

    repo.push_mission_branch("origin", "kranz/mission-1")
        .expect("a clean repo config must push");
    assert!(
        raw_git(&bare, &["for-each-ref", "--format=%(refname)"])
            .contains("refs/heads/kranz/mission-1"),
        "the mission branch must be on the remote"
    );
    // And the read-only probe agrees.
    assert!(repo
        .remote_has_branch("origin", "kranz/mission-1")
        .expect("ls-remote against the bare remote"));
}

/// `.git/config.worktree` is a second writable scope on a linked worktree —
/// exactly the shape the integration worktree has — and the pre-flight must
/// see it. A `--local` read would not.
#[cfg(unix)]
#[test]
fn the_preflight_sees_a_planted_worktree_scoped_config() {
    if !setup() {
        return;
    }
    let (dir, _repo, _) = seeded_repo();
    let bare = bare_remote(&dir);
    let wt = dir.path().join("wt");
    raw_git(
        dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "kranz/mission-1",
            &wt.display().to_string(),
        ],
    );
    raw_git(dir.path(), &["config", "extensions.worktreeConfig", "true"]);
    let linked = GitRepo::open(&wt).expect("open the linked worktree");
    // Control: the linked worktree pushes fine before anything is planted.
    linked
        .push_mission_branch("origin", "kranz/mission-1")
        .expect("a clean linked worktree must push");
    assert!(raw_git(&bare, &["for-each-ref", "--format=%(refname)"])
        .contains("refs/heads/kranz/mission-1"));

    let (sentinel, payload) = sentinel_payload(&dir);
    raw_git(
        &wt,
        &["config", "--worktree", "credential.helper", &payload],
    );
    let err = linked
        .push_mission_branch("origin", "kranz/mission-1")
        .expect_err("a worktree-scoped credential helper must refuse the push");
    assert!(
        matches!(&err, EngineError::Git(msg) if msg.contains("credential.helper")),
        "the refusal must name the worktree-scoped key: {err:?}"
    );
    assert!(!sentinel.exists(), "the payload must never execute");
}

/// Audit F-10: the neutralization segment does not merely exist, it blanks
/// the transport programs a worker can name in the repository's own config.
/// `remote.<name>.uploadpack` is single-valued, so the empty `-c` override
/// really does replace a planted value.
#[test]
fn a_planted_remote_transport_program_is_blanked_on_local_operations() {
    if !setup() {
        return;
    }
    let (dir, _repo, _) = seeded_repo();
    raw_git(
        dir.path(),
        &["remote", "add", "origin", "https://example.invalid/r.git"],
    );
    raw_git(
        dir.path(),
        &["config", "remote.origin.uploadpack", "/tmp/evil-upload"],
    );
    // A LOCAL operation on a handle opened the ordinary way still works, and
    // the handle carries the blanking override (proved at unit level); what
    // this pins is that enumerating the armed remote does not fail the open.
    let reopened = GitRepo::open(dir.path()).expect("open must survive an armed remote config");
    assert!(reopened.is_clean().expect("status on a hardened handle"));
}

// Audit 2026-09-11 F2: the original handle survives worker execution, so its
// construction-time list cannot authorize later driver names.
#[cfg(unix)]
fn harmless_filter_fixture(root: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let script = root.join(".git/filter-fixture.sh");
    let marker = root.join(".git/filter-fixture-fired");
    let quoted_marker = marker.display().to_string().replace('\'', "'\\''");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf 'fixture' > '{quoted_marker}'\ncat\n"),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    (script, marker)
}

#[cfg(unix)]
#[test]
fn local_operations_refuse_a_new_filter_on_the_original_handle_and_its_clone() {
    if !setup() {
        return;
    }
    let (dir, repo, initial_sha) = seeded_repo();
    let clone = repo.with_hooks_disabled().unwrap();
    let (script, marker) = harmless_filter_fixture(dir.path());
    write(&dir, ".gitattributes", "*.txt filter=late.driver\n");
    write(&dir, "late.txt", "unmodified delivery bytes\n");
    raw_git(
        dir.path(),
        &[
            "config",
            "filter.late.driver.clean",
            script.to_str().unwrap(),
        ],
    );
    raw_git(dir.path(), &["hash-object", "--path=late.txt", "late.txt"]);
    assert!(marker.exists(), "the harmless filter fixture must be armed");
    std::fs::remove_file(&marker).unwrap();
    let config_before = std::fs::read(dir.path().join(".git/config")).unwrap();

    for err in [
        repo.is_clean().unwrap_err(),
        clone.commit_dirty_paths("must refuse").unwrap_err(),
        repo.checkout("main").unwrap_err(),
    ] {
        let detail = err.to_string();
        assert!(
            detail.contains("changed after opening the handle"),
            "{detail}"
        );
        assert!(detail.contains("filter.late.driver.clean"), "{detail}");
        assert!(
            !detail.contains(script.to_str().unwrap()),
            "values must stay private"
        );
    }
    assert!(!marker.exists(), "engine Git must never run the new filter");
    assert_eq!(
        raw_git(dir.path(), &["rev-parse", "HEAD"]).trim(),
        initial_sha
    );
    assert_eq!(
        std::fs::read(dir.path().join(".git/config")).unwrap(),
        config_before
    );

    // Opening again explicitly establishes a new boundary. Its overrides
    // must still suppress the reviewed driver and preserve exact file bytes.
    let reopened = GitRepo::open(dir.path()).unwrap();
    reopened.add_all_and_commit("delivery").unwrap();
    assert!(!marker.exists());
    assert_eq!(
        reopened.show_file("HEAD", "late.txt").unwrap().unwrap(),
        b"unmodified delivery bytes\n"
    );
    assert_eq!(
        std::fs::read(dir.path().join(".git/config")).unwrap(),
        config_before
    );
}

#[cfg(unix)]
#[test]
fn an_existing_driver_remains_disabled_when_its_command_changes_after_open() {
    if !setup() {
        return;
    }
    let (dir, _, _) = seeded_repo();
    raw_git(dir.path(), &["config", "filter.existing.clean", "cat"]);
    let repo = GitRepo::open(dir.path()).unwrap();
    let (script, marker) = harmless_filter_fixture(dir.path());
    raw_git(
        dir.path(),
        &["config", "filter.existing.clean", script.to_str().unwrap()],
    );
    write(&dir, ".gitattributes", "*.txt filter=existing\n");
    write(&dir, "late.txt", "exact bytes\n");
    repo.add_all_and_commit("delivery").unwrap();
    assert!(!marker.exists());
    assert_eq!(
        repo.show_file("HEAD", "late.txt").unwrap().unwrap(),
        b"exact bytes\n"
    );
}

#[cfg(unix)]
#[test]
fn inactive_conditional_includes_cannot_arm_a_worktree_child_checkout() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();
    write(&dir, ".gitattributes", "*.txt filter=conditional\n");
    write(&dir, "conditional.txt", "exact bytes\n");
    let sha = repo.add_all_and_commit("conditional attributes").unwrap();
    let (script, marker) = harmless_filter_fixture(dir.path());
    let included = dir.path().join(".git/conditional.cfg");
    raw_git(
        dir.path(),
        &[
            "config",
            "--file",
            included.to_str().unwrap(),
            "filter.conditional.smudge",
            script.to_str().unwrap(),
        ],
    );
    raw_git(
        dir.path(),
        &[
            "config",
            "includeIf.onbranch:kranz/conditional.path",
            included.to_str().unwrap(),
        ],
    );
    let current_filters = Command::new("git")
        .args(["config", "--get-regexp", "^filter\\."])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(
        current_filters.status.code(),
        Some(1),
        "the include is inactive on main"
    );
    let wt = dir.path().join("conditional-worktree");
    for error in [
        repo.add_worktree(&wt, "kranz/conditional", &sha)
            .unwrap_err(),
        GitRepo::open(dir.path()).unwrap_err(),
    ] {
        assert!(error
            .to_string()
            .contains("conditional repository config includes"));
    }
    assert!(!marker.exists(), "no child checkout may execute the filter");
    assert!(!wt.exists());
    assert_eq!(raw_git(dir.path(), &["rev-parse", "HEAD"]).trim(), sha);

    // Positive fixture: the ordinary command changes branch context within
    // its own child checkout and activates the previously invisible driver.
    raw_git(
        dir.path(),
        &[
            "worktree",
            "add",
            "-b",
            "kranz/conditional",
            wt.to_str().unwrap(),
            &sha,
        ],
    );
    assert!(
        marker.exists(),
        "the harmless conditional smudge fixture must run"
    );
}

#[cfg(unix)]
#[test]
fn a_linked_handle_refuses_new_worktree_scoped_drivers() {
    if !setup() {
        return;
    }
    let (dir, repo, initial_sha) = seeded_repo();
    let wt = dir.path().join("linked");
    repo.add_worktree(&wt, "kranz/linked", &initial_sha)
        .unwrap();
    let linked = GitRepo::open(&wt).unwrap();
    raw_git(dir.path(), &["config", "extensions.worktreeConfig", "true"]);
    let (script, marker) = harmless_filter_fixture(dir.path());
    raw_git(
        &wt,
        &[
            "config",
            "--worktree",
            "filter.late.clean",
            script.to_str().unwrap(),
        ],
    );
    let err = linked.is_clean().unwrap_err().to_string();
    assert!(err.contains("filter.late.clean"), "{err}");
    assert!(!marker.exists());
}

#[test]
fn unreadable_driver_configuration_fails_closed_after_open() {
    if !setup() {
        return;
    }
    let (dir, repo, _) = seeded_repo();
    std::fs::write(dir.path().join(".git/config"), b"[invalid fixture syntax\n").unwrap();
    let err = repo.is_clean().unwrap_err().to_string();
    assert!(
        err.contains("cannot enumerate executable repository configuration"),
        "{err}"
    );
    assert!(!err.contains("invalid fixture syntax"));
}

#[test]
fn driver_names_that_cannot_be_overridden_fail_closed() {
    if !setup() {
        return;
    }
    for key in ["filter.name=other.clean", "merge.name=other.driver"] {
        let (dir, repo, _) = seeded_repo();
        raw_git(dir.path(), &["config", key, "false"]);
        for err in [
            repo.is_clean().unwrap_err(),
            GitRepo::open(dir.path()).unwrap_err(),
        ] {
            assert!(err
                .to_string()
                .contains("driver name cannot be safely overridden"));
        }
    }
}

#[cfg(unix)]
#[test]
fn hardened_local_git_drops_ambient_authority() {
    use std::os::unix::fs::PermissionsExt as _;
    const CHILD_ROOT: &str = "KRANZ_GIT_ENV_FIXTURE_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let repo = GitRepo::open(PathBuf::from(root)).unwrap();
        assert!(repo.is_clean().unwrap());
        repo.resolved_identity().unwrap();
        return;
    }
    if !setup() {
        return;
    }
    let (dir, _, _) = seeded_repo();
    let real_git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .unwrap();
    let bin = dir.path().join(".git/fixture-bin");
    std::fs::create_dir(&bin).unwrap();
    let marker = dir.path().join(".git/ambient-authority-received");
    let quote = |path: &Path| path.display().to_string().replace('\'', "'\\''");
    let wrapper = bin.join("git");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ -n \"${{KRANZ_GIT_ENV_SENTINEL-}}${{GIT_CONFIG_COUNT-}}${{GIT_CONFIG_PARAMETERS-}}\" ]; then\n  printf 'fixture' > '{}'\n  exit 91\nfi\nexec '{}' \"$@\"\n",
            quote(&marker),
            quote(&real_git)
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut path = vec![bin];
    path.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "hardened_local_git_drops_ambient_authority",
            "--nocapture",
        ])
        .env(CHILD_ROOT, dir.path())
        .env("PATH", std::env::join_paths(path).unwrap())
        .env("KRANZ_GIT_ENV_SENTINEL", "harmless-test-sentinel")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "filter.injected.clean")
        .env("GIT_CONFIG_VALUE_0", "false")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!marker.exists(), "local Git inherited ambient authority");
}
