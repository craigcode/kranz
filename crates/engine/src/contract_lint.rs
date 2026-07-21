//! Pure classification + synchronous base-tree runner for command-assertion
//! linting at `approve_plan` time (M8 tier 1, feature f-1-1).
//!
//! At approval, each `check: command` assertion in the mission contract is
//! run once against the untouched base tree so the operator learns whether
//! it already passes (a polarity/vacuity SUSPECT — a correctly-scoped "the
//! work landed" assertion must FAIL before the work lands; the abandoned
//! m-0c885b mission shipped an `a6` grep that could only pass while the
//! requirement was unmet) or fails as expected (the feature simply hasn't
//! landed yet — the usual, benign case). This never blocks approval; it only
//! surfaces information.
//!
//! This module takes plain inputs and does not depend on `MissionEngine`, so
//! it is unit-testable in isolation (mirrors `contract_sweep.rs`).
//!
//! CRITICAL: `approve_plan` is a synchronous `pub fn` called from inside the
//! process `#[tokio::main]` runtime. Constructing a new `tokio::runtime::Runtime`
//! and calling `block_on` from there panics unconditionally ("Cannot start a
//! runtime from within a runtime"). The runner here therefore uses
//! `std::process::Command` with a poll/try_wait+sleep timeout loop, copying
//! the proven-safe pattern of `run_with_timeout` in `orchestrator.rs`. It must
//! NOT call the async `run_shell_command` or `run_bounded_gate_command` (the
//! latter builds a current-thread runtime and would panic under the ambient
//! runtime).

use crate::types::{Assertion, AssertionCheck};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

/// Per-command wall-clock timeout for the base-tree lint run.
const PER_COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

/// Overall wall-clock budget for linting an entire contract's command
/// assertions; once spent, remaining assertions are recorded `NotLinted`.
const OVERALL_BUDGET: Duration = Duration::from_secs(180);

/// Max characters of combined stdout+stderr kept per lint result.
const OUTPUT_TAIL_MAX: usize = 1500;

/// Verdict for one `check: command` assertion run once against the
/// untouched base tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssertionLintOutcome {
    /// The command already exits zero on the untouched base tree — a
    /// SUSPECT: a correctly-scoped "the work landed" assertion should FAIL
    /// before the work lands, so this may indicate a polarity/vacuity bug in
    /// the assertion itself.
    PassedOnBase,
    /// The command exits non-zero on the untouched base tree — the usual,
    /// benign case: the feature simply hasn't landed yet.
    FailedOnBase,
    /// The command hit the per-command timeout and never produced an exit
    /// code — treated as a suspect since no verdict could be reached.
    CouldNotVerdict,
    /// Skipped because the overall wall-clock lint budget was already spent
    /// before this assertion's turn.
    NotLinted,
}

impl AssertionLintOutcome {
    /// True for outcomes that warrant operator attention as a possible
    /// author bug: already passing on base, or no verdict could be reached.
    pub fn is_author_bug_suspect(&self) -> bool {
        matches!(
            self,
            AssertionLintOutcome::PassedOnBase | AssertionLintOutcome::CouldNotVerdict
        )
    }
}

/// Lint result for a single command assertion.
#[derive(Debug, Clone)]
pub struct AssertionLint {
    pub id: String,
    pub command: String,
    pub outcome: AssertionLintOutcome,
    pub output_tail: String,
}

/// Full lint report for a contract's command assertions.
#[derive(Debug, Clone)]
pub struct ContractLintReport {
    pub results: Vec<AssertionLint>,
    /// Whether the tree was clean (no uncommitted changes) at the moment the
    /// lint ran — when false, results may not reflect the pristine base.
    pub tree_clean_at_base: bool,
}

impl ContractLintReport {
    /// Results whose outcome is an author-bug suspect.
    pub fn suspects(&self) -> Vec<&AssertionLint> {
        self.results
            .iter()
            .filter(|r| r.outcome.is_author_bug_suspect())
            .collect()
    }

    /// True when no command assertions were linted at all.
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    /// Compact operator-facing summary: suspects first under a clear label,
    /// then base-expected-to-fail assertions under a separate label. When
    /// `tree_clean_at_base` is false, prepends a note that the lint ran
    /// against a dirty working tree.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();

