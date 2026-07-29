//! Pure terminal rendering: the status tree, the plan review, and the
//! pre-mission cost estimate line. No I/O here — everything returns String
//! so tests can assert on the exact output.

use kranz_engine::cost::{Confidence, CostEstimate, MIN_CALIBRATION_MISSIONS};
use kranz_engine::escalation_metrics::EscalationMetrics;
use kranz_engine::outcomes::Outcomes;
use kranz_engine::types::{
    AssertionCheck, FeatureStatus, MilestoneStatus, MissionState, MissionStatus, Plan,
};

/// Raw ANSI escape codes (no color crates; callers gate on a tty check).
pub mod ansi {
    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[2m";
    pub const RED: &str = "\x1b[31m";
    pub const GREEN: &str = "\x1b[32m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const BLUE: &str = "\x1b[34m";
    pub const MAGENTA: &str = "\x1b[35m";
    pub const CYAN: &str = "\x1b[36m";
}

/// UPPERCASE mission status for the status headline.
pub fn mission_status_label(status: MissionStatus) -> &'static str {
    match status {
        MissionStatus::Planning => "PLANNING",
        MissionStatus::Approved => "APPROVED",
        MissionStatus::Running => "RUNNING",
        MissionStatus::Paused => "PAUSED",
        MissionStatus::Blocked => "BLOCKED",
        MissionStatus::Validating => "VALIDATING",
        MissionStatus::Complete => "COMPLETE",
        MissionStatus::Failed => "FAILED",
        MissionStatus::Abandoned => "ABANDONED",
    }
}

/// Milestone status icon: pending ○, active ◐, validating ▶, complete ●,
/// blocked ✖.
pub fn milestone_icon(status: MilestoneStatus) -> char {
    match status {
        MilestoneStatus::Pending => '○',
        MilestoneStatus::Active => '◐',
        MilestoneStatus::Validating => '▶',
        MilestoneStatus::Complete => '●',
        MilestoneStatus::Blocked => '✖',
    }
}

/// Feature status icon: pending ○, active ◐, complete ●, skipped ⊘, failed ✗.
pub fn feature_icon(status: FeatureStatus) -> char {
    match status {
        FeatureStatus::Pending => '○',
        FeatureStatus::Active => '◐',
        FeatureStatus::Complete => '●',
        FeatureStatus::Skipped => '⊘',
        FeatureStatus::Failed => '✗',
    }
}

/// How many of the newest decisions the status view shows.
const STATUS_DECISIONS: usize = 3;

/// Render the `kranz status` terminal tree from a reduced state.
pub fn render_status(state: &MissionState) -> String {
    let mission = &state.mission;
    let mut out = String::new();

    out.push_str(&format!(
        "mission {}  {}  {}\n",
        mission.id,
        mission_status_label(mission.status),
        mission.goal
    ));
    out.push_str(&format!(
        "branch  {} (base {})\n",
        mission.mission_branch, mission.base_branch
    ));

    for milestone in &mission.milestones {
        out.push_str(&format!(
            "  [{}] {} {} (fixCycles {})\n",
            milestone_icon(milestone.status),
            milestone.id,
            milestone.title,
            milestone.fix_cycles
        ));
        for feature in &milestone.features {
            out.push_str(&format!(
                "      [{}] {} {} (runs {}, respawns {})\n",
                feature_icon(feature.status),
                feature.id,
                feature.title,
                feature.worker_runs.len(),
                feature.respawns
            ));
        }
    }

    out.push_str(&format!(
        "totals: tokens {} in / {} out, cache {} r / {} w, cost ${:.2}\n",
        state.totals.input,
        state.totals.output,
        state.totals.cache_read,
        state.totals.cache_write,
        state.total_cost_usd
    ));

    if !state.pending_user_messages.is_empty() {
        out.push_str("pending user messages:\n");
        for message in &state.pending_user_messages {
            out.push_str(&format!("  - {message}\n"));
        }
    }

    let skip = state
        .recent_decisions
        .len()
        .saturating_sub(STATUS_DECISIONS);
    let recent = &state.recent_decisions[skip..];
    if !recent.is_empty() {
        out.push_str(&format!("last {} decision(s):\n", recent.len()));
        for decision in recent {
            out.push_str(&format!("  - {decision}\n"));
        }
    }

    out
}

/// Render a plan for the interactive approval prompt: contract, then the
/// milestone/feature tree with specs and criteria.
pub fn render_plan(plan: &Plan) -> String {
    let mut out = String::new();
    out.push_str(&format!("PLAN — {}\n", plan.goal));

    out.push_str("validation contract:\n");
    if plan.validation_contract.is_empty() {
        out.push_str("  (none)\n");
    }
    for assertion in &plan.validation_contract {
        let id = if assertion.id.trim().is_empty() {
            "?"
        } else {
            assertion.id.as_str()
        };
        match assertion.check {
            AssertionCheck::Command => {
                let command = assertion
                    .command
                    .as_deref()
                    .map(|c| format!(" — `{c}`"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "  [{id}] (command) {}{command}\n",
                    assertion.statement
                ));
            }
            AssertionCheck::AgentJudgement => {
                out.push_str(&format!(
                    "  [{id}] (agent-judgement) {}\n",
                    assertion.statement
                ));
            }
        }
    }

    if let Some(alternatives) = &plan.considered_alternatives {
        out.push_str("considered alternatives:\n");
        out.push_str(&format!("  chosen: {}\n", alternatives.chosen.trim()));
        for rejected in &alternatives.rejected {
            out.push_str(&format!(
                "  rejected: {} — {}\n",
                rejected.approach.trim(),
                rejected.trade_off.trim()
            ));
        }
    }

    out.push_str("milestones:\n");
    for (mi, milestone) in plan.milestones.iter().enumerate() {
        out.push_str(&format!("  {}. {}\n", mi + 1, milestone.title));
        for (fi, feature) in milestone.features.iter().enumerate() {
            out.push_str(&format!("     {}.{} {}\n", mi + 1, fi + 1, feature.title));
            let mut spec_lines = feature.spec.lines();
            if let Some(first) = spec_lines.next() {
                out.push_str(&format!("         spec: {first}\n"));
            }
            for line in spec_lines {
                out.push_str(&format!("               {line}\n"));
            }
            for criterion in &feature.validation_criteria {
                out.push_str(&format!("         - {criterion}\n"));
            }
        }
    }
    out
}

/// The one-line cost estimate shown at plan review time, with the provenance
/// of its params: calibrated from `missions_used` completed missions, or the
/// built-in defaults when there are none. The range is wide on purpose; live
/// usage is always authoritative.
pub fn render_cost_estimate(estimate: &CostEstimate, missions_used: usize) -> String {
    let provenance = if missions_used == 0 {
        "built-in defaults — no completed missions yet".to_string()
    } else if missions_used < MIN_CALIBRATION_MISSIONS {
        format!(
            "per-run costs from {missions_used} completed mission(s), but too few to fit the range \
             yet — treat the low end as a floor until {MIN_CALIBRATION_MISSIONS}+ complete"
        )
    } else {
        format!("range fit to {missions_used} completed missions")
    };
    match estimate.confidence {
        Confidence::High => format!(
            "estimated ${:.2}-${:.2} (expected ~${:.2}; rough estimate — live usage is \
             authoritative; {provenance})",
            estimate.low_usd, estimate.high_usd, estimate.expected_usd
        ),
        Confidence::Low => format!(
            "estimated ${:.2}-${:.2} (expected ~${:.2}; doc-heavy / judgement-heavy shape — \
             the calibration corpus lacks a comparable mission, so this is LOW CONFIDENCE and \
             ${:.2} is a soft ceiling, not a tight bound; {provenance})",
            estimate.low_usd, estimate.high_usd, estimate.expected_usd, estimate.high_usd
        ),
    }
}

/// Render `kranz outcomes`'s default text view: an Autonomy section always,
/// then Grant latency and Escalation ledger sections — unless there is no
/// history at all (no closed missions, no escalations, no decided grants),
/// in which case only the Autonomy section (zeros) plus a short note is
/// printed, per the spec's empty-history rule.
pub fn render_outcomes(outcomes: &Outcomes) -> String {
    let ratio = &outcomes.autonomy_ratio;
    let mut out = String::new();

    out.push_str("Autonomy\n");
    out.push_str(&format!(
        "  interventions per closed mission: {:.2}\n",
        ratio.interventions_per_closed_mission
    ));
    out.push_str(&format!(
        "  zero-intervention share: {:.0}%\n",
        ratio.zero_intervention_share * 100.0
    ));
    out.push_str(&format!("  closed missions: {}\n", ratio.closed_missions));

    let has_history = ratio.closed_missions > 0
        || !outcomes.escalations.is_empty()
        || outcomes.grant_latency.total_decided > 0;

    if !has_history {
        out.push('\n');
        out.push_str("no grants or escalations recorded yet\n");
        return out;
    }

    out.push('\n');
    out.push_str("Grant latency\n");
    for bucket in &outcomes.grant_latency.buckets {
        out.push_str(&format!("  {}: {}\n", bucket.label, bucket.count));
    }
    out.push_str(&format!(
        "  total decided: {}\n",
        outcomes.grant_latency.total_decided
    ));

    let cost = &outcomes.cost_per_change;
    out.push('\n');
    out.push_str("Cost per change\n");
    match cost.usd_per_commit {
        Some(per) => out.push_str(&format!(
            "  ${per:.2} per non-meta commit ({} commits, ${:.2} total)\n",
            cost.non_meta_commits, cost.total_cost_usd
        )),
        None => out.push_str("  no non-meta commits recorded yet\n"),
    }

    let cycle = &outcomes.cycle_time;
    out.push('\n');
    out.push_str("Cycle time\n");
    match cycle.mean_ms {
        Some(mean) => out.push_str(&format!(
            "  mean {} across {} closed mission{} (paused spans excluded)\n",
            format_duration_ms(mean as u64),
            cycle.closed_missions,
            if cycle.closed_missions == 1 { "" } else { "s" }
        )),
        None => out.push_str("  no closed missions yet\n"),
    }

    out.push('\n');
    out.push_str("Escalation ledger\n");
    for row in &outcomes.escalations {
        let latency = row
            .latency_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&format!(
            "  {}  {}  {}  {}  {}  {}\n",
            row.ts.to_rfc3339(),
            row.mission_id,
            row.kind.as_str(),
            row.summary,
            row.decision,
            latency
        ));
    }

