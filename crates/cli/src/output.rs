//! Pure terminal rendering: the status tree, the plan review, and the
//! pre-mission cost estimate line. No I/O here — everything returns String
//! so tests can assert on the exact output.

use kranz_engine::cost::CostEstimate;
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
    } else {
        format!("based on {missions_used} completed mission(s)")
    };
    format!(
        "estimated ${:.2}-${:.2} (expected ~${:.2}; rough estimate — live usage is \
         authoritative; {provenance})",
        estimate.low_usd, estimate.high_usd, estimate.expected_usd
    )
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
    use kranz_engine::types::{Assertion, AssertionCheck, MissionConfig, Plan, PlanFeature, PlanMilestone};

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
