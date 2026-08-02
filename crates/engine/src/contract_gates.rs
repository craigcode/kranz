//! Named, deterministic contract-validation gates (ticket
//! `.kranz/tickets/contract-validation-gates.md`, KRZ-327 wedge).
//!
//! The contract-authoring defect class — the escapes documented in-repo by
//! the m-66aff8 a3 vacuous-filter incident (AGENTS.md anti-vacuity rule) and
//! the SUSPECT classes [`crate::contract_lint`] detects at approval — becomes
//! a set of NAMED gates registered through the first-class gate interface
//! ([`crate::gate`], KRZ-311). Each gate's name IS the defect-class name, so
//! the class travels verbatim into every verdict, approval decision, and
//! rendered report:
//!
//! - **`vacuous-filter`** — a test-gate command whose success can be vacuous:
//!   a `cargo test` (or similar test-runner) pipeline whose grep does not
//!   anchor a nonzero count (the `test result: ok. [1-9]` guard), or a filter
//!   substring that collides with an existing test name in the repo (the
//!   m-66aff8 a3 shape: `refus` matched the pre-existing
//!   `dirty_tracked_tree_is_refused_...`, gating green with zero
//!   implementation). Static and fast: it greps test files for the substring,
//!   it never compiles anything. The collision signal runs only where the
//!   work has NOT landed yet (approval) — at the final gate a well-formed
//!   filter collides with its own just-landed tests by design.
//! - **`wrong-polarity`** — an assertion that passes BECAUSE its target is
//!   absent rather than because the property holds: a negated grep
//!   (`! grep -q pattern <path>`) or a neutralized one
//!   (`grep -q ... || true`) whose target path does not exist in the tree
//!   being checked. Deterministic: check the path's existence.
//! - **`passes-on-base`** — the existing `PassedOnBase` lint class,
//!   graduated: a command assertion that already exits zero on the untouched
//!   base tree (a correctly-scoped "the work landed" assertion must FAIL
//!   before the work lands; the m-0c885b `a6` inverted-grep did not). This
//!   gate is constructed from the approval-time lint report, so it only runs
//!   where that report exists.
//! - **`env-sensitive`** — a check whose verdict depends on the environment
//!   rather than the tree: `$HOME`/`~`/absolute user paths, wall-clock `date`
//!   comparisons, or unproxied network tools (`curl`/`wget`) in the command
//!   text.
//!
//! WHY the analysis is deliberately conservative: these gates run over
//! model-authored shell without a parser, so every rule only fires on a
//! shape it can fully account for — an undeterminable path (glob, variable,
//! tilde), an unparseable grep, or an unfamiliar test runner is a PASS, never
//! a guess. The regression bar is the repo's own well-formed contract
//! commands (`.kranz/merge-gates.json`, the [`crate::contract_lint`] test
//! fixtures): those must pass all gates unchanged.
//!
//! WHY the posture is advisory: approval blocking behavior is unchanged by
//! this ticket. The gates SURFACE named verdicts — through the existing
//! `emit_decision` path at approval and at the final gate, and through the
//! lint section of plan.md — they do not refuse approval and do not add
//! findings. Recording the verdict is the deliverable; gate-refusal policy
//! is a separate operator decision.
//!
//! Like [`crate::contract_lint`], this module takes plain inputs and does
//! not depend on `MissionEngine`, so it is unit-testable in isolation.

use crate::contract_lint::{AssertionLint, AssertionLintOutcome, ContractLintReport};
use crate::gate::{
    ArtefactRef, Gate, GateKind, GateOutcome, GatePipeline, GateReport, GateVerdict,
};
use crate::types::{Assertion, AssertionCheck};
use std::path::{Path, PathBuf};

/// Defect-class names — the gate identities carried into every verdict.
pub const VACUOUS_FILTER: &str = "vacuous-filter";
pub const WRONG_POLARITY: &str = "wrong-polarity";
pub const PASSES_ON_BASE: &str = "passes-on-base";
pub const ENV_SENSITIVE: &str = "env-sensitive";

/// One command assertion reduced to what the static gates inspect.
#[derive(Debug, Clone)]
struct CommandCheck {
    id: String,
    command: String,
}

/// Collect the contract's `check: command` assertions as [`CommandCheck`]s —
/// the same filter [`crate::contract_lint`] applies before running them.
fn command_checks(contract: &[Assertion]) -> Vec<CommandCheck> {
    contract
        .iter()
        .filter(|a| a.check == AssertionCheck::Command)
        .filter_map(|a| {
            a.command.as_deref().map(|command| CommandCheck {
                id: a.id.clone(),
                command: command.to_string(),
            })
        })
        .collect()
}

/// Build and evaluate the contract-gate pipeline, returning one
/// [`GateReport`] per registered gate in ticket order (vacuous-filter,
/// wrong-polarity, passes-on-base, env-sensitive).
///
/// `lint` is the approval-time base-tree lint report when it exists
/// (approve time); the `passes-on-base` gate is only registered then — at
/// the final gate the work has landed, so passing on base is expected and
/// the class is meaningless. `tree_root` is the tree the path-existence and
/// filter-collision checks run against (the repo root at approve time, the
/// active root at the final gate).
///
/// Returns an empty vec when the contract has no command assertions —
/// mirroring [`ContractLintReport::is_empty`], so callers surface nothing.
pub fn contract_gate_reports(
    contract: &[Assertion],
    lint: Option<&ContractLintReport>,
    tree_root: &Path,
) -> Vec<GateReport> {
    let mut pipeline = GatePipeline::new();
    register_contract_gates(&mut pipeline, contract, lint, tree_root);
    pipeline.evaluate()
}

