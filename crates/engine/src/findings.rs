//! Validation-findings conversion — extracted from `orchestrator.rs` in the
//! monolith split (pure code motion, no behavior change). The §4.5 g
//! conversion turn ([`MissionEngine::convert_findings`]) puts every finding
//! (validator report, engine sweep, final gate) to the orchestrator: convert
//! it to a fix feature, waive it with a one-line justification, or — for
//! `command-assertion` findings only — escalate it to the operator as an
//! author-broken assertion. The fix-cycle cap
//! ([`MissionEngine::fix_cycle_exhausted`]) and its local-tier escalation
//! valve ([`MissionEngine::escalate_or_block`]) bound how many fix rounds a
//! milestone gets before it blocks.

use crate::error::Result;
use crate::events::EventKind;
use crate::orchestrator::MissionEngine;
use crate::scrub;
use crate::types::*;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// JSON decision shapes (parsed leniently via runner::parse_report)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FixFeatureSpec {
    pub(crate) title: String,
    pub(crate) spec: String,
    #[serde(default)]
    pub(crate) validation_criteria: Vec<String>,
}

/// One finding the orchestrator waived instead of converting (conversion
/// turn, §4.5 g).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WaivedFinding {
    pub(crate) subject: String,
    #[serde(default)]
    pub(crate) reason: String,
}

/// One finding the orchestrator judged to be an author-broken command
/// assertion (a false negative: the requirement is genuinely met but the
/// assertion's own command is wrong) — escalated to the operator instead of
/// spent as a fix cycle. Only honored for `class == "command-assertion"`
/// findings; see [`MissionEngine::convert_findings`]'s escape-hatch guard.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CommandBrokenFinding {
    pub(crate) subject: String,
    #[serde(default)]
    pub(crate) diagnosis: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixFeaturesDecision {
    #[serde(default)]
    fix_features: Vec<FixFeatureSpec>,
    /// Findings waived with a justification instead of fixed. Defaulted so
    /// old-shape answers (fixFeatures + summary only) still parse.
    #[serde(default)]
    waived: Vec<WaivedFinding>,
    /// Findings judged author-broken command assertions. Defaulted so
    /// old-shape answers still parse.
    #[serde(default)]
    command_broken: Vec<CommandBrokenFinding>,
    #[serde(default)]
    summary: String,
}

/// What the findings-conversion turn decided (see [`MissionEngine::convert_findings`]).
pub(crate) enum FindingsConversion {
    /// Convert into fix features. Unparseable answers and answers that
    /// neither fix nor waive land here with specs synthesized 1:1 from the
    /// findings — the conservative default.
    Fix {
        specs: Vec<FixFeatureSpec>,
        summary: String,
        text: String,
    },
    /// Every finding waived, each with a one-line justification.
    Waive { waived: Vec<WaivedFinding> },
    /// At least one finding judged an author-broken command assertion
    /// (false negative). Supersedes fix/waive for the round — the milestone
    /// blocks for operator review instead.
    Escalate {
        escalations: Vec<CommandBrokenFinding>,
        text: String,
    },
}

impl MissionEngine {
    /// Would one more fix round exceed `max_fix_cycles_per_milestone`?
    /// Checked after the conversion turn (waivable findings must reach the
    /// orchestrator even at the cap) but before any `fixfeature.created` is
    /// emitted (§4.5 g: never create the features first).
    pub(crate) fn fix_cycle_exhausted(&self, mi: usize) -> bool {
        self.state.mission.milestones[mi].fix_cycles + 1
            > self.state.config.max_fix_cycles_per_milestone
    }

    /// Fix-cycle-cap valve (feature f-2-2): a cap-exhausted milestone whose
    /// executor is still on the local tier escalates to frontier instead of
    /// blocking (the reducer's `TierEscalated` fold resets the Worker backend
    /// and the milestone's `fix_cycles`, so this is naturally one-shot per
    /// mission — the second time a milestone hits the cap, the tier is
    /// already `Frontier` and it blocks like today). Returns `true` when the
    /// caller should proceed to `emit_fix_features` (escalated or never
    /// exhausted in the first place), `false` when it must block instead.
    pub(crate) fn escalate_or_block(&mut self, milestone_id: &str) -> Result<bool> {
        if self.state.executor_tier() != ExecutorTier::Local {
            return Ok(false);
        }
        self.emit_decision(
            &format!(
                "fix-cycle cap reached for {milestone_id} while the executor is on the \
                 local tier; escalating to frontier instead of blocking"
            ),
            None,
        )?;
        self.emit(EventKind::TierEscalated {
            milestone_id: milestone_id.to_string(),
            from: ExecutorTier::Local,
            to: ExecutorTier::Frontier,
            reason: "two failed local validations".to_string(),
        })?;
        Ok(true)
    }

