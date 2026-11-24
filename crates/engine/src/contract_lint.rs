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
//! Approval is synchronous but normally called inside Tokio. Execution uses
//! `command_exec`'s scoped-thread bridge into the same bounded, process-tree
//! killed, sandbox-aware gate runner as validation/final gates; it never
//! starts a nested runtime on the caller's runtime thread.

use crate::types::{Assertion, AssertionCheck, MissionConfig};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

/// Per-command wall-clock timeout for the base-tree lint run.
const PER_COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

/// Overall wall-clock budget for linting an entire contract's command
/// assertions; once spent, remaining assertions are recorded `NotLinted`.
const OVERALL_BUDGET: Duration = Duration::from_secs(180);

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
    /// The command hit some other no-exit-code condition (spawn failure,
    /// sandbox wrap failure) — treated as a suspect since no verdict could
    /// be reached.
    CouldNotVerdict,
    /// The command hit a typed wall-clock timeout (reported by the bounded
    /// executor itself, never inferred from elapsed time) before producing
    /// an exit code. Not an author-bug suspect: a slow command is not
    /// evidence of a polarity/vacuity bug, so this is named separately in
    /// the approval lint rather than lumped with [`Self::CouldNotVerdict`].
    TimedOut {
        /// The wall-clock limit actually enforced for this lint run:
        /// `min(the lint's own per-command cap, the resolved final-gate
        /// limit)`.
        lint_limit_secs: u64,
        /// The resolved final-gate limit for this assertion (per-assertion
        /// `timeoutSecs`, else operator `contractCommandTimeoutSecs`, else
        /// the 600s default).
        gate_limit_secs: u64,
    },
    /// Skipped because the overall wall-clock lint budget was already spent
    /// before this assertion's turn.
    NotLinted,
}

impl AssertionLintOutcome {
    /// True for outcomes that warrant operator attention as a possible
    /// author bug: already passing on base, or no verdict could be reached.
    /// A timeout is NOT included: it is reported separately (see
    /// [`ContractLintReport::timed_out`]) rather than as an author-bug
    /// suspect.
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
    /// The [`crate::contract_shell::HostShell::label`] this command ran
    /// under.
    pub shell_label: String,
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

    /// Results that were never run (budget spent, or no containment).
    pub fn not_linted(&self) -> Vec<&AssertionLint> {
        self.results
            .iter()
            .filter(|r| r.outcome == AssertionLintOutcome::NotLinted)
            .collect()
    }

    /// Results that hit a typed wall-clock timeout against the base — named
    /// separately from [`Self::suspects`] since a timeout is not evidence of
    /// a polarity/vacuity author bug.
    pub fn timed_out(&self) -> Vec<&AssertionLint> {
        self.results
            .iter()
            .filter(|r| matches!(r.outcome, AssertionLintOutcome::TimedOut { .. }))
            .collect()
    }