/// Register the engine floor gates into `pipeline` WITHOUT evaluating —
/// the composition seam for surfaces that evaluate the floor together with
/// MORE gates (the pack contract's final-gate surface, ticket
/// `.kranz/tickets/pack-contract-gates-prompts.md`). Registration order IS
/// the evaluation order within the pipeline's deterministic section
/// (gate.rs), so a caller building a shared pipeline must call this FIRST:
/// the floor then precedes anything registered later by construction, and a
/// pack can add to the floor but never precede or displace it.
///
/// Registers nothing when the contract has no command assertions (the same
/// emptiness rule as [`contract_gate_reports`]). `lint` and `tree_root`
/// carry the same meaning as there.
pub fn register_contract_gates(
    pipeline: &mut GatePipeline,
    contract: &[Assertion],
    lint: Option<&ContractLintReport>,
    tree_root: &Path,
) {
    let checks = command_checks(contract);
    if checks.is_empty() {
        return;
    }
    pipeline
        .register(Box::new(VacuousFilterGate {
            checks: checks.clone(),
            repo_root: tree_root.to_path_buf(),
            // The filter-collision signal is only meaningful BEFORE the work
            // lands: at approval the new tests must not exist yet, while at
            // the final gate a well-formed filter collides BY DESIGN (the
            // tests it gates on just landed). `lint` exists exactly at
            // approval (passes-on-base is keyed on it the same way).
            filter_collision: lint.is_some(),
        }))
        .register(Box::new(WrongPolarityGate {
            checks: checks.clone(),
            tree_root: tree_root.to_path_buf(),
        }));
    if let Some(lint) = lint {
        pipeline.register(Box::new(PassesOnBaseGate {
            results: lint.results.clone(),
        }));
    }
    pipeline.register(Box::new(EnvSensitiveGate { checks }));
}

/// Names of the gates that failed, in pipeline order — the defect-class
/// names a caller folds into a headline.
pub fn failed_gate_names(reports: &[GateReport]) -> Vec<&str> {
    reports
        .iter()
        .filter(|r| !r.outcome.passed())
        .map(|r| r.name.as_str())
        .collect()
}

/// Render the named-verdict block shared by the approval-decision detail
/// and the plan.md lint section: one `- <class>: PASS|FAIL` line per gate,
/// with the failing gate's per-assertion findings indented beneath it.
pub fn render_gate_verdicts(reports: &[GateReport]) -> String {
    render_verdict_block("named contract gates (defect classes):", reports)
}

/// [`render_gate_verdicts`] with a caller-chosen header line — the pack
/// contract's final-gate surface renders the same per-gate block under its
/// own header rather than the contract-defect one.
pub fn render_verdict_block(header: &str, reports: &[GateReport]) -> String {
    let mut out = String::from(header);
    for report in reports {
        let verdict = match report.outcome.verdict {
            GateVerdict::Pass => "PASS",
            GateVerdict::Fail => "FAIL",
        };
        out.push_str(&format!("\n- {}: {verdict}", report.name));
        if let Some(detail) = &report.outcome.artefact.detail {
            for line in detail.lines() {
                out.push_str(&format!("\n  {line}"));
            }
        }
    }
    out
}

/// Shared verdict shape: any findings means the gate FAILs and the findings
/// become the artefact detail; none means PASS.
fn outcome_from_findings(name: &str, findings: Vec<String>) -> GateOutcome {
    let artefact = ArtefactRef::new(format!("contract gate {name}"));
    if findings.is_empty() {
        GateOutcome::pass(artefact)
    } else {
        GateOutcome::fail(artefact.with_detail(findings.join("\n")))
    }
}

// ---------------------------------------------------------------------------
// vacuous-filter
// ---------------------------------------------------------------------------

/// The `vacuous-filter` gate: static analysis of test-runner pipelines and
/// `cargo test` filter substrings (see the module docs).
struct VacuousFilterGate {
    checks: Vec<CommandCheck>,
    repo_root: PathBuf,
    /// Whether the filter-collision signal runs. It is only meaningful
    /// BEFORE the work lands (approval): a well-formed filter names the
    /// not-yet-written tests, so it must collide with nothing. At the final
    /// gate the same filter collides BY DESIGN — the tests it gates on just
    /// landed — so the signal is disabled there (the unanchored-grep signal
    /// is phase-independent and always runs).
    filter_collision: bool,
}

impl Gate for VacuousFilterGate {
    fn name(&self) -> &str {
        VACUOUS_FILTER
    }
    fn kind(&self) -> GateKind {
        GateKind::Deterministic
    }
    fn evaluate(&self) -> GateOutcome {
        outcome_from_findings(
            VACUOUS_FILTER,
            vacuous_filter_findings(&self.checks, &self.repo_root, self.filter_collision),
        )
    }
}

/// Per-assertion findings for the vacuous-filter class. Two independent
/// signals, both static:
///
/// 1. A test-runner invocation (`cargo test`, `cargo nextest run`,
///    `npm test`, `pytest`, `go test`) sharing a command with a grep whose
///    pattern anchors no nonzero count. Zero matching tests still print
///    `test result: ok. 0 passed` — the AGENTS.md anti-vacuity rule exists
///    because `grep -q 'test result: ok'` gates green on it. The anchor we
///    recognize is the `[1-9]` digit class the rule prescribes.
/// 2. A `cargo test <filter>` whose filter substring names an EXISTING test
///    fn in the repo — the m-66aff8 a3 collision, where the gate passes on
///    pre-existing tests with zero implementation. Checked by grepping test
///    files' `fn` names, never by compiling. Runs only when
///    `filter_collision` is set (approval — see [`VacuousFilterGate`]).
fn vacuous_filter_findings(
    checks: &[CommandCheck],
    repo_root: &Path,
    filter_collision: bool,
) -> Vec<String> {
    let mut findings = Vec::new();
    for check in checks {
        let words = lex(&check.command);
        let segments = segments(&words);

        if segments.iter().any(|s| is_test_runner(&s.words)) {
            for segment in &segments {
                let Some(args) = grep_invocation_args(&segment.words) else {
                    continue;
                };
                let Some(patterns) = grep_patterns(args) else {
                    continue;
                };
                if !patterns.iter().any(|p| p.contains("[1-9]")) {
                    findings.push(format!(
                        "[{}] test-runner pipeline's grep anchors no nonzero count \
                         (missing the `[1-9]` guard — zero matching tests still print \
                         `test result: ok.`): `{}`",
                        check.id, check.command
                    ));
                }
            }
        }

        if !filter_collision {
            continue;
        }
        for segment in &segments {
            if let Some(filter) = cargo_test_filter(&segment.words) {
                if let Some(site) = find_test_name_collision(repo_root, &filter) {
                    findings.push(format!(
                        "[{}] cargo test filter `{filter}` collides with an existing \
                         test at {site} — the gate can pass on pre-existing tests with \
                         zero implementation; the filter must match ONLY the \
                         not-yet-written tests: `{}`",
                        check.id, check.command
                    ));
                }
            }
        }
    }
    findings
}

