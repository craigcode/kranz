//! Event envelope and event set (plan §4.3) — the resumability backbone.
//!
//! CONTRACT FILE — do not modify in implementation phases. If a change seems
//! necessary, report it instead of editing.
//!
//! Every line of `events.jsonl` is one `Event` serialized as:
//! `{ "seq": 412, "ts": "...", "missionId": "m-01", "type": "worker.completed", "payload": { ... } }`
//!
//! Rules (§4.3):
//! - `seq` is monotonically increasing, assigned by the single writer (the
//!   engine). Gaps are a corruption signal; the loader must detect and refuse.
//! - Every status value in the data model must be reachable through some
//!   event, or the reducer has a dead state (enforced by test).

use crate::types::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub mission_id: String,
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Serialized as `"type": "<dotted.name>", "payload": { ... }`.
// large_enum_variant: MissionCreated carries the full MissionConfig (~456B).
// It occurs once per mission and events are I/O-bound; boxing would ripple
// through every construction/match site for no measurable win.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum EventKind {
    #[serde(rename = "mission.created")]
    MissionCreated {
        goal: String,
        #[serde(rename = "baseBranch")]
        base_branch: String,
        #[serde(rename = "missionBranch")]
        mission_branch: String,
        config: MissionConfig,
    },

    #[serde(rename = "plan.approved")]
    PlanApproved {
        plan: Plan,
        /// Base-branch commit SHA pinned at approval time (validation
        /// contract diffs against this, not the moving base branch).
        #[serde(rename = "baseSha", default, skip_serializing_if = "Option::is_none")]
        base_sha: Option<String>,
    },

    #[serde(rename = "milestone.started")]
    MilestoneStarted {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        /// SHA at milestone start; validators diff start..HEAD (§4.4).
        #[serde(rename = "startSha")]
        start_sha: String,
    },

    #[serde(rename = "feature.started")]
    FeatureStarted {
        #[serde(rename = "featureId")]
        feature_id: String,
    },

    #[serde(rename = "worker.spawned")]
    WorkerSpawned {
        #[serde(rename = "runId")]
        run_id: String,
        role: Role,
        #[serde(rename = "featureId", skip_serializing_if = "Option::is_none")]
        feature_id: Option<String>,
        #[serde(rename = "milestoneId", skip_serializing_if = "Option::is_none")]
        milestone_id: Option<String>,
        #[serde(rename = "sdkSessionId")]
        sdk_session_id: String,
        model: String,
        #[serde(rename = "promptHash")]
        prompt_hash: String,
        #[serde(rename = "transcriptPath")]
        transcript_path: String,
    },

    /// Throttled stream deltas; also carries `denied` guardrail hits (§4.7).
    #[serde(rename = "worker.message")]
    WorkerMessage {
        #[serde(rename = "runId")]
        run_id: String,
        /// "text" | "tool-use" | "tool-result" | "denied" | "system"
        tag: String,
        /// Scrubbed + truncated human-readable content.
        content: String,
    },

    #[serde(rename = "worker.completed")]
    WorkerCompleted {
        #[serde(rename = "runId")]
        run_id: String,
        result: RunResult,
        tokens: TokenUsage,
        #[serde(rename = "costUsd", skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        report: Option<WorkerReport>,
    },

    #[serde(rename = "feature.completed")]
    FeatureCompleted {
        #[serde(rename = "featureId")]
        feature_id: String,
        commits: Vec<String>,
    },

    #[serde(rename = "feature.failed")]
    FeatureFailed {
        #[serde(rename = "featureId")]
        feature_id: String,
        reason: String,
    },

    #[serde(rename = "feature.skipped")]
    FeatureSkipped {
        #[serde(rename = "featureId")]
        feature_id: String,
        reason: String,
    },

    #[serde(rename = "milestone.validating")]
    MilestoneValidating {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
    },

    #[serde(rename = "validation.finding")]
    ValidationFinding {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        #[serde(rename = "runId")]
        run_id: String,
        finding: Finding,
    },

    /// Orchestrator converted findings into a fix-feature (origin: fix).
    #[serde(rename = "fixfeature.created")]
    FixFeatureCreated {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        feature: Feature,
    },

    #[serde(rename = "milestone.blocked")]
    MilestoneBlocked {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        reason: String,
    },

    #[serde(rename = "milestone.unblocked")]
    MilestoneUnblocked {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        /// e.g. "raised fix-cycle cap", "user skipped findings"
        reason: String,
    },

    #[serde(rename = "milestone.completed")]
    MilestoneCompleted {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tag: Option<String>,
    },

    /// Final contract gate started (plan §4.5).
    #[serde(rename = "mission.validating")]
    MissionValidating {},

    #[serde(rename = "mission.paused")]
    MissionPaused {},

    #[serde(rename = "mission.resumed")]
    MissionResumed {},

    #[serde(rename = "user.message")]
    UserMessage { text: String, interrupt: bool },

    #[serde(rename = "orchestrator.decision")]
    OrchestratorDecision {
        summary: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },

    #[serde(rename = "secret.redacted")]
    SecretRedacted {
        #[serde(rename = "ruleId")]
        rule_id: String,
        fingerprint: String,
        location: String,
    },

    #[serde(rename = "config.changed")]
    ConfigChanged { patch: serde_json::Value },

    #[serde(rename = "mission.completed")]
    MissionCompleted {},

    #[serde(rename = "mission.failed")]
    MissionFailed { reason: String },

    /// Operator retired the mission (`kranz abandon`) — terminal, not a failure.
    #[serde(rename = "mission.abandoned")]
    MissionAbandoned { reason: String },
}

