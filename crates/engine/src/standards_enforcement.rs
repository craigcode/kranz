//! Flight Rules checker binding and enforcement policy (KRZ-346, D-B/D-F).
//!
//! This module is deliberately pure: it classifies lifecycle/level posture,
//! selects final/merge-stage pinned rules from actual paths, resolves typed
//! checker references against approval-pinned gate declarations, and builds
//! rule-cited findings. The orchestrator owns async command/model execution;
//! merge owns its scratch integration tree. Keeping the policy here gives
//! both surfaces one fail-closed matrix.

use crate::pack::resolution::{resolve_pin, TouchInput};
use crate::pack::standards::RuleStage;
use crate::types::{Finding, PinnedGate, PinnedRule, RuleCitation, StandardsPin};

/// An already-computed checker outcome adapted back into the shared gate
/// pipeline. Async commands/model turns finish before construction; pipeline
/// registration still structurally orders deterministic outcomes before
/// model judgement.
pub struct PreparedGate {
    report: crate::gate::GateReport,
}

impl PreparedGate {
    pub fn new(report: crate::gate::GateReport) -> Self {
        Self { report }
    }
}

impl crate::gate::Gate for PreparedGate {
    fn name(&self) -> &str {
        &self.report.name
    }

    fn kind(&self) -> crate::gate::GateKind {
        self.report.kind
    }