/// True when the segment invokes a recognized test runner. Conservative:
/// only the exact runner shapes count — anything else is not analyzed.
fn is_test_runner(words: &[Word]) -> bool {
    let Some(first) = words.first() else {
        return false;
    };
    match first.text.as_str() {
        "cargo" => {
            let rest: Vec<&str> = words[1..].iter().map(|w| w.text.as_str()).collect();
            rest.first() == Some(&"test")
                || (rest.first() == Some(&"nextest") && rest.get(1) == Some(&"run"))
        }
        "pytest" | "py.test" => true,
        "go" => words.get(1).is_some_and(|w| w.text == "test"),
        "npm" | "yarn" | "pnpm" => {
            let rest: Vec<&str> = words[1..].iter().map(|w| w.text.as_str()).collect();
            rest.first() == Some(&"test")
                || (rest.first() == Some(&"run") && rest.get(1) == Some(&"test"))
        }
        _ => false,
    }
}

/// Extract the `cargo test` filter substring from a segment, or None when
/// the segment is not a `cargo test` invocation or carries no filter.
/// Skips flags (and the values of flags that take one) and redirection
/// words (`2>&1`, `>out`) so none of those is mistaken for the filter.
fn cargo_test_filter(words: &[Word]) -> Option<String> {
    if words.first().map(|w| w.text.as_str()) != Some("cargo") {
        return None;
    }
    if words.get(1).map(|w| w.text.as_str()) != Some("test") {
        return None;
    }
    /// Cargo flags that consume the following word as their value.
    const VALUE_FLAGS: &[&str] = &[
        "-p",
        "--package",
        "--test",
        "--bench",
        "--bin",
        "--example",
        "--features",
        "--exclude",
        "--config",
        "--target",
        "--profile",
        "-j",
        "--jobs",
        "--manifest-path",
        "--message-format",
        "--target-dir",
    ];
    let mut iter = words[2..].iter().peekable();
    while let Some(word) = iter.next() {
        let text = word.text.as_str();
        if text == "--" {
            continue;
        }
        if text.contains('>') || text.contains('<') {
            continue;
        }
        if let Some(flag) = text.strip_prefix('-') {
            if !flag.is_empty() && !text.contains('=') && VALUE_FLAGS.contains(&text) {
                iter.next();
            }
            continue;
        }
        return Some(text.to_string());
    }
    None
}

/// First repo site where `filter` names an existing test fn: a `fn <ident>`
/// whose identifier contains `filter`, in a `.rs` file, with a
/// `#[test]`-family attribute in the lines just above it (so helper fns and
/// non-test code never collide). Returns a `path:line (fn ident)` label.
/// Dependency/build dirs (`target`, `node_modules`, hidden dirs) are
/// skipped; the walk never compiles anything.
fn find_test_name_collision(repo_root: &Path, filter: &str) -> Option<String> {
    let mut files = Vec::new();
    collect_rs_files(repo_root, &mut files);
    for file in files {
        let Ok(body) = std::fs::read_to_string(&file) else {
            continue;
        };
        let lines: Vec<&str> = body.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            let Some(ident) = fn_ident_after(line, "fn ") else {
                continue;
            };
            if !ident.contains(filter) {
                continue;
            }
            let window_start = i.saturating_sub(3);
            let is_test = lines[window_start..i].iter().any(|l| {
                let l = l.trim_start();
                l.starts_with("#[") && l.contains("test")
            });
            if is_test {
                let rel = file.strip_prefix(repo_root).unwrap_or(&file);
                return Some(format!("{}:{} (`fn {ident}`)", rel.display(), i + 1));
            }
        }
    }
    None
}

/// The identifier following the first `needle` (`fn `) in `line`, if any.
fn fn_ident_after<'a>(line: &'a str, needle: &str) -> Option<&'a str> {
    let start = line.find(needle)? + needle.len();
    let rest = &line[start..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// Recursive `.rs` file walk, sorted per directory for determinism.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "target" || name == "node_modules" || name.starts_with('.') {
                continue;
            }
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

// ---------------------------------------------------------------------------
// wrong-polarity
// ---------------------------------------------------------------------------

/// The `wrong-polarity` gate: negated/neutralized greps whose target path
/// is absent from the tree being checked (see the module docs).
struct WrongPolarityGate {
    checks: Vec<CommandCheck>,
    tree_root: PathBuf,
}

impl Gate for WrongPolarityGate {
    fn name(&self) -> &str {
        WRONG_POLARITY
    }
    fn kind(&self) -> GateKind {
        GateKind::Deterministic
    }
    fn evaluate(&self) -> GateOutcome {
        outcome_from_findings(
            WRONG_POLARITY,
            wrong_polarity_findings(&self.checks, &self.tree_root),
        )
    }
}

