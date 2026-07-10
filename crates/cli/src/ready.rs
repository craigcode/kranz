//! Repository readiness scanner for `kranz ready`.
//!
//! This is deliberately read-only and deterministic: it detects the signals a
//! repo exposes for autonomous missions, but does not run test suites or mutate
//! anything. It may run agent CLIs with `--version`, but never starts a model
//! turn or a repository test suite. The output is a serializable scorecard so
//! the dashboard can reuse the same shape later.

use kranz_engine::cost;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyReport {
    pub score: u8,
    pub level: ReadyLevel,
    pub highest_leverage_fix: String,
    pub dimensions: Vec<ReadyDimension>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReadyLevel {
    Ready,
    Warmup,
    ColdStart,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyDimension {
    pub name: &'static str,
    pub score: u8,
    pub weight: u8,
    pub status: ReadyStatus,
    pub evidence: String,
    pub remedy: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReadyStatus {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone)]
struct ValidationCommand {
    command: String,
    program: String,
    available: bool,
}

pub fn assess(repo: &Path) -> ReadyReport {
    let validation_commands = detect_validation_commands(repo);
    let dimensions = vec![
        planner_instructions(repo),
        readme_docs(repo),
        test_runner(repo, &validation_commands),
        ci_config(repo),
        gitignore_hygiene(repo),
        backend_lanes(repo),
        contract_prerequisites(&validation_commands),
        clean_git_state(repo),
        calibration_corpus(repo),
    ];
    let total: u16 = dimensions.iter().map(|d| d.score as u16).sum();
    let score = total.min(100) as u8;
    let level = if score >= 80 {
        ReadyLevel::Ready
    } else if score >= 50 {
        ReadyLevel::Warmup
    } else {
        ReadyLevel::ColdStart
    };
    let highest_leverage_fix = dimensions
        .iter()
        .filter(|d| !d.remedy.is_empty())
        .max_by_key(|d| d.weight.saturating_sub(d.score))
        .map(|d| d.remedy.clone())
        .unwrap_or_else(|| {
            "No obvious fix: this repo exposes the main signals kranz needs.".into()
        });
    ReadyReport {
        score,
        level,
        highest_leverage_fix,
        dimensions,
    }
}

pub fn render(report: &ReadyReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "kranz ready: {}/100 ({})\n",
        report.score,
        level_label(report.level)
    ));
    out.push_str(&format!(
        "highest-leverage fix: {}\n\n",
        report.highest_leverage_fix
    ));
    out.push_str("scorecard:\n");
    for dim in &report.dimensions {
        out.push_str(&format!(
            "  [{:>2}/{:<2}] {} — {}\n",
            dim.score, dim.weight, dim.name, dim.evidence
        ));
        if !dim.remedy.is_empty() && dim.status != ReadyStatus::Pass {
            out.push_str(&format!("          remedy: {}\n", dim.remedy));
        }
    }
    out
}

fn level_label(level: ReadyLevel) -> &'static str {
    match level {
        ReadyLevel::Ready => "ready",
        ReadyLevel::Warmup => "warmup",
        ReadyLevel::ColdStart => "cold-start",
    }
}

fn planner_instructions(repo: &Path) -> ReadyDimension {
    let agents = repo.join("AGENTS.md").exists();
    let claude = repo.join("CLAUDE.md").exists();
    match (agents, claude) {
        (true, true) => dim(
            "planner instructions",
            20,
            20,
            ReadyStatus::Pass,
            "AGENTS.md and CLAUDE.md present",
            "",
        ),
        (true, false) => dim(
            "planner instructions",
            20,
            20,
            ReadyStatus::Pass,
            "AGENTS.md present",
            "",
        ),
        (false, true) => dim(
            "planner instructions",
            16,
            20,
            ReadyStatus::Warn,
            "CLAUDE.md present; AGENTS.md absent",
            "add AGENTS.md with repo-specific build/test/change rules",
        ),
        (false, false) => dim(
            "planner instructions",
            0,
            20,
            ReadyStatus::Fail,
            "no AGENTS.md or CLAUDE.md",
            "add AGENTS.md so planners inherit repo-specific constraints",
        ),
    }
}

fn readme_docs(repo: &Path) -> ReadyDimension {
    if repo.join("README.md").exists() || repo.join("readme.md").exists() {
        dim("repo docs", 10, 10, ReadyStatus::Pass, "README present", "")
    } else {
        dim(
            "repo docs",
            0,
            10,
            ReadyStatus::Fail,
            "README missing",
            "add a README with setup, architecture, and common commands",
        )
    }
}

