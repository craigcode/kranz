//! Judgement turns — extracted from `orchestrator.rs` in the monolith split
//! (pure code motion, no behavior change). The post-run worker judgement
//! (§4.5 f), the final contract gate's verdicts turn, and the cross-mission
//! lesson capture at mission completion — all running through the shared
//! lenient-parse JSON decision turn ([`MissionEngine::json_decision`]) that
//! the orchestrator's unblock / dirty-tree / parallel / fix-features turns
//! also call.

use crate::error::Result;
use crate::gate::{ArtefactRef, GateKind, GateOutcome, GateReport};
use crate::git_ops::GitRepo;
use crate::lessons;
use crate::orchestrator::{
    first_nonempty_line, run_outcome_summary, MissionEngine, JSON_RETRY_MSG,
};
use crate::runner;
use crate::types::*;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// JSON decision shapes (parsed leniently via runner::parse_report)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct JudgementDecision {
    decision: String,
    #[serde(default)]
    guidance: String,
    #[serde(default)]
    summary: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Verdict {
    id: String,
    pass: bool,
    #[serde(default)]
    evidence: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VerdictsDecision {
    #[serde(default)]
    verdicts: Vec<Verdict>,
    #[serde(default)]
    summary: String,
}

/// What the judgement turn decided for a worker run.
pub(crate) enum JudgementOutcome {
    Complete,
    Failed(String),
    /// Respawn with this guidance (budget enforced by the caller).
    Respawn(String),
}

impl MissionEngine {
    // -----------------------------------------------------------------------
    // Post-run judgement (§4.5 f) + final-gate verdicts
    // -----------------------------------------------------------------------

    /// Post-run judgement turn (§4.5 f): report + commits + diff stat →
    /// JSON `{decision, guidance, summary}`. Unparseable after retry →
    /// conservative default: respawn-if-budget-else-fail (mapped to Respawn
    /// here; the caller enforces the budget).
    pub(crate) async fn judge_worker_run(
        &mut self,
        feature_id: &str,
        outcome: &runner::RunOutcome,
        commits: &[String],
        diff_stat: &str,
    ) -> Result<JudgementOutcome> {
        let runner_summary = run_outcome_summary(outcome);
        if outcome.result != RunResult::Pass {
            let summary = format!("worker run not trusted: {runner_summary}");
            self.emit_decision(&format!("judgement for {feature_id}: {summary}"), None)?;
            return Ok(JudgementOutcome::Respawn(format!(
                "{summary}. Re-run the feature; do not rely on the previous worker report."
            )));
        }

        let report_text = match outcome.report.as_ref() {
            Some(r) => serde_json::to_string_pretty(r)?,
            None => "NO REPORT — treat sceptically".to_string(),
        };
        let commits_text = if commits.is_empty() {
            "(none)".to_string()
        } else {
            commits
                .iter()
                .map(|c| format!("- {c}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let message = format!(
            "A worker run for feature {feature_id} just finished. Judge it.\n\n\
             RUNNER VERDICT:\n{runner_summary}\n\n\
             WORKER REPORT:\n{report_text}\n\n\
             COMMITS THIS RUN:\n{commits_text}\n\n\
             DIFF STAT:\n{diff_stat}\n\n\
             Respond with ONLY this JSON:\n\
             {{\"decision\":\"complete\"|\"failed\"|\"respawn\",\"guidance\":\"string\",\"summary\":\"string\"}}"
        );
        let (decision, text) = self.json_decision::<JudgementDecision>(&message).await?;

        let (verdict, guidance, summary) = match decision {
            Some(d) => {
                let verdict = d.decision.trim().to_ascii_lowercase();
                let summary = if d.summary.is_empty() {
                    verdict.clone()
                } else {
                    d.summary
                };
                (verdict, d.guidance, summary)
            }
            None => (
                // Conservative default (documented): respawn while budget
                // remains, else fail — never silently complete.
                "respawn".to_string(),
                "previous judgement was unparseable; re-attempt the feature and produce \
                 a clear worker report"
                    .to_string(),
                "judgement unparseable; conservative default (respawn/fail)".to_string(),
            ),
        };
        self.emit_decision(
            &format!("judgement for {feature_id}: {summary}"),
            Some(text),
        )?;

        Ok(match verdict.as_str() {
            "complete" => JudgementOutcome::Complete,
            "failed" | "fail" => JudgementOutcome::Failed(summary),
            // "respawn" and anything unrecognized take the conservative path.
            _ => JudgementOutcome::Respawn(if guidance.is_empty() {
                summary
            } else {
                guidance
            }),
        })
    }

    /// One verdicts turn for all agent-judgement assertions. Unparseable
    /// (after retry) or missing verdicts fail conservatively — a gate that
    /// cannot be verified must not pass.
    pub(crate) async fn judge_contract_assertions(
        &mut self,
        assertions: &[&Assertion],
    ) -> Result<Vec<Finding>> {
        let listed = assertions
            .iter()
            .map(|a| format!("- [{}] {}", a.id, a.statement))
            .collect::<Vec<_>>()
            .join("\n");
        // Diff against the base pinned at approval, never the live base branch
        // (a base that advanced mid-mission would silently change the final
        // judgement's diff). See judge_diff_base / judge_gate_diff_uses_pinned_base_sha.
        let base = judge_diff_base(
            self.state.mission.base_sha.as_deref(),
            &self.state.mission.base_branch,
        );
        let diff_stat = self
            .active_repo()
            .diff_stat(&base, "HEAD")
            .unwrap_or_default();
        let message = format!(
            "Final contract gate. Verify each of these agent-judgement assertions against \
             the mission's work (diff stat of {base}..HEAD below). Inspect the repository \
             read-only as needed.\n\nASSERTIONS:\n{listed}\n\nDIFF STAT:\n{diff_stat}\n\n\
             Respond with ONLY this JSON:\n\
             {{\"verdicts\":[{{\"id\":\"string\",\"pass\":true,\"evidence\":\"string\"}}],\"summary\":\"string\"}}"
        );
        let (decision, text) = self.json_decision::<VerdictsDecision>(&message).await?;

        let mut findings = Vec::new();
        let summary = match decision {
            Some(d) => {
                for assertion in assertions {
                    match d.verdicts.iter().find(|v| v.id == assertion.id) {
                        Some(v) if v.pass => {}
                        Some(v) => findings.push(Finding {
                            subject: assertion.id.clone(),
                            severity: "critical".to_string(),
                            evidence: if v.evidence.is_empty() {
                                "orchestrator judged the assertion failed".to_string()
                            } else {
                                v.evidence.clone()
                            },
                            suggested_fix: String::new(),
                            class: String::new(),
                            rule: None,
                        }),
                        None => findings.push(Finding {
                            subject: assertion.id.clone(),
                            severity: "critical".to_string(),
                            evidence: "no verdict returned for this assertion".to_string(),
                            suggested_fix: String::new(),
                            class: String::new(),
                            rule: None,
                        }),
                    }
                }
                d.summary
            }
            None => {
                for assertion in assertions {
                    findings.push(Finding {
                        subject: assertion.id.clone(),
                        severity: "critical".to_string(),
                        evidence: "verdict turn unparseable; assertion could not be verified"
                            .to_string(),
                        suggested_fix: String::new(),
                        class: String::new(),
                        rule: None,
                    });
                }
                "unparseable verdicts; all judgement assertions failed conservatively".to_string()
            }
        };
        self.emit_decision(&format!("final gate verdicts: {summary}"), Some(text))?;
        Ok(findings)
    }

    /// One contextual Flight Rules verdict per pinned rule. The engine owns
    /// the checker prompt and validates cardinality strictly: a missing OR
    /// duplicate id is a failing gate outcome, never a pass by omission or
    /// "first verdict wins". Returned reports are model-judged gate entries
    /// and carry their exact rule id for the coverage fold.
    pub(crate) async fn judge_standards_rules(
        &mut self,
        rules: &[PinnedRule],
    ) -> Result<Vec<GateReport>> {
        if rules.is_empty() {
            return Ok(Vec::new());
        }
        let listed = rules
            .iter()
            .map(|rule| {
                format!(
                    "- [{} r{}; {}; {}] {}",
                    rule.id, rule.revision, rule.effective_status, rule.level, rule.statement
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let base = judge_diff_base(
            self.state.mission.base_sha.as_deref(),
            &self.state.mission.base_branch,
        );
        let diff_stat = self
            .active_repo()
            .diff_stat(&base, "HEAD")
            .unwrap_or_default();
        let message = format!(
            "Flight Rules contextual final checker. Judge EVERY listed rule against the full \
             repository diff {base}..HEAD and current tree. Return exactly one verdict for each \
             listed id and no duplicate ids. The verdict is authoritative; do not infer policy \
             from prose outside these pinned statements.\n\nRULES:\n{listed}\n\nDIFF STAT:\n{diff_stat}\n\n\
             Respond with ONLY this JSON:\n\
             {{\"verdicts\":[{{\"id\":\"string\",\"pass\":true,\"evidence\":\"string\"}}],\"summary\":\"string\"}}"
        );
        let (decision, text) = self.json_decision::<VerdictsDecision>(&message).await?;
        let (reports, summary) = match decision {
            Some(decision) => {
                let reports = strict_standards_reports(rules, Some(&decision.verdicts));
                (reports, decision.summary)
            }
            None => (
                strict_standards_reports(rules, None),
                "unparseable contextual standards verdict; all rules failed closed".to_string(),
            ),
        };
        self.emit_decision(
            &format!("Flight Rules contextual checker: {summary}"),
            Some(text),
        )?;
        Ok(reports)
    }

    // -----------------------------------------------------------------------
    // Cross-mission lesson capture
    // -----------------------------------------------------------------------

    /// One final orchestrator turn at mission completion: distill at most one
    /// reusable lesson for a future mission in this repo, write it to
    /// `.kranz/lessons/<mission-id>.md`, and append it to the lesson index.
    ///
    /// Best-effort BY DESIGN, same contract as [`Self::write_mission_report`]:
    /// the orchestrator only PRODUCES the lesson text — this engine method is
    /// the one that writes files — and any turn/parse/write failure is
    /// downgraded to a warning rather than stranding a mission that already
    /// passed its final gate. Returns the paths written (lesson file, index),
    /// or `None` if there was nothing worth carrying forward or capture
    /// failed.
    pub async fn capture_lesson(&mut self) -> Option<Vec<PathBuf>> {
        match self.try_capture_lesson().await {
            Ok(written) => written,
            Err(e) => {
                tracing::warn!(error = %e, "lesson capture failed; completing without a lesson");
                None
            }
        }
    }

    /// Fallible body of [`Self::capture_lesson`].
    async fn try_capture_lesson(&mut self) -> Result<Option<Vec<PathBuf>>> {
        let message = format!(
            "MISSION GOAL:\n{}\n\nThe mission has just completed. Distill at most ONE \
             reusable lesson that a FUTURE mission in THIS repository would need — as a \
             short imperative note — or reply with the single word NONE if there is \
             nothing worth carrying forward.",
            self.state.mission.goal
        );
        let text = self.orch_turn(&message).await?;
        let trimmed = text.trim();
        if trimmed.is_empty() || is_none_reply(trimmed) {
            return Ok(None);
        }

        // Written under active_paths (the integration worktree in worktree
        // mode) because these files are folded into write_mission_report's
        // commit, which commits via active_repo — see that method's doc note.
        let active_paths = self.active_paths();
        let mission_id = self.state.mission.id.clone();
        let body = normalize_lesson_body(trimmed);
        Ok(Some(lessons::write_lesson(
            &active_paths.repo_root,
            &mission_id,
            &body,
        )?))
    }

    // -----------------------------------------------------------------------
    // Shared JSON decision turn
    // -----------------------------------------------------------------------

    /// One JSON decision turn: send, parse leniently, retry once demanding
    /// bare JSON. Returns the parsed value (None = caller applies its
    /// conservative default) plus the raw text of the last reply.
    pub(crate) async fn json_decision<T: DeserializeOwned>(
        &mut self,
        message: &str,
    ) -> Result<(Option<T>, String)> {
        let text = self.orch_turn(message).await?;
        if let Some(parsed) = runner::parse_report::<T>(&text) {
            return Ok((Some(parsed), text));
        }
        let retry = self.orch_turn(JSON_RETRY_MSG).await?;
        match runner::parse_report::<T>(&retry) {
            Some(parsed) => Ok((Some(parsed), retry)),
            None => Ok((None, retry)),
        }
    }
}

fn strict_standards_reports(rules: &[PinnedRule], verdicts: Option<&[Verdict]>) -> Vec<GateReport> {
    rules
        .iter()
        .map(|rule| {
            let matching: Vec<&Verdict> = verdicts
                .unwrap_or_default()
                .iter()
                .filter(|verdict| verdict.id == rule.id)
                .collect();
            let (pass, detail) = match matching.as_slice() {
                [verdict] if verdict.pass => (true, verdict.evidence.clone()),
                [verdict] => (
                    false,
                    if verdict.evidence.is_empty() {
                        "contextual checker judged the rule failed".to_string()
                    } else {
                        verdict.evidence.clone()
                    },
                ),
                [] => (
                    false,
                    "contextual checker returned no verdict for this rule".to_string(),
                ),
                _ => (
                    false,
                    format!(
                        "contextual checker returned {} duplicate verdicts for this rule",
                        matching.len()
                    ),
                ),
            };
            let artefact =
                ArtefactRef::new(format!("contextual Flight Rules verdict for {}", rule.id));
            let outcome = if pass {
                GateOutcome::pass(if detail.is_empty() {
                    artefact
                } else {
                    artefact.with_detail(detail)
                })
            } else {
                GateOutcome::fail(artefact.with_detail(detail))
            }
            .with_rule_ids(vec![rule.id.clone()]);
            GateReport {
                name: format!("standards-agent:{}", rule.id),
                kind: GateKind::ModelJudged,
                outcome,
            }
        })
        .collect()
}

/// Which base ref the final gate's agent-judgement turn diffs against: the base
/// pinned at approval (`base_sha`), never the moving base branch — a base that
/// advanced mid-mission would silently change the judge's diff (same
/// never-re-resolve rule as the command env's `KRANZ_BASE_SHA`). Falls back to
/// `base_branch` only for legacy missions with no pinned sha.
fn judge_diff_base(base_sha: Option<&str>, base_branch: &str) -> String {
    match base_sha {
        Some(sha) if !sha.is_empty() => sha.to_string(),
        _ => base_branch.to_string(),
    }
}

/// Whether a lesson file (`<id>.md`) was legitimately produced by the engine:
/// added — in the current branch's reachable history — by a `[kranz] mission
/// report` commit whose `Kranz-Mission` trailer equals the file's mission id.
///
/// Rejects the two "outside the engine's commit flow" cases the manifest must
/// exclude: an untracked file dropped into `.kranz/lessons/` (no adding
/// commit → `None`), and a worker feature-commit (subject/trailer mismatch). A
/// perfectly forged report commit is separately caught by the contract sweep —
/// a lesson path is not mission-record, so a spoofed-subject commit touching
/// one is still swept as out-of-contract in its own mission.
pub(crate) fn lesson_provenance_clean(repo: &GitRepo, filename: &str) -> bool {
    let Some(id) = filename.strip_suffix(".md") else {
        return false;
    };
    if id.is_empty() {
        return false;
    }
    let rel = format!(".kranz/lessons/{filename}");
    let Ok(Some(add)) = repo.commit_that_added(&rel) else {
        return false;
    };
    let trailer = format!("Kranz-Mission: {id}");
    add.subject
        .trim_start()
        .starts_with("[kranz] mission report")
        && add.body.lines().any(|line| line.trim() == trailer)
}

/// Whether a lesson-turn reply is the single word NONE (case-insensitive,
/// ignoring surrounding whitespace/punctuation).
fn is_none_reply(text: &str) -> bool {
    text.trim()
        .trim_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation())
        .eq_ignore_ascii_case("none")
}

/// Cap on the lesson index-entry line, so a long paragraph reply can't
/// masquerade as the one-line summary read by the injection step.
const LESSON_SUMMARY_CAP: usize = 140;

/// Normalize a lesson reply so its first line is a short, usable one-line
/// summary (the injection step reads the first line as the index entry).
/// If the reply's first line already fits under the cap it is left as-is;
/// otherwise a capped version of it is prepended as a new first line, ahead
/// of the reply's full prose.
fn normalize_lesson_body(text: &str) -> String {
    let trimmed = text.trim();
    let first_line = first_nonempty_line(trimmed);
    if first_line.chars().count() <= LESSON_SUMMARY_CAP {
        return trimmed.to_string();
    }
    let capped: String = first_line.chars().take(LESSON_SUMMARY_CAP - 1).collect();
    format!("{capped}…\n\n{trimmed}")
}

// ---------------------------------------------------------------------------
// Test support — shared with orchestrator.rs's tests
// ---------------------------------------------------------------------------

/// One streaming orchestrator script: session init + one turn reply. Lives
/// outside `mod tests` because `findings.rs`'s convert_findings and
/// `orchestrator.rs`'s planning-seed / codex-fallback tests script their
/// orchestrator turns with it too.
#[cfg(test)]
pub(crate) fn lesson_orch_script(reply: &str) -> crate::backend_mock::MockScript {
    use crate::backend_mock::{mock_init, mock_result_text, mock_text};
    crate::backend_mock::MockScript::streaming(vec![
        mock_init("orch-session"),
        mock_result_text("ready"),
    ])
    .responding(vec![vec![mock_text(reply), mock_result_text(reply)]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::AgentBackend;
    use crate::orchestrator::tests::lessons_test_repo;
    use std::sync::Arc;

    fn contextual_rule(id: &str) -> PinnedRule {
        PinnedRule {
            id: id.to_string(),
            revision: 1,
            rfc: "RFC-001".to_string(),
            level: "must".to_string(),
            effective_status: "enforced".to_string(),
            statement: "Review the full change.".to_string(),
            domains: Vec::new(),
            stages: vec!["validation".to_string()],
            when_paths: Vec::new(),
            task_classes: Vec::new(),
            checker: Some("agent-judgement".to_string()),
            waivable: false,
        }
    }

    #[test]
    fn flight_rules_enforcement_contextual_missing_and_duplicate_fail_closed() {
        let rules = vec![contextual_rule("ZZ-CONTEXT-001")];
        let missing = strict_standards_reports(&rules, Some(&[]));
        assert_eq!(missing.len(), 1);
        assert!(!missing[0].outcome.passed());
        assert!(missing[0]
            .outcome
            .artefact
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("no verdict")));

        let duplicates = [
            Verdict {
                id: "ZZ-CONTEXT-001".to_string(),
                pass: true,
                evidence: "first".to_string(),
            },
            Verdict {
                id: "ZZ-CONTEXT-001".to_string(),
                pass: true,
                evidence: "second".to_string(),
            },
        ];
        let duplicate = strict_standards_reports(&rules, Some(&duplicates));
        assert_eq!(duplicate.len(), 1);
        assert!(!duplicate[0].outcome.passed());
        assert!(duplicate[0]
            .outcome
            .artefact
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("duplicate")));
    }

    #[test]
    fn flight_rules_enforcement_contextual_emits_exactly_one_linked_report_per_rule() {
        let rules = vec![
            contextual_rule("ZZ-CONTEXT-001"),
            contextual_rule("ZZ-CONTEXT-002"),
        ];
        let verdicts = [
            Verdict {
                id: "ZZ-CONTEXT-002".to_string(),
                pass: false,
                evidence: "unsafe behavior remains".to_string(),
            },
            Verdict {
                id: "ZZ-CONTEXT-001".to_string(),
                pass: true,
                evidence: "reviewed".to_string(),
            },
            Verdict {
                id: "unrequested".to_string(),
                pass: true,
                evidence: String::new(),
            },
        ];
        let reports = strict_standards_reports(&rules, Some(&verdicts));
        assert_eq!(reports.len(), 2);
        assert_eq!(
            reports[0].outcome.rule_ids,
            vec!["ZZ-CONTEXT-001".to_string()]
        );
        assert!(reports[0].outcome.passed());
        assert_eq!(
            reports[1].outcome.rule_ids,
            vec!["ZZ-CONTEXT-002".to_string()]
        );
        assert!(!reports[1].outcome.passed());
    }

    #[test]
    fn judge_gate_diff_uses_pinned_base_sha() {
        // The final gate's judge diffs against the base pinned at approval,
        // never the moving base branch. Selection: pinned sha wins; None/empty
        // falls back to base_branch (legacy missions).
        assert_eq!(judge_diff_base(Some("abc123"), "main"), "abc123");
        assert_eq!(judge_diff_base(None, "main"), "main");
        assert_eq!(judge_diff_base(Some(""), "main"), "main");

        // Non-vacuity, in a real repo: when the base branch MOVES after the
        // mission forks, the pinned-sha diff stays put while the moved-branch
        // diff changes — diffing the wrong base would silently alter the judge's
        // view of the mission's work. Skip when git is unavailable.
        let git_ok = std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !git_ok {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(dir.path())
                    .output()
                    .unwrap()
                    .status
                    .success(),
                "git {args:?} failed"
            );
        };
        if !std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            git(&["init"]);
            git(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        }
        git(&["config", "user.name", "t"]);
        git(&["config", "user.email", "t@e"]);
        std::fs::write(dir.path().join("base.txt"), "base\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-m", "base"]);

        let repo = GitRepo::open(dir.path()).unwrap();
        let pinned = repo.rev_parse("main").unwrap(); // base tip pinned at approval

        // Mission forks and adds its own feature commit.
        git(&["checkout", "-b", "kranz/mission-x"]);
        std::fs::write(dir.path().join("feature.txt"), "feature\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-m", "feature"]);

        // The base branch MOVES after the fork.
        git(&["checkout", "main"]);
        std::fs::write(dir.path().join("unrelated.txt"), "moved\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-m", "base moved"]);
        git(&["checkout", "kranz/mission-x"]);

        let diff_pinned = repo.diff_stat(&pinned, "HEAD").unwrap();
        let diff_moved = repo.diff_stat("main", "HEAD").unwrap();
        assert!(
            diff_pinned.contains("feature.txt"),
            "pinned diff should be the mission's own work: {diff_pinned}"
        );
        assert_ne!(
            diff_pinned, diff_moved,
            "a moved base must change the diff (else the guard is vacuous): \
             pinned={diff_pinned:?} moved={diff_moved:?}"
        );
    }

    /// Provenance gate (lessons-manifest-body-split): a lesson only reaches a
    /// planning prompt if a genuine `[kranz] mission report` commit with a
    /// MATCHING `Kranz-Mission` trailer introduced its file. Pins the four
    /// rejection cases a forged/dropped lesson must fail.
    #[test]
    fn lesson_provenance_clean_accepts_only_engine_report_commits() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output()
                .expect("spawn git");
            assert!(out.status.success(), "git {args:?} failed: {out:?}");
        };
        let lessons = root.join(".kranz/lessons");
        std::fs::create_dir_all(&lessons).unwrap();

        // (1) genuine engine report commit adding the lesson + matching trailer.
        std::fs::write(lessons.join("m-good.md"), "GOOD\n").unwrap();
        git(&["add", ".kranz/lessons/m-good.md"]);
        git(&[
            "commit",
            "-m",
            "[kranz] mission report for m-good\n\nKranz-Mission: m-good",
        ]);
        // (2) a worker feature-commit (plain subject) adding a lesson-shaped file.
        std::fs::write(lessons.join("m-worker.md"), "WORKER\n").unwrap();
        git(&["add", ".kranz/lessons/m-worker.md"]);
        git(&["commit", "-m", "[f-1-1] implement thing"]);
        // (3) report subject but a trailer pointing at a DIFFERENT mission.
        std::fs::write(lessons.join("m-mismatch.md"), "MISMATCH\n").unwrap();
        git(&["add", ".kranz/lessons/m-mismatch.md"]);
        git(&[
            "commit",
            "-m",
            "[kranz] mission report for m-mismatch\n\nKranz-Mission: m-other",
        ]);
        // (4) an untracked drop — never committed at all.
        std::fs::write(lessons.join("m-drop.md"), "DROP\n").unwrap();

        let repo = GitRepo::open(&root).unwrap();
        assert!(
            lesson_provenance_clean(&repo, "m-good.md"),
            "a genuine engine report commit is clean"
        );
        assert!(
            !lesson_provenance_clean(&repo, "m-worker.md"),
            "a worker feature-commit must be rejected"
        );
        assert!(
            !lesson_provenance_clean(&repo, "m-mismatch.md"),
            "a mismatched Kranz-Mission trailer must be rejected"
        );
        assert!(
            !lesson_provenance_clean(&repo, "m-drop.md"),
            "an untracked dropped file must be rejected"
        );
        assert!(
            !lesson_provenance_clean(&repo, "not-a-lesson"),
            "a non-.md name must be rejected"
        );
    }

    #[tokio::test]
    async fn lessons_capture_writes_file_and_index() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> =
            Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
                lesson_orch_script(
                    "Always check the plan for a base_branch override before assuming main.",
                ),
            ]));
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();
        let mission_id = engine.state.mission.id.clone();

        let written = engine.capture_lesson().await.expect("lesson written");
        assert_eq!(written.len(), 2, "expects lesson file + index path");

        let lesson_file = engine.paths.lessons_dir().join(format!("{mission_id}.md"));
        assert!(written.contains(&lesson_file));
        let body = std::fs::read_to_string(&lesson_file).expect("lesson file exists");
        assert_eq!(
            first_nonempty_line(&body),
            "Always check the plan for a base_branch override before assuming main."
        );

        let index = engine.paths.lessons_index();
        assert!(written.contains(&index));
        let index_text = std::fs::read_to_string(&index).expect("index exists");
        assert_eq!(
            index_text.lines().count(),
            1,
            "one manifest line per capture"
        );
        assert!(index_text.contains(&format!("{mission_id}.md")));
        assert!(index_text.contains("Always check the plan for a base_branch override"));
    }

    #[tokio::test]
    async fn lessons_capture_none_reply_writes_nothing() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let backend: Arc<dyn AgentBackend> =
            Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
                lesson_orch_script("  none.  "),
            ]));
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();

        let written = engine.capture_lesson().await;
        assert!(written.is_none(), "NONE reply must write nothing");
        assert!(
            !engine.paths.lessons_dir().exists(),
            "lessons dir must not be created"
        );
    }

    #[tokio::test]
    async fn lessons_capture_turn_error_returns_none() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        // No scripts queued: the orchestrator turn fails immediately.
        let backend: Arc<dyn AgentBackend> = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();

        let written = engine.capture_lesson().await;
        assert!(
            written.is_none(),
            "a failed turn must downgrade to None, not panic"
        );
        assert!(!engine.paths.lessons_dir().exists());
    }

    #[test]
    fn lessons_is_none_reply_matches_case_and_punctuation() {
        assert!(is_none_reply("NONE"));
        assert!(is_none_reply("  none.  "));
        assert!(is_none_reply("None!"));
        assert!(!is_none_reply("none of this applies, still worth a lesson"));
    }

    #[test]
    fn lessons_normalize_body_prepends_capped_summary_when_first_line_too_long() {
        let long_first_line = "x".repeat(200);
        let text = format!("{long_first_line}\nmore detail");
        let normalized = normalize_lesson_body(&text);
        let first = first_nonempty_line(&normalized);
        assert!(first.chars().count() <= LESSON_SUMMARY_CAP);
        assert!(normalized.contains("more detail"));
    }

    #[test]
    fn lessons_normalize_body_keeps_short_first_line_as_is() {
        let text = "Short imperative note.\n\nMore context below.";
        assert_eq!(normalize_lesson_body(text), text);
    }
}
