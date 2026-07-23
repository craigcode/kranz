//! AMM-compatible readiness projection over kranz-native signals.
//!
//! The factory-autonomy-maturity-model whitepaper's L1–L5, **mapped — never
//! adopted**: kranz-native signals (ready.rs dimensions + contract health)
//! are the source of truth, and the AMM level is a derived VIEW for people
//! who think in the paper's vocabulary. The paper is silent on the
//! contract/consent axis that is kranz's differentiator, so L4+ require
//! mission history the paper cannot measure.

use crate::ready::{ReadyDimension, ReadyStatus};
use kranz_engine::contract_health::ContractHealth;
use serde::Serialize;

/// Bump on ANY change to the ladder below; `--json` consumers pin on it.
pub const MAPPING_VERSION: u32 = 1;

/// AMM maturity level. Serializes as "L1".."L5".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum AmmLevel {
    L1,
    L2,
    L3,
    L4,
    L5,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AmmProjection {
    pub level: AmmLevel,
    /// Signals missing for the NEXT level up (empty at L5).
    pub missing_signals: Vec<String>,
    pub mapping_version: u32,
}

/// THE ladder — the ONLY place AMM levels meet kranz-native dimensions.
///
/// Levels are ORDINAL LABELS, not sacred integers: Factory's
/// 5/19/36/60/100 point thresholds are marketing numbers that will drift
/// whenever the paper revises. What defines a level here is which native
/// signals it presupposes: (ready.rs dimension name, minimum status).
/// Levels are cumulative — L3 presupposes everything L2 does.
const LADDER: &[(AmmLevel, &str, ReadyStatus)] = &[
    // L2 — assisted: an agent can find instructions and a test command.
    (AmmLevel::L2, "planner instructions", ReadyStatus::Pass),
    (AmmLevel::L2, "test runner", ReadyStatus::Warn),
    (AmmLevel::L2, "repo docs", ReadyStatus::Warn),
    // L3 — guarded autonomy: contracts run against a gated, clean tree.
    (AmmLevel::L3, "CI config", ReadyStatus::Warn),
    (AmmLevel::L3, "merge gates", ReadyStatus::Pass),
    (AmmLevel::L3, "contract prerequisites", ReadyStatus::Pass),
    (AmmLevel::L3, "clean git state", ReadyStatus::Warn),
    (AmmLevel::L3, "kranz runtime gitignore", ReadyStatus::Warn),
    // L4 — orchestrated: multiple backend lanes and a calibration corpus,
    // plus mission history (the contract-health axis exists to be read).
    (AmmLevel::L4, "agent backend lanes", ReadyStatus::Warn),
    (AmmLevel::L4, "calibration corpus", ReadyStatus::Warn),
];

/// Contract-health thresholds for L5 (kranz-native numbers — ours to tune;
/// NOT Factory's). A flywheel repo's contract machinery is quiet: lint
/// catches author bugs before missions do, waivers are rare, and nothing
/// blocks on a contract bug.
const L5_MIN_LINT_PASS_RATE: f64 = 0.8;
const L5_MAX_WAIVERS_PER_MISSION: f64 = 0.5;

fn unmet_for(
    target: AmmLevel,
    dimensions: &[ReadyDimension],
    health: Option<&ContractHealth>,
) -> Vec<String> {
    let mut missing = Vec::new();
    for (level, name, min) in LADDER {
        if *level != target {
            continue;
        }
        let met = dimensions
            .iter()
            .find(|d| d.name == *name)
            .map(|d| d.status <= *min)
            .unwrap_or(false);
        if !met {
            missing.push((*name).to_string());
        }
    }
    match target {
        AmmLevel::L4 => {
            if health.is_none() {
                missing.push("mission history (contract-health axis)".to_string());
            }
        }
        AmmLevel::L5 => {
            if let Some(h) = health {
                if h.lint_pass_rate.unwrap_or(0.0) < L5_MIN_LINT_PASS_RATE {
                    missing.push(format!("contract-lint pass rate ≥ {L5_MIN_LINT_PASS_RATE}"));
                }
                if h.waivers_per_mission.unwrap_or(f64::MAX) > L5_MAX_WAIVERS_PER_MISSION {
                    missing.push(format!(
                        "finding waivers per mission ≤ {L5_MAX_WAIVERS_PER_MISSION}"
                    ));
                }
                if h.blocked.contract_bug > 0 {
                    missing.push("zero contract-bug blocked events".to_string());
                }
            }
        }
        _ => {}
    }
    missing
}