    out
}

/// Serialize `kranz outcomes --json`'s output — the source of truth for the
/// dashboard/Slack "identical data" claim (see the module doc for the
/// outcomes fold).
pub fn render_outcomes_json(outcomes: &Outcomes) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(outcomes)?)
}

/// Optional share as a percentage ("33%", or "—" when the denominator was 0).
fn fmt_share_pct(share: Option<f64>) -> String {
    share
        .map(|s| format!("{:.0}%", s * 100.0))
        .unwrap_or_else(|| "—".to_string())
}

/// Render `kranz escalation-metrics`'s default text view: Autonomy (split by
/// outcome), Rubber-stamp signal, False greens, and the Escalation ledger —
/// unless there is no history at all, in which case only the Autonomy section
/// plus a short note is printed (the outcomes empty-history rule).
pub fn render_escalation_metrics(metrics: &EscalationMetrics) -> String {
    let autonomy = &metrics.autonomy;
    let mut out = String::new();

    out.push_str("Autonomy\n");
    out.push_str(&format!(
        "  zero-intervention share: {} ({} of {} closed missions)\n",
        fmt_share_pct(autonomy.zero_intervention_share),
        autonomy.zero_intervention_missions,
        autonomy.closed_missions
    ));
    out.push_str(&format!(
        "  completed: {} ({} of {})   failed: {} ({} of {})\n",
        fmt_share_pct(autonomy.completed.zero_intervention_share),
        autonomy.completed.zero_intervention,
        autonomy.completed.missions,
        fmt_share_pct(autonomy.failed.zero_intervention_share),
        autonomy.failed.zero_intervention,
        autonomy.failed.missions
    ));

    let has_history = autonomy.closed_missions > 0
        || !metrics.ledger.is_empty()
        || metrics.rubber_stamp.decided_grants > 0
        || !metrics.false_greens.traced_defects.is_empty();
    if !has_history {
        out.push('\n');
        out.push_str("no escalations recorded yet\n");
        return out;
    }

    let stamp = &metrics.rubber_stamp;
    out.push('\n');
    out.push_str("Rubber-stamp signal\n");
    out.push_str(&format!(
        "  decided grants: {}   under 10s: {}\n",
        stamp.decided_grants, stamp.under_ten_seconds
    ));
    let fmt_ms = |ms: Option<u64>| {
        ms.map(format_duration_ms)
            .unwrap_or_else(|| "—".to_string())
    };
    out.push_str(&format!(
        "  p50: {}   p90: {}\n",
        fmt_ms(stamp.p50_ms),
        fmt_ms(stamp.p90_ms)
    ));

    let greens = &metrics.false_greens;
    out.push('\n');
    out.push_str("False greens\n");
    out.push_str(&format!(
        "  {} of {} completed missions ({}) produced a traced defect\n",
        greens.false_greens,
        greens.completed_missions,
        fmt_share_pct(greens.false_green_rate)
    ));
    out.push_str(&format!(
        "  with interventions: {} of {} ({})   zero-intervention: {} of {} ({})\n",
        greens.with_interventions.false_greens,
        greens.with_interventions.completed_missions,
        fmt_share_pct(greens.with_interventions.rate),
        greens.zero_intervention.false_greens,
        greens.zero_intervention.completed_missions,
        fmt_share_pct(greens.zero_intervention.rate)
    ));
    for defect in &greens.traced_defects {
        out.push_str(&format!(
            "  traced: {} → {}\n",
            defect.ticket, defect.mission_id
        ));
    }

    out.push('\n');
    out.push_str("Escalation ledger\n");
    for row in &metrics.ledger {
        let milestone = row.milestone_id.as_deref().unwrap_or("-");
        let latency = row
            .latency_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&format!(
            "  {}  {}  {}  {}  {}  {}  {}\n",
            row.ts.to_rfc3339(),
            row.mission_id,
            row.kind.as_str(),
            milestone,
            row.ask,
            row.decision,
            latency
        ));
    }

    out
}

