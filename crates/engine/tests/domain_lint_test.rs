//! Integration tests for `kranz_engine::domain_lint` tree enumeration:
//! the candidate set must respect .gitignore via git's own engine and must
//! exclude mission runtime artifacts (operator content) and the lint's own
//! policy files. Pure scanning/normalization lives in the module's unit
//! tests.
//!
//! Each test builds a throwaway repository inside a tempdir and skips
//! cleanly (eprintln + return) when git is not on PATH; the host's
//! global/system git config is masked so nothing leaks in (the
//! git_ops_test idiom).

use kranz_engine::domain_lint::{self, Denylist};
use std::path::Path;
use std::process::Command;
use std::sync::Once;
use tempfile::TempDir;

static ENV_ISOLATION: Once = Once::new();

/// Point global/system git config at a file that never exists, and stop git
/// from discovering a repository above the temp dir.
fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing = std::env::temp_dir().join(format!(
            "kranz-domain-lint-test-no-config-{}",
            std::process::id()
        ));
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

fn raw_git(dir: &Path, args: &[&str]) {
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
}

fn init_repo() -> TempDir {
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
    dir
}

fn write(dir: &TempDir, name: &str, content: &str) {
    let path = dir.path().join(name);
    std::fs::create_dir_all(path.parent().expect("parent")).unwrap();
    std::fs::write(path, content).expect("write file");
}

/// A denylist over purely synthetic vocabulary (the `zz-` convention —
/// test fixtures never carry real protected terms).
fn test_denylist() -> Denylist {
    let config = domain_lint::seed_config(None, "zz-acme lint canary\n").expect("seed");
    domain_lint::load_denylist(&config).expect("load")
}

fn hit_paths(report: &domain_lint::LintReport) -> Vec<String> {
    let mut paths: Vec<String> = report.findings.iter().map(|f| f.path.clone()).collect();
    paths.sort();
    paths
}

#[test]
fn domain_lint_enumeration_respects_gitignore_and_excludes_missions() {
    if !setup() {
        return;
    }
    let dir = init_repo();
    let denylist = test_denylist();
    const LEAK: &str = "this line plants the zz acme lint canary\n";

    // In scope: a tracked source file and an untracked-but-not-ignored one.
    write(&dir, "crates/core/leak.md", LEAK);
    write(&dir, "docs/untracked-leak.md", LEAK);
    // Out of scope by .gitignore (git's own engine decides).
    write(&dir, ".gitignore", "ignored/\n");
    write(&dir, "ignored/leak.md", LEAK);
    // Out of scope by rule: mission runtime artifacts are operator content,
    // even when TRACKED (a plan committed under .kranz/missions/).
    write(&dir, ".kranz/missions/m-zz/plan.md", LEAK);
    // Out of scope by rule: the lint never reads its own policy files.
    write(&dir, domain_lint::DENYLIST_PATH, LEAK);
    write(&dir, domain_lint::ALLOWLIST_PATH, LEAK);
    raw_git(dir.path(), &["add", "-A"]);

    let report =
        domain_lint::lint_tree(dir.path(), &denylist, &Default::default()).expect("lint tree");
    assert_eq!(
        hit_paths(&report),
        vec![
            "crates/core/leak.md".to_string(),
            "docs/untracked-leak.md".to_string()
        ],
        "only in-scope files report: {report:?}"
    );

    // The untracked file joins the index and keeps reporting (scope is
    // tracked + untracked-not-ignored, not commit state).
    raw_git(dir.path(), &["add", "-A"]);
    let report =
        domain_lint::lint_tree(dir.path(), &denylist, &Default::default()).expect("lint tree");
    assert_eq!(hit_paths(&report).len(), 2, "{report:?}");
}

#[test]
fn domain_lint_enumeration_skips_binary_candidates() {
    if !setup() {
        return;
    }
    let dir = init_repo();
    let denylist = test_denylist();

    // A binary candidate (NUL byte) is skipped, never tokenized — the
    // planted term inside it must not report.
    std::fs::write(
        dir.path().join("blob.bin"),
        [b"zz acme lint canary".as_slice(), &[0]].concat(),
    )
    .unwrap();
    write(&dir, "real.md", "zz acme lint canary\n");
    raw_git(dir.path(), &["add", "-A"]);

    let report =
        domain_lint::lint_tree(dir.path(), &denylist, &Default::default()).expect("lint tree");
    assert_eq!(
        hit_paths(&report),
        vec!["real.md".to_string()],
        "binary skipped: {report:?}"
    );
    assert_eq!(report.files_skipped, 1, "{report:?}");
}

/// Symlink creation needs no privilege on unix; the scrub tests gate their
/// no-follow cases the same way.
#[cfg(unix)]
#[test]
fn domain_lint_enumeration_never_reads_through_symlinks() {
    if !setup() {
        return;
    }
    let dir = init_repo();
    let denylist = test_denylist();

    write(&dir, "real-target.md", "zz acme lint canary\n");
    std::os::unix::fs::symlink(
        dir.path().join("real-target.md"),
        dir.path().join("linked.md"),
    )
    .unwrap();
    raw_git(dir.path(), &["add", "-A"]);

    // Only the real file reports; the checked-in symlink carries no
    // lintable bytes of its own and is never read through.
    let report =
        domain_lint::lint_tree(dir.path(), &denylist, &Default::default()).expect("lint tree");
    assert_eq!(
        hit_paths(&report),
        vec!["real-target.md".to_string()],
        "symlink not read through: {report:?}"
    );
    assert_eq!(report.files_skipped, 1, "{report:?}");

    // Anti-vacuity: pointing the scanner at the symlink DIRECTLY still
    // yields nothing, so the exclusion — not a dead denylist — did the
    // filtering above.
    let report = domain_lint::lint_files(
        dir.path(),
        std::slice::from_ref(&std::path::PathBuf::from("linked.md")),
        &denylist,
    );
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.files_skipped, 1, "{report:?}");
}