/// Per-assertion findings for the wrong-polarity class. A grep whose target
/// path does not exist exits non-zero for ABSENCE, so a negated
/// (`! grep -q pat path`) or neutralized (`grep -q pat path || true`) form
/// reports success without the property ever being examined. Positive
/// greps on missing paths fail loudly and are not this class; greps reading
/// stdin (piped) have no path to check; undeterminable paths (glob,
/// variable, tilde) are passed conservatively.
fn wrong_polarity_findings(checks: &[CommandCheck], tree_root: &Path) -> Vec<String> {
    let mut findings = Vec::new();
    for check in checks {
        let words = lex(&check.command);
        let segs = segments(&words);
        for (i, segment) in segs.iter().enumerate() {
            // Negation prefixes the command word: `! grep …` (separate word)
            // or the rarer glued `!grep …`. Strip it before parsing.
            let mut negated = false;
            let mut seg_words: Vec<Word> = segment.words.clone();
            if let Some(first) = seg_words.first() {
                if first.text == "!" {
                    negated = true;
                    seg_words.remove(0);
                } else if let Some(rest) = first.text.strip_prefix('!') {
                    if !rest.is_empty() {
                        negated = true;
                        seg_words[0] = Word {
                            text: rest.to_string(),
                            quote: first.quote.clone(),
                        };
                    }
                }
            }
            let Some(args) = grep_invocation_args(&seg_words) else {
                continue;
            };
            // `grep ... || true`: the grep's failure (including a missing
            // target) is swallowed into success.
            let or_true = segment.op_after.as_deref() == Some("||")
                && segs
                    .get(i + 1)
                    .is_some_and(|next| next.words.len() == 1 && next.words[0].text == "true");
            if !negated && !or_true {
                continue;
            }
            let Some(paths) = grep_target_paths(args) else {
                continue;
            };
            for path in paths {
                if path.chars().any(|c| "*?[]$~`(".contains(c)) {
                    continue;
                }
                let resolved = if Path::new(&path).is_absolute() {
                    PathBuf::from(&path)
                } else {
                    tree_root.join(&path)
                };
                if !resolved.exists() {
                    let form = if negated {
                        "negated"
                    } else {
                        "`|| true`-neutralized"
                    };
                    findings.push(format!(
                        "[{}] {form} grep targets missing path `{path}` — the assertion \
                         passes because the target is absent, not because the property \
                         holds: `{}`",
                        check.id, check.command
                    ));
                }
            }
        }
    }
    findings
}

// ---------------------------------------------------------------------------
// passes-on-base
// ---------------------------------------------------------------------------

/// The `passes-on-base` gate: the graduation of the lint's `PassedOnBase`
/// class into a typed verdict. Constructed from the approval-time lint
/// report; not registered at the final gate (see [`contract_gate_reports`]).
struct PassesOnBaseGate {
    results: Vec<AssertionLint>,
}

impl Gate for PassesOnBaseGate {
    fn name(&self) -> &str {
        PASSES_ON_BASE
    }
    fn kind(&self) -> GateKind {
        GateKind::Deterministic
    }
    fn evaluate(&self) -> GateOutcome {
        let findings = self
            .results
            .iter()
            .filter(|r| r.outcome == AssertionLintOutcome::PassedOnBase)
            .map(|r| {
                format!(
                    "[{}] already exits zero on the untouched base tree — a \
                     correctly-scoped \"the work landed\" assertion must FAIL \
                     before the work lands: `{}`",
                    r.id, r.command
                )
            })
            .collect();
        outcome_from_findings(PASSES_ON_BASE, findings)
    }
}

// ---------------------------------------------------------------------------
// env-sensitive
// ---------------------------------------------------------------------------

/// The `env-sensitive` gate: command text whose verdict can depend on the
/// environment rather than the tree (see the module docs).
struct EnvSensitiveGate {
    checks: Vec<CommandCheck>,
}

impl Gate for EnvSensitiveGate {
    fn name(&self) -> &str {
        ENV_SENSITIVE
    }
    fn kind(&self) -> GateKind {
        GateKind::Deterministic
    }
    fn evaluate(&self) -> GateOutcome {
        outcome_from_findings(ENV_SENSITIVE, env_sensitive_findings(&self.checks))
    }
}

/// Per-assertion findings for the env-sensitive class. Signals, all on the
/// command text: `$HOME`/`${HOME}` expansion (single-quoted text never
/// expands, so it is exempt), an unquoted `~`/`~user` word or `=~/`
/// assignment, an absolute user path (`/Users/…`, `/home/…`), a wall-clock
/// `date` invocation feeding a comparison (`-gt`, `==`, …), and `curl`/`wget`
/// in command position (unproxied network — verdicts must come from the
/// tree, and the egress proxy cannot see these at lint time).
fn env_sensitive_findings(checks: &[CommandCheck]) -> Vec<String> {
    const COMPARISONS: &[&str] = &["-lt", "-gt", "-le", "-ge", "-eq", "-ne", "==", "!="];
    let mut findings = Vec::new();
    for check in checks {
        let words = lex(&check.command);
        let mut reasons: Vec<&str> = Vec::new();

        if words.iter().any(|w| {
            w.quote != Quote::Single && (w.text.contains("$HOME") || w.text.contains("${HOME}"))
        }) {
            reasons.push(
                "references $HOME (a per-run scratch HOME makes the verdict environment-dependent)",
            );
        }
        if words
            .iter()
            .any(|w| w.quote == Quote::None && (w.text.starts_with('~') || w.text.contains("=~/")))
        {
            reasons.push("references `~` (tilde expands against the runner's HOME, not the tree)");
        }
        if words.iter().any(|w| {
            w.quote != Quote::Single && (w.text.contains("/Users/") || w.text.starts_with("/home/"))
        }) {
            reasons.push("references an absolute user path (/Users/… or /home/…)");
        }
        let has_date = words.iter().any(|w| invocation_word(&w.text) == "date");
        let has_comparison = words.iter().any(|w| COMPARISONS.contains(&w.text.as_str()));
        if has_date && has_comparison {
            reasons.push("compares wall-clock `date` output (verdict drifts with the clock)");
        }
        let segs = segments(&words);
        for segment in &segs {
            if let Some(first) = segment.words.first() {
                let invoked = invocation_word(&first.text);
                if invoked == "curl" || invoked == "wget" {
                    reasons.push("invokes an unproxied network tool (`curl`/`wget`) — the verdict depends on the network, not the tree");
                    break;
                }
            }
        }

        for reason in reasons {
            findings.push(format!("[{}] {reason}: `{}`", check.id, check.command));
        }
    }
    findings
}

