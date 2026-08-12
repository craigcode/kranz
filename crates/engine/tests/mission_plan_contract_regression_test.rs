//! Regression guard for validation finding a3 (lint never blocks / no
//! nested-runtime panic): the mission's own committed `validation_contract`
//! command for a3 must use the `cargo test -p <pkg> -- <FILTER> <FILTER>`
//! form. `cargo test -p <pkg> <FILTER> <FILTER>` (no `--`) is rejected by
//! cargo with "unexpected argument ... found" for a second bare filter, so a
//! downstream gate running that string verbatim would fail even though the
//! underlying tests pass. The historical assertion is preserved as a
//! committed fixture so cleanup of runtime mission state cannot break the
//! workspace gate.

fn a3_command_from_plan_json() -> String {
    let text = include_str!("fixtures/m-2d5583-plan.json");
    let plan: serde_json::Value = serde_json::from_str(text).expect("plan.json parses");
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
    let text = include_str!("fixtures/m-2d5583-plan.md");
    assert!(
        text.contains(
            "cargo test -p kranz-engine -- approval_lint_never_blocks approval_lint_no_nested_runtime_panic"
        ),
        "plan.md's rendered a3 command must match the runnable `--` form from plan.json"
    );
}

/// Pins the other half of the contract without recursively invoking Cargo.
///
/// `cargo test --workspace` already executes these library tests in this
/// same suite. Spawning the historical `cargo test ...` contract from an
/// integration test can wait forever on the outer Cargo process's target
/// lock, so the two syntax tests above own the exact command shape while
/// this test proves every named filter still resolves to a real test.
#[test]
fn a3_command_names_existing_tests() {
    let command = a3_command_from_plan_json();
    let source = include_str!("mission_test.rs");
    for name in [
        "approval_lint_never_blocks",
        "approval_lint_no_nested_runtime_panic",
    ] {
        assert!(
            command.split_ascii_whitespace().any(|word| word == name),
            "a3 command must name {name}: {command}"
        );
        assert!(
            source.contains(&format!("fn {name}(")),
            "a3 filter must resolve to an existing test: {name}"
        );
    }
}