impl EventKind {
    /// The dotted wire name of this event (matches the serde rename).
    pub fn type_name(&self) -> &'static str {
        match self {
            EventKind::MissionCreated { .. } => "mission.created",
            EventKind::PlanApproved { .. } => "plan.approved",
            EventKind::MilestoneStarted { .. } => "milestone.started",
            EventKind::FeatureStarted { .. } => "feature.started",
            EventKind::WorkerSpawned { .. } => "worker.spawned",
            EventKind::WorkerMessage { .. } => "worker.message",
            EventKind::WorkerCompleted { .. } => "worker.completed",
            EventKind::FeatureCompleted { .. } => "feature.completed",
            EventKind::FeatureFailed { .. } => "feature.failed",
            EventKind::FeatureSkipped { .. } => "feature.skipped",
            EventKind::MilestoneValidating { .. } => "milestone.validating",
            EventKind::ValidationFinding { .. } => "validation.finding",
            EventKind::FixFeatureCreated { .. } => "fixfeature.created",
            EventKind::MilestoneBlocked { .. } => "milestone.blocked",
            EventKind::MilestoneUnblocked { .. } => "milestone.unblocked",
            EventKind::MilestoneCompleted { .. } => "milestone.completed",
            EventKind::MissionValidating { .. } => "mission.validating",
            EventKind::MissionPaused {} => "mission.paused",
            EventKind::MissionResumed {} => "mission.resumed",
            EventKind::UserMessage { .. } => "user.message",
            EventKind::OrchestratorDecision { .. } => "orchestrator.decision",
            EventKind::SecretRedacted { .. } => "secret.redacted",
            EventKind::ConfigChanged { .. } => "config.changed",
            EventKind::MissionCompleted {} => "mission.completed",
            EventKind::MissionFailed { .. } => "mission.failed",
            EventKind::MissionAbandoned { .. } => "mission.abandoned",
        }
    }

    /// Lifecycle events are fsynced per append; stream deltas (worker.message)
    /// may be batched (§4.3).
    pub fn is_stream_delta(&self) -> bool {
        matches!(self, EventKind::WorkerMessage { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Plan;

    fn sample_plan() -> Plan {
        Plan {
            goal: "g".into(),
            validation_contract: vec![],
            milestones: vec![],
            command_grants: vec![],
            touch_set: vec![],
        }
    }

    #[test]
    fn command_grants_backcompat_defaults_empty() {
        // A Plan JSON that omits commandGrants deserializes to an empty vec.
        let plan_json = r#"{
            "goal": "g",
            "validationContract": [],
            "milestones": []
        }"#;
        let plan: Plan = serde_json::from_str(plan_json).unwrap();
        assert!(plan.command_grants.is_empty());

        // A plan.approved event payload omitting commandGrants folds to an
        // empty vec on the nested plan too.
        let event_json = r#"{
            "seq": 1,
            "ts": "2026-01-02T03:04:05Z",
            "missionId": "m-1",
            "type": "plan.approved",
            "payload": {
                "plan": {
                    "goal": "g",
                    "validationContract": [],
                    "milestones": []
                }
            }
        }"#;
        let event: Event = serde_json::from_str(event_json).unwrap();
        match event.kind {
            EventKind::PlanApproved { plan, .. } => {
                assert!(plan.command_grants.is_empty())
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn touch_set_backcompat_defaults_empty() {
        // A Plan JSON that omits touchSet deserializes to an empty vec.
        let plan_json = r#"{
            "goal": "g",
            "validationContract": [],
            "milestones": []
        }"#;
        let plan: Plan = serde_json::from_str(plan_json).unwrap();
        assert!(plan.touch_set.is_empty());
    }

    #[test]
    fn touch_set_round_trips_through_serde() {
        let mut plan = sample_plan();
        plan.touch_set = vec!["src/**/*.rs".to_string(), "!src/generated/**".to_string()];
        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(
            json["touchSet"],
            serde_json::json!(["src/**/*.rs", "!src/generated/**"])
        );
        let round_tripped: Plan = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped.touch_set, plan.touch_set);
    }

    #[test]
    fn plan_approved_base_sha_backcompat() {
        // Some(sha) round-trips through serialization.
        let with_sha = EventKind::PlanApproved {
            plan: sample_plan(),
            base_sha: Some("deadbeef".to_string()),
        };
        let json = serde_json::to_value(&with_sha).unwrap();
        assert_eq!(json["payload"]["baseSha"], "deadbeef");
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::PlanApproved { base_sha, .. } => {
                assert_eq!(base_sha, Some("deadbeef".to_string()))
            }
            _ => panic!("wrong variant"),
        }

        // None is omitted from the wire (byte-identical to pre-baseSha logs)
        // and round-trips back to None.
        let without_sha = EventKind::PlanApproved {
            plan: sample_plan(),
            base_sha: None,
        };
        let json = serde_json::to_value(&without_sha).unwrap();
        assert!(
            !json["payload"].as_object().unwrap().contains_key("baseSha"),
            "payload must not contain baseSha when None: {json}"
        );
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::PlanApproved { base_sha, .. } => assert_eq!(base_sha, None),
            _ => panic!("wrong variant"),
        }

        // Old-log event JSON with no baseSha key at all still deserializes.
        let old_log = r#"{"type":"plan.approved","payload":{"plan":{"goal":"g","validationContract":[],"milestones":[]}}}"#;
        let event: EventKind = serde_json::from_str(old_log).unwrap();
        match event {
            EventKind::PlanApproved { base_sha, .. } => assert_eq!(base_sha, None),
            _ => panic!("wrong variant"),
        }
    }
}
