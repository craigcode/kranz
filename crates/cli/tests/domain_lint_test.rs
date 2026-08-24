//! End-to-end tests for `kranz domain-lint` (KRZ-314): seed the hashed
//! denylist from an operator-local plaintext file, trip the lint on a
//! planted synthetic term (exit 1), go green on removal (exit 0). Engine
//! mechanics (normalization, enumeration, waivers) are covered in
//! `crates/engine`; these tests pin the CLI contract.
//!
//! Throwaway git repositories in tempdirs; skips cleanly when git is not on
//! PATH; host git config is masked (the git_ops_test idiom). All vocabulary
//! is synthetic (`zz-` convention) — fixtures never carry real protected
//! terms.

use kranz_cli::commands;
use std::path::Path;
use std::process::Command;
use std::sync::Once;
use tempfile::TempDir;

static ENV_ISOLATION: Once = Once::new();

fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing = std::env::temp_dir().join(format!(
            "kranz-domain-lint-cli-test-no-config-{}",
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

#[test]
fn domain_lint_cli_seed_plant_hit_remove_green() {
    if !setup() {
        return;
    }
    let repo = init_repo();
    // The plaintext terms file lives OUTSIDE the repo — that separation is
    // the whole point of the seed mechanism.
    let terms_dir = tempfile::tempdir().unwrap();
    let terms_file = terms_dir.path().join("terms.local");
    std::fs::write(&terms_file, "zz-acme lint canary\n").unwrap();

    // Seed: exit 0, config written, and the config carries no plaintext.
    let code = commands::cmd_domain_lint(repo.path(), Some(terms_file.as_path()), false)
        .expect("seed run");
    assert_eq!(code, 0, "seed exits 0");
    let config =
        std::fs::read_to_string(repo.path().join(kranz_engine::domain_lint::DENYLIST_PATH))
            .expect("config written");
    for fragment in ["zz", "acme", "canary"] {
        assert!(
            !config.contains(fragment),
            "seeded config leaks plaintext {fragment:?}: {config}"
        );
    }

    // Plant a banned synthetic term on a known line of a scratch fixture:
    // the lint fails with exit 1.
    std::fs::create_dir_all(repo.path().join("docs")).unwrap();
    std::fs::write(
        repo.path().join("docs/scratch.md"),
        "fine\nplants the zz acme lint canary\n",
    )
    .unwrap();
    raw_git(repo.path(), &["add", "-A"]);
    let code = commands::cmd_domain_lint(repo.path(), None, false).expect("lint run");
    assert_eq!(code, 1, "a planted banned term exits 1");
    let code = commands::cmd_domain_lint(repo.path(), None, true).expect("lint run");
    assert_eq!(code, 1, "json mode exits 1 too");

    // Removal goes green.
    std::fs::write(repo.path().join("docs/scratch.md"), "fine\nclean\n").unwrap();
    let code = commands::cmd_domain_lint(repo.path(), None, false).expect("lint run");
    assert_eq!(code, 0, "removal exits 0");
}

#[test]
fn domain_lint_cli_missing_config_fails_loud() {
    if !setup() {
        return;
    }
    let repo = init_repo();
    // No seeded denylist: an error, never a vacuous pass.
    assert!(commands::cmd_domain_lint(repo.path(), None, false).is_err());
}