    /// The findings-conversion turn (§4.5 g): every finding is put to the
    /// orchestrator, which converts each into a fix feature or waives it
    /// with a one-line justification. The contract is the bar — severity
    /// alone decides nothing.
    ///
    /// Conservative fallbacks: an unparseable answer (after retry) and an
    /// answer that neither fixes nor waives are both treated as
    /// convert-everything, with specs synthesized 1:1 from the findings — a
    /// parse failure must never silently waive, and an empty round would
    /// re-validate immediately and spin without ever bumping `fix_cycles`.
    pub(crate) async fn convert_findings(
        &mut self,
        milestone_id: &str,
        findings: &[Finding],
    ) -> Result<FindingsConversion> {
        let findings_json = serde_json::to_string_pretty(findings)?;
        let message = format!(
            "Validation of milestone {milestone_id} produced these findings:\n{findings_json}\n\n\
             For each finding decide: convert it to a fix-feature (it violates or endangers \
             the validation contract / feature criteria; fresh worker sessions will implement \
             fix features), WAIVE it with a one-line justification (cosmetic, \
             out-of-contract, or not worth a fresh worker session), or — ONLY for a finding \
             whose class is \"command-assertion\" — mark it commandBroken when you judge the \
             underlying requirement is genuinely met (the milestone validators already passed \
             it) but the assertion's own command is wrong, e.g. it can only pass pre-change or \
             it counts the harness's own commits. commandBroken escalates the finding to a \
             HUMAN operator (the mission blocks) with the evidence attached; it is not a way \
             to dodge a real product defect, and it is ignored for any finding that is not a \
             command assertion. The contract is the bar; minor severity is not automatically \
             waivable and major severity is not automatically fixable — judge. Respond with \
             ONLY this JSON:\n\
             {{\"fixFeatures\":[{{\"title\":\"string\",\"spec\":\"string\",\"validationCriteria\":[\"string\"]}}],\"waived\":[{{\"subject\":\"string\",\"reason\":\"string\"}}],\"commandBroken\":[{{\"subject\":\"string\",\"diagnosis\":\"string\"}}],\"summary\":\"string\"}}"
        );
        let (decision, text) = self.json_decision::<FixFeaturesDecision>(&message).await?;
        let (specs, waived, command_broken, summary) = match decision {
            Some(d) => (d.fix_features, d.waived, d.command_broken, d.summary),
            None => (
                Vec::new(),
                Vec::new(),
                Vec::new(),
                "unparseable fix-features decision; synthesized from findings".to_string(),
            ),
        };
        // Escape hatch: commandBroken is honored ONLY for findings whose
        // class is "command-assertion" — a mislabelled agent-judgement or
        // validator finding (class == "") must never escalate, it falls
        // through to the normal fix/waive handling below.
        let command_assertion_subjects: std::collections::HashSet<&str> = findings
            .iter()
            .filter(|f| f.class == "command-assertion")
            .map(|f| f.subject.as_str())
            .collect();
        let escalations: Vec<CommandBrokenFinding> = command_broken
            .into_iter()
            .filter(|c| command_assertion_subjects.contains(c.subject.as_str()))
            .collect();
        if !escalations.is_empty() {
            return Ok(FindingsConversion::Escalate { escalations, text });
        }
        let waived_subjects: std::collections::HashSet<&str> =
            waived.iter().map(|w| w.subject.as_str()).collect();
        let unwaived_findings: Vec<&Finding> = findings
            .iter()
            .filter(|f| !waived_subjects.contains(f.subject.as_str()))
            .collect();
        if specs.is_empty() && !waived.is_empty() && unwaived_findings.is_empty() {
            return Ok(FindingsConversion::Waive { waived });
        }
        // Partial fixFeatures must not drop uncovered findings. When the model
        // returns fewer specs than unwaived findings, union with synthesized
        // fixes for subjects its titles do not reference. When it returns at
        // least one spec per unwaived finding, trust the model — titles are
        // often generic ("fix issue 1") and must not force an extra worker.
        let specs = if specs.is_empty() {
            synthesize_fix_specs(unwaived_findings)
        } else if specs.len() >= unwaived_findings.len() {
            specs
        } else {
            let covered: std::collections::HashSet<String> =
                specs.iter().map(|s| s.title.clone()).collect();
            let mut merged = specs;
            let uncovered: Vec<&Finding> = unwaived_findings
                .iter()
                .copied()
                .filter(|f| {
                    !covered.iter().any(|title| {
                        title == &f.subject
                            || title.contains(&f.subject)
                            || title == &format!("fix {}", f.subject)
                    })
                })
                .collect();
            merged.extend(synthesize_fix_specs(uncovered));
            merged
        };
        Ok(FindingsConversion::Fix {
            specs,
            summary,
            text,
        })
    }