/// Serialize `kranz escalation-metrics --json`'s output.
pub fn render_escalation_metrics_json(metrics: &EscalationMetrics) -> anyhow::Result<String> {
    Ok(serde_json::to_string_pretty(metrics)?)
}

/// Milliseconds as a compact duration ("12s", "47m", "2.3h", "3.1d") for
/// the cycle-time readout.
fn format_duration_ms(ms: u64) -> String {
    const S: u64 = 1_000;
    const M: u64 = 60 * S;
    const H: u64 = 60 * M;
    const D: u64 = 24 * H;
    if ms >= D {
        format!("{:.1}d", ms as f64 / D as f64)
    } else if ms >= H {
        format!("{:.1}h", ms as f64 / H as f64)
    } else if ms >= M {
        format!("{}m", ms / M)
    } else {
        format!("{}s", ms / S)
    }
}

/// Collapse whitespace/newlines into single spaces and truncate to `max`
/// characters (char-safe; appends `…` when truncated).
pub fn one_line(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let mut truncated: String = collapsed.chars().take(max.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use kranz_engine::events::{Event, EventKind};
    use kranz_engine::reducer::fold;
    use kranz_engine::types::{
        Assertion, AssertionCheck, MissionConfig, Plan, PlanFeature, PlanMilestone,
    };

    mod outcomes_cli {
        use super::*;
        use kranz_engine::event_log::{EventLog, LockForce};
        use kranz_engine::events::EventKind;
        use kranz_engine::outcomes::compute_outcomes;
        use kranz_engine::paths::MissionPaths;
        use kranz_engine::types::{GrantKind, MissionConfig};
        use std::time::Duration;
        use tempfile::TempDir;

        fn seed_mission(repo_root: &std::path::Path, id: &str, kinds: Vec<EventKind>) {
            let paths = MissionPaths::new(repo_root, id);
            let mut log = EventLog::acquire(&paths, id, Duration::ZERO, LockForce::No).unwrap();
            for kind in kinds {
                log.append(kind).unwrap();
            }
        }

        fn created(goal: &str) -> EventKind {
            EventKind::MissionCreated {
                goal: goal.into(),
                base_branch: "main".into(),
                mission_branch: "kranz/mission-x".into(),
                config: MissionConfig::default(),
            }
        }

        #[test]
        fn outcomes_cli_json_round_trips_to_compute_outcomes_value() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            seed_mission(
                root,
                "m-1",
                vec![
                    created("goal"),
                    EventKind::GrantRequested {
                        milestone_id: "ms-1".into(),
                        kind: GrantKind::Command,
                        command: "cargo test".into(),
                    },
                    EventKind::GrantApproved {
                        kind: GrantKind::Command,
                        command: "cargo test".into(),
                    },
                    EventKind::MissionCompleted {},
                ],
            );

            let expected = compute_outcomes(root).unwrap();
            let json = render_outcomes_json(&expected).unwrap();
            let round_tripped: Outcomes = serde_json::from_str(&json).unwrap();
            assert_eq!(round_tripped, expected);
        }

        #[test]
        fn outcomes_cli_empty_history_text_shows_autonomy_alone() {
            let tmp = TempDir::new().unwrap();
            let outcomes = compute_outcomes(tmp.path()).unwrap();

            let text = render_outcomes(&outcomes);
            assert!(text.contains("Autonomy"));
            assert!(text.contains("no grants or escalations recorded yet"));
            assert!(!text.contains("Grant latency"));
            assert!(!text.contains("Escalation ledger"));
        }

        #[test]
        fn outcomes_cli_populated_text_includes_all_sections() {
            let tmp = TempDir::new().unwrap();
            let root = tmp.path();
            seed_mission(
                root,
                "m-1",
                vec![
                    created("goal"),
                    EventKind::GrantRequested {
                        milestone_id: "ms-1".into(),
                        kind: GrantKind::Command,
                        command: "cargo test".into(),
                    },
                    EventKind::GrantApproved {
                        kind: GrantKind::Command,
                        command: "cargo test".into(),
                    },
                    EventKind::MissionCompleted {},
                ],
            );

            let outcomes = compute_outcomes(root).unwrap();
            let text = render_outcomes(&outcomes);
            assert!(text.contains("Autonomy"));
            assert!(text.contains("Grant latency"));
            assert!(text.contains("Cost per change"));
            assert!(text.contains("Cycle time"));
            assert!(text.contains("Escalation ledger"));
        }
    }

    mod escalation_metrics_cli {
        use super::*;
        use kranz_engine::escalation_metrics::{compute_escalation_metrics, EscalationMetrics};
        use kranz_engine::event_log::{EventLog, LockForce};
        use kranz_engine::events::EventKind;
        use kranz_engine::paths::MissionPaths;
        use kranz_engine::types::{GrantKind, MissionConfig};
        use std::time::Duration;
        use tempfile::TempDir;

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

        fn seed_repo(root: &std::path::Path) {
            seed_mission(
                root,
                "m-1",
                vec![
                    created(),
                    EventKind::GrantRequested {
                        milestone_id: "ms-1".into(),
                        kind: GrantKind::Command,
                        command: "cargo test".into(),
                    },
                    EventKind::GrantApproved {
                        kind: GrantKind::Command,
                        command: "cargo test".into(),
                    },
                    EventKind::MissionCompleted {},
                ],
            );
            seed_mission(root, "m-2", vec![created(), EventKind::MissionCompleted {}]);
            let tickets = kranz_engine::ticket::Ticket::tickets_dir(root);
            std::fs::create_dir_all(&tickets).unwrap();
            std::fs::write(
                tickets.join("defect-regression.md"),
                "---\ntitle: Regression\ntraced-from-mission: m-1\n---\n\n## Goal\nfix\n",
            )
            .unwrap();
        }

        #[test]
        fn escalation_metrics_cli_json_round_trips_to_compute_value() {
            let tmp = TempDir::new().unwrap();
            seed_repo(tmp.path());
            let expected = compute_escalation_metrics(tmp.path()).unwrap();
            let json = render_escalation_metrics_json(&expected).unwrap();
            let round_tripped: EscalationMetrics = serde_json::from_str(&json).unwrap();
            assert_eq!(round_tripped, expected);
        }

        #[test]
        fn escalation_metrics_cli_empty_history_text_shows_autonomy_alone() {
            let tmp = TempDir::new().unwrap();
            let metrics = compute_escalation_metrics(tmp.path()).unwrap();
            let text = render_escalation_metrics(&metrics);
            assert!(text.contains("Autonomy"));
            assert!(text.contains("no escalations recorded yet"));
            assert!(!text.contains("Rubber-stamp signal"));
            assert!(!text.contains("Escalation ledger"));
        }

        #[test]
        fn escalation_metrics_cli_populated_text_includes_all_sections() {
            let tmp = TempDir::new().unwrap();
            seed_repo(tmp.path());
            let metrics = compute_escalation_metrics(tmp.path()).unwrap();
            let text = render_escalation_metrics(&metrics);
            assert!(text.contains("Autonomy"));
            assert!(text.contains("Rubber-stamp signal"));
            assert!(text.contains("False greens"));
            assert!(text.contains("Escalation ledger"));
            // m-2 completed clean, m-1 did not; the defect traces to m-1.
            assert!(text.contains("zero-intervention share: 50% (1 of 2 closed missions)"));
            assert!(text.contains("1 of 2 completed missions (50%) produced a traced defect"));
            assert!(text.contains("traced: defect-regression → m-1"));
            assert!(text.contains("command: cargo test"));
        }
    }

    #[test]
    fn approved_status_label_is_uppercase() {
        assert_eq!(mission_status_label(MissionStatus::Approved), "APPROVED");
    }

    #[test]
    fn approved_status_folded_from_events_labels_as_approved_not_running() {
        let ts = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        let plan = Plan {
            goal: "build the thing".to_string(),
            validation_contract: vec![Assertion {
                id: "a-1".to_string(),
                statement: "cargo test passes".to_string(),
                check: AssertionCheck::Command,
                command: Some("cargo test".to_string()),
            }],
            milestones: vec![PlanMilestone {
                title: "milestone one".to_string(),
                features: vec![PlanFeature {
                    title: "alpha".to_string(),
                    spec: "spec for alpha".to_string(),
                    validation_criteria: vec!["alpha works".to_string()],
                }],
            }],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
        };
        let events = vec![
            Event {
                seq: 1,
                ts,
                mission_id: "m-1".to_string(),
                kind: EventKind::MissionCreated {
                    goal: "build the thing".to_string(),
                    base_branch: "main".to_string(),
                    mission_branch: "kranz/mission-m-1".to_string(),
                    config: MissionConfig::default(),
                },
            },
            Event {
                seq: 2,
                ts,
                mission_id: "m-1".to_string(),
                kind: EventKind::PlanApproved {
                    plan,
                    base_sha: None,
                },
            },
        ];
        let state = fold(&events).unwrap();
        let label = mission_status_label(state.mission.status);
        assert_eq!(label, "APPROVED");
        assert_ne!(label, "RUNNING");
    }
}