/// Normalize a word that may carry invocation punctuation (`$(`, backticks,
/// a leading `!`) down to the bare command name it invokes.
fn invocation_word(text: &str) -> &str {
    text.trim_start_matches("$(")
        .trim_start_matches('!')
        .trim_matches('`')
}

// ---------------------------------------------------------------------------
// Minimal shell-word lexer
// ---------------------------------------------------------------------------
//
// WHY hand-rolled and minimal: the gates only need word boundaries, the
// pipeline/negation operators, and enough quote tracking to know whether
// `$HOME`/`~` would expand (single quotes suppress expansion; double quotes
// do not). No new dependency (AGENTS.md prefers std), no glob/var/command
// expansion — shapes the lexer cannot fully account for are passed
// conservatively by the detectors above.

/// How a word was quoted — decides whether `$VAR`/`~` would expand.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Quote {
    None,
    Single,
    Double,
}

/// One shell word: its literal text (quotes stripped) and how it was
/// quoted. Operator words (`|`, `||`, `&&`, `;`) carry `Quote::None`.
#[derive(Debug, Clone)]
struct Word {
    text: String,
    quote: Quote,
}

/// Split a command line into words. `&&`, `||`, `|`, and `;` become their
/// own words; a single `&` stays glued (so `2>&1` survives as one word).
/// Quotes are stripped and recorded on the word — the quote of the first
/// QUOTED character accumulated, since a word's opening quote closes before
/// the word does (`'$HOME'` must stay `Quote::Single`); backslash escapes
/// the next character outside single quotes.
fn lex(command: &str) -> Vec<Word> {
    let chars: Vec<char> = command.chars().collect();
    let mut words = Vec::new();
    let mut text = String::new();
    // The quote state at the current input position (toggles at quote
    // characters)…
    let mut quote = Quote::None;
    // …and the quote recorded for the word being accumulated: the state in
    // effect when its first quoted character was pushed.
    let mut word_quote = Quote::None;
    let mut i = 0;

    fn flush(words: &mut Vec<Word>, text: &mut String, word_quote: &mut Quote) {
        if !text.is_empty() {
            words.push(Word {
                text: std::mem::take(text),
                quote: std::mem::replace(word_quote, Quote::None),
            });
        }
    }

    while i < chars.len() {
        let c = chars[i];
        match c {
            '\'' if quote != Quote::Double => {
                if quote == Quote::Single {
                    quote = Quote::None;
                } else {
                    quote = Quote::Single;
                }
                i += 1;
            }
            '"' if quote != Quote::Single => {
                if quote == Quote::Double {
                    quote = Quote::None;
                } else {
                    quote = Quote::Double;
                }
                i += 1;
            }
            '\\' if quote != Quote::Single => {
                if let Some(next) = chars.get(i + 1) {
                    if word_quote == Quote::None {
                        word_quote = quote.clone();
                    }
                    text.push(*next);
                    i += 2;
                } else {
                    i += 1;
                }
            }
            c if c.is_whitespace() && quote == Quote::None => {
                flush(&mut words, &mut text, &mut word_quote);
                i += 1;
            }
            '|' if quote == Quote::None => {
                flush(&mut words, &mut text, &mut word_quote);
                if chars.get(i + 1) == Some(&'|') {
                    words.push(Word {
                        text: "||".to_string(),
                        quote: Quote::None,
                    });
                    i += 2;
                } else {
                    words.push(Word {
                        text: "|".to_string(),
                        quote: Quote::None,
                    });
                    i += 1;
                }
            }
            '&' if quote == Quote::None && chars.get(i + 1) == Some(&'&') => {
                flush(&mut words, &mut text, &mut word_quote);
                words.push(Word {
                    text: "&&".to_string(),
                    quote: Quote::None,
                });
                i += 2;
            }
            ';' if quote == Quote::None => {
                flush(&mut words, &mut text, &mut word_quote);
                words.push(Word {
                    text: ";".to_string(),
                    quote: Quote::None,
                });
                i += 1;
            }
            c => {
                if word_quote == Quote::None {
                    word_quote = quote.clone();
                }
                text.push(c);
                i += 1;
            }
        }
    }
    flush(&mut words, &mut text, &mut word_quote);
    words
}

/// One pipeline segment: the words between operators, plus the operator
/// that follows the segment (`|`, `||`, `&&`, `;`) so detectors can
/// recognize `grep … || true`.
struct Segment {
    words: Vec<Word>,
    op_after: Option<String>,
}

/// Split lexed words into segments at the operator words.
fn segments(words: &[Word]) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut current: Vec<Word> = Vec::new();
    let mut close = |current: &mut Vec<Word>, op: Option<String>| {
        if !current.is_empty() {
            out.push(Segment {
                words: std::mem::take(current),
                op_after: op,
            });
        }
    };
    for word in words {
        match word.text.as_str() {
            "|" | "||" | "&&" | ";" if word.quote == Quote::None => {
                close(&mut current, Some(word.text.clone()));
            }
            _ => current.push(word.clone()),
        }
    }
    close(&mut current, None);
    out
}

/// If the segment invokes grep (or egrep/fgrep), return its argument words
/// (everything after the command name); None otherwise.
fn grep_invocation_args(words: &[Word]) -> Option<&[Word]> {
    let first = words.first()?;
    matches!(first.text.as_str(), "grep" | "egrep" | "fgrep").then(|| &words[1..])
}

