//! Cross-mission Flight Rules effectiveness metrics (KRZ-348, D-H/D-K).
//!
//! This is a deterministic fold over existing mission events plus the
//! existing `traced-from-mission` defect links. It evaluates rule/checker
//! behavior, never people or model backends, and persists no analytics state.

use crate::events::{Event, EventKind};
use crate::gate::GateVerdict;
use crate::standards_coverage::{standards_coverage, RuleDisposition};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const MINIMUM_SAMPLES: u64 = 5;
pub const HIGH_WAIVER_SHARE: f64 = 0.30;
pub const NEAR_CONSTANT_SCORE_RANGE: f64 = 0.01;
pub const NEAR_THRESHOLD_DISTANCE: f64 = 0.10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandardsMetricsReport {
    pub minimum_samples: u64,
    pub definitions: Vec<String>,
    pub rules: Vec<RuleMetrics>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleMetrics {
    pub id: String,
    pub revision: u64,
    pub statement: String,
    pub checker: Option<String>,
    pub lifecycles: Vec<String>,
    pub applicable_missions: u64,
    pub evaluated_missions: u64,
    pub advisory_missions: u64,
    pub failed_missions: u64,
    pub blocked_missions: u64,
    pub waived_missions: u64,
    pub not_evaluated_missions: u64,
    pub false_green_missions: u64,
    pub evaluation_rate: Option<f64>,
    pub advisory_rate: Option<f64>,
    pub failure_rate: Option<f64>,
    pub block_rate: Option<f64>,
    pub waiver_rate: Option<f64>,
    pub mean_resolution_ms: Option<f64>,
    pub score_distribution: Option<RuleScoreDistribution>,
    pub conclusions_suppressed: bool,
    pub smells: Vec<RuleMetricSmell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleScoreDistribution {
    pub samples: u64,
    pub minimum: f64,
    pub maximum: f64,
    pub mean: f64,
    pub near_threshold: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleMetricSmell {
    pub kind: String,
    pub observed: String,
    pub definition: String,
    pub samples: u64,
}

#[derive(Default)]
struct Accumulator {
    statement: String,
    checker: Option<String>,
    lifecycles: BTreeSet<String>,
    applicable: u64,
    evaluated: u64,
    advisory: u64,
    failed: u64,
    blocked: u64,
    waived: u64,
    not_evaluated: u64,
    false_greens: u64,
    scores: Vec<(f64, f64)>,
    resolutions_ms: Vec<u64>,
}

fn rate(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

fn mission_resolution_times(events: &[Event]) -> HashMap<(String, u64), Vec<u64>> {
    let mut open: HashMap<(String, u64), chrono::DateTime<chrono::Utc>> = HashMap::new();
    let mut durations: HashMap<(String, u64), Vec<u64>> = HashMap::new();
    let mut revisions: HashMap<String, u64> = HashMap::new();
    for event in events {
        match &event.kind {
            EventKind::PlanApproved { plan, .. } => {
                revisions.clear();
                if let Some(pin) = plan.standards_manifest.as_deref() {
                    revisions.extend(
                        pin.rules
                            .iter()
                            .map(|rule| (rule.id.clone(), rule.revision)),
                    );
                }
            }
            EventKind::GateResult {
                verdict, rule_ids, ..
            } => {
                for id in rule_ids {
                    let Some(revision) = revisions.get(id) else {
                        continue;
                    };
                    let key = (id.clone(), *revision);
                    match verdict {
                        GateVerdict::Fail => {
                            open.entry(key).or_insert(event.ts);
                        }
                        GateVerdict::Pass => {
                            if let Some(started) = open.remove(&key) {
                                let millis = (event.ts - started).num_milliseconds().max(0) as u64;
                                durations.entry(key).or_default().push(millis);
                            }
                        }
                    }
                }
            }
            EventKind::ValidationFinding { finding, .. } => {
                if let Some(rule) = &finding.rule {
                    open.entry((rule.id.clone(), rule.revision))
                        .or_insert(event.ts);
                }
            }
            EventKind::StandardsWaiverApproved {
                rule_id,
                rule_revision,
                ..
            } => {
                let key = (rule_id.clone(), *rule_revision);
                if let Some(started) = open.remove(&key) {
                    let millis = (event.ts - started).num_milliseconds().max(0) as u64;
                    durations.entry(key).or_default().push(millis);
                }
            }
            _ => {}
        }
    }
    durations
}

pub fn aggregate(
    missions: &[(String, Vec<Event>)],
    traced_defects: &[crate::escalation_metrics::TracedDefect],
) -> StandardsMetricsReport {
    let false_green_missions: BTreeSet<&str> = traced_defects
        .iter()
        .map(|defect| defect.mission_id.as_str())
        .collect();
    let mut rows: BTreeMap<(String, u64), Accumulator> = BTreeMap::new();

    for (mission_id, events) in missions {
        let Some(coverage) = standards_coverage(mission_id, events) else {
            continue;
        };
        let completed = events
            .iter()
            .any(|event| matches!(event.kind, EventKind::MissionCompleted {}));
        let resolutions = mission_resolution_times(events);
        let blocked_ids: BTreeSet<(String, u64)> = events
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::ValidationFinding { finding, .. }
                    if finding.class == "standards-authoritative" =>
                {
                    finding
                        .rule
                        .as_ref()
                        .map(|rule| (rule.id.clone(), rule.revision))
                }
                _ => None,
            })
            .collect();
        let mut scores: HashMap<(String, u64), Vec<(f64, f64)>> = HashMap::new();
        for event in events {
            if let EventKind::GateResult {
                score: Some(score),
                threshold: Some(threshold),
                rule_ids,
                ..
            } = &event.kind
            {
                if !score.is_finite() || !threshold.is_finite() {
                    continue;
                }
                for id in rule_ids {
                    if let Some(rule) = coverage.rules.iter().find(|rule| {
                        rule.id == *id && rule.disposition != RuleDisposition::NotApplicable
                    }) {
                        scores
                            .entry((id.clone(), rule.revision))
                            .or_default()
                            .push((*score, *threshold));
                    }
                }
            }
        }

        for rule in coverage
            .rules
            .iter()
            .filter(|rule| rule.disposition != RuleDisposition::NotApplicable)
        {
            let key = (rule.id.clone(), rule.revision);
            let row = rows.entry(key.clone()).or_default();
            if row.statement.is_empty() || rule.statement < row.statement {
                row.statement = rule.statement.clone();
            }
            if let Some(checker) = &rule.checker {
                if row.checker.as_ref().is_none_or(|current| checker < current) {
                    row.checker = Some(checker.clone());
                }
            }
            row.lifecycles.insert(rule.lifecycle.clone());
            row.applicable += 1;
            match rule.disposition {
                RuleDisposition::Passed => row.evaluated += 1,
                RuleDisposition::Failed => {
                    row.evaluated += 1;
                    row.failed += 1;
                }
                RuleDisposition::Advisory => {
                    row.evaluated += 1;
                    row.advisory += 1;
                }
                RuleDisposition::Waived => {
                    row.evaluated += 1;
                    row.waived += 1;
                }
                RuleDisposition::NotEvaluated => row.not_evaluated += 1,
                RuleDisposition::NotApplicable => unreachable!(),
            }
            if blocked_ids.contains(&key) {
                row.blocked += 1;
            }
            if completed
                && false_green_missions.contains(mission_id.as_str())
                && matches!(
                    rule.disposition,
                    RuleDisposition::Passed | RuleDisposition::Waived
                )
            {
                row.false_greens += 1;
            }
            row.scores.extend(scores.remove(&key).unwrap_or_default());
            row.resolutions_ms
                .extend(resolutions.get(&key).cloned().unwrap_or_default());
        }
    }

    let rules = rows
        .into_iter()
        .map(|((id, revision), row)| {
            let score_distribution = if row.scores.is_empty() {
                None
            } else {
                let minimum = row
                    .scores
                    .iter()
                    .map(|(score, _)| *score)
                    .fold(f64::INFINITY, f64::min);
                let maximum = row
                    .scores
                    .iter()
                    .map(|(score, _)| *score)
                    .fold(f64::NEG_INFINITY, f64::max);
                Some(RuleScoreDistribution {
                    samples: row.scores.len() as u64,
                    minimum,
                    maximum,
                    mean: row.scores.iter().map(|(score, _)| score).sum::<f64>()
                        / row.scores.len() as f64,
                    near_threshold: row
                        .scores
                        .iter()
                        .filter(|(score, threshold)| {
                            (*score - *threshold).abs() <= NEAR_THRESHOLD_DISTANCE
                        })
                        .count() as u64,
                })
            };
            let mut smells = Vec::new();
            if row.applicable >= MINIMUM_SAMPLES && row.evaluated == 0 {
                smells.push(RuleMetricSmell {
                    kind: "never-selected".to_string(),
                    observed: format!("0 of {} applicable missions produced checker evidence", row.applicable),
                    definition: "Flagged when a rule is applicable in at least the minimum sample count but its checker is never selected/evaluated.".to_string(),
                    samples: row.applicable,
                });
            }
            if row.evaluated >= MINIMUM_SAMPLES && row.failed == row.evaluated {
                smells.push(RuleMetricSmell {
                    kind: "always-fail".to_string(),
                    observed: format!("{} of {} evaluations failed", row.failed, row.evaluated),
                    definition: "Flagged when every evaluated mission's latest rule disposition is failed.".to_string(),
                    samples: row.evaluated,
                });
            }
            if row.evaluated >= MINIMUM_SAMPLES
                && rate(row.waived, row.evaluated).is_some_and(|share| share >= HIGH_WAIVER_SHARE)
            {
                smells.push(RuleMetricSmell {
                    kind: "high-waiver".to_string(),
                    observed: format!("{} of {} evaluations were waived", row.waived, row.evaluated),
                    definition: format!("Flagged when waivers are at least {:.0}% of evaluated missions.", HIGH_WAIVER_SHARE * 100.0),
                    samples: row.evaluated,
                });
            }
            if let Some(scores) = &score_distribution {
                if scores.samples >= MINIMUM_SAMPLES
                    && scores.maximum - scores.minimum <= NEAR_CONSTANT_SCORE_RANGE
                {
                    smells.push(RuleMetricSmell {
                        kind: "near-constant-score".to_string(),
                        observed: format!("score range {:.4}", scores.maximum - scores.minimum),
                        definition: format!("Flagged when at least {MINIMUM_SAMPLES} scores span no more than {NEAR_CONSTANT_SCORE_RANGE:.2}."),
                        samples: scores.samples,
                    });
                }
                if scores.samples >= MINIMUM_SAMPLES && scores.near_threshold == 0 {
                    smells.push(RuleMetricSmell {
                        kind: "never-near-threshold".to_string(),
                        observed: format!("0 of {} scores were within {:.2} of threshold", scores.samples, NEAR_THRESHOLD_DISTANCE),
                        definition: "Flagged when no sufficiently-sampled score approaches its gate-owned threshold; the threshold may not discriminate.".to_string(),
                        samples: scores.samples,
                    });
                }
            }
            RuleMetrics {
                id,
                revision,
                statement: row.statement,
                checker: row.checker,
                lifecycles: row.lifecycles.into_iter().collect(),
                applicable_missions: row.applicable,
                evaluated_missions: row.evaluated,
                advisory_missions: row.advisory,
                failed_missions: row.failed,
                blocked_missions: row.blocked,
                waived_missions: row.waived,
                not_evaluated_missions: row.not_evaluated,
                false_green_missions: row.false_greens,
                evaluation_rate: rate(row.evaluated, row.applicable),
                advisory_rate: rate(row.advisory, row.evaluated),
                failure_rate: rate(row.failed, row.evaluated),
                block_rate: rate(row.blocked, row.applicable),
                waiver_rate: rate(row.waived, row.evaluated),
                mean_resolution_ms: (!row.resolutions_ms.is_empty()).then(|| {
                    row.resolutions_ms
                        .iter()
                        .map(|millis| *millis as f64)
                        .sum::<f64>()
                        / row.resolutions_ms.len() as f64
                }),
                score_distribution,
                conclusions_suppressed: if row.evaluated == 0 {
                    row.applicable < MINIMUM_SAMPLES
                } else {
                    row.evaluated < MINIMUM_SAMPLES
                },
                smells,
            }
        })
        .collect();

    StandardsMetricsReport {
        minimum_samples: MINIMUM_SAMPLES,
        definitions: vec![
            "applicable = the stable rule/revision appears in a mission's approval pin"
                .to_string(),
            "evaluated = rule-linked gate/finding evidence exists; absence remains not-evaluated"
                .to_string(),
            "block = an authoritative standards finding interrupted a mission, even if later repaired"
                .to_string(),
            "false green = a completed mission with passed/waived rule evidence later received a traced-from-mission defect ticket"
                .to_string(),
            format!("interpretive smells are suppressed below {MINIMUM_SAMPLES} samples; raw counts are never suppressed"),
        ],
        rules,
    }
}

pub fn compute(repo_root: &std::path::Path) -> crate::error::Result<StandardsMetricsReport> {
    let mut missions = Vec::new();
    for id in crate::paths::MissionPaths::list_missions(repo_root) {
        let path = crate::paths::MissionPaths::new(repo_root, &id).events_file();
        if !path.is_file() {
            continue;
        }
        let events = crate::event_log::EventLog::read_events(&path)?;
        missions.push((id, events));
    }
    Ok(aggregate(
        &missions,
        &crate::escalation_metrics::traced_defects_from_tickets(repo_root),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::{GateKind, GateSurface};
    use crate::types::{
        MissionConfig, PinnedRule, Plan, RuleCitation, StandardsPin, StandardsPinSource,
    };

    fn event(seq: u64, seconds: i64, mission: &str, kind: EventKind) -> Event {
        Event {
            seq,
            ts: chrono::DateTime::from_timestamp(1_800_000_000 + seconds, 0).unwrap(),
            mission_id: mission.to_string(),
            kind,
        }
    }

    fn rule() -> PinnedRule {
        PinnedRule {
            id: "ZZ-METRIC-001".to_string(),
            revision: 2,
            rfc: "RFC-001".to_string(),
            level: "must".to_string(),
            effective_status: "enforced".to_string(),
            statement: "Metric rule statement.".to_string(),
            domains: Vec::new(),
            stages: vec!["validation".to_string()],
            when_paths: Vec::new(),
            task_classes: Vec::new(),
            checker: Some("gate:metric".to_string()),
            waivable: true,
        }
    }

    fn mission(id: &str, index: u64, disposition: &str) -> (String, Vec<Event>) {
        let rule = rule();
        let pin = StandardsPin {
            pack_name: "zz".to_string(),
            pack_dir: "vendor/zz".to_string(),
            standards_root: "standards".to_string(),
            digest: "ab".repeat(32),
            source: StandardsPinSource::RepoTracked,
            task_class: None,
            touch_set: Vec::new(),
            gates: Vec::new(),
            rules: vec![rule.clone()],
        };
        let plan = Plan {
            goal: "g".to_string(),
            validation_contract: Vec::new(),
            milestones: Vec::new(),
            considered_alternatives: None,
            command_grants: Vec::new(),
            touch_set: Vec::new(),
            standards_manifest: Some(Box::new(pin.clone())),
        };
        let mut events = vec![
            event(
                1,
                index as i64 * 100,
                id,
                EventKind::MissionCreated {
                    goal: "g".to_string(),
                    base_branch: "main".to_string(),
                    mission_branch: format!("kranz/{id}"),
                    config: MissionConfig::default(),
                },
            ),
            event(
                2,
                index as i64 * 100 + 1,
                id,
                EventKind::PlanApproved {
                    plan,
                    base_sha: Some("deadbeef".to_string()),
                },
            ),
        ];
        let verdict = if disposition == "pass" {
            GateVerdict::Pass
        } else {
            GateVerdict::Fail
        };
        events.push(event(
            3,
            index as i64 * 100 + 2,
            id,
            EventKind::GateResult {
                gate: "metric".to_string(),
                surface: GateSurface::FinalGate,
                kind: GateKind::Deterministic,
                index: 0,
                verdict,
                artefact_ref: "inline".to_string(),
                artefact_detail: None,
                score: Some(0.9),
                threshold: Some(0.5),
                rule_ids: vec![rule.id.clone()],
            },
        ));
        if disposition != "pass" {
            events.push(event(
                4,
                index as i64 * 100 + 3,
                id,
                EventKind::ValidationFinding {
                    milestone_id: "ms-1".to_string(),
                    run_id: crate::reducer::ENGINE_RUN_ID.to_string(),
                    finding: crate::types::Finding {
                        subject: format!("flight-rule:{}", rule.id),
                        severity: "critical".to_string(),
                        evidence: "failed".to_string(),
                        suggested_fix: String::new(),
                        class: "standards-authoritative".to_string(),
                        rule: Some(RuleCitation {
                            id: rule.id.clone(),
                            revision: rule.revision,
                            source: "zz standards".to_string(),
                            digest: pin.digest.clone(),
                            lifecycle: "enforced".to_string(),
                            level: "must".to_string(),
                            checker: rule.checker.clone(),
                        }),
                    },
                },
            ));
        }
        if disposition == "waived" {
            events.push(event(
                5,
                index as i64 * 100 + 12,
                id,
                EventKind::StandardsWaiverApproved {
                    rule_id: rule.id.clone(),
                    rule_revision: rule.revision,
                    manifest_digest: pin.digest,
                    approval_seq: 2,
                    finding_fingerprint: crate::standards_waiver::finding_fingerprint(
                        crate::reducer::ENGINE_RUN_ID,
                        match &events[3].kind {
                            EventKind::ValidationFinding { finding, .. } => finding,
                            _ => unreachable!(),
                        },
                    ),
                    paths: Vec::new(),
                    diff_digest: "cd".repeat(32),
                    reason: "reviewed exception".to_string(),
                    approver: crate::standards_waiver::LOCAL_OPERATOR.to_string(),
                    surface: "cli".to_string(),
                    expires_at: chrono::DateTime::from_timestamp(1_900_000_000, 0).unwrap(),
                },
            ));
        }
        if disposition == "pass" {
            events.push(event(
                4,
                index as i64 * 100 + 3,
                id,
                EventKind::MissionCompleted {},
            ));
        }
        (id.to_string(), events)
    }

    #[test]
    fn flight_rules_metrics_keeps_revision_counts_denominators_and_false_greens_honest() {
        let mut missions = vec![
            mission("m-1", 0, "pass"),
            mission("m-2", 1, "pass"),
            mission("m-3", 2, "fail"),
            mission("m-4", 3, "waived"),
            mission("m-5", 4, "pass"),
        ];
        let (revision_id, mut revision_events) = mission("m-6", 5, "pass");
        let EventKind::PlanApproved { plan, .. } = &mut revision_events[1].kind else {
            unreachable!();
        };
        plan.standards_manifest.as_mut().unwrap().rules[0].revision = 3;
        missions.push((revision_id, revision_events));
        let report = aggregate(
            &missions,
            &[crate::escalation_metrics::TracedDefect {
                ticket: "defect-one".to_string(),
                mission_id: "m-1".to_string(),
            }],
        );
        let row = &report.rules[0];
        assert_eq!((row.id.as_str(), row.revision), ("ZZ-METRIC-001", 2));
        assert_eq!(row.applicable_missions, 5);
        assert_eq!(row.evaluated_missions, 5);
        assert_eq!(row.failed_missions, 1);
        assert_eq!(row.blocked_missions, 2);
        assert_eq!(row.waived_missions, 1);
        assert_eq!(row.false_green_missions, 1);
        assert_eq!(row.failure_rate, Some(0.2));
        assert_eq!(row.waiver_rate, Some(0.2));
        assert_eq!(row.mean_resolution_ms, Some(10_000.0));
        assert!(!row.conclusions_suppressed);
        assert!(row
            .smells
            .iter()
            .any(|smell| smell.kind == "near-constant-score"));
        assert!(row
            .smells
            .iter()
            .any(|smell| smell.kind == "never-near-threshold"));
        assert_eq!(report.rules.len(), 2);
        assert_eq!(report.rules[1].revision, 3);
        assert_eq!(report.rules[1].applicable_missions, 1);
        assert_eq!(report.rules[1].evaluated_missions, 1);
    }

    #[test]
    fn flight_rules_metrics_suppresses_small_sample_conclusions_not_raw_counts() {
        let report = aggregate(&[mission("m-1", 0, "fail")], &[]);
        let row = &report.rules[0];
        assert_eq!(row.applicable_missions, 1);
        assert_eq!(row.failed_missions, 1);
        assert!(row.conclusions_suppressed);
        assert!(row.smells.is_empty());
    }

    #[test]
    fn flight_rules_metrics_no_evidence_is_not_evaluated_never_green() {
        let (id, mut events) = mission("m-1", 0, "pass");
        events.retain(|event| !matches!(event.kind, EventKind::GateResult { .. }));
        let report = aggregate(&[(id, events)], &[]);
        let row = &report.rules[0];
        assert_eq!(row.evaluated_missions, 0);
        assert_eq!(row.not_evaluated_missions, 1);
        assert_eq!(row.evaluation_rate, Some(0.0));
        assert_eq!(row.failure_rate, None);
    }
}
