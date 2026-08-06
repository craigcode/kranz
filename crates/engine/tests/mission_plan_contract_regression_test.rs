//! Regression guard for validation finding a3 (lint never blocks / no
//! nested-runtime panic): the mission's own committed `validation_contract`
//! command for a3 must use the `cargo test -p <pkg> -- <FILTER> <FILTER>`
//! form. `cargo test -p <pkg> <FILTER> <FILTER>` (no `--`) is rejected by
//! cargo with "unexpected argument ... found" for a second bare filter, so a
//! downstream gate running that string verbatim would fail even though the
//! underlying tests pass. See .kranz/missions/m-2d5583/plan.json a3.

use std::path::PathBuf;
use std::process::Command;

fn mission_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(".kranz")
        .join("missions")
        .join("m-2d5583")
}

fn a3_command_from_plan_json() -> String {
    let text = std::fs::read_to_string(mission_root().join("plan.json")).expect("read plan.json");
    let plan: serde_json::Value = serde_json::from_str(&text).expect("plan.json parses");
    let assertions = plan["validationContract"]
        .as_array()
        .expect("validationContract array");
    let a3 = assertions
        .iter()
        .find(|a| a["id"] == "a3")
        .expect("a3 assertion present");
    a3["command"]
        .as_str()
        .expect("a3 command is a string")
        .to_string()
}

#[test]
fn plan_json_a3_command_uses_dashdash_filter_form() {
    let command = a3_command_from_plan_json();
    assert!(
        command.contains("-- approval_lint_never_blocks approval_lint_no_nested_runtime_panic"),
        "a3 command must separate the two bare TESTNAME filters with `--` \
         (cargo rejects a second positional filter otherwise): {command}"
    );
}

#[test]
fn plan_md_a3_command_uses_dashdash_filter_form() {
    let text = std::fs::read_to_string(mission_root().join("plan.md")).expect("read plan.md");
    assert!(
        text.contains(
            "cargo test -p kranz-engine -- approval_lint_never_blocks approval_lint_no_nested_runtime_panic"
        ),
        "plan.md's rendered a3 command must match the runnable `--` form from plan.json"
    );
}

/// Runs the exact a3 command string (interpreted the same way a downstream
/// gate would: `sh -c "<command>"`) and asserts it exits successfully,
/// proving finding a3 no longer reproduces.
#[test]
fn a3_command_runs_successfully_verbatim() {
    if Command::new("git").arg("--version").output().is_err() {
        eprintln!("skipping: git not on PATH");
        return;
    }
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let command = a3_command_from_plan_json();
    // The contract command ends in a grep -q pipe, so on failure its output
    // is EMPTY BY CONSTRUCTION (grep -q prints nothing and swallows cargo's
    // stream). A failing run is otherwise undiagnosable on an ephemeral CI
    // runner (three consecutive ubuntu CI failures showed zero evidence).
    // On failure, re-run the cargo half WITHOUT the grep pipe for the tail.
    let output = Command::new("sh")
        .arg("-c")
        .arg(&command)
        .current_dir(&repo_root)
        .output()
        .expect("spawn a3 command via sh -c");
    if !output.status.success() {
        let inner = command
            .split(" 2>&1 |")
            .next()
            .unwrap_or(&command)
            .to_string();
        let diag = Command::new("sh")
            .arg("-c")
            .arg(&inner)
            .current_dir(&repo_root)
            .output()
            .expect("spawn a3 inner command via sh -c");
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&diag.stdout),
            String::from_utf8_lossy(&diag.stderr)
        );
        let lines: Vec<&str> = combined.lines().collect();
        let tail = &lines[lines.len().saturating_sub(40)..];
        panic!(
            "a3 command must exit 0 when run verbatim: {command}\n\
             (pipeline exit: {:?}; diagnostic re-run of `{inner}`)\n\
             --- inner output tail ---\n{}",
            output.status.code(),
            tail.join("\n")
        );
    }
}
