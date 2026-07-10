//! Repo-configured merge-gate runner.
//!
//! The gate suite is read from the live base branch's tracked
//! [`.kranz/merge-gates.json`](MERGE_GATES_PATH). Keeping the suite on the
//! base branch prevents a mission from weakening the checks that judge its own
//! diff, and lets non-Rust repositories define gates in their own language.
//! The runner itself is pure and injectable: production wraps the existing
//! bounded shell runner while tests inject deterministic outcomes.

use serde::Deserialize;
use std::path::{Component, Path, PathBuf};

pub const MERGE_GATES_PATH: &str = ".kranz/merge-gates.json";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GateSuite {
    pub gates: Vec<Gate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Gate {
    pub command: String,
    #[serde(default = "default_cwd")]
    pub cwd: String,
    /// Relative path prefixes. Empty means the gate always runs; otherwise it
    /// runs when at least one changed path equals or is below a prefix.
    #[serde(default)]
    pub when_paths: Vec<String>,
}

fn default_cwd() -> String {
    ".".to_string()
}

/// Result of running the applicable gates in a suite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateSuiteResult {
    Passed,
    Failed { gate: String, output: String },
}

/// Parse and validate a tracked gate-suite file. Invalid or empty suites fail
/// closed: a Merge action must never become green merely because its repo
/// config is absent or malformed.
pub fn parse_gate_suite(bytes: &[u8]) -> Result<GateSuite, String> {
    let suite: GateSuite =
        serde_json::from_slice(bytes).map_err(|e| format!("invalid {MERGE_GATES_PATH}: {e}"))?;
    validate_gate_suite(&suite)?;
    Ok(suite)
}

fn validate_gate_suite(suite: &GateSuite) -> Result<(), String> {
    if suite.gates.is_empty() {
        return Err(format!(
            "{MERGE_GATES_PATH} must define at least one merge gate"
        ));
    }
    if !suite.gates.iter().any(|gate| gate.when_paths.is_empty()) {
        return Err(format!(
            "{MERGE_GATES_PATH} must include at least one unconditional gate so every diff is validated"
        ));
    }

    for (index, gate) in suite.gates.iter().enumerate() {
        if gate.command.trim().is_empty() {
            return Err(format!(
                "{MERGE_GATES_PATH} gate {} has an empty command",
                index + 1
            ));
        }
        if gate.command.contains(['\n', '\r', '\0']) {
            return Err(format!(
                "{MERGE_GATES_PATH} gate {} command must be a single non-NUL line",
                index + 1
            ));
        }
        validate_relative_path(&gate.cwd, "cwd", index)?;
        for prefix in &gate.when_paths {
            validate_relative_path(prefix, "whenPaths entry", index)?;
            if prefix == "." {
                return Err(format!(
                    "{MERGE_GATES_PATH} gate {} should omit whenPaths to run unconditionally",
                    index + 1
                ));
            }
        }
    }
    Ok(())
}

fn validate_relative_path(raw: &str, field: &str, gate_index: usize) -> Result<(), String> {
    if raw.trim().is_empty() {
        return Err(format!(
            "{MERGE_GATES_PATH} gate {} has an empty {field}",
            gate_index + 1
        ));
    }
    let path = Path::new(raw);
    if path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::CurDir | Component::Normal(_)))
    {
        return Err(format!(
            "{MERGE_GATES_PATH} gate {} {field} must be repo-relative without parent components: {raw:?}",
            gate_index + 1
        ));
    }
    Ok(())
}

/// Run applicable gates in declared order, stopping on the first failure.
pub fn run_gate_suite<F>(
    repo_root: &Path,
    changed_paths: &[String],
    suite: &GateSuite,
    executor: F,
) -> GateSuiteResult
where
    F: Fn(&str, &Path) -> (bool, String),
{
    for gate in &suite.gates {
        if !gate_applies(gate, changed_paths) {
            continue;
        }
        let cwd: PathBuf = if gate.cwd == "." {
            repo_root.to_path_buf()
        } else {
            repo_root.join(&gate.cwd)
        };
        let (ok, output) = executor(&gate.command, &cwd);
        if !ok {
            return GateSuiteResult::Failed {
                gate: gate.command.clone(),
                output,
            };
        }
    }
    GateSuiteResult::Passed
}

