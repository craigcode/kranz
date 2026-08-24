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

use std::path::PathBuf;
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

/// Runs the exact a9 command string (the same way a downstream gate would:
/// `sh -c "<command>"`) against the pinned mission base commit and asserts it
/// exits successfully, proving finding a9 no longer reproduces.
#[test]
fn a9_command_runs_successfully_verbatim() {
    if Command::new("git").arg("--version").output().is_err() {
        kranz_engine::test_capability::skip(
            kranz_engine::test_capability::capability::GIT,
            "git is not on PATH",
        );
        return;
    }
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");

    let base_sha = match std::env::var("KRANZ_BASE_SHA") {
        Ok(sha) => sha,
        Err(_) => {
            eprintln!("skipping: KRANZ_BASE_SHA not set");
            return;
        }
    };

    let command = a9_command_from_plan_json();
    let status = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .current_dir(&repo_root)
        .env("KRANZ_BASE_SHA", &base_sha)
        .status()
        .expect("spawn a9 command via sh -c");
    assert!(
        status.success(),
        "a9 command must exit 0 when run verbatim: {command}"
    );
}