/// Project the AMM level from native dimensions + optional contract health.
/// The level is the highest whose requirements are ALL met; missing_signals
/// are what blocks the next level.
pub fn project(dimensions: &[ReadyDimension], health: Option<&ContractHealth>) -> AmmProjection {
    let mut level = AmmLevel::L1;
    let mut missing = unmet_for(AmmLevel::L2, dimensions, health);
    for target in [AmmLevel::L2, AmmLevel::L3, AmmLevel::L4, AmmLevel::L5] {
        let unmet = unmet_for(target, dimensions, health);
        if unmet.is_empty() {
            level = target;
            missing = Vec::new();
        } else {
            missing = unmet;
            break;
        }
    }
    AmmProjection {
        level,
        missing_signals: missing,
        mapping_version: MAPPING_VERSION,
    }
}

pub fn level_label(level: AmmLevel) -> &'static str {
    match level {
        AmmLevel::L1 => "L1",
        AmmLevel::L2 => "L2",
        AmmLevel::L3 => "L3",
        AmmLevel::L4 => "L4",
        AmmLevel::L5 => "L5",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ready::ReadyStatus::*;

    fn dim(name: &'static str, status: ReadyStatus) -> ReadyDimension {
        ReadyDimension {
            name,
            score: 0,
            weight: 0,
            status,
            evidence: String::new(),
            remedy: String::new(),
        }
    }

    /// A report shaped to satisfy exactly the levels below `top_unmet`'s.
    fn dims(failing: &[&'static str]) -> Vec<ReadyDimension> {
        [
            "planner instructions",
            "test runner",
            "repo docs",
            "CI config",
            "merge gates",
            "contract prerequisites",
            "clean git state",
            "kranz runtime gitignore",
            "agent backend lanes",
            "calibration corpus",
        ]
        .into_iter()
        .map(|name| {
            let status = if failing.contains(&name) { Fail } else { Pass };
            dim(name, status)
        })
        .collect()
    }

    fn strong_health() -> ContractHealth {
        ContractHealth {
            missions: 10,
            lint_linted: 10,
            lint_clean: 9,
            lint_pass_rate: Some(0.9),
            finding_waivers: 2,
            missions_with_waivers: 2,
            waivers_per_mission: Some(0.2),
            blocked: Default::default(),
        }
    }

    #[test]
    fn full_repo_with_strong_health_is_l5() {
        let p = project(&dims(&[]), Some(&strong_health()));
        assert_eq!(p.level, AmmLevel::L5);
        assert!(p.missing_signals.is_empty());
        assert_eq!(p.mapping_version, MAPPING_VERSION);
    }

    #[test]
    fn l3_boundary_stops_at_missing_l4_dimensions() {
        let p = project(&dims(&["agent backend lanes", "calibration corpus"]), None);
        assert_eq!(p.level, AmmLevel::L3);
        assert!(p
            .missing_signals
            .contains(&"agent backend lanes".to_string()));
        assert!(p
            .missing_signals
            .contains(&"calibration corpus".to_string()));
    }

    #[test]
    fn bare_repo_is_l1_with_l2_gaps_named() {
        let p = project(&[], None);
        assert_eq!(p.level, AmmLevel::L1);
        assert!(p
            .missing_signals
            .contains(&"planner instructions".to_string()));
    }

    #[test]
    fn l4_requires_mission_history() {
        // Every dimension green but no event logs: stops at L3, and the
        // missing signal names the contract-health axis rather than a pillar.
        let p = project(&dims(&[]), None);
        assert_eq!(p.level, AmmLevel::L3);
        assert!(p
            .missing_signals
            .contains(&"mission history (contract-health axis)".to_string()));
    }

    #[test]
    fn weak_contract_health_caps_at_l4() {
        let mut health = strong_health();
        health.lint_pass_rate = Some(0.5);
        health.blocked.contract_bug = 1;
        let p = project(&dims(&[]), Some(&health));
        assert_eq!(p.level, AmmLevel::L4);
        assert!(p
            .missing_signals
            .iter()
            .any(|m| m.contains("contract-lint pass rate")));
        assert!(p
            .missing_signals
            .contains(&"zero contract-bug blocked events".to_string()));
    }
}