    /// A report that runs nothing: every command assertion is
    /// [`AssertionLintOutcome::NotLinted`]. Approval uses this when no
    /// containment resolves, because the commands are planner-authored and
    /// no human has reviewed the plan yet.
    pub(crate) fn skipped(contract: &[Assertion]) -> Self {
        let results = contract
            .iter()
            .filter(|a| a.check == AssertionCheck::Command)
            .filter_map(|a| {
                Some(AssertionLint {
                    id: a.id.clone(),
                    command: a.command.clone()?,
                    outcome: AssertionLintOutcome::NotLinted,
                    output_tail: String::new(),
                    shell_label: "sh".to_string(),
                })
            })
            .collect();
        ContractLintReport {
            results,
            tree_clean_at_base: true,
        }
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

        let timed_out = self.timed_out();
        if !timed_out.is_empty() {
            let list = timed_out
                .iter()
                .map(|a| {
                    let AssertionLintOutcome::TimedOut {
                        lint_limit_secs,
                        gate_limit_secs,
                    } = a.outcome
                    else {
                        unreachable!("filtered to TimedOut above")
                    };
                    let detail = if gate_limit_secs <= lint_limit_secs {
                        format!(
                            "timed out after {lint_limit_secs}s, the gate limit for this \
                             assertion; it will time out at the final gate too unless its \
                             limit is raised (timeoutSecs on the assertion, or \
                             contractCommandTimeoutSecs in ~/.kranz/config.json, at most 14400s)"
                        )
                    } else {
                        format!(
                            "did not finish within the lint's {lint_limit_secs}s cap; the gate \
                             allows {gate_limit_secs}s"
                        )
                    };
                    format!("[{}] {} — {detail}", a.id, a.command)
                })
                .collect::<Vec<_>>()
                .join("; ");
            parts.push(format!("timed out on the base (no verdict): {list}"));
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

        let not_linted = self.not_linted();
        if !not_linted.is_empty() {
            let list = not_linted
                .iter()
                .map(|a| format!("[{}] {}", a.id, a.command))
                .collect::<Vec<_>>()
                .join("; ");
            parts.push(format!("not linted (never run on the base): {list}"));
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

/// Environment for the base-tree lint run: the SAME cleared contract env
/// the validation round and final gate use (agent-env-clear —
/// [`crate::agent_env::contract_command_env`] over a per-process scratch
/// HOME: minimal allowlist + `KRANZ_BASE_SHA` + toolchain caches + any
/// `contractEnvPassthrough` names, ambient secrets cleared), plus
/// git-hook-disabling keys so any `git` invoked by a contract command runs
/// with hooks off. The lint is advisory-only, so a command that depended on
/// a now-cleared ambient var flips to `FailedOnBase`/`CouldNotVerdict` —
/// exactly the signal that it needs a `contractEnvPassthrough` entry.
pub fn lint_env(
    scratch: &Path,
    base_sha: Option<&str>,
    passthrough: &[String],
) -> HashMap<String, String> {
    with_git_hooks_disabled(crate::agent_env::contract_command_env(
        scratch,
        base_sha,
        passthrough,
    ))
}

pub(crate) fn with_git_hooks_disabled(mut env: HashMap<String, String>) -> HashMap<String, String> {
    env.insert("GIT_CONFIG_COUNT".to_string(), "1".to_string());
    env.insert("GIT_CONFIG_KEY_0".to_string(), "core.hooksPath".to_string());
    env.insert("GIT_CONFIG_VALUE_0".to_string(), "/dev/null".to_string());
    env
}

/// Whether `sandbox` can actually run a command here. Resolution only picks a
/// provider; applying it can still fail (a nested Seatbelt apply inside
/// another sandbox, an unprepared host). A lint run through a sandbox that
/// cannot start would report every assertion as failing on the base, so the
/// approval path probes first and skips the lint instead.
pub(crate) fn containment_usable(
    cwd: &Path,
    scratch: &Path,
    base_sha: Option<&str>,
    sandbox: &crate::command_exec::GateSandbox,
    shell: &crate::contract_shell::HostShell,
) -> bool {
    let env = lint_env(scratch, base_sha, &[]);
    let (code, _) = crate::command_exec::run_contract_command_sandboxed_blocking(
        cwd,
        "exit 0",
        Duration::from_secs(30),
        &env,
        sandbox,
        shell,
    );
    code == Some(0)
}

/// Lint every `check: command` assertion in `contract` against the
/// untouched base tree, using the module's default per-command timeout cap
/// and overall budget. `passthrough` is the mission config's
/// `contractEnvPassthrough` (see [`lint_env`]). `config` resolves each
/// assertion's final-gate timeout limit ([`crate::contract_timeout::resolve`])
/// so a timed-out lint run can report whether the gate would time out too.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_contract_lint(
    cwd: &Path,
    scratch: &Path,
    base_sha: Option<&str>,
    contract: &[Assertion],
    tree_clean_at_base: bool,
    passthrough: &[String],
    sandbox: &crate::command_exec::GateSandbox,
    shell: &crate::contract_shell::HostShell,
    config: &MissionConfig,
) -> ContractLintReport {
    run_contract_lint_with_limits(
        cwd,
        scratch,
        base_sha,
        contract,
        tree_clean_at_base,
        PER_COMMAND_TIMEOUT,
        OVERALL_BUDGET,
        passthrough,
        sandbox,
        shell,
        config,
    )
}

/// Same as [`run_contract_lint`] but with injectable `lint_cap` (the lint's
/// own per-command timeout cap — never exceeded regardless of the resolved
/// gate limit) and `overall` budget, so tests can exercise timeout/budget
/// behavior without waiting on the production defaults.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_contract_lint_with_limits(
    cwd: &Path,
    scratch: &Path,
    base_sha: Option<&str>,
    contract: &[Assertion],
    tree_clean_at_base: bool,
    lint_cap: Duration,
    overall: Duration,
    passthrough: &[String],
    sandbox: &crate::command_exec::GateSandbox,
    shell: &crate::contract_shell::HostShell,
    config: &MissionConfig,
) -> ContractLintReport {
    let env = lint_env(scratch, base_sha, passthrough);
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
                shell_label: shell.label(),
            });
            continue;
        }

        let gate_timeout = crate::contract_timeout::resolve(assertion, config);
        let run_timeout = lint_cap.min(gate_timeout.duration());
        let run = crate::command_exec::run_contract_command_timed_blocking(
            cwd,
            command,
            run_timeout,
            &env,
            sandbox,
            shell,
        );
        let outcome = if run.timed_out {
            AssertionLintOutcome::TimedOut {
                lint_limit_secs: run_timeout.as_secs(),
                gate_limit_secs: gate_timeout.secs,
            }
        } else {
            classify(run.code.is_some(), run.code == Some(0))
        };
        results.push(AssertionLint {
            id: assertion.id.clone(),
            command: command.to_string(),
            outcome,
            output_tail: run.output,
            shell_label: shell.label(),
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
    use crate::contract_shell::{HostShell, ShellKind, ShellLane};
    use crate::test_shell::{FAIL, SUCCEED};

    /// Resolve the shell this test's contract-lane commands actually run
    /// under, exactly the way production does: on Unix this is always bare
    /// `sh`; on Windows it is Git's absolute `sh` (or `cmd`, if Git's `sh`
    /// cannot be resolved outside `roots`).
    fn resolve_test_shell(roots: &[&Path]) -> HostShell {
        let roots: Vec<std::path::PathBuf> = roots.iter().map(|p| p.to_path_buf()).collect();
        crate::contract_shell::resolve_contract_shell(ShellLane::Contract, false, &roots)
    }

    /// A command that blocks for at least `millis`, spelled for whichever
    /// shell `shell` resolved to.
    fn sleep_for(shell: &HostShell, cwd: &Path, millis: u64) -> String {
        match shell.kind {
            ShellKind::Sh => crate::test_shell::contract::sleep_millis(cwd, millis),
            ShellKind::Cmd => crate::test_shell::sleep_millis(cwd, millis),
        }
    }

    fn run_lint(cwd: &Path, base_sha: Option<&str>, contract: &[Assertion]) -> ContractLintReport {
        let scratch = tempfile::tempdir().unwrap();
        let shell = resolve_test_shell(&[cwd, scratch.path()]);
        run_contract_lint(
            cwd,
            scratch.path(),
            base_sha,
            contract,
            true,
            &[],
            &crate::command_exec::GateSandbox::Disabled,
            &shell,
            &MissionConfig::default(),
        )
    }

    fn command_assertion(id: &str, command: &str) -> Assertion {
        Assertion {
            id: id.to_string(),
            statement: format!("statement for {id}"),
            check: AssertionCheck::Command,
            command: Some(command.to_string()),
            milestone: None,
            negative_control: None,
            timeout_secs: None,
            pty_script: None,
        }
    }

    fn judgement_assertion(id: &str) -> Assertion {
        Assertion {
            id: id.to_string(),
            statement: format!("statement for {id}"),
            check: AssertionCheck::AgentJudgement,
            command: None,
            milestone: None,
            negative_control: None,
            timeout_secs: None,
            pty_script: None,
        }
    }

    #[test]
    fn approval_lint_summary_notes_dirty_tree_at_base() {
        // finding f-1-2: when the lint ran against a working tree that was
        // not clean at base, `summary()` must prepend a note saying so —
        // asserted directly on the struct rather than only by inspection.
        let dirty = ContractLintReport {
            results: vec![AssertionLint {
                id: "a1".to_string(),
                command: "true".to_string(),
                outcome: AssertionLintOutcome::PassedOnBase,
                output_tail: String::new(),
                shell_label: "sh".to_string(),
            }],
            tree_clean_at_base: false,
        };
        let summary = dirty.summary();
        assert!(
            summary.contains(
                "note: contract lint ran against a working tree with uncommitted changes"
            ),
            "{summary}"
        );

        let clean = ContractLintReport {
            tree_clean_at_base: true,
            ..dirty
        };
        assert!(
            !clean.summary().contains("uncommitted changes"),
            "{}",
            clean.summary()
        );
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
    fn skipped_report_runs_nothing_and_says_so() {
        let command = |id: &str, cmd: &str| Assertion {
            id: id.into(),
            statement: "s".into(),
            check: AssertionCheck::Command,
            command: Some(cmd.into()),
            milestone: None,
            negative_control: None,
            timeout_secs: None,
            pty_script: None,
        };
        let mut judged = command("a-2", "unused");
        judged.check = AssertionCheck::AgentJudgement;
        let report = ContractLintReport::skipped(&[command("a-1", "exit 0"), judged]);
        assert_eq!(
            report.results.len(),
            1,
            "only command assertions are listed"
        );
        assert_eq!(report.not_linted().len(), 1);
        assert!(report.suspects().is_empty());
        assert_eq!(
            report.summary(),
            "not linted (never run on the base): [a-1] exit 0"
        );
    }

    #[test]
    fn approval_lint_env_has_base_sha_and_disables_hooks() {
        let scratch = tempfile::tempdir().unwrap();
        let with_sha = lint_env(scratch.path(), Some("deadbeef"), &[]);
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

        let without_sha = lint_env(scratch.path(), None, &[]);
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
            command_assertion("a1", SUCCEED),
            command_assertion("a2", FAIL),
            judgement_assertion("a3"),
        ];
        let report = run_lint(&std::env::temp_dir(), None, &contract);

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
        let scratch = tempfile::tempdir().unwrap();
        let shell = resolve_test_shell(&[scratch.path()]);
        let contract = vec![command_assertion(
            "a1",
            &sleep_for(&shell, scratch.path(), 5_000),
        )];
        let start = Instant::now();
        let report = run_contract_lint_with_limits(
            scratch.path(),
            scratch.path(),
            None,
            &contract,
            true,
            Duration::from_millis(200),
            Duration::from_secs(600),
            &[],
            &crate::command_exec::GateSandbox::Disabled,
            &shell,
            &MissionConfig::default(),
        );
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(2),
            "runner should not wait for the full sleep, took {elapsed:?}"
        );
        assert_eq!(report.results.len(), 1);
        assert!(matches!(
            report.results[0].outcome,
            AssertionLintOutcome::TimedOut { .. }
        ));
        assert!(!report.results[0].outcome.is_author_bug_suspect());
    }

    fn command_assertion_with_timeout(id: &str, command: &str, timeout_secs: u64) -> Assertion {
        let mut a = command_assertion(id, command);
        a.timeout_secs = Some(timeout_secs);
        a
    }

    /// A command sleeping 5s with a resolved 1s limit (`timeoutSecs: 1`)
    /// must be classified `TimedOut`, excluded from `suspects()`, and named
    /// on the "timed out on the base" summary line rather than the
    /// author-bug-suspects line.
    #[test]
    fn contract_timeout_lint_classifies_timed_out_separately_from_suspects() {
        let scratch = tempfile::tempdir().unwrap();
        let shell = resolve_test_shell(&[scratch.path()]);
        let contract = vec![command_assertion_with_timeout(
            "a1",
            &sleep_for(&shell, scratch.path(), 5_000),
            1,
        )];
        let report = run_contract_lint_with_limits(
            scratch.path(),
            scratch.path(),
            None,
            &contract,
            true,
            Duration::from_secs(600),
            Duration::from_secs(600),
            &[],
            &crate::command_exec::GateSandbox::Disabled,
            &shell,
            &MissionConfig::default(),
        );

        assert_eq!(report.results.len(), 1);
        assert!(matches!(
            report.results[0].outcome,
            AssertionLintOutcome::TimedOut {
                lint_limit_secs: 1,
                gate_limit_secs: 1,
            }
        ));
        assert!(report.suspects().is_empty());
        assert_eq!(report.timed_out().len(), 1);

        let summary = report.summary();
        assert!(
            summary.contains("timed out on the base (no verdict): [a1]"),
            "{summary}"
        );
        assert!(
            !summary.contains("author-bug suspects"),
            "a timeout must not appear on the suspects line: {summary}"
        );
    }

    /// When the lint's own cap equals the resolved gate limit, the summary
    /// warns that the final gate will time out too unless the limit is
    /// raised; when the gate allows more than the lint's cap, it instead
    /// says how many seconds the gate allows.
    #[test]
    fn contract_timeout_lint_summary_distinguishes_gate_limit_from_lint_cap() {
        let scratch = tempfile::tempdir().unwrap();
        let shell = resolve_test_shell(&[scratch.path()]);

        // gate limit (1s) <= lint cap (600s) -> the run is bounded by the
        // gate limit, so lint_limit_secs == gate_limit_secs.
        let contract = vec![command_assertion_with_timeout(
            "a1",
            &sleep_for(&shell, scratch.path(), 5_000),
            1,
        )];
        let report = run_contract_lint_with_limits(
            scratch.path(),
            scratch.path(),
            None,
            &contract,
            true,
            Duration::from_secs(600),
            Duration::from_secs(600),
            &[],
            &crate::command_exec::GateSandbox::Disabled,
            &shell,
            &MissionConfig::default(),
        );
        let summary = report.summary();
        assert!(
            summary.contains("it will time out at the final gate too unless its limit is raised"),
            "{summary}"
        );

        // lint cap (1s) < gate limit (600s, default) -> the gate allows more
        // time than the lint used. A fresh directory: on Windows the sleep
        // fixture is a copied executable, and the first run's timed-out copy
        // can still be mapped when this one is written, which fails the copy
        // with a sharing violation.
        let scratch2 = tempfile::tempdir().unwrap();
        let shell2 = resolve_test_shell(&[scratch2.path()]);
        let contract2 = vec![command_assertion(
            "a2",
            &sleep_for(&shell2, scratch2.path(), 5_000),
        )];
        let report2 = run_contract_lint_with_limits(
            scratch2.path(),
            scratch2.path(),
            None,
            &contract2,
            true,
            Duration::from_secs(1),
            Duration::from_secs(600),
            &[],
            &crate::command_exec::GateSandbox::Disabled,
            &shell2,
            &MissionConfig::default(),
        );
        let summary2 = report2.summary();
        assert!(
            summary2.contains("did not finish within the lint's 1s cap; the gate allows 600s"),
            "{summary2}"
        );
    }

    #[test]
    fn approval_lint_runner_budget_skips_remainder() {
        let scratch = tempfile::Builder::new()
            .prefix("kranz timer's ")
            .tempdir()
            .unwrap();
        let shell = resolve_test_shell(&[scratch.path()]);
        let contract = vec![
            command_assertion("a1", &sleep_for(&shell, scratch.path(), 200)),
            command_assertion("a2", SUCCEED),
            command_assertion("a3", FAIL),
        ];
        let start = Instant::now();
        let report = run_contract_lint_with_limits(
            scratch.path(),
            scratch.path(),
            None,
            &contract,
            true,
            Duration::from_secs(10),
            Duration::from_millis(50),
            &[],
            &crate::command_exec::GateSandbox::Disabled,
            &shell,
            &MissionConfig::default(),
        );

        assert_eq!(report.results.len(), 3);
        let a1 = report.results.iter().find(|r| r.id == "a1").unwrap();
        assert_eq!(
            a1.outcome,
            AssertionLintOutcome::PassedOnBase,
            "{}",
            a1.output_tail
        );
        assert!(start.elapsed() >= Duration::from_millis(200));
        if cfg!(windows) {
            assert!(
                a1.output_tail.contains("KRANZ_TEST_SLEEP_COMPLETED=200"),
                "timer fixture did not complete: {}",
                a1.output_tail
            );
        }
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
        // This assertion is ABOUT POSIX grep's exit-status semantics, so a
        // portable rewrite would test something else and `findstr` has no
        // equivalent of `-L`. Windows ships no grep; CI and any box with Git's
        // `usr/bin` on PATH have one, so run it there and say so plainly when
        // there is nothing to run against rather than failing a stock box.
        if !crate::sandbox::command_available("grep") {
            crate::test_capability::skip(
                crate::test_capability::capability::GREP,
                "no grep on PATH; the inverted-polarity shape is unexercised here",
            );
            return;
        }
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
        let report = run_lint(&repo_root, None, &contract);

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
            command_assertion("a1", SUCCEED),
            command_assertion("a2", FAIL),
        ];
        let report = run_lint(&std::env::temp_dir(), None, &contract);
        assert_eq!(report.results.len(), 2);
    }

    /// agent-env-clear: the approval-time lint executes the SAME
    /// model-drafted contract commands as the final gate, so it runs them
    /// in the same cleared env — a poisoned ambient secret must be
    /// invisible to the linted command.
    #[cfg(unix)]
    #[test]
    fn approval_lint_command_cannot_see_ambient_secrets() {
        let _poison = crate::agent_env::EnvTestGuard::engage(&[("GH_TOKEN", "hunter2-lint")]);

        let contract = vec![command_assertion(
            "a1",
            "test -z \"$GH_TOKEN\" && env | grep -c hunter2-lint | grep -q '^0$'",
        )];
        let report = run_lint(&std::env::temp_dir(), None, &contract);

        assert_eq!(report.results.len(), 1);
        assert_eq!(
            report.results[0].outcome,
            AssertionLintOutcome::PassedOnBase,
            "the poisoned ambient GH_TOKEN must be cleared from the lint env: {}",
            report.results[0].output_tail
        );
    }
}