fn test_runner(repo: &Path, commands: &[ValidationCommand]) -> ReadyDimension {
    if commands.is_empty() {
        return dim(
            "test runner",
            0,
            20,
            ReadyStatus::Fail,
            "no common runnable test/build command detected",
            "add a single documented runnable test command kranz can bind to",
        );
    }
    let evidence = commands
        .iter()
        .map(|c| c.command.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let has_test_signal = has_tests_dir(repo)
        || repo.join("Cargo.toml").exists()
        || package_json_has_script(repo, "test");
    if has_test_signal {
        dim("test runner", 20, 20, ReadyStatus::Pass, evidence, "")
    } else {
        dim(
            "test runner",
            10,
            20,
            ReadyStatus::Warn,
            evidence,
            "add committed tests or a clearly named test target",
        )
    }
}

fn ci_config(repo: &Path) -> ReadyDimension {
    let has_ci = repo.join(".github").join("workflows").exists()
        || repo.join(".gitlab-ci.yml").exists()
        || repo.join(".circleci").join("config.yml").exists();
    if has_ci {
        dim(
            "CI config",
            10,
            10,
            ReadyStatus::Pass,
            "CI config detected",
            "",
        )
    } else {
        dim(
            "CI config",
            0,
            10,
            ReadyStatus::Fail,
            "no common CI config detected",
            "add CI that runs the same validation commands kranz will gate on",
        )
    }
}

fn gitignore_hygiene(repo: &Path) -> ReadyDimension {
    // Ask Git about concrete sentinel paths instead of parsing the root
    // .gitignore as text. Kranz writes its canonical rules to the nested
    // .kranz/.gitignore, and Git's own matcher also handles negation and any
    // equivalent rule shapes correctly.
    let sentinels = [
        ".kranz/missions/m-ready/events.jsonl",
        ".kranz/missions/m-ready/events.jsonl.lock",
        ".kranz/missions/m-ready/state.json",
        ".kranz/missions/m-ready/runs/run.jsonl",
        ".kranz/missions/m-ready/control/0001.json",
        ".kranz/config.json",
        ".kranz/serve.token",
        ".kranz/tickets/ready.status",
    ];
    let present = sentinels
        .iter()
        .filter(|path| git_ignores(repo, path))
        .count();
    if present == sentinels.len() {
        dim(
            "kranz runtime gitignore",
            10,
            10,
            ReadyStatus::Pass,
            "runtime files ignored",
            "",
        )
    } else if present > 0 {
        dim(
            "kranz runtime gitignore",
            ((present * 10) / sentinels.len()) as u8,
            10,
            ReadyStatus::Warn,
            format!("{present}/{} runtime paths ignored", sentinels.len()),
            "add missing .kranz runtime patterns to .gitignore",
        )
    } else {
        dim(
            "kranz runtime gitignore",
            0,
            10,
            ReadyStatus::Fail,
            ".kranz runtime files not ignored",
            "ignore .kranz runtime logs, snapshots, runs, control files, tokens, and status files",
        )
    }
}

fn git_ignores(repo: &Path, relative_path: &str) -> bool {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        // Readiness is a repository property: ignore an operator's global
        // excludes, and ask for the rule source so `.git/info/exclude` and an
        // uncommitted `.gitignore` cannot make a repo look ready.
        .args([
            "-c",
            "core.excludesFile=",
            "check-ignore",
            "--verbose",
            "--",
        ])
        .arg(relative_path)
        .output();
    let Ok(output) = output else { return false };
    if !output.status.success() {
        return false;
    }
    let Ok(verbose) = std::str::from_utf8(&output.stdout) else {
        return false;
    };
    let Some(metadata) = verbose.split_once('\t').map(|(metadata, _)| metadata) else {
        return false;
    };
    // `--verbose` is `<source>:<line>:<pattern>\t<path>`. Locate the numeric
    // line segment rather than splitting on the first colon (Windows sources
    // begin with a drive designator such as `C:`).
    let source_end = metadata.char_indices().find_map(|(index, character)| {
        if character != ':' {
            return None;
        }
        let rest = &metadata[index + 1..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        (digits > 0 && rest.as_bytes().get(digits) == Some(&b':')).then_some(index)
    });
    let Some(source_end) = source_end else {
        return false;
    };
    let source = Path::new(&metadata[..source_end]);
    let relative_source = if source.is_absolute() {
        match source.strip_prefix(repo) {
            Ok(relative) => relative,
            Err(_) => return false,
        }
    } else {
        source
    };
    if relative_source.file_name().and_then(|name| name.to_str()) != Some(".gitignore") {
        return false;
    }
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(relative_source)
        .output()
        .map(|tracked| tracked.status.success())
        .unwrap_or(false)
}

fn contract_prerequisites(commands: &[ValidationCommand]) -> ReadyDimension {
    if commands.is_empty() {
        return dim(
            "contract prerequisites",
            0,
            5,
            ReadyStatus::Fail,
            "no validation command to preflight",
            "add a runnable validation command before the first mission",
        );
    }
    let missing: Vec<&ValidationCommand> = commands.iter().filter(|c| !c.available).collect();
    if missing.is_empty() {
        dim(
            "contract prerequisites",
            5,
            5,
            ReadyStatus::Pass,
            "validation command programs are on PATH",
            "",
        )
    } else {
        let names = missing
            .iter()
            .map(|c| c.program.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        dim(
            "contract prerequisites",
            2,
            5,
            ReadyStatus::Warn,
            format!("missing program(s): {names}"),
            "install the missing validation-command programs or document the setup",
        )
    }
}

fn backend_lanes(repo: &Path) -> ReadyDimension {
    let cfg = match kranz_engine::config::load(repo) {
        Ok(cfg) => cfg,
        Err(error) => {
            return dim(
                "agent backend lanes",
                0,
                5,
                ReadyStatus::Fail,
                format!("cannot resolve mission config: {error}"),
                "fix mission config before probing agent backends",
            )
        }
    };
    let probes = [
        (
            "claude",
            kranz_engine::backend_claude::discover_claude_binary(cfg.claude_binary.as_deref())
                .is_ok(),
        ),
        (
            "codex",
            kranz_engine::backend_codex::discover_codex_binary(None).is_ok(),
        ),
        (
            "droid",
            kranz_engine::backend_droid::discover_droid_binary(None).is_ok(),
        ),
    ];
    let required = [
        cfg.orchestrator.backend.as_deref().unwrap_or("claude"),
        cfg.worker.backend.as_deref().unwrap_or("claude"),
        cfg.validator_scrutiny
            .backend
            .as_deref()
            .unwrap_or("claude"),
        cfg.validator_functional
            .backend
            .as_deref()
            .unwrap_or("claude"),
    ];
    let missing_required: Vec<&str> = required
        .iter()
        .copied()
        .filter(|required| {
            !probes
                .iter()
                .any(|(backend, available)| backend == required && *available)
        })
        .collect();
    let evidence = format!(
        "{}; executable/version probes only (authentication is proven by the first live mission)",
        probes
            .iter()
            .map(|(backend, available)| format!(
                "{backend}={}",
                if *available { "ready" } else { "unavailable" }
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if missing_required.is_empty() {
        dim("agent backend lanes", 5, 5, ReadyStatus::Pass, evidence, "")
    } else {
        let mut missing = missing_required;
        missing.sort_unstable();
        missing.dedup();
        dim(
            "agent backend lanes",
            0,
            5,
            ReadyStatus::Fail,
            evidence,
            format!(
                "install or configure the selected backend CLI(s): {}",
                missing.join(", ")
            ),
        )
    }
}

fn clean_git_state(repo: &Path) -> ReadyDimension {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .arg("status")
        .arg("--porcelain")
        .output();
    match output {
        Ok(out) if out.status.success() && out.stdout.is_empty() => dim(
            "clean git state",
            10,
            10,
            ReadyStatus::Pass,
            "worktree clean",
            "",
        ),
        Ok(out) if out.status.success() => dim(
            "clean git state",
            0,
            10,
            ReadyStatus::Fail,
            "worktree has uncommitted changes",
            "commit or shelve local changes before autonomous missions",
        ),
        _ => dim(
            "clean git state",
            0,
            10,
            ReadyStatus::Fail,
            "git status unavailable",
            "initialize a git repo and make sure git is available",
        ),
    }
}

fn calibration_corpus(repo: &Path) -> ReadyDimension {
    let missions = cost::calibrate(repo).missions_used;
    match missions {
        0 => dim(
            "calibration corpus",
            0,
            10,
            ReadyStatus::Warn,
            "0 completed missions; estimates use built-in defaults",
            "run a small first mission to seed cost calibration",
        ),
        1 | 2 => dim(
            "calibration corpus",
            7,
            10,
            ReadyStatus::Warn,
            format!("{missions} completed mission(s)"),
            "complete a few more representative missions to tighten estimates",
        ),
        _ => dim(
            "calibration corpus",
            10,
            10,
            ReadyStatus::Pass,
            format!("{missions} completed mission(s)"),
            "",
        ),
    }
}

fn detect_validation_commands(repo: &Path) -> Vec<ValidationCommand> {
    let mut commands = Vec::new();
    if repo.join("Cargo.toml").exists() {
        commands.push(validation_command("cargo test --workspace"));
    }
    if package_json_has_script(repo, "test") {
        commands.push(validation_command("npm test"));
    }
    if repo.join("pytest.ini").exists()
        || repo.join("pyproject.toml").exists()
        || repo.join("tests").exists()
    {
        commands.push(validation_command("pytest"));
    }
    commands
}

fn validation_command(command: &str) -> ValidationCommand {
    let program = command
        .split_whitespace()
        .next()
        .unwrap_or(command)
        .to_string();
    let available = program_available(&program);
    ValidationCommand {
        command: command.to_string(),
        program,
        available,
    }
}

fn package_json_has_script(repo: &Path, script: &str) -> bool {
    let Ok(text) = fs::read_to_string(repo.join("package.json")) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    value
        .get("scripts")
        .and_then(|scripts| scripts.get(script))
        .and_then(|script| script.as_str())
        .is_some_and(|script| !script.trim().is_empty())
}

fn has_tests_dir(repo: &Path) -> bool {
    ["tests", "test", "__tests__"]
        .iter()
        .any(|name| repo.join(name).exists())
}

fn program_available(program: &str) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return path.exists();
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let candidate: PathBuf = dir.join(program);
                candidate.is_file()
            })
        })
        .unwrap_or(false)
}

fn dim(
    name: &'static str,
    score: u8,
    weight: u8,
    status: ReadyStatus,
    evidence: impl Into<String>,
    remedy: impl Into<String>,
) -> ReadyDimension {
    ReadyDimension {
        name,
        score,
        weight,
        status,
        evidence: evidence.into(),
        remedy: remedy.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn commit_all(dir: &Path) {
        git(dir, &["config", "user.name", "ready-test"]);
        git(dir, &["config", "user.email", "ready@example.com"]);
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-m", "fixture"]);
    }

    #[test]
    fn bare_repo_scores_low_and_names_test_runner_first() {
        let dir = TempDir::new().unwrap();
        let report = assess(dir.path());

        assert!(report.score < 50, "{report:?}");
        assert!(
            report.highest_leverage_fix.contains("test command"),
            "{report:?}"
        );
        assert!(
            report
                .dimensions
                .iter()
                .any(|d| d.evidence.contains("0 completed missions")),
            "cold-start calibration warning present: {report:?}"
        );
    }

    #[test]
    fn this_shape_scores_high_with_core_signals() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("AGENTS.md"), "rules");
        write(&dir.path().join("README.md"), "readme");
        write(&dir.path().join("Cargo.toml"), "[workspace]\n");
        write(&dir.path().join(".github/workflows/ci.yml"), "name: ci\n");
        Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .arg("init")
            .output()
            .unwrap();
        write(
            &dir.path().join(".kranz/.gitignore"),
            "missions/*/events.jsonl\n\
             missions/*/events.jsonl.lock\n\
             missions/*/state.json\n\
             missions/*/runs/\n\
             missions/*/control/\n\
             config.json\n\
             serve.token\n\
             tickets/*.status\n",
        );
        commit_all(dir.path());

        let report = assess(dir.path());

        assert!(report.score >= 80, "{report:?}");
        assert_eq!(report.level, ReadyLevel::Ready);
        let hygiene = report
            .dimensions
            .iter()
            .find(|dimension| dimension.name == "kranz runtime gitignore")
            .unwrap();
        assert_eq!(hygiene.status, ReadyStatus::Pass, "{hygiene:?}");
    }

    #[test]
    fn gitignore_hygiene_rejects_tracked_runtime_files() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        write(
            &dir.path().join(".kranz/.gitignore"),
            "missions/\nconfig.json\nserve.token\ntickets/*.status\n",
        );
        write(
            &dir.path().join(".kranz/serve.token"),
            "must-not-be-tracked",
        );
        git(dir.path(), &["config", "user.name", "ready-test"]);
        git(dir.path(), &["config", "user.email", "ready@example.com"]);
        git(dir.path(), &["add", ".kranz/.gitignore"]);
        git(dir.path(), &["add", "-f", ".kranz/serve.token"]);
        git(dir.path(), &["commit", "-m", "tracked token fixture"]);

        let hygiene = gitignore_hygiene(dir.path());
        assert_ne!(hygiene.status, ReadyStatus::Pass, "{hygiene:?}");
        assert!(hygiene.evidence.contains("7/8"), "{hygiene:?}");
    }

    #[test]
    fn gitignore_hygiene_ignores_global_and_git_info_excludes() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        let global = dir.path().join("global-excludes");
        write(&global, ".kranz/\n");
        git(
            dir.path(),
            &["config", "core.excludesFile", global.to_str().unwrap()],
        );
        write(&dir.path().join(".git/info/exclude"), ".kranz/\n");

        let hygiene = gitignore_hygiene(dir.path());
        assert_eq!(hygiene.status, ReadyStatus::Fail, "{hygiene:?}");
    }
}
