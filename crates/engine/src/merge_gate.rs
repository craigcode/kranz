//! Gate-suite runner: replays the CI gate suite (`.github/workflows/ci.yml`)
//! locally, stopping at the first failing gate, so the gated merge action can
//! refuse a merge exactly when CI would refuse it.
//!
//! The runner is pure and injectable: it never calls a shell directly. It
//! takes a command executor closure so tests can inject fake command
//! outcomes and production code can wrap
//! [`crate::orchestrator`]'s existing shell runner without duplicating its
//! timeout/process-group kill semantics.

use std::path::{Path, PathBuf};

/// Result of running the full gate suite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateSuiteResult {
    /// Every gate in the suite passed.
    Passed,
    /// A gate failed; later gates were not run.
    Failed {
        /// Human label identifying the failing gate (its command string).
        gate: String,
        /// That gate's verbatim captured output (stdout+stderr).
        output: String,
    },
}

/// One gate command to run, and the directory to run it in (relative to the
/// repo root).
struct Gate {
    command: &'static str,
    cwd_suffix: &'static str,
}

const RUST_GATES: [Gate; 3] = [
    Gate {
        command: "cargo fmt --all --check",
        cwd_suffix: "",
    },
    Gate {
        command: "cargo clippy --workspace --all-targets -- -D warnings",
        cwd_suffix: "",
    },
    Gate {
        command: "cargo test --workspace",
        cwd_suffix: "",
    },
];

const DASHBOARD_GATES: [Gate; 5] = [
    Gate {
        command: "npm ci",
        cwd_suffix: "apps/dashboard",
    },
    Gate {
        command: "npx tsc --noEmit",
        cwd_suffix: "apps/dashboard",
    },
    Gate {
        command: "npm run build",
        cwd_suffix: "apps/dashboard",
    },
    Gate {
        command: "npm run test",
        cwd_suffix: "apps/dashboard",
    },
    Gate {
        command: "npm run lint",
        cwd_suffix: "apps/dashboard",
    },
];