/// Grep long flags that consume the FOLLOWING word as their value (when not
/// given in `--flag=value` form) — the value must not be mistaken for the
/// pattern or a target path.
const GREP_LONG_VALUE_FLAGS: &[&str] = &[
    "--context",
    "--after-context",
    "--before-context",
    "--max-count",
    "--include",
    "--exclude",
    "--exclude-dir",
    "--label",
    "--binary-files",
];

/// The patterns a grep invocation searches for. Handles `-e <pat>` /
/// `--regexp=<pat>` / bundled `-qe <pat>` and the default first-non-flag
/// pattern; returns None when the pattern is undeterminable (e.g. `-f`
/// pattern-file), which every caller treats conservatively as a pass.
fn grep_patterns(args: &[Word]) -> Option<Vec<String>> {
    let mut patterns = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let text = args[i].text.as_str();
        if text == "-e" || text == "--regexp" {
            patterns.push(args.get(i + 1)?.text.clone());
            i += 2;
            continue;
        }
        if let Some(rest) = text.strip_prefix("--regexp=") {
            patterns.push(rest.to_string());
            i += 1;
            continue;
        }
        if text.starts_with('-') && !text.starts_with("--") && text.len() > 1 {
            let flags: Vec<char> = text[1..].chars().collect();
            // `-e` wins over a later `f`-looking letter: in a bundled
            // `-qefoo` the remainder after `e` IS the pattern (the `f`
            // belongs to "foo", not to `-f`).
            if let Some(pos) = flags.iter().position(|c| *c == 'e') {
                if pos + 1 < flags.len() {
                    patterns.push(flags[pos + 1..].iter().collect());
                    i += 1;
                } else {
                    patterns.push(args.get(i + 1)?.text.clone());
                    i += 2;
                }
                continue;
            }
            if flags.contains(&'f') {
                // -f reads patterns from a file: undeterminable statically.
                return None;
            }
            i += 1;
            continue;
        }
        if text.starts_with("--") {
            if GREP_LONG_VALUE_FLAGS.contains(&text) {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        // First non-flag word is the pattern.
        patterns.push(text.to_string());
        break;
    }
    (!patterns.is_empty()).then_some(patterns)
}

/// The path arguments a grep invocation searches (everything after the
/// pattern). None when grep reads stdin (no paths — piped use) or the
/// pattern is undeterminable.
fn grep_target_paths(args: &[Word]) -> Option<Vec<String>> {
    let mut paths = Vec::new();
    let mut i = 0;
    let mut pattern_consumed = false;
    while i < args.len() {
        let text = args[i].text.as_str();
        if text == "-e" || text == "--regexp" {
            pattern_consumed = true;
            i += 2;
            continue;
        }
        if text.starts_with("--regexp=") {
            pattern_consumed = true;
            i += 1;
            continue;
        }
        if text.starts_with('-') && !text.starts_with("--") && text.len() > 1 {
            let flags: Vec<char> = text[1..].chars().collect();
            // `-e` before `f`, as in grep_patterns: `-qefoo`'s `f` is part
            // of the bundled pattern, not the `-f` flag.
            if let Some(pos) = flags.iter().position(|c| *c == 'e') {
                pattern_consumed = true;
                if pos + 1 == flags.len() {
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if flags.contains(&'f') {
                return None;
            }
            i += 1;
            continue;
        }
        if text.starts_with("--") {
            if GREP_LONG_VALUE_FLAGS.contains(&text) {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if !pattern_consumed {
            pattern_consumed = true;
        } else {
            paths.push(text.to_string());
        }
        i += 1;
    }
    Some(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract_lint::AssertionLint;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/engine has a workspace root two levels up")
            .to_path_buf()
    }

    fn check(id: &str, command: &str) -> CommandCheck {
        CommandCheck {
            id: id.to_string(),
            command: command.to_string(),
        }
    }

    fn command_assertion(id: &str, command: &str) -> Assertion {
        Assertion {
            id: id.to_string(),
            statement: format!("statement for {id}"),
            check: AssertionCheck::Command,
            command: Some(command.to_string()),
        }
    }

    fn lint_result(id: &str, command: &str, outcome: AssertionLintOutcome) -> AssertionLint {
        AssertionLint {
            id: id.to_string(),
            command: command.to_string(),
            outcome,
            output_tail: String::new(),
        }
    }

    // ---- vacuous-filter -------------------------------------------------

    /// The documented escape shape (AGENTS.md rule 5): `grep -q
    /// 'test result: ok'` passes on `0 passed` — the pipeline MUST anchor a
    /// nonzero count.
    #[test]
    fn contract_gate_vacuous_filter_catches_unanchored_test_result_grep() {
        let checks = vec![check(
            "a3",
            "cargo test --workspace zz_contract_gate_no_such_filter 2>&1 | grep -qE 'test result: ok\\.'",
        )];
        let findings = vacuous_filter_findings(&checks, &workspace_root(), true);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(findings[0].contains("[a3]"), "{findings:?}");
        assert!(findings[0].contains("[1-9]"), "{findings:?}");
    }

    /// The same pipeline WITH the prescribed `[1-9]` guard must pass.
    #[test]
    fn contract_gate_vacuous_filter_passes_anchored_nonzero_grep() {
        let checks = vec![check(
            "a3",
            "cargo test --workspace zz_contract_gate_no_such_filter 2>&1 | grep -qE 'test result: ok\\. [1-9]'",
        )];
        let findings = vacuous_filter_findings(&checks, &workspace_root(), true);
        assert!(findings.is_empty(), "{findings:?}");
    }

    /// The m-66aff8 a3 incident: the filter substring (`refus` there) names
    /// a PRE-EXISTING test, so the gate greens with zero implementation.
    /// Here the filter names the contract_lint.rs `approval_lint_*` tests.
    #[test]
    fn contract_gate_vacuous_filter_catches_filter_colliding_with_existing_test() {
        let checks = vec![check(
            "a3",
            "cargo test --workspace approval_lint_ 2>&1 | grep -qE 'test result: ok\\. [1-9]'",
        )];
        let findings = vacuous_filter_findings(&checks, &workspace_root(), true);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(findings[0].contains("collides"), "{findings:?}");
        assert!(findings[0].contains("approval_lint_"), "{findings:?}");
    }

    /// A fresh filter (no existing test names contain it) must pass the
    /// collision check even though the pipeline itself is well-formed.
    #[test]
    fn contract_gate_vacuous_filter_passes_fresh_filter() {
        let checks = vec![check(
            "a3",
            "cargo test --workspace zz_contract_gate_no_such_filter 2>&1 | grep -qE 'test result: ok\\. [1-9]'",
        )];
        let findings = vacuous_filter_findings(&checks, &workspace_root(), true);
        assert!(findings.is_empty(), "{findings:?}");
    }

    /// Filter extraction must skip flags and redirections: in
    /// `cargo test --workspace approval_lint_ 2>&1 | …` the filter is
    /// `approval_lint_`, not `2>&1` or `--workspace`.
    #[test]
    fn contract_gate_vacuous_filter_extracts_filter_past_flags_and_redirects() {
        let words = lex("cargo test --workspace approval_lint_ 2>&1 | grep -q x");
        let segs = segments(&words);
        assert_eq!(
            cargo_test_filter(&segs[0].words).as_deref(),
            Some("approval_lint_")
        );
        // Bare `cargo test --workspace` (the repo's own merge gate) has no
        // filter to collide.
        let words = lex("cargo test --workspace");
        let segs = segments(&words);
        assert_eq!(cargo_test_filter(&segs[0].words), None);
    }

    /// At the final gate the work HAS landed: a well-formed filter now
    /// collides with its own (just-landed) tests by design, so the
    /// collision signal is disabled (`filter_collision: false`). The
    /// unanchored-grep signal is phase-independent and still runs there.
    #[test]
    fn contract_gate_vacuous_filter_skips_collision_at_final_gate_phase() {
        let colliding = vec![check(
            "a3",
            "cargo test --workspace approval_lint_ 2>&1 | grep -qE 'test result: ok\\. [1-9]'",
        )];
        let findings = vacuous_filter_findings(&colliding, &workspace_root(), false);
        assert!(findings.is_empty(), "{findings:?}");

        // …but an unanchored grep is still caught in the final-gate phase.
        let unanchored = vec![check(
            "a3",
            "cargo test --workspace zz_contract_gate_no_such_filter 2>&1 | grep -qE 'test result: ok\\.'",
        )];
        let findings = vacuous_filter_findings(&unanchored, &workspace_root(), false);
        assert_eq!(findings.len(), 1, "{findings:?}");
    }

    // ---- wrong-polarity -------------------------------------------------

    /// `! grep -q pat <missing>` passes because grep errors on absence —
    /// the assertion never examined the property.
    #[test]
    fn contract_gate_wrong_polarity_catches_negated_grep_on_missing_path() {
        let dir = std::env::temp_dir().join(format!("kranz-cg-wp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let checks = vec![
            check("a1", "! grep -q landed-marker no/such/file.txt"),
            check("a2", "grep -q landed-marker no/such/file.txt || true"),
        ];
        let findings = wrong_polarity_findings(&checks, &dir);
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert!(findings[0].contains("[a1]"), "{findings:?}");
        assert!(findings[0].contains("negated"), "{findings:?}");
        assert!(findings[1].contains("[a2]"), "{findings:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Well-formed negated greps must pass: the target exists (the property
    /// IS examined), the grep is positive (fails loudly on absence), the
    /// path is undeterminable (glob — conservative pass), or grep reads
    /// stdin.
    #[test]
    fn contract_gate_wrong_polarity_passes_examined_or_undeterminable_targets() {
        let dir = std::env::temp_dir().join(format!("kranz-cg-wp2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("present.txt"), "contents\n").unwrap();
        let checks = vec![
            // Negated grep on an EXISTING path: the property is checked.
            check("a1", "! grep -q forbidden-marker present.txt"),
            // Positive grep on a missing path: fails loudly, not this class.
            check("a2", "grep -q landed-marker no/such/file.txt"),
            // Undeterminable path (glob): conservative pass.
            check("a3", "! grep -q marker no/such/*.txt"),
            // Piped grep reads stdin: no path to check.
            check("a4", "cargo build 2>&1 | grep -q warning"),
        ];
        let findings = wrong_polarity_findings(&checks, &dir);
        assert!(findings.is_empty(), "{findings:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- passes-on-base -------------------------------------------------

    /// The graduated lint class: `PassedOnBase` fails the gate; the benign
    /// `FailedOnBase` and the separate `CouldNotVerdict` lint class pass it.
    #[test]
    fn contract_gate_passes_on_base_fails_only_on_base_pass() {
        let gate = PassesOnBaseGate {
            results: vec![
                lint_result("a1", "true", AssertionLintOutcome::PassedOnBase),
                lint_result("a2", "false", AssertionLintOutcome::FailedOnBase),
                lint_result("a3", "sleep 99", AssertionLintOutcome::CouldNotVerdict),
            ],
        };
        let outcome = gate.evaluate();
        assert!(!outcome.passed());
        let detail = outcome.artefact.detail.expect("fail carries findings");
        assert!(detail.contains("[a1]"), "{detail}");
        assert!(detail.contains("untouched base tree"), "{detail}");
        assert!(!detail.contains("[a2]"), "{detail}");
        assert!(!detail.contains("[a3]"), "{detail}");

        let clean = PassesOnBaseGate {
            results: vec![lint_result(
                "a2",
                "false",
                AssertionLintOutcome::FailedOnBase,
            )],
        };
        assert!(clean.evaluate().passed());
    }

    // ---- env-sensitive --------------------------------------------------

    /// Each documented environment signal fails the gate with its reason.
    #[test]
    fn contract_gate_env_sensitive_catches_home_tilde_userpath_date_network() {
        let checks = vec![
            check("a1", "cat $HOME/.config/tool.toml | grep -q enabled"),
            check("a2", "grep -q marker ~/output.txt"),
            check("a3", "/Users/alice/bin/tool --check"),
            check("a4", "test $(date +%s) -gt 1700000000"),
            check("a5", "curl -fsS https://example.com/health | grep -q ok"),
        ];
        let findings = env_sensitive_findings(&checks);
        assert_eq!(findings.len(), 5, "{findings:?}");
        assert!(findings.iter().any(|f| f.contains("$HOME")), "{findings:?}");
        assert!(findings.iter().any(|f| f.contains("~")), "{findings:?}");
        assert!(
            findings.iter().any(|f| f.contains("absolute user path")),
            "{findings:?}"
        );
        assert!(
            findings.iter().any(|f| f.contains("wall-clock")),
            "{findings:?}"
        );
        assert!(
            findings.iter().any(|f| f.contains("network tool")),
            "{findings:?}"
        );
    }

    /// Well-formed commands must pass: single-quoted `$HOME` (never
    /// expands), a grep PATTERN containing `curl` (a literal, not an
    /// invocation), `date` without a comparison (no verdict drift), and
    /// ordinary tree-relative checks.
    #[test]
    fn contract_gate_env_sensitive_passes_well_formed_commands() {
        let checks = vec![
            check("a1", "grep -q '$HOME' src/config.rs"),
            check("a2", "grep -q 'curl' docs/api.md"),
            check("a3", "date"),
            check("a4", "cargo test --workspace"),
            check("a5", "test -f .kranz/merge-gates.json"),
        ];
        let findings = env_sensitive_findings(&checks);
        assert!(findings.is_empty(), "{findings:?}");
    }

    // ---- pipeline + regression ------------------------------------------

    /// The pipeline registers the four gates in ticket order (passes-on-base
    /// only with a lint report), each report carries its defect-class name,
    /// and the rendered verdict block names the failed classes.
    #[test]
    fn contract_gate_pipeline_names_classes_in_ticket_order() {
        let contract = vec![
            command_assertion(
                "a1",
                "cargo test --workspace zz_contract_gate_no_such_filter 2>&1 | grep -q 'test result: ok'",
            ),
            command_assertion("a2", "true"),
        ];
        let lint = ContractLintReport {
            results: vec![
                lint_result("a1", "cargo test …", AssertionLintOutcome::FailedOnBase),
                lint_result("a2", "true", AssertionLintOutcome::PassedOnBase),
            ],
            tree_clean_at_base: true,
        };
        let reports = contract_gate_reports(&contract, Some(&lint), &workspace_root());
        let names: Vec<&str> = reports.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                VACUOUS_FILTER,
                WRONG_POLARITY,
                PASSES_ON_BASE,
                ENV_SENSITIVE
            ]
        );
        assert!(reports.iter().all(|r| r.kind == GateKind::Deterministic));

        let failed = failed_gate_names(&reports);
        assert_eq!(failed, vec![VACUOUS_FILTER, PASSES_ON_BASE]);

        let rendered = render_gate_verdicts(&reports);
        assert!(rendered.contains("vacuous-filter: FAIL"), "{rendered}");
        assert!(rendered.contains("wrong-polarity: PASS"), "{rendered}");
        assert!(rendered.contains("passes-on-base: FAIL"), "{rendered}");
        assert!(rendered.contains("env-sensitive: PASS"), "{rendered}");

        // Without a lint report (the final-gate surface) passes-on-base is
        // not registered.
        let reports = contract_gate_reports(&contract, None, &workspace_root());
        let names: Vec<&str> = reports.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec![VACUOUS_FILTER, WRONG_POLARITY, ENV_SENSITIVE]);

        // No command assertions → no gates, mirroring the lint's is_empty.
        assert!(contract_gate_reports(&[], None, &workspace_root()).is_empty());
    }

    /// Regression: the repo's own `.kranz/merge-gates.json` commands are
    /// well-formed — every one must pass the static gates unchanged.
    #[test]
    fn contract_gate_well_formed_merge_gates_pass_all_static_gates() {
        let root = workspace_root();
        let bytes = std::fs::read(root.join(crate::merge_gate::MERGE_GATES_PATH))
            .expect("repo merge-gates.json readable");
        let suite = crate::merge_gate::parse_gate_suite(&bytes).expect("suite parses");
        let contract: Vec<Assertion> = suite
            .gates
            .iter()
            .enumerate()
            .map(|(i, g)| command_assertion(&format!("g{}", i + 1), &g.command))
            .collect();
        let reports = contract_gate_reports(&contract, None, &root);
        assert_eq!(reports.len(), 3, "{reports:?}");
        let failed = failed_gate_names(&reports);
        assert!(
            failed.is_empty(),
            "repo merge gates must pass the static contract gates: {failed:?}\n{}",
            render_gate_verdicts(&reports)
        );
    }

    /// Regression: the command fixtures the existing contract_lint tests
    /// exercise (`true`/`false`, the m-0c885b `grep -L` shape, the
    /// secret-scan probe) are all well-formed for the STATIC gates — the
    /// `grep -L` polarity bug is the passes-on-base class's job, not a
    /// static signal.
    #[test]
    fn contract_gate_existing_lint_fixtures_pass_static_gates() {
        let root = workspace_root();
        let contract = vec![
            command_assertion("a1", "true"),
            command_assertion("a2", "false"),
            command_assertion("a3", "sleep 5"),
            command_assertion("a6", r#"grep -L '^name = "tokio"' Cargo.lock"#),
            command_assertion(
                "a7",
                "test -z \"$GH_TOKEN\" && env | grep -c hunter2-lint | grep -q '^0$'",
            ),
        ];
        let reports = contract_gate_reports(&contract, None, &root);
        let failed = failed_gate_names(&reports);
        assert!(
            failed.is_empty(),
            "existing lint fixtures must pass the static gates: {failed:?}\n{}",
            render_gate_verdicts(&reports)
        );
    }
}
