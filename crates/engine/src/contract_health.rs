//! Contract/consent health — the second readiness axis the AMM whitepaper
//! cannot see (the validator-repair-era lesson: the bites came from the
//! contract/consent machinery — author-broken assertions, scan friction,
//! grant stalls — not from the foundation signals the paper measures).
//!
//! Folded per repo from mission event logs, mirroring
//! [`crate::outcomes::compute_outcomes`]'s enumeration. Everything here is a
//! projection over events the engine already records; nothing new is
//! measured and no second source of truth is introduced.

use crate::events::{Event, EventKind};
use serde::Serialize;

/// Per-repo contract/consent health, folded from every mission's
/// `events.jsonl`. Raw counts are the payload; the derived rates are
/// convenience fields for JSON consumers (None when the denominator is 0).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractHealth {
    /// Missions with a readable event log.
    pub missions: u64,
    /// Missions whose approval-time contract lint produced a decision.
    pub lint_linted: u64,
    /// Linted missions with zero author-bug suspects (every command
    /// assertion correctly failed on the untouched base).
    pub lint_clean: u64,
    /// `lint_clean / lint_linted` (None when nothing was linted).
    pub lint_pass_rate: Option<f64>,
    /// Validation findings waived across all missions (the contract/
    /// validation friction counter).
    pub finding_waivers: u64,
    /// Missions with at least one waiver.
    pub missions_with_waivers: u64,
    /// `finding_waivers / missions` (None when no missions).
    pub waivers_per_mission: Option<f64>,
    /// milestone.blocked causes histogram.
    pub blocked: BlockedCauses,
}

/// `milestone.blocked` reasons classified by their emit-site templates
/// (orchestrator.rs). A cause is counted exactly once per blocked event.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedCauses {
    /// Command/egress grant denied/timed out ("validator command denied: …" /
    /// "egress denied: …").
    pub grant: u64,
    /// Dirty-tree checkpoint "refused by secret scan".
    pub secret_scan: u64,
    /// Author-broken assertion escalations ("appear buggy (false negative)"
    /// / "author-broken").
    pub contract_bug: u64,
    /// "fix-cycle cap" reached (validation round or final gate).
    pub fix_cycle_cap: u64,
    /// Validator "did not produce a trusted report" after retry.
    pub untrusted_validator: u64,
    /// Anything else (operator blocks, legacy reasons).
    pub other: u64,
}

/// Classify one `milestone.blocked` reason string. Order matters only for
/// defense; the emit-site templates are mutually exclusive in practice.
fn classify_block(reason: &str) -> &'static str {
    if reason.starts_with("validator command denied:") || reason.starts_with("egress denied:") {
        "grant"
    } else if reason.contains("refused by secret scan") {
        "secret-scan"
    } else if reason.contains("appear buggy (false negative)") || reason.contains("author-broken") {
        "contract-bug"
    } else if reason.contains("fix-cycle cap") {
        "fix-cycle-cap"
    } else if reason.contains("did not produce a trusted report") {
        "untrusted-validator"
    } else {
        "other"
    }
}

/// Fold one mission's events into the repo-wide accumulator.
fn fold_mission(health: &mut ContractHealth, events: &[Event]) {
    health.missions += 1;
    let mut waived_here = false;
    for event in events {
        match &event.kind {
            EventKind::OrchestratorDecision { summary, .. } => {
                if let Some(rest) = summary.strip_prefix("contract lint:") {
                    health.lint_linted += 1;
                    if rest.contains("all command assertions correctly fail") {
                        health.lint_clean += 1;
                    }
                } else if let Some(rest) = summary.strip_prefix("waived") {
                    // "waived {N} finding(s): …" — parse N (default 1: a
                    // waiver decision with an unparseable count still waives).
                    let n = rest
                        .split_whitespace()
                        .next()
                        .and_then(|t| t.parse::<u64>().ok())
                        .unwrap_or(1);
                    health.finding_waivers += n;
                    waived_here = true;
                }
            }
            EventKind::MilestoneBlocked { reason, .. } => match classify_block(reason) {
                "grant" => health.blocked.grant += 1,
                "secret-scan" => health.blocked.secret_scan += 1,
                "contract-bug" => health.blocked.contract_bug += 1,
                "fix-cycle-cap" => health.blocked.fix_cycle_cap += 1,
                "untrusted-validator" => health.blocked.untrusted_validator += 1,
                _ => health.blocked.other += 1,
            },
            _ => {}
        }
    }
    if waived_here {
        health.missions_with_waivers += 1;
    }
}