    fn evaluate(&self) -> crate::gate::GateOutcome {
        self.report.outcome.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleMode {
    /// Draft/retired policy never participates in a mission evaluation.
    Absent,
    /// Approved rules and enforced SHOULDs produce evidence but never block.
    Advisory,
    /// Enforced MUST: a failing/missing checker blocks unless an exact live
    /// human waiver covers its failure.
    Authoritative,
}

/// D-B's lifecycle × level matrix. Unknown pin spellings fail closed as
/// authoritative: pins are engine-authored, so an unknown value means a
/// corrupt/hand-edited consent artifact, never a reason to weaken policy.
pub fn rule_mode(rule: &PinnedRule) -> RuleMode {
    match (rule.effective_status.as_str(), rule.level.as_str()) {
        ("draft" | "retired", _) => RuleMode::Absent,
        ("approved", "must" | "should") | ("enforced", "should") => RuleMode::Advisory,
        ("enforced", "must") => RuleMode::Authoritative,
        _ => RuleMode::Authoritative,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckerBinding<'a> {
    Gate(&'a PinnedGate),
    AgentJudgement,
    ManualAttestation,
    Unavailable(String),
}

/// Resolve one typed checker from the approval pin. This never consults the
/// pack directory or mission worktree. A gate declaration whose own path
/// scope does not match the actual diff is unavailable for this evaluation:
/// silently skipping it would turn a scoped enforced MUST green by omission.
pub fn checker_binding<'a>(
    pin: &'a StandardsPin,
    rule: &PinnedRule,
    actual_paths: &[String],
) -> CheckerBinding<'a> {
    match rule.checker.as_deref() {
        Some("agent-judgement") => CheckerBinding::AgentJudgement,
        Some("manual-attestation") => CheckerBinding::ManualAttestation,
        Some(checker) => match checker.strip_prefix("gate:") {
            Some(id) if !id.is_empty() => match pin.gates.iter().find(|gate| gate.id == id) {
                Some(gate)
                    if crate::merge_gate::when_paths_match(&gate.when_paths, actual_paths) =>
                {
                    CheckerBinding::Gate(gate)
                }
                Some(_) => CheckerBinding::Unavailable(format!(
                    "checker `gate:{id}` is stage/path-incompatible with the actual diff"
                )),
                None => CheckerBinding::Unavailable(format!(
                    "checker `gate:{id}` has no approval-pinned gate declaration"
                )),
            },
            _ => CheckerBinding::Unavailable(format!(
                "unknown checker `{checker}`; supported forms are gate:<id>, \
                 agent-judgement, and manual-attestation"
            )),
        },
        None => CheckerBinding::Unavailable("rule has no approval-pinned checker".to_string()),
    }
}

/// Stable union of rules applicable at any requested stage against actual
/// paths. A rule listed for validation AND merge appears once.
pub fn applicable_rules(
    pin: &StandardsPin,
    stages: &[RuleStage],
    actual_paths: &[String],
) -> Vec<PinnedRule> {
    let mut rules = std::collections::BTreeMap::new();
    for stage in stages {
        for rule in resolve_pin(pin, *stage, &TouchInput::Actual(actual_paths)) {
            if rule_mode(&rule) != RuleMode::Absent {
                rules.entry(rule.id.clone()).or_insert(rule);
            }
        }
    }
    rules.into_values().collect()
}

pub fn citation(pin: &StandardsPin, rule: &PinnedRule) -> RuleCitation {
    RuleCitation {
        id: rule.id.clone(),
        revision: rule.revision,
        source: format!("{} {}", pin.pack_name, pin.standards_root),
        digest: pin.digest.clone(),
        lifecycle: rule.effective_status.clone(),
        level: rule.level.clone(),
        checker: rule.checker.clone(),
    }
}

/// Canonical failure shape shared by deterministic, contextual, manual, and
/// unavailable-checker paths. Keeping it stable is important: the D-I waiver
/// fingerprint binds every byte of this finding and must survive a re-check
/// of the same unchanged diff.
pub fn failure_finding(pin: &StandardsPin, rule: &PinnedRule, evidence: &str) -> Finding {
    Finding {
        subject: format!("flight-rule:{}", rule.id),
        severity: if rule_mode(rule) == RuleMode::Authoritative {
            "critical".to_string()
        } else {
            "major".to_string()
        },
        evidence: evidence.to_string(),
        suggested_fix: format!(
            "bring the change into compliance with {} r{} or request an authorized exact waiver when the rule permits one",
            rule.id, rule.revision
        ),
        class: if rule_mode(rule) == RuleMode::Authoritative {
            "standards-authoritative".to_string()
        } else {
            "standards-advisory".to_string()
        },
        rule: Some(citation(pin, rule)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PinnedGate, StandardsPinSource};

    fn rule(status: &str, level: &str, checker: Option<&str>) -> PinnedRule {
        PinnedRule {
            id: "ZZ-RULE-001".to_string(),
            revision: 1,
            rfc: "RFC-001".to_string(),
            level: level.to_string(),
            effective_status: status.to_string(),
            statement: "Do the safe thing.".to_string(),
            domains: Vec::new(),
            stages: vec!["validation".to_string(), "merge".to_string()],
            when_paths: Vec::new(),
            task_classes: Vec::new(),
            checker: checker.map(str::to_string),
            waivable: true,
        }
    }

    fn pin(rule: PinnedRule) -> StandardsPin {
        StandardsPin {
            pack_name: "zz-pack".to_string(),
            pack_dir: "vendor/pack".to_string(),
            standards_root: "standards".to_string(),
            digest: "ab".repeat(32),
            source: StandardsPinSource::RepoTracked,
            task_class: None,
            touch_set: vec!["src/**".to_string()],
            gates: vec![PinnedGate {
                id: "zz-gate".to_string(),
                command: "true".to_string(),
                when_paths: Vec::new(),
            }],
            rules: vec![rule],
        }
    }

    #[test]
    fn flight_rules_enforcement_lifecycle_level_matrix_is_exact() {
        for (status, level, expected) in [
            ("draft", "must", RuleMode::Absent),
            ("retired", "must", RuleMode::Absent),
            ("approved", "must", RuleMode::Advisory),
            ("approved", "should", RuleMode::Advisory),
            ("enforced", "should", RuleMode::Advisory),
            ("enforced", "must", RuleMode::Authoritative),
        ] {
            assert_eq!(
                rule_mode(&rule(status, level, Some("gate:zz-gate"))),
                expected
            );
        }
        assert_eq!(
            rule_mode(&rule("unknown", "unknown", None)),
            RuleMode::Authoritative,
            "corrupt pins fail closed"
        );
    }

    #[test]
    fn flight_rules_enforcement_checker_uses_only_pinned_gate_binding() {
        let enforced_rule = rule("enforced", "must", Some("gate:zz-gate"));
        let pin = pin(enforced_rule.clone());
        let CheckerBinding::Gate(gate) =
            checker_binding(&pin, &enforced_rule, &["src/x.rs".into()])
        else {
            panic!("expected pinned gate")
        };
        assert_eq!(gate.command, "true");

        let missing = rule("enforced", "must", Some("gate:not-there"));
        let CheckerBinding::Unavailable(reason) = checker_binding(&pin, &missing, &[]) else {
            panic!("missing binding must fail closed")
        };
        assert!(reason.contains("no approval-pinned"), "{reason}");
    }

    #[test]
    fn flight_rules_enforcement_stage_selection_is_stable_and_nonduplicating() {
        let rule = rule("enforced", "must", Some("gate:zz-gate"));
        let pin = pin(rule);
        let selected = applicable_rules(
            &pin,
            &[RuleStage::Validation, RuleStage::Merge],
            &["src/x.rs".to_string()],
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "ZZ-RULE-001");
    }
}