fn gate_applies(gate: &Gate, changed_paths: &[String]) -> bool {
    gate.when_paths.is_empty()
        || gate.when_paths.iter().any(|prefix| {
            let prefix = prefix.trim_end_matches('/');
            changed_paths.iter().any(|path| {
                path == prefix
                    || path
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeExecutor {
        calls: RefCell<Vec<(String, PathBuf)>>,
        failing_command: Option<&'static str>,
    }

    impl FakeExecutor {
        fn all_pass() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                failing_command: None,
            }
        }

        fn failing(command: &'static str) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                failing_command: Some(command),
            }
        }

        fn run(&self, command: &str, cwd: &Path) -> (bool, String) {
            self.calls
                .borrow_mut()
                .push((command.to_string(), cwd.to_path_buf()));
            if self.failing_command == Some(command) {
                (false, "gate failed".to_string())
            } else {
                (true, String::new())
            }
        }
    }

    fn suite() -> GateSuite {
        parse_gate_suite(
            br#"{
                "gates": [
                    {"command":"cargo test --workspace","cwd":"."},
                    {"command":"npm test","cwd":"apps/dashboard","whenPaths":["apps/dashboard"]}
                ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn parse_rejects_empty_or_conditional_only_suites() {
        assert!(parse_gate_suite(br#"{"gates":[]}"#)
            .unwrap_err()
            .contains("at least one"));
        assert!(
            parse_gate_suite(br#"{"gates":[{"command":"npm test","whenPaths":["web"]}]}"#)
                .unwrap_err()
                .contains("unconditional")
        );
    }

    #[test]
    fn parse_rejects_paths_that_escape_the_repo() {
        for text in [
            br#"{"gates":[{"command":"test","cwd":"../outside"}]}"#.as_slice(),
            br#"{"gates":[{"command":"test","whenPaths":["/tmp"]},{"command":"ok"}]}"#.as_slice(),
        ] {
            assert!(parse_gate_suite(text)
                .unwrap_err()
                .contains("repo-relative without parent components"));
        }
    }

    #[test]
    fn unconditional_and_matching_conditional_gates_run_in_order() {
        let root = PathBuf::from("/repo");
        let exec = FakeExecutor::all_pass();
        let result = run_gate_suite(
            &root,
            &["apps/dashboard/src/App.tsx".to_string()],
            &suite(),
            |cmd, cwd| exec.run(cmd, cwd),
        );
        assert_eq!(result, GateSuiteResult::Passed);
        assert_eq!(
            *exec.calls.borrow(),
            vec![
                ("cargo test --workspace".to_string(), root.clone()),
                ("npm test".to_string(), root.join("apps/dashboard")),
            ]
        );
    }

    #[test]
    fn unrelated_diff_skips_conditional_gate() {
        let root = PathBuf::from("/repo");
        let exec = FakeExecutor::all_pass();
        run_gate_suite(
            &root,
            &["crates/engine/src/lib.rs".to_string()],
            &suite(),
            |cmd, cwd| exec.run(cmd, cwd),
        );
        assert_eq!(exec.calls.borrow().len(), 1);
        assert_eq!(exec.calls.borrow()[0].0, "cargo test --workspace");
    }

    #[test]
    fn first_failure_stops_the_suite() {
        let root = PathBuf::from("/repo");
        let exec = FakeExecutor::failing("cargo test --workspace");
        let result = run_gate_suite(
            &root,
            &["apps/dashboard/src/App.tsx".to_string()],
            &suite(),
            |cmd, cwd| exec.run(cmd, cwd),
        );
        assert_eq!(
            result,
            GateSuiteResult::Failed {
                gate: "cargo test --workspace".to_string(),
                output: "gate failed".to_string(),
            }
        );
        assert_eq!(exec.calls.borrow().len(), 1);
    }
}