/// Enumerate every mission under `repo_root` exactly as
/// [`crate::outcomes::compute_outcomes`] does and fold contract health.
/// Returns None when the repo has no mission history at all (the readiness
/// report omits the axis rather than showing a vacuous zero).
pub fn compute_contract_health(
    repo_root: &std::path::Path,
) -> anyhow::Result<Option<ContractHealth>> {
    let index_contents = std::fs::read_to_string(
        crate::paths::MissionPaths::new(repo_root, "_")
            .missions_dir()
            .join("index.md"),
    )
    .unwrap_or_default();

    let mut ids = crate::paths::MissionPaths::list_missions(repo_root);
    for id in crate::mission_catalog::mission_index_ids(&index_contents) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.sort();

    let mut health = ContractHealth::default();
    for id in ids {
        let paths = crate::paths::MissionPaths::new(repo_root, &id);
        let events_path = paths.events_file();
        if !events_path.is_file() {
            continue;
        }
        let events = match crate::event_log::EventLog::read_events(&events_path) {
            Ok(events) => events,
            Err(_) => continue, // corrupt log degrades per-mission, never fails
        };
        fold_mission(&mut health, &events);
    }

    if health.missions == 0 {
        return Ok(None);
    }
    health.lint_pass_rate =
        (health.lint_linted > 0).then(|| health.lint_clean as f64 / health.lint_linted as f64);
    health.waivers_per_mission = Some(health.finding_waivers as f64 / health.missions as f64);
    Ok(Some(health))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::{EventLog, LockForce};
    use crate::paths::MissionPaths;
    use crate::types::{Assertion, AssertionCheck, MissionConfig, Plan};
    use std::time::Duration;
    use tempfile::TempDir;

    /// Seed a mission's `events.jsonl` with the given kinds, in order.
    fn seed_mission(repo_root: &std::path::Path, id: &str, kinds: Vec<EventKind>) {
        let paths = MissionPaths::new(repo_root, id);
        let mut log = EventLog::acquire(&paths, id, Duration::ZERO, LockForce::No).unwrap();
        for kind in kinds {
            log.append(kind).unwrap();
        }
    }

    fn created() -> EventKind {
        EventKind::MissionCreated {
            goal: "g".into(),
            base_branch: "main".into(),
            mission_branch: "kranz/mission-x".into(),
            config: MissionConfig::default(),
        }
    }

    fn decision(summary: &str) -> EventKind {
        EventKind::OrchestratorDecision {
            summary: summary.into(),
            detail: None,
        }
    }

    fn blocked(reason: &str) -> EventKind {
        EventKind::MilestoneBlocked {
            milestone_id: "ms-1".into(),
            reason: reason.into(),
        }
    }

    #[allow(unused)]
    fn plan_with_commands(n: usize) -> Plan {
        Plan {
            goal: "g".into(),
            validation_contract: (0..n)
                .map(|i| Assertion {
                    id: format!("a-{i}"),
                    statement: "s".into(),
                    check: AssertionCheck::Command,
                    command: Some("true".into()),
                })
                .collect(),
            milestones: vec![],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
        }
    }

    #[test]
    fn empty_repo_has_no_contract_health() {
        let tmp = TempDir::new().unwrap();
        assert!(compute_contract_health(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn lint_waivers_and_blocked_causes_fold() {
        let tmp = TempDir::new().unwrap();
        seed_mission(
            tmp.path(),
            "m-1",
            vec![
                created(),
                decision("contract lint: all command assertions correctly fail on the untouched base"),
                decision("waived 2 finding(s): a-1, a-2"),
                blocked("validator command denied: `cargo test` — operator denied"),
                blocked("egress denied: `registry.npmjs.org:443` — operator denied"),
                blocked("dirty-tree checkpoint for f-1-1 refused by secret scan; the working tree still holds the refused content"),
                blocked("contract command assertion(s) a-3 appear buggy (false negative) — command still fails but the requirement is verified met"),
            ],
        );
        seed_mission(
            tmp.path(),
            "m-2",
            vec![
                created(),
                decision("contract lint: 1 author-bug suspect assertion(s) already pass on the untouched base — see plan.md"),
                blocked("2 validation finding(s) but the fix-cycle cap (1) is reached"),
                blocked("claude functional validation did not produce a trusted report after retry: crashed"),
                blocked("an operator mystery"),
            ],
        );

        let health = compute_contract_health(tmp.path()).unwrap().unwrap();
        assert_eq!(health.missions, 2);
        assert_eq!(health.lint_linted, 2);
        assert_eq!(health.lint_clean, 1);
        assert_eq!(health.lint_pass_rate, Some(0.5));
        assert_eq!(health.finding_waivers, 2);
        assert_eq!(health.missions_with_waivers, 1);
        assert_eq!(health.waivers_per_mission, Some(1.0));
        assert_eq!(health.blocked.grant, 2);
        assert_eq!(health.blocked.secret_scan, 1);
        assert_eq!(health.blocked.contract_bug, 1);
        assert_eq!(health.blocked.fix_cycle_cap, 1);
        assert_eq!(health.blocked.untrusted_validator, 1);
        assert_eq!(health.blocked.other, 1);
    }

    #[test]
    fn waiver_with_unparseable_count_still_counts_one() {
        let tmp = TempDir::new().unwrap();
        seed_mission(
            tmp.path(),
            "m-1",
            vec![created(), decision("waived findings after operator review")],
        );
        let health = compute_contract_health(tmp.path()).unwrap().unwrap();
        assert_eq!(health.finding_waivers, 1);
        assert_eq!(health.missions_with_waivers, 1);
    }
}
