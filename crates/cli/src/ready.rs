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
    /// AMM-compatible projection (derived view over the native dimensions;
    /// crates/cli/src/amm.rs owns the mapping table).
    pub amm: crate::amm::AmmProjection,
    /// The second readiness axis — present only for repos with mission
    /// history (omitted from JSON otherwise, never a vacuous zero).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contract_health: Option<kranz_engine::contract_health::ContractHealth>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
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
        merge_gates(repo),
        gitignore_hygiene(repo),
        backend_lanes(repo),
        contract_prerequisites(&validation_commands),
        clean_git_state(repo),
        calibration_corpus(repo),
        context_not_credentials(repo),
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
    // The two-axis view: AMM projection over the native dimensions, and the
    // contract/consent axis from mission event logs (absent without history).
    let contract_health = kranz_engine::contract_health::compute_contract_health(repo)
        .ok()
        .flatten();
    let amm = crate::amm::project(&dimensions, contract_health.as_ref());
    ReadyReport {
        score,
        level,
        highest_leverage_fix,
        dimensions,
        amm,
        contract_health,
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
    out.push_str(&format!(
        "\namm: {} (projection v{})",
        crate::amm::level_label(report.amm.level),
        report.amm.mapping_version
    ));
    if !report.amm.missing_signals.is_empty() {
        out.push_str(&format!(
            " — missing for next level: {}",
            report.amm.missing_signals.join(", ")
        ));
    }
    out.push('\n');
    if let Some(health) = &report.contract_health {
        let lint = health
            .lint_pass_rate
            .map(|r| {
                format!(
                    "{:.0}% ({} of {} linted)",
                    r * 100.0,
                    health.lint_clean,
                    health.lint_linted
                )
            })
            .unwrap_or_else(|| "n/a (no linted missions)".to_string());
        out.push_str(&format!(
            "contract health ({} missions): lint pass {lint}, waivers/mission {:.2}, blocked [{}]\n",
            health.missions,
            health.waivers_per_mission.unwrap_or(0.0),
            [
                ("grant", health.blocked.grant),
                ("scan", health.blocked.secret_scan),
                ("contract-bug", health.blocked.contract_bug),
                ("cap", health.blocked.fix_cycle_cap),
                ("untrusted", health.blocked.untrusted_validator),
                ("other", health.blocked.other),
            ]
            .into_iter()
            .filter(|(_, n)| *n > 0)
            .map(|(k, n)| format!("{k}:{n}"))
            .collect::<Vec<_>>()
            .join(" "),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Org view (`kranz ready --all`) — the M8 catalog scored per repo
// ---------------------------------------------------------------------------

/// One catalog repo's row in the org report. Unavailable repos degrade with
/// a reason, never silently (and are excluded from the L3+ numerator).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgRepoReport {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    pub score: Option<u8>,
    pub level: Option<crate::amm::AmmLevel>,
    pub missing_for_next_level: Vec<String>,
    pub unavailable: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgReport {
    pub repos: Vec<OrgRepoReport>,
    /// The org headline: repos at AMM L3 or better.
    pub at_l3_plus: usize,
    pub total: usize,
    /// Set when the catalog itself is missing or malformed (the repos vec is
    /// empty then — the headline must explain rather than show 0-of-0).
    pub note: Option<String>,
}

/// Score every repo in the host catalog at `config_path`
/// (`~/.kranz/config.json` for the CLI; injected for tests).
pub fn assess_all(config_path: &Path) -> OrgReport {
    let host = match kranz_server::load_host_config(config_path) {
        Ok(host) => host,
        Err(error) => {
            return OrgReport {
                repos: Vec::new(),
                at_l3_plus: 0,
                total: 0,
                note: Some(format!(
                    "cannot read host catalog {}: {error:#}",
                    config_path.display()
                )),
            }
        }
    };
    if host.repos.is_empty() {
        return OrgReport {
            repos: Vec::new(),
            at_l3_plus: 0,
            total: 0,
            note: Some(format!(
                "no host catalog at {} — register repos with `kranz init --register`",
                config_path.display()
            )),
        };
    }

    let mut repos = Vec::new();
    for repo in &host.repos {
        let name = repo.display_name.clone().unwrap_or_else(|| repo.id.clone());
        let unavailable = if !repo.root.is_dir() {
            Some("root missing".to_string())
        } else if !repo.root.join(".git").exists() {
            Some("not a git repository".to_string())
        } else {
            None
        };
        if let Some(reason) = unavailable {
            repos.push(OrgRepoReport {
                id: repo.id.clone(),
                name,
                root: repo.root.clone(),
                score: None,
                level: None,
                missing_for_next_level: Vec::new(),
                unavailable: Some(reason),
            });
            continue;
        }
        let report = assess(&repo.root);
        repos.push(OrgRepoReport {
            id: repo.id.clone(),
            name,
            root: repo.root.clone(),
            score: Some(report.score),
            level: Some(report.amm.level),
            missing_for_next_level: report.amm.missing_signals.clone(),
            unavailable: None,
        });
    }
    let at_l3_plus = repos
        .iter()
        .filter(|r| {
            r.level
                .map(|l| l >= crate::amm::AmmLevel::L3)
                .unwrap_or(false)
        })
        .count();
    OrgReport {
        total: repos.len(),
        repos,
        at_l3_plus,
        note: None,
    }
}

pub fn render_org(report: &OrgReport) -> String {
    let mut out = String::new();
    if let Some(note) = &report.note {
        out.push_str(&format!("kranz ready --all: {note}\n"));
        return out;
    }
    out.push_str(&format!(
        "kranz ready --all: {} of {} repos at L3+\n",
        report.at_l3_plus, report.total
    ));
    for repo in &report.repos {
        if let Some(reason) = &repo.unavailable {
            out.push_str(&format!(
                "  {:<20} —   unavailable: {reason} ({})\n",
                repo.name,
                repo.root.display()
            ));
        } else {
            out.push_str(&format!(
                "  {:<20} {:<3} {:>3}/100  ({})\n",
                repo.name,
                crate::amm::level_label(repo.level.expect("available repos have a level")),
                repo.score.unwrap_or(0),
                repo.root.display()
            ));
            if !repo.missing_for_next_level.is_empty() {
                out.push_str(&format!(
                    "  {:<20}     missing for next level: {}\n",
                    "",
                    repo.missing_for_next_level.join(", ")
                ));
            }
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
            5,
            5,
            ReadyStatus::Pass,
            "CI config detected",
            "",
        )
    } else {
        dim(
            "CI config",
            0,
            5,
            ReadyStatus::Fail,
            "no common CI config detected",
            "add CI that runs the same validation commands kranz will gate on",
        )
    }
}

/// `kranz merge` fails closed without a tracked, parseable gate suite on the
/// base branch, so readiness validates the same artifact the merge will read:
/// the file committed to the CURRENT branch's tree (never the working tree —
/// an uncommitted suite would not gate the first merge).
fn merge_gates(repo: &Path) -> ReadyDimension {
    use kranz_engine::merge_gate::MERGE_GATES_PATH;
    let committed = kranz_engine::git_ops::GitRepo::open(repo)
        .and_then(|git| git.show_file("HEAD", MERGE_GATES_PATH));
    let bytes = match committed {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return dim(
                "merge gates",
                0,
                5,
                ReadyStatus::Fail,
                format!("no tracked {MERGE_GATES_PATH} on the current branch"),
                format!(
                    "commit a {MERGE_GATES_PATH} gate suite so the first merge does not fail closed"
                ),
            )
        }
        Err(error) => {
            return dim(
                "merge gates",
                0,
                5,
                ReadyStatus::Fail,
                format!("cannot read committed {MERGE_GATES_PATH}: {error}"),
                format!("commit a parseable {MERGE_GATES_PATH} on a committed branch"),
            )
        }
    };
    match kranz_engine::merge_gate::parse_gate_suite(&bytes) {
        Ok(suite) => dim(
            "merge gates",
            5,
            5,
            ReadyStatus::Pass,
            format!(
                "committed {MERGE_GATES_PATH} defines {} gate(s)",
                suite.gates.len()
            ),
            "",
        ),
        Err(detail) => dim(
            "merge gates",
            0,
            5,
            ReadyStatus::Fail,
            detail,
            format!("fix {MERGE_GATES_PATH} so merges can run real gates"),
        ),
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
    // Verdict first. `check-ignore --verbose` exits 0 whenever ANY rule
    // decides the path — including a negation such as `!config.json` that
    // makes it committable — so only `--quiet` (success == actually ignored)
    // is trusted for the verdict; `--verbose` below is source attribution
    // only.
    let ignored = Command::new("git")
        .arg("-C")
        .arg(repo)
        // Readiness is a repository property: ignore an operator's global
        // excludes here and reject non-repo rule sources below, so
        // `.git/info/exclude` and an uncommitted `.gitignore` cannot make a
        // repo look ready.
        .args(["-c", "core.excludesFile=", "check-ignore", "--quiet", "--"])
        .arg(relative_path)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    if !ignored {
        return false;
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
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
    // Engine-canonical exception (gitignore-self-ignore ticket, option b):
    // `.kranz/.gitignore` is materialized by the engine on init and ignores
    // ITSELF by design, so it can never satisfy the committed-bytes rule
    // below — yet it is the canonical runtime hygiene for every repo kranz
    // initializes. Its rules travel with the tool, not the repo. Accept it
    // only when the on-disk file carries the full canonical rule set —
    // operator additions are fine; anything else falls through to the
    // committed-bytes standard below.
    if relative_source == Path::new(".kranz/.gitignore") {
        let canonical = std::fs::read_to_string(repo.join(relative_source))
            .map(|text| {
                kranz_engine::paths::KRANZ_GITIGNORE_RULES
                    .iter()
                    .all(|rule| text.lines().any(|line| line.trim() == *rule))
            })
            .unwrap_or(false);
        if canonical {
            return true;
        }
    }
    // The decisive rule must come from the committed .gitignore bytes, not an
    // uncommitted edit to a file that merely also exists in HEAD.
    let source_spec = relative_source
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    let Ok(git) = kranz_engine::git_ops::GitRepo::open(repo) else {
        return false;
    };
    let Ok(Some(_)) = git.show_file("HEAD", &source_spec) else {
        return false;
    };
    if !git.has_normal_index_entry(&source_spec).unwrap_or(false) {
        return false;
    }
    // Let Git compare through its normal text conversion rules. This rejects
    // staged and unstaged rule changes while accepting a clean CRLF worktree
    // backed by an LF blob under core.autocrlf / .gitattributes.
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["diff", "--quiet", "HEAD", "--"])
        .arg(&source_spec)
        .status()
        .map(|status| status.success())
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
    backend_lanes_for_config(&cfg)
}

fn backend_lanes_for_config(cfg: &kranz_engine::types::MissionConfig) -> ReadyDimension {
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
    // Probe only backends some role actually selects: a `--version` probe
    // still execs a real binary, so an unrelated broken (or hung) CLI on
    // PATH must not slow down or fail readiness for a repo that never
    // dispatches to it. `None` marks a lane that was skipped, not probed.
    let probes: Vec<(&str, Option<bool>)> = ["claude", "codex", "droid", "kimi"]
        .iter()
        .map(|&backend| {
            if !required.contains(&backend) {
                return (backend, None);
            }
            let available = match backend {
                "claude" => kranz_engine::backend_claude::discover_claude_binary(
                    cfg.claude_binary.as_deref(),
                )
                .is_ok(),
                "codex" => kranz_engine::backend_codex::discover_codex_binary(None).is_ok(),
                "droid" => kranz_engine::backend_droid::discover_droid_binary(None).is_ok(),
                _ => kranz_engine::backend_kimi::discover_kimi_binary(None).is_ok(),
            };
            (backend, Some(available))
        })
        .collect();
    let missing_required: Vec<&str> = required
        .iter()
        .copied()
        .filter(|required| {
            !probes
                .iter()
                .any(|(backend, available)| backend == required && *available == Some(true))
        })
        .collect();
    let evidence = format!(
        "{}; executable/version probes only (authentication is proven by the first live mission)",
        probes
            .iter()
            .map(|(backend, available)| format!(
                "{backend}={}",
                match available {
                    Some(true) => "ready",
                    Some(false) => "unavailable",
                    None => "skipped (no role selects it)",
                }
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

/// Context-rich WITHOUT credential-rich (the ready-context-vs-credentials
/// ticket): agents should get context from git artifacts — a knowledge
/// vault, docs, and a secret-scan gate keeping the tree clean — not from
/// live secrets. Four signals: knowledge vault present, merge-gate suite
/// committed (the scan gate lives there), no tracked .env-shaped file, and
/// worker-readable setup docs.
fn context_not_credentials(repo: &Path) -> ReadyDimension {
    let mut signals: Vec<(&str, bool, &str)> = Vec::new();

    let vault = repo.join("docs/knowledge").is_dir();
    signals.push((
        "knowledge vault",
        vault,
        "create docs/knowledge/ (or an equivalent committed vault) so agent context lives in git",
    ));

    let gate = repo.join(".kranz/merge-gates.json").is_file();
    signals.push((
        "secret-scan gate",
        gate,
        "commit a .kranz/merge-gates.json suite so the secret-scan gate runs before every merge",
    ));

    // Tracked .env-shaped files: context-as-secret is the anti-signal. Read
    // the index (never the working tree) so untracked local .env files don't
    // count against the repo.
    let tracked_env: Vec<String> = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["ls-files"])
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|line| {
                    let name = line.rsplit('/').next().unwrap_or(line);
                    name == ".env" || name.starts_with(".env.") || name.ends_with(".env")
                })
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let no_env = tracked_env.is_empty();
    signals.push((
        "no tracked .env",
        no_env,
        "untrack env files and move secrets to the credential store — tracked .env: see evidence",
    ));

    let docs = repo.join("README.md").is_file();
    signals.push((
        "worker-readable docs",
        docs,
        "document setup/build/test in the README so workers don't need tribal (or credential-gated) knowledge",
    ));

    let passed = signals.iter().filter(|(_, ok, _)| *ok).count();
    let score = ((passed * 10) / signals.len()) as u8;
    let status = match passed {
        n if n == signals.len() => ReadyStatus::Pass,
        0 | 1 => ReadyStatus::Fail,
        _ => ReadyStatus::Warn,
    };
    let evidence = if passed == signals.len() {
        "context lives in git, not credentials".to_string()
    } else {
        let mut parts: Vec<String> = signals
            .iter()
            .filter(|(_, ok, _)| !ok)
            .map(|(name, _, _)| format!("{name} missing"))
            .collect();
        if !tracked_env.is_empty() {
            parts.push(format!("tracked env files: {}", tracked_env.join(", ")));
        }
        parts.join("; ")
    };
    let remedy = signals
        .iter()
        .filter(|(_, ok, _)| !ok)
        .map(|(_, _, hint)| *hint)
        .collect::<Vec<_>>()
        .join("; ");
    dim(
        "context over credentials",
        score,
        10,
        status,
        evidence,
        remedy,
    )
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
    let names = executable_names(program);
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                names.iter().any(|name| {
                    let candidate: PathBuf = dir.join(name);
                    candidate.is_file()
                })
            })
        })
        .unwrap_or(false)
}

/// The bare name on unix; the bare name plus PATHEXT variants on Windows,
/// where `cargo` itself is never a file — `cargo.exe`/`cargo.cmd` is.
/// Without this every PATH probe reports unavailable on Windows and
/// `kranz ready` calls every program missing.
fn executable_names(program: &str) -> Vec<String> {
    if !cfg!(windows) {
        return vec![program.to_string()];
    }
    let mut names = vec![program.to_string()];
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    for ext in pathext.split(';').filter(|e| !e.is_empty()) {
        names.push(format!("{program}{ext}"));
        // Case-insensitive volumes make `cargo.EXE` find `cargo.exe`, but a
        // case-sensitive one needs the lowercase spelling too.
        names.push(format!("{program}{}", ext.to_ascii_lowercase()));
    }
    names
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
        write(
            &dir.path().join(".kranz/merge-gates.json"),
            r#"{"gates":[{"command":"cargo test --workspace"}]}"#,
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
        let gates = report
            .dimensions
            .iter()
            .find(|dimension| dimension.name == "merge gates")
            .unwrap();
        assert_eq!(gates.status, ReadyStatus::Pass, "{gates:?}");
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

    #[test]
    fn gitignore_hygiene_rejects_negated_ignore_rules() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        // `*` ignores the runtime files, but the trailing negation makes
        // config.json committable without -f — the probe must not count it
        // as ignored (check-ignore --verbose exits 0 for negated matches).
        write(
            &dir.path().join(".kranz/.gitignore"),
            "*\n!.gitignore\n!config.json\n",
        );
        commit_all(dir.path());

        assert!(!git_ignores(dir.path(), ".kranz/config.json"));
        let hygiene = gitignore_hygiene(dir.path());
        assert_ne!(hygiene.status, ReadyStatus::Pass, "{hygiene:?}");
        assert!(hygiene.evidence.contains("7/8"), "{hygiene:?}");
    }

    #[test]
    fn gitignore_hygiene_rejects_staged_but_uncommitted_gitignore() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        write(&dir.path().join("README.md"), "readme");
        commit_all(dir.path());
        // Staged but never committed: the rules are not yet a repository
        // property, so the dimension must not pass.
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
        git(dir.path(), &["add", ".kranz/.gitignore"]);

        assert!(!git_ignores(dir.path(), ".kranz/config.json"));
        let hygiene = gitignore_hygiene(dir.path());
        assert_eq!(hygiene.status, ReadyStatus::Fail, "{hygiene:?}");
    }

    #[test]
    fn gitignore_hygiene_rejects_uncommitted_rules_in_a_tracked_gitignore() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        write(&dir.path().join(".kranz/.gitignore"), "missions/\n");
        commit_all(dir.path());
        write(
            &dir.path().join(".kranz/.gitignore"),
            "missions/\nconfig.json\n",
        );

        let git_verdict = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["check-ignore", "--quiet", "--", ".kranz/config.json"])
            .status()
            .unwrap();
        assert!(
            git_verdict.success(),
            "fixture's working-tree rule must ignore config.json"
        );
        assert!(
            !git_ignores(dir.path(), ".kranz/config.json"),
            "readiness must evaluate the committed .gitignore bytes"
        );
    }

    #[test]
    fn gitignore_hygiene_accepts_engine_materialized_kranz_gitignore() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        write(&dir.path().join("README.md"), "readme");
        commit_all(dir.path());
        // The engine-canonical file: the full canonical rule set, UNTRACKED
        // (it ignores itself by design) — exactly what kranz init
        // materializes. Its rules travel with the tool, so the probe credits
        // it without a commit.
        let mut text = "# kranz engine bookkeeping — never part of mission commits\n".to_string();
        for rule in kranz_engine::paths::KRANZ_GITIGNORE_RULES {
            text.push_str(rule);
            text.push('\n');
        }
        write(&dir.path().join(".kranz/.gitignore"), &text);

        assert!(git_ignores(dir.path(), ".kranz/config.json"));
        let hygiene = gitignore_hygiene(dir.path());
        assert_eq!(hygiene.status, ReadyStatus::Pass, "{hygiene:?}");
    }

    #[test]
    fn gitignore_hygiene_rejects_gutted_kranz_gitignore_when_uncommitted() {
        // A hand-rolled .kranz/.gitignore without the full canonical set is
        // NOT the engine artifact — it falls back to the committed-bytes
        // standard and loses (untracked).
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        write(&dir.path().join("README.md"), "readme");
        commit_all(dir.path());
        write(
            &dir.path().join(".kranz/.gitignore"),
            "missions/\nconfig.json\nserve.token\ntickets/*.status\n",
        );

        assert!(!git_ignores(dir.path(), ".kranz/config.json"));
    }

    #[test]
    fn gitignore_hygiene_rejects_rules_hidden_by_index_flags() {
        for flag in ["--assume-unchanged", "--skip-worktree"] {
            let dir = TempDir::new().unwrap();
            git(dir.path(), &["init"]);
            write(&dir.path().join(".kranz/.gitignore"), "missions/\n");
            commit_all(dir.path());
            git(dir.path(), &["update-index", flag, ".kranz/.gitignore"]);
            write(
                &dir.path().join(".kranz/.gitignore"),
                "missions/\nconfig.json\n",
            );

            let git_verdict = Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(["check-ignore", "--quiet", "--", ".kranz/config.json"])
                .status()
                .unwrap();
            assert!(git_verdict.success(), "fixture failed for {flag}");
            assert!(
                !git_ignores(dir.path(), ".kranz/config.json"),
                "readiness trusted a .gitignore hidden by {flag}"
            );
        }
    }

    #[test]
    fn gitignore_hygiene_rejects_rules_hidden_by_fsmonitor_valid() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        write(&dir.path().join(".kranz/.gitignore"), "missions/\n");
        commit_all(dir.path());
        git(dir.path(), &["config", "core.fsmonitor", "true"]);
        write(
            &dir.path().join(".kranz/.gitignore"),
            "missions/\nconfig.json\n",
        );
        git(
            dir.path(),
            &["update-index", "--fsmonitor-valid", ".kranz/.gitignore"],
        );

        // Whether `update-index --fsmonitor-valid` sticks is git-version
        // dependent (ubuntu-latest's git leaves the entry `H`); when this
        // host's git can't establish the fixture, skip rather than fail —
        // the protection is still exercised wherever git supports the bit
        // (same pattern as the sandbox-exec skips in backend_claude_test).
        let index_tag = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["ls-files", "-f", "--", ".kranz/.gitignore"])
            .output()
            .unwrap();
        if String::from_utf8_lossy(&index_tag.stdout) != "h .kranz/.gitignore\n" {
            eprintln!("this git does not honor --fsmonitor-valid; skipping");
            return;
        }
        let hidden_diff = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["diff", "--quiet", "HEAD", "--", ".kranz/.gitignore"])
            .status()
            .unwrap();
        if !hidden_diff.success() {
            eprintln!("this git does not hide fsmonitor-valid worktree bytes; skipping");
            return;
        }
        assert!(
            !git_ignores(dir.path(), ".kranz/config.json"),
            "readiness trusted a .gitignore hidden by fsmonitor-valid"
        );
    }

    #[test]
    fn merge_gates_dimension_requires_a_tracked_suite() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        write(&dir.path().join("README.md"), "readme");
        commit_all(dir.path());
        // A working-tree-only suite must not count: merges read the file
        // from the committed tree, never the working tree.
        write(
            &dir.path().join(".kranz/merge-gates.json"),
            r#"{"gates":[{"command":"cargo test --workspace"}]}"#,
        );

        let gates = merge_gates(dir.path());
        assert_eq!(gates.status, ReadyStatus::Fail, "{gates:?}");
        assert!(gates.evidence.contains("no tracked"), "{gates:?}");
        assert!(
            gates.remedy.contains(".kranz/merge-gates.json"),
            "{gates:?}"
        );
    }

    #[test]
    fn merge_gates_dimension_rejects_unparseable_suites() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        write(&dir.path().join(".kranz/merge-gates.json"), "not json");
        commit_all(dir.path());

        let gates = merge_gates(dir.path());
        assert_eq!(gates.status, ReadyStatus::Fail, "{gates:?}");
        assert!(
            gates.evidence.contains("invalid .kranz/merge-gates.json"),
            "{gates:?}"
        );
        assert!(!gates.remedy.is_empty(), "{gates:?}");
    }

    #[test]
    fn merge_gates_dimension_passes_on_a_committed_valid_suite() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        write(
            &dir.path().join(".kranz/merge-gates.json"),
            r#"{"gates":[{"command":"cargo test --workspace"}]}"#,
        );
        commit_all(dir.path());

        let gates = merge_gates(dir.path());
        assert_eq!(gates.status, ReadyStatus::Pass, "{gates:?}");
        assert!(gates.evidence.contains("1 gate"), "{gates:?}");
    }

    #[test]
    fn backend_lanes_probe_only_backends_some_role_selects() {
        // The default config dispatches every role to claude, so the codex
        // and droid lanes must be skipped instead of exec'd.
        let cfg = kranz_engine::types::MissionConfig::default();
        let lanes = backend_lanes_for_config(&cfg);
        assert!(!lanes.evidence.contains("claude=skipped"), "{lanes:?}");
        assert!(lanes.evidence.contains("codex=skipped"), "{lanes:?}");
        assert!(lanes.evidence.contains("droid=skipped"), "{lanes:?}");
        assert!(lanes.evidence.contains("kimi=skipped"), "{lanes:?}");
    }

    // -----------------------------------------------------------------------
    // Org view (kranz ready --all)
    // -----------------------------------------------------------------------

    fn host_config(dir: &Path, repos: serde_json::Value) -> PathBuf {
        let path = dir.join("config.json");
        // serde_json, never string interpolation: Windows roots contain
        // backslashes, which become invalid JSON escapes when pasted raw.
        write(
            &path,
            &serde_json::json!({ "host": { "repos": repos } }).to_string(),
        );
        path
    }

    fn repo_entry(id: &str, root: &Path) -> serde_json::Value {
        serde_json::json!({ "id": id, "root": root })
    }

    fn git_repo(dir: &Path) {
        git(dir, &["init"]);
        write(&dir.join("README.md"), "readme");
        commit_all(dir);
    }

    #[test]
    fn context_dimension_passes_when_context_lives_in_git() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        write(&dir.path().join("README.md"), "readme");
        write(&dir.path().join("docs/knowledge/index.md"), "# vault");
        write(
            &dir.path().join(".kranz/merge-gates.json"),
            r#"{"gates":[{"command":"cargo test"}]}"#,
        );
        commit_all(dir.path());

        let dimension = context_not_credentials(dir.path());
        assert_eq!(dimension.status, ReadyStatus::Pass, "{dimension:?}");
        assert_eq!(dimension.score, 10);
    }

    #[test]
    fn context_dimension_flags_tracked_env_and_missing_vault() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        write(&dir.path().join("README.md"), "readme");
        write(&dir.path().join(".env.production"), "SECRET=hunter2");
        write(
            &dir.path().join(".kranz/merge-gates.json"),
            r#"{"gates":[{"command":"cargo test"}]}"#,
        );
        commit_all(dir.path());

        let dimension = context_not_credentials(dir.path());
        assert_eq!(dimension.status, ReadyStatus::Warn, "{dimension:?}");
        assert!(
            dimension.evidence.contains("knowledge vault missing"),
            "vault gap named: {dimension:?}"
        );
        assert!(
            dimension.evidence.contains(".env.production"),
            "the tracked env file is named in evidence: {dimension:?}"
        );
        assert!(dimension.remedy.contains("untrack"), "{dimension:?}");
    }

    #[test]
    fn context_dimension_fails_when_nothing_is_in_place() {
        let dir = TempDir::new().unwrap();
        git(dir.path(), &["init"]);
        write(&dir.path().join("x.txt"), "x");
        commit_all(dir.path());

        let dimension = context_not_credentials(dir.path());
        assert_eq!(dimension.status, ReadyStatus::Fail, "{dimension:?}");
        assert_eq!(dimension.score, 2, "only the env-absence signal scores");
    }

    #[test]
    fn org_view_scores_catalog_and_counts_l3_plus() {
        let dir = TempDir::new().unwrap();
        let strong = dir.path().join("strong");
        let weak = dir.path().join("weak");
        fs::create_dir_all(&strong).unwrap();
        fs::create_dir_all(&weak).unwrap();
        // A fully-shaped repo (mirrors this_shape_scores_high_with_core_signals).
        write(&strong.join("AGENTS.md"), "rules");
        write(&strong.join("README.md"), "readme");
        write(&strong.join("Cargo.toml"), "[workspace]\n");
        write(&strong.join(".github/workflows/ci.yml"), "name: ci\n");
        git(&strong, &["init"]);
        write(
            &strong.join(".kranz/.gitignore"),
            "missions/*/events.jsonl\nmissions/*/events.jsonl.lock\nmissions/*/state.json\nmissions/*/runs/\nmissions/*/control/\nconfig.json\nserve.token\ntickets/*.status\n",
        );
        write(
            &strong.join(".kranz/merge-gates.json"),
            r#"{"gates":[{"command":"cargo test --workspace"}]}"#,
        );
        commit_all(&strong);
        git_repo(&weak);

        let config = host_config(
            dir.path(),
            serde_json::json!([repo_entry("strong", &strong), repo_entry("weak", &weak)]),
        );
        let report = assess_all(&config);

        assert_eq!(report.total, 2);
        assert!(report.note.is_none());
        let strong_row = report.repos.iter().find(|r| r.id == "strong").unwrap();
        let weak_row = report.repos.iter().find(|r| r.id == "weak").unwrap();
        assert!(
            strong_row.level.unwrap() >= crate::amm::AmmLevel::L3,
            "{strong_row:?}"
        );
        assert_eq!(
            weak_row.level,
            Some(crate::amm::AmmLevel::L1),
            "{weak_row:?}"
        );
        assert_eq!(report.at_l3_plus, 1, "{report:?}");
        // The headline renders the N-of-M line.
        let text = render_org(&report);
        assert!(text.contains("1 of 2 repos at L3+"), "{text}");
    }

    #[test]
    fn org_view_degrades_unavailable_repos_with_a_reason() {
        let dir = TempDir::new().unwrap();
        let present = dir.path().join("present");
        fs::create_dir_all(&present).unwrap();
        git_repo(&present);
        let missing = dir.path().join("missing");
        let not_git = dir.path().join("not-git");
        fs::create_dir_all(&not_git).unwrap();

        let mut notgit = repo_entry("notgit", &not_git);
        notgit["displayName"] = serde_json::Value::String("Not Git".to_string());
        let config = host_config(
            dir.path(),
            serde_json::json!([
                repo_entry("present", &present),
                repo_entry("missing", &missing),
                notgit,
            ]),
        );
        let report = assess_all(&config);

        assert_eq!(report.total, 3);
        let missing_row = report.repos.iter().find(|r| r.id == "missing").unwrap();
        assert_eq!(missing_row.unavailable.as_deref(), Some("root missing"));
        assert!(missing_row.level.is_none());
        let notgit_row = report.repos.iter().find(|r| r.id == "notgit").unwrap();
        assert_eq!(
            notgit_row.unavailable.as_deref(),
            Some("not a git repository")
        );
        assert_eq!(notgit_row.name, "Not Git");
        // Unavailable repos never enter the L3+ numerator.
        assert_eq!(report.at_l3_plus, 0, "{report:?}");
        let text = render_org(&report);
        assert!(text.contains("unavailable: root missing"), "{text}");
    }

    #[test]
    fn org_view_explains_empty_and_malformed_catalogs() {
        let dir = TempDir::new().unwrap();
        // Missing file → empty catalog with guidance, not a 0-of-0 shrug.
        let missing = assess_all(&dir.path().join("nope.json"));
        assert_eq!(missing.total, 0);
        assert!(missing.note.as_deref().unwrap().contains("no host catalog"));
        assert!(render_org(&missing).contains("kranz init --register"));

        // Malformed → explicit error, never silent.
        let bad = dir.path().join("bad.json");
        write(&bad, "{not json");
        let malformed = assess_all(&bad);
        assert_eq!(malformed.total, 0);
        assert!(malformed
            .note
            .as_deref()
            .unwrap()
            .contains("cannot read host catalog"));
    }
}