        if !self.tree_clean_at_base {
            parts.push(
                "note: contract lint ran against a working tree with uncommitted changes; \
                 results may not reflect the pristine base"
                    .to_string(),
            );
        }

        let suspects: Vec<&AssertionLint> = self.suspects();
        if !suspects.is_empty() {
            let list = suspects
                .iter()
                .map(|a| format!("[{}] {}", a.id, a.command))
                .collect::<Vec<_>>()
                .join("; ");
            parts.push(format!(
                "author-bug suspects (already pass / no verdict on the untouched base): {list}"
            ));
        }

        let expected: Vec<&AssertionLint> = self
            .results
            .iter()
            .filter(|r| r.outcome == AssertionLintOutcome::FailedOnBase)
            .collect();
        if !expected.is_empty() {
            let list = expected
                .iter()
                .map(|a| format!("[{}] {}", a.id, a.command))
                .collect::<Vec<_>>()
                .join("; ");
            parts.push(format!("base-expected-to-fail (benign): {list}"));
        }

        parts.join("\n")
    }
}

/// Classify a completed (or timed-out) command run.
///
/// - `!ran_to_completion` → [`AssertionLintOutcome::CouldNotVerdict`]
/// - `ran_to_completion && exited_success` → [`AssertionLintOutcome::PassedOnBase`]
/// - `ran_to_completion && !exited_success` → [`AssertionLintOutcome::FailedOnBase`]
///
/// [`AssertionLintOutcome::NotLinted`] is assigned by the runner when the
/// overall budget is spent, not by this function.
pub fn classify(ran_to_completion: bool, exited_success: bool) -> AssertionLintOutcome {
    if !ran_to_completion {
        AssertionLintOutcome::CouldNotVerdict
    } else if exited_success {
        AssertionLintOutcome::PassedOnBase
    } else {
        AssertionLintOutcome::FailedOnBase
    }
}

/// Environment for the base-tree lint run: starts from [`crate::runner::contract_env`]
/// (so `KRANZ_BASE_SHA` is set exactly as the final gate sets it), then adds
/// git-hook-disabling keys so any `git` invoked by a contract command runs
/// with hooks off.
pub fn lint_env(base_sha: Option<&str>) -> HashMap<String, String> {
    let mut env = crate::runner::contract_env(base_sha);
    env.insert("GIT_CONFIG_COUNT".to_string(), "1".to_string());
    env.insert("GIT_CONFIG_KEY_0".to_string(), "core.hooksPath".to_string());
    env.insert("GIT_CONFIG_VALUE_0".to_string(), "/dev/null".to_string());
    env
}

/// Run `sh -c command` in `cwd` with `env`, polling with a bounded
/// wall-clock `timeout` rather than blocking forever (mirrors
/// `orchestrator::run_with_timeout`). Returns `(ran_to_completion,
/// exited_success, output_tail)`.
fn run_command_bounded(
    cwd: &Path,
    command: &str,
    env: &HashMap<String, String>,
    timeout: Duration,
) -> (bool, bool, String) {
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(cwd)
        .env_clear()
        .envs(std::env::vars())
        .envs(env)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (false, false, format!("failed to spawn: {e}")),
    };

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child
                    .wait_with_output()
                    .unwrap_or_else(|_| std::process::Output {
                        status,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    });
                let mut combined = String::from_utf8_lossy(&output.stdout).to_string();
                combined.push_str(&String::from_utf8_lossy(&output.stderr));
                let tail = last_chars(&combined, OUTPUT_TAIL_MAX);
                return (true, status.success(), tail);
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (false, false, "timed out".to_string());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return (false, false, format!("wait error: {e}")),
        }
    }
}

/// Last `max` characters of `text` (never splits a code point).
fn last_chars(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(max);
    chars[start..].iter().collect()
}

/// Lint every `check: command` assertion in `contract` against the
/// untouched base tree, using the module's default per-command timeout and
/// overall budget.
pub fn run_contract_lint(
    cwd: &Path,
    base_sha: Option<&str>,
    contract: &[Assertion],
    tree_clean_at_base: bool,
) -> ContractLintReport {
    run_contract_lint_with_limits(
        cwd,
        base_sha,
        contract,
        tree_clean_at_base,
        PER_COMMAND_TIMEOUT,
        OVERALL_BUDGET,
    )
}