/// Runs the Kranz CI gate suite, matching `.github/workflows/ci.yml` exactly
/// and in order: `cargo fmt --all --check`, `cargo clippy --workspace
/// --all-targets -- -D warnings`, `cargo test --workspace`, and — only when
/// `dashboard_touched` — `npm ci` / `npx tsc --noEmit` / `npm run build` /
/// `npm run test` / `npm run lint` run with cwd `apps/dashboard`. Stops at
/// the first failing gate.
///
/// `executor` is called as `executor(command, cwd)` and must return
/// `(success, combined_stdout_stderr)`; production callers wrap
/// [`crate::orchestrator`]'s shell runner, tests inject a scripted fake.
pub fn run_gate_suite<F>(repo_root: &Path, dashboard_touched: bool, executor: F) -> GateSuiteResult
where
    F: Fn(&str, &Path) -> (bool, String),
{
    let mut gates: Vec<&Gate> = RUST_GATES.iter().collect();
    if dashboard_touched {
        gates.extend(DASHBOARD_GATES.iter());
    }

    for gate in gates {
        let cwd: PathBuf = if gate.cwd_suffix.is_empty() {
            repo_root.to_path_buf()
        } else {
            repo_root.join(gate.cwd_suffix)
        };
        let (ok, output) = executor(gate.command, &cwd);
        if !ok {
            return GateSuiteResult::Failed {
                gate: gate.command.to_string(),
                output,
            };
        }
    }

    GateSuiteResult::Passed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    /// Records every (command, cwd) invocation in call order, and returns a
    /// scripted (success, output) per command (default: pass, empty output).
    struct FakeExecutor {
        calls: RefCell<Vec<(String, PathBuf)>>,
        failing_command: Option<&'static str>,
        failing_output: &'static str,
    }

    impl FakeExecutor {
        fn all_pass() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                failing_command: None,
                failing_output: "",
            }
        }

        fn failing(command: &'static str, output: &'static str) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                failing_command: Some(command),
                failing_output: output,
            }
        }

        fn run(&self, command: &str, cwd: &Path) -> (bool, String) {
            self.calls
                .borrow_mut()
                .push((command.to_string(), cwd.to_path_buf()));
            match self.failing_command {
                Some(failing) if failing == command => (false, self.failing_output.to_string()),
                _ => (true, String::new()),
            }
        }

        fn commands(&self) -> Vec<String> {
            self.calls.borrow().iter().map(|(c, _)| c.clone()).collect()
        }
    }

    const RUST_GATE_COMMANDS: [&str; 3] = [
        "cargo fmt --all --check",
        "cargo clippy --workspace --all-targets -- -D warnings",
        "cargo test --workspace",
    ];

    const DASHBOARD_GATE_COMMANDS: [&str; 5] = [
        "npm ci",
        "npx tsc --noEmit",
        "npm run build",
        "npm run test",
        "npm run lint",
    ];

    #[test]
    fn all_gates_green_yields_passed_and_issues_all_ci_commands_in_order() {
        let repo_root = PathBuf::from("/repo");
        let exec = FakeExecutor::all_pass();

        let result = run_gate_suite(&repo_root, true, |cmd, cwd| exec.run(cmd, cwd));

        assert_eq!(result, GateSuiteResult::Passed);

        let expected: Vec<String> = RUST_GATE_COMMANDS
            .iter()
            .chain(DASHBOARD_GATE_COMMANDS.iter())
            .map(|s| s.to_string())
            .collect();
        assert_eq!(exec.commands(), expected);
    }

    #[test]
    fn a_failing_gate_returns_failed_with_its_label_and_output_and_stops_the_suite() {
        let repo_root = PathBuf::from("/repo");
        let exec = FakeExecutor::failing(
            "cargo clippy --workspace --all-targets -- -D warnings",
            "warning: unused variable `x`\nerror: could not compile",
        );

        let result = run_gate_suite(&repo_root, true, |cmd, cwd| exec.run(cmd, cwd));

        match result {
            GateSuiteResult::Failed { gate, output } => {
                assert_eq!(
                    gate,
                    "cargo clippy --workspace --all-targets -- -D warnings"
                );
                assert_eq!(
                    output,
                    "warning: unused variable `x`\nerror: could not compile"
                );
            }
            GateSuiteResult::Passed => panic!("expected Failed, got Passed"),
        }

        // clippy is gate #2; only fmt and clippy should have been invoked —
        // `cargo test` and the dashboard gates must never run.
        assert_eq!(
            exec.commands(),
            vec![
                "cargo fmt --all --check".to_string(),
                "cargo clippy --workspace --all-targets -- -D warnings".to_string(),
            ]
        );
    }

    #[test]
    fn dashboard_not_touched_omits_all_dashboard_commands() {
        let repo_root = PathBuf::from("/repo");
        let exec = FakeExecutor::all_pass();

        let result = run_gate_suite(&repo_root, false, |cmd, cwd| exec.run(cmd, cwd));

        assert_eq!(result, GateSuiteResult::Passed);
        let commands = exec.commands();
        for dashboard_cmd in DASHBOARD_GATE_COMMANDS {
            assert!(
                !commands.iter().any(|c| c == dashboard_cmd),
                "dashboard command {dashboard_cmd} must not run when dashboard_touched=false"
            );
        }
        assert_eq!(
            commands,
            RUST_GATE_COMMANDS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn dashboard_touched_runs_dashboard_commands_after_rust_gates_with_correct_cwd() {
        let repo_root = PathBuf::from("/repo");
        let exec = FakeExecutor::all_pass();

        let result = run_gate_suite(&repo_root, true, |cmd, cwd| exec.run(cmd, cwd));
        assert_eq!(result, GateSuiteResult::Passed);

        let calls = exec.calls.borrow();
        assert_eq!(calls.len(), 8);
        // dashboard commands come after the three rust gates, in order.
        assert_eq!(calls[3].0, "npm ci");
        assert_eq!(calls[4].0, "npx tsc --noEmit");
        assert_eq!(calls[5].0, "npm run build");
        assert_eq!(calls[6].0, "npm run test");
        assert_eq!(calls[7].0, "npm run lint");
        for (_, cwd) in calls.iter().skip(3) {
            assert_eq!(cwd, &repo_root.join("apps/dashboard"));
        }
        for (_, cwd) in calls.iter().take(3) {
            assert_eq!(cwd, &repo_root);
        }
    }

    #[test]
    fn issued_gate_command_strings_match_ci_yml_exactly() {
        let repo_root = PathBuf::from("/repo");
        let exec = FakeExecutor::all_pass();
        run_gate_suite(&repo_root, true, |cmd, cwd| exec.run(cmd, cwd));

        assert_eq!(
            exec.commands(),
            vec![
                "cargo fmt --all --check",
                "cargo clippy --workspace --all-targets -- -D warnings",
                "cargo test --workspace",
                "npm ci",
                "npx tsc --noEmit",
                "npm run build",
                "npm run test",
                "npm run lint",
            ]
        );
    }
}