    /// Emit the all-waived `orchestrator.decision`: summary names the waived
    /// subjects, detail carries the justifications. Both fields are
    /// credential-scrubbed by [`Self::emit_decision`] — waiver reasons are
    /// model-authored text.
    pub(crate) fn emit_waive_decision(&mut self, waived: &[WaivedFinding]) -> Result<()> {
        let subjects = waived
            .iter()
            .map(|w| w.subject.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let reasons = waived
            .iter()
            .map(|w| format!("- {}: {}", w.subject, w.reason))
            .collect::<Vec<_>>()
            .join("\n");
        self.emit_decision(
            &format!("waived {} finding(s): {subjects}", waived.len()),
            Some(reasons),
        )
    }

    /// Emit the fix-features decision plus one `fixfeature.created` per spec
    /// (the fix path of a conversion turn; the caller has already checked
    /// the fix-cycle cap).
    pub(crate) fn emit_fix_features(
        &mut self,
        mi: usize,
        specs: Vec<FixFeatureSpec>,
        summary: &str,
        text: String,
    ) -> Result<()> {
        let milestone_id = self.state.mission.milestones[mi].id.clone();
        self.emit_decision(
            &format!(
                "{} fix feature(s) for {milestone_id}: {summary}",
                specs.len()
            ),
            Some(text),
        )?;

        let cycle = self.state.mission.milestones[mi].fix_cycles + 1;
        for (i, spec) in specs.into_iter().enumerate() {
            // Belt and braces: both sources (the orchestrator turn text and
            // validator findings) are already scrubbed, but these strings are
            // model-authored and land verbatim in `fixfeature.created` events,
            // so scrub them once more at the emit boundary.
            let feature = Feature {
                id: format!("{milestone_id}-fix-{cycle}-{}", i + 1),
                title: scrub::scrub(&spec.title),
                spec: scrub::scrub(&spec.spec),
                validation_criteria: spec
                    .validation_criteria
                    .iter()
                    .map(|c| scrub::scrub(c))
                    .collect(),
                origin: FeatureOrigin::Fix,
                status: FeatureStatus::Pending,
                worker_runs: Vec::new(),
                commits: Vec::new(),
                respawns: 0,
            };
            self.emit(EventKind::FixFeatureCreated {
                milestone_id: milestone_id.clone(),
                feature,
            })?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Conservative fix-spec synthesis (unparseable / partial conversion answers)
// ---------------------------------------------------------------------------

pub(crate) fn synthesize_fix_specs(findings: Vec<&Finding>) -> Vec<FixFeatureSpec> {
    findings
        .into_iter()
        .map(|f| FixFeatureSpec {
            title: format!("Fix finding: {}", f.subject),
            spec: format!(
                "Address this validation finding.\nEvidence: {}\nSuggested fix: {}",
                f.evidence, f.suggested_fix
            ),
            validation_criteria: vec![format!("finding '{}' no longer reproduces", f.subject)],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::AgentBackend;
    use crate::judgement::lesson_orch_script;
    use crate::orchestrator::tests::lessons_test_repo;
    use std::sync::Arc;

    fn finding(subject: &str) -> Finding {
        Finding {
            subject: subject.to_string(),
            severity: "major".to_string(),
            evidence: format!("{subject} evidence"),
            suggested_fix: format!("fix {subject}"),
            class: String::new(),
            rule: None,
        }
    }

    #[tokio::test]
    async fn partial_waiver_synthesizes_unwaived_findings() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let reply = serde_json::json!({
            "fixFeatures": [],
            "waived": [{ "subject": "covered", "reason": "not contract relevant" }],
            "summary": "waive one"
        })
        .to_string();
        let backend: Arc<dyn AgentBackend> =
            Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
                lesson_orch_script(&reply),
            ]));
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();

        let conversion = engine
            .convert_findings("ms-1", &[finding("covered"), finding("uncovered")])
            .await
            .unwrap();

        match conversion {
            FindingsConversion::Fix { specs, .. } => {
                assert_eq!(specs.len(), 1, "only the unwaived finding needs a fix");
                assert!(
                    specs[0].title.contains("uncovered"),
                    "expected synthesized fix for the uncovered finding: {}",
                    specs[0].title
                );
            }
            FindingsConversion::Waive { .. } => {
                panic!("a partial waiver must not waive the whole finding set")
            }
            FindingsConversion::Escalate { .. } => panic!("no command-assertion finding here"),
        }
    }

    #[tokio::test]
    async fn partial_fix_features_synthesizes_when_fewer_specs_than_findings() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        // Model returns one fixFeature for two unwaived findings — must
        // synthesize a fix for the omitted subject.
        let reply = serde_json::json!({
            "fixFeatures": [{
                "title": "fix covered",
                "spec": "address covered",
                "validationCriteria": ["covered fixed"]
            }],
            "waived": [],
            "summary": "fix one"
        })
        .to_string();
        let backend: Arc<dyn AgentBackend> =
            Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
                lesson_orch_script(&reply),
            ]));
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();

        let conversion = engine
            .convert_findings("ms-1", &[finding("covered"), finding("uncovered")])
            .await
            .unwrap();

        match conversion {
            FindingsConversion::Fix { specs, .. } => {
                assert_eq!(
                    specs.len(),
                    2,
                    "model fix + synthesized uncovered: {specs:?}"
                );
                assert!(
                    specs.iter().any(|s| s.title.contains("uncovered")),
                    "expected synthesized fix for uncovered: {specs:?}"
                );
            }
            FindingsConversion::Waive { .. } => {
                panic!("partial fixFeatures must not waive")
            }
            FindingsConversion::Escalate { .. } => panic!("no command-assertion finding here"),
        }
    }

    #[tokio::test]
    async fn fix_features_matching_finding_count_are_trusted() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        // Generic titles that do not contain finding subjects — still trusted
        // when the model returned one spec per unwaived finding.
        let reply = serde_json::json!({
            "fixFeatures": [{
                "title": "fix issue 1",
                "spec": "resolve validation finding 1",
                "validationCriteria": ["finding 1 resolved"]
            }],
            "waived": [],
            "summary": "1 fix feature(s)"
        })
        .to_string();
        let backend: Arc<dyn AgentBackend> =
            Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
                lesson_orch_script(&reply),
            ]));
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();

        let conversion = engine
            .convert_findings("ms-1", &[finding("part 1 works")])
            .await
            .unwrap();

        match conversion {
            FindingsConversion::Fix { specs, .. } => {
                assert_eq!(specs.len(), 1, "must not synthesize a duplicate: {specs:?}");
                assert_eq!(specs[0].title, "fix issue 1");
            }
            FindingsConversion::Waive { .. } => panic!("expected Fix"),
            FindingsConversion::Escalate { .. } => panic!("no command-assertion finding here"),
        }
    }

    #[tokio::test]
    async fn all_waived_findings_still_waive() {
        let Some((_dir, root)) = lessons_test_repo() else {
            return;
        };
        let reply = serde_json::json!({
            "fixFeatures": [],
            "waived": [
                { "subject": "one", "reason": "not contract relevant" },
                { "subject": "two", "reason": "duplicate of one" }
            ],
            "summary": "waive all"
        })
        .to_string();
        let backend: Arc<dyn AgentBackend> =
            Arc::new(crate::backend_mock::MockBackend::with_scripts(vec![
                lesson_orch_script(&reply),
            ]));
        let mut engine =
            MissionEngine::create(backend, &root, "goal", MissionConfig::default()).unwrap();

        let conversion = engine
            .convert_findings("ms-1", &[finding("one"), finding("two")])
            .await
            .unwrap();

        match conversion {
            FindingsConversion::Waive { waived } => assert_eq!(waived.len(), 2),
            FindingsConversion::Fix { .. } => panic!("a full waiver should still waive"),
            FindingsConversion::Escalate { .. } => panic!("no command-assertion finding here"),
        }
    }
}