/// Same as [`run_contract_lint`] but with injectable `per_command` timeout
/// and `overall` budget, so tests can exercise timeout/budget behavior
/// without waiting on the production defaults.
pub fn run_contract_lint_with_limits(
    cwd: &Path,
    base_sha: Option<&str>,
    contract: &[Assertion],
    tree_clean_at_base: bool,
    per_command: Duration,
    overall: Duration,
) -> ContractLintReport {
    let env = lint_env(base_sha);
    let overall_start = Instant::now();
    let mut results = Vec::new();

    for assertion in contract {
        if assertion.check != AssertionCheck::Command {
            continue;
        }
        let Some(command) = assertion.command.as_deref() else {
            continue;
        };

        if overall_start.elapsed() >= overall {
            results.push(AssertionLint {
                id: assertion.id.clone(),
                command: command.to_string(),
                outcome: AssertionLintOutcome::NotLinted,
                output_tail: String::new(),
            });
            continue;
        }

        let (ran_to_completion, exited_success, output_tail) =
            run_command_bounded(cwd, command, &env, per_command);
        let outcome = classify(ran_to_completion, exited_success);
        results.push(AssertionLint {
            id: assertion.id.clone(),
            command: command.to_string(),
            outcome,
            output_tail,
        });
    }

    ContractLintReport {
        results,
        tree_clean_at_base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_assertion(id: &str, command: &str) -> Assertion {
        Assertion {
            id: id.to_string(),
            statement: format!("statement for {id}"),
            check: AssertionCheck::Command,
            command: Some(command.to_string()),
        }
    }

    fn judgement_assertion(id: &str) -> Assertion {
        Assertion {
            id: id.to_string(),
            statement: format!("statement for {id}"),
            check: AssertionCheck::AgentJudgement,
            command: None,
        }
    }

    #[test]
    fn approval_lint_classify_polarity() {
        assert_eq!(classify(true, true), AssertionLintOutcome::PassedOnBase);
        assert_eq!(classify(true, false), AssertionLintOutcome::FailedOnBase);
        assert_eq!(classify(false, true), AssertionLintOutcome::CouldNotVerdict);
        assert_eq!(
            classify(false, false),
            AssertionLintOutcome::CouldNotVerdict
        );

        assert!(AssertionLintOutcome::PassedOnBase.is_author_bug_suspect());
        assert!(AssertionLintOutcome::CouldNotVerdict.is_author_bug_suspect());
        assert!(!AssertionLintOutcome::FailedOnBase.is_author_bug_suspect());
        assert!(!AssertionLintOutcome::NotLinted.is_author_bug_suspect());
    }

    #[test]
    fn approval_lint_env_has_base_sha_and_disables_hooks() {
        let with_sha = lint_env(Some("deadbeef"));
        assert_eq!(
            with_sha.get("KRANZ_BASE_SHA").map(String::as_str),
            Some("deadbeef")
        );
        assert_eq!(
            with_sha.get("GIT_CONFIG_COUNT").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            with_sha.get("GIT_CONFIG_KEY_0").map(String::as_str),
            Some("core.hooksPath")
        );
        assert_eq!(
            with_sha.get("GIT_CONFIG_VALUE_0").map(String::as_str),
            Some("/dev/null")
        );

        let without_sha = lint_env(None);
        assert!(!without_sha.contains_key("KRANZ_BASE_SHA"));
        assert_eq!(
            without_sha.get("GIT_CONFIG_COUNT").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            without_sha.get("GIT_CONFIG_KEY_0").map(String::as_str),
            Some("core.hooksPath")
        );
        assert_eq!(
            without_sha.get("GIT_CONFIG_VALUE_0").map(String::as_str),
            Some("/dev/null")
        );
    }

    #[test]
    fn approval_lint_runner_buckets_true_false() {
        let contract = vec![
            command_assertion("a1", "true"),
            command_assertion("a2", "false"),
            judgement_assertion("a3"),
        ];
        let report = run_contract_lint(&std::env::temp_dir(), None, &contract, true);

        assert_eq!(report.results.len(), 2);

        let a1 = report.results.iter().find(|r| r.id == "a1").unwrap();
        assert_eq!(a1.outcome, AssertionLintOutcome::PassedOnBase);

        let a2 = report.results.iter().find(|r| r.id == "a2").unwrap();
        assert_eq!(a2.outcome, AssertionLintOutcome::FailedOnBase);

        let suspects = report.suspects();
        assert_eq!(suspects.len(), 1);
        assert_eq!(suspects[0].id, "a1");
    }

    #[test]
    fn approval_lint_runner_times_out_slow_command() {
        let contract = vec![command_assertion("a1", "sleep 5")];
        let start = Instant::now();
        let report = run_contract_lint_with_limits(
            &std::env::temp_dir(),
            None,
            &contract,
            true,
            Duration::from_millis(200),
            Duration::from_secs(600),
        );
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(2),
            "runner should not wait for the full sleep, took {elapsed:?}"
        );
        assert_eq!(report.results.len(), 1);
        assert_eq!(
            report.results[0].outcome,
            AssertionLintOutcome::CouldNotVerdict
        );
        assert!(report.results[0].outcome.is_author_bug_suspect());
    }

    #[test]
    fn approval_lint_runner_budget_skips_remainder() {
        let contract = vec![
            command_assertion("a1", "sleep 0.2"),
            command_assertion("a2", "true"),
            command_assertion("a3", "false"),
        ];
        let report = run_contract_lint_with_limits(
            &std::env::temp_dir(),
            None,
            &contract,
            true,
            Duration::from_secs(600),
            Duration::from_millis(50),
        );

        assert_eq!(report.results.len(), 3);
        let a2 = report.results.iter().find(|r| r.id == "a2").unwrap();
        let a3 = report.results.iter().find(|r| r.id == "a3").unwrap();
        assert_eq!(a2.outcome, AssertionLintOutcome::NotLinted);
        assert_eq!(a3.outcome, AssertionLintOutcome::NotLinted);
        assert!(!a2.outcome.is_author_bug_suspect());
        assert!(!a3.outcome.is_author_bug_suspect());
    }

    #[test]
    fn approval_lint_runner_flags_inverted_lockfile_grep_shape() {
        // Reproduces the concrete m-0c885b `a6` bug shape (research.md:24,
        // reconstructed from `git show 65d3201:.kranz/missions/m-8b3ec3/plan.json`):
        // `grep -L <old-pin> Cargo.lock`, authored to assert "the old
        // dependency pin is gone (the upgrade landed)". Both GNU and BSD
        // grep base the exit status on whether the PATTERN matched
        // anywhere, not on whether a filename was printed by `-L` — so this
        // command exits 0 (success) exactly when the old pin is STILL
        // PRESENT, which is the pre-upgrade / untouched-base state. That is
        // the inverted-polarity bug: it already passes before the work
        // lands. Run it for real, against this repo's own Cargo.lock,
        // targeting a dependency ("tokio") that is genuinely present on the
        // untouched base, to prove the lint flags this exact shape as a
        // suspect rather than relying on `true`/`false` proxies.
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/engine has a workspace root two levels up")
            .to_path_buf();
        assert!(
            repo_root.join("Cargo.lock").is_file(),
            "expected {:?} to contain Cargo.lock",
            repo_root
        );

        let contract = vec![command_assertion(
            "a6",
            r#"grep -L '^name = "tokio"' Cargo.lock"#,
        )];
        let report = run_contract_lint(&repo_root, None, &contract, true);

        assert_eq!(report.results.len(), 1);
        let a6 = &report.results[0];
        assert_eq!(a6.outcome, AssertionLintOutcome::PassedOnBase);
        assert!(a6.outcome.is_author_bug_suspect());

        let suspects = report.suspects();
        assert_eq!(suspects.len(), 1);
        assert_eq!(suspects[0].id, "a6");
    }

    #[tokio::test]
    async fn approval_lint_runner_safe_under_tokio() {
        let contract = vec![
            command_assertion("a1", "true"),
            command_assertion("a2", "false"),
        ];
        let report = run_contract_lint(&std::env::temp_dir(), None, &contract, true);
        assert_eq!(report.results.len(), 2);
    }
}
