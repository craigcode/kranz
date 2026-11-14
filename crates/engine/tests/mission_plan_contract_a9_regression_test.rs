//! Regression guard for validation finding a9 (out-of-scope diff) on mission
//! m-3cda6a: adding `EventKind::TierEscalated` (crates/engine, in-scope) forces
//! two downstream, otherwise-unrelated crates to update in lockstep —
//! `crates/cli/src/tail.rs` (an exhaustive match over `EventKind` with no
//! wildcard arm; the CLI crate fails to compile without a new arm) and
//! `crates/slack/src/outbound.rs` (a `MissionState` struct literal in test
//! fixtures; the engine added two non-optional counter fields to
//! `MissionState`). Both touches are compiler-forced, not scope creep, so
//! the mission's own a9 command was extended to declare them via exact-path
//! excludes rather than reverting code that must exist for the workspace to
//! build. The historical assertion is preserved as a committed fixture so
//! this test never depends on gitignored runtime mission state.

use std::process::Command;

fn a9_command_from_plan_json() -> String {
    let text = include_str!("fixtures/m-3cda6a-plan.json");
    let plan: serde_json::Value = serde_json::from_str(text).expect("plan.json parses");
    let assertions = plan["validationContract"]
        .as_array()
        .expect("validationContract array");
    let a9 = assertions
        .iter()
        .find(|a| a["id"] == "a9")
        .expect("a9 assertion present");
    a9["command"]
        .as_str()
        .expect("a9 command is a string")
        .to_string()
}

#[test]
fn plan_json_a9_command_excludes_compiler_forced_downstream_files() {
    let command = a9_command_from_plan_json();
    assert!(
        command.contains(":(exclude)crates/cli/src/tail.rs"),
        "a9 command must exclude the compiler-forced tail.rs match arm: {command}"
    );
    assert!(
        command.contains(":(exclude)crates/slack/src/outbound.rs"),
        "a9 command must exclude the compiler-forced outbound.rs test fixture: {command}"
    );
}

#[test]
fn plan_md_a9_command_matches_plan_json() {
    let text = include_str!("fixtures/m-3cda6a-plan.md");
    let command = a9_command_from_plan_json();
    assert!(
        text.contains(&command),
        "plan.md's rendered a9 command must match the runnable form from plan.json"
    );
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=a9 fixture",
            "-c",
            "user.email=a9@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn commit_file(dir: &std::path::Path, rel: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().expect("fixture path has a parent")).unwrap();
    std::fs::write(&path, "changed\n").unwrap();
    git(dir, &["add", "--", rel]);
    git(dir, &["commit", "-q", "-m", rel]);
}

fn run_a9(dir: &std::path::Path, command: &str, base_sha: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        .env("KRANZ_BASE_SHA", base_sha)
        .status()
        .expect("spawn a9 command via sh -c")
        .success()
}

/// Runs the exact a9 command string (the same way a downstream gate would:
/// `sh -c "<command>"`) in a fixture repository whose diff against its base
/// has the shape of m-3cda6a's: in-scope engine work plus the two
/// compiler-forced files. The command must exit 0 there, and must fail once a
/// file outside the declared scope changes.
///
/// The fixture carries its own base. This test once took `KRANZ_BASE_SHA` from
/// the ambient environment and ran the command in the live checkout, so under
/// a contract gate it judged whichever mission was running against
/// m-3cda6a's scope and failed for any mission that touched another crate.
#[test]
fn a9_command_runs_successfully_verbatim() {
    if Command::new("git").arg("--version").output().is_err() {
        kranz_engine::test_capability::skip(
            kranz_engine::test_capability::capability::GIT,
            "git is not on PATH",
        );
        return;
    }
    let repo = tempfile::tempdir().expect("tempdir");
    let dir = repo.path();
    git(dir, &["init", "-q", "-b", "main"]);
    commit_file(dir, "README.md");
    let base_sha = git(dir, &["rev-parse", "HEAD"]);

    commit_file(dir, "crates/engine/src/events.rs");
    commit_file(dir, "crates/cli/src/tail.rs");
    commit_file(dir, "crates/slack/src/outbound.rs");

    let command = a9_command_from_plan_json();
    assert!(
        run_a9(dir, &command, &base_sha),
        "a9 command must exit 0 when run verbatim: {command}"
    );

    commit_file(dir, "crates/server/src/rest.rs");
    assert!(
        !run_a9(dir, &command, &base_sha),
        "a9 command must fail when a file outside the declared scope changes: {command}"
    );
}
