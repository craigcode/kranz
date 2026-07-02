//! Orchestrator context digest (plan §4.8).
//!
//! Every injected orchestrator turn is prefixed with a deterministic digest
//! rendered from [`MissionState`]: goal, contract, milestone/feature table,
//! recent decisions, open user messages (~1-2k tokens). Because it is a pure
//! function of state, [`render`] on the same state is byte-identical across
//! runs — the layout is snapshot-tested.
//!
//! Re-seed path (§4.5/§4.8): if the streaming orchestrator session dies and
//! `--resume` fails, [`render_reseed`] (digest + `plan.json`) is the ENTIRE
//! context a fresh session needs.

use crate::scrub::truncate_chars;
use crate::types::{
    AssertionCheck, FeatureOrigin, FeatureStatus, MilestoneStatus, MissionState, MissionStatus,
};

/// Max chars for individual titles, statements, commands, and user messages.
const ITEM_MAX: usize = 160;

/// Max chars for a decision summary line.
const DECISION_MAX: usize = 200;

/// Max decision lines rendered (oldest dropped; the reducer caps at the same
/// number, this is defense in depth).
const MAX_DECISIONS: usize = 10;

/// Render the deterministic mission digest. Layout is stable and
/// snapshot-tested — change it only together with the tests.
pub fn render(state: &MissionState) -> String {
    let mission = &state.mission;
    let mut out = String::new();

    out.push_str(&format!(
        "MISSION {} [{}] — {}\n",
        mission.id,
        mission_status(mission.status),
        truncate_chars(&mission.goal, ITEM_MAX)
    ));
    out.push_str(&format!(
        "branch {} (from {}) | tokens in/out {}/{} | cost ${:.2}\n",
        mission.mission_branch,
        mission.base_branch,
        state.totals.input,
        state.totals.output,
        state.total_cost_usd
    ));

    out.push_str("CONTRACT:\n");
    for assertion in &mission.validation_contract {
        out.push_str(&format!(
            "- [{}|{}] {}",
            assertion.id,
            check_kind(assertion.check),
            truncate_chars(&assertion.statement, ITEM_MAX)
        ));
        if let Some(command) = &assertion.command {
            out.push_str(&format!(" :: {}", truncate_chars(command, ITEM_MAX)));
        }
        out.push('\n');
    }

    out.push_str("MILESTONES:\n");
    for milestone in &mission.milestones {
        out.push_str(&format!(
            "{} [{}] {} (fixCycles {})\n",
            milestone.id,
            milestone_status(milestone.status),
            truncate_chars(&milestone.title, ITEM_MAX),
            milestone.fix_cycles
        ));
        for feature in &milestone.features {
            out.push_str(&format!(
                "  {} [{}|{}] {} (runs {}, respawns {})\n",
                feature.id,
                feature_status(feature.status),
                feature_origin(feature.origin),
                truncate_chars(&feature.title, ITEM_MAX),
                feature.worker_runs.len(),
                feature.respawns
            ));
        }
    }

    out.push_str("RECENT DECISIONS:\n");
    let skip = state.recent_decisions.len().saturating_sub(MAX_DECISIONS);
    for decision in &state.recent_decisions[skip..] {
        out.push_str(&format!("- {}\n", truncate_chars(decision, DECISION_MAX)));
    }

    out.push_str("OPEN USER MESSAGES:\n");
    if state.pending_user_messages.is_empty() {
        out.push_str("(none)\n");
    } else {
        for message in &state.pending_user_messages {
            out.push_str(&format!("- {}\n", truncate_chars(message, ITEM_MAX)));
        }
    }

    out.push_str("You are resuming from durable state; the event log is authoritative.");
    out
}

/// The full re-seed context (§4.8): the digest plus the approved plan JSON,
/// verbatim. Digest + plan.json is the ENTIRE context a fresh orchestrator
/// session is seeded with.
pub fn render_reseed(state: &MissionState, plan_json: &str) -> String {
    format!("{}\n\nAPPROVED PLAN (plan.json):\n{}", render(state), plan_json)
}

fn mission_status(status: MissionStatus) -> &'static str {
    match status {
        MissionStatus::Planning => "planning",
        MissionStatus::Running => "running",
        MissionStatus::Paused => "paused",
        MissionStatus::Blocked => "blocked",
        MissionStatus::Validating => "validating",
        MissionStatus::Complete => "complete",
        MissionStatus::Failed => "failed",
    }
}

fn milestone_status(status: MilestoneStatus) -> &'static str {
    match status {
        MilestoneStatus::Pending => "pending",
        MilestoneStatus::Active => "active",
        MilestoneStatus::Validating => "validating",
        MilestoneStatus::Complete => "complete",
        MilestoneStatus::Blocked => "blocked",
    }
}

fn feature_status(status: FeatureStatus) -> &'static str {
    match status {
        FeatureStatus::Pending => "pending",
        FeatureStatus::Active => "active",
        FeatureStatus::Complete => "complete",
        FeatureStatus::Skipped => "skipped",
        FeatureStatus::Failed => "failed",
    }
}

fn feature_origin(origin: FeatureOrigin) -> &'static str {
    match origin {
        FeatureOrigin::Plan => "plan",
        FeatureOrigin::Fix => "fix",
    }
}

fn check_kind(check: AssertionCheck) -> &'static str {
    match check {
        AssertionCheck::Command => "command",
        AssertionCheck::AgentJudgement => "judgement",
    }
}
