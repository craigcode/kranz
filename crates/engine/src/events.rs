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

/// Default `quant` for worker.spawned events predating provenance fields.
fn default_quant() -> String {
    "n/a".to_string()
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

    #[serde(rename = "plan.revision.proposed")]
    PlanRevisionProposed {
        revision: u32,
        plan: Plan,
        instructions: String,
    },

    #[serde(rename = "plan.revised")]
    PlanRevised { revision: u32, plan: Plan },

    #[serde(rename = "plan.revision.rejected")]
    PlanRevisionRejected { revision: u32, reason: String },

    /// A run was stopped by a capability boundary and parks the milestone's
    /// validation for an operator approve/deny decision — the capability-denial
    /// analogue of `plan.revision.proposed`. `kind` selects the boundary: a
    /// `command` (validator command outside its allow-set → `command_grants`),
    /// a `touch-path` (worker write outside the `touch_set` → `touch_set`), a
    /// `worker-deny` (worker command blocked by a deny rule → `deny_exceptions`),
    /// or an `egress` (sandboxed run refused a destination by the egress proxy
    /// → `egress_grants`). Validators/sweeps are keyed to a milestone (they
    /// diff its start..HEAD), so this is too. Deny is the default; an
    /// unanswered request times out to `grant.denied`. `command` holds the
    /// target (a command, a path glob, a deny rule, or `host:port`).
    #[serde(rename = "grant.requested")]
    GrantRequested {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        #[serde(default)]
        kind: crate::types::GrantKind,
        command: String,
    },

    /// Operator approved the parked grant. The reducer extends the list `kind`
    /// selects (`command_grants`, `touch_set`, `deny_exceptions`, or
    /// `egress_grants`), extend-only, so the retried run clears the boundary.
    #[serde(rename = "grant.approved")]
    GrantApproved {
        #[serde(default)]
        kind: crate::types::GrantKind,
        command: String,
    },

    /// Operator denied the parked grant, or it timed out (deny-default). A
    /// denied command or egress grant blocks the milestone (refusal); a denied
    /// touch-path grant lets the out-of-contract write flow to the normal
    /// fix/waive path.
    #[serde(rename = "grant.denied")]
    GrantDenied {
        #[serde(default)]
        kind: crate::types::GrantKind,
        command: String,
        reason: String,
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
        /// Quantization of the model weights used for this run (provenance).
        #[serde(default = "default_quant")]
        quant: String,
        /// Hash of the model weights used for this run, when known (provenance).
        #[serde(
            rename = "weightHash",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        weight_hash: Option<String>,
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

    /// Orchestrator escalated the executor tier after repeated failed local
    /// validations, rather than blocking the milestone.
    #[serde(rename = "tier.escalated")]
    TierEscalated {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        from: ExecutorTier,
        to: ExecutorTier,
        reason: String,
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
        /// Operator guidance carried verbatim into the next validator task
        /// (and its retry). Folded into milestone state so it survives a
        /// process restart; replaced by each new unblock, cleared on
        /// milestone completion. Absent in pre-field logs.
        #[serde(
            rename = "validatorGuidance",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        validator_guidance: Option<String>,
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

    /// A [`crate::workspace_provider::WorkspaceProvider`] provisioned the
    /// mission workspace (design D-B/D-E): records the provider kind and the
    /// execution cwd so the audit trail names the environment workers ran
    /// in. Emitted once per `run()` invocation, before readiness.
    #[serde(rename = "workspace.provisioned")]
    WorkspaceProvisioned {
        #[serde(default)]
        provider: String,
        #[serde(default)]
        cwd: String,
        /// Additive (ticket `local-container-workspace`): provider-specific
        /// detail — the container provider records its compose project name.
        /// Absent on old logs and for providers without extra detail.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        /// Additive (ticket `workspace-remote-coder-provider`): the
        /// substrate-reported takeover URL (SSH/web) for remote providers.
        /// Absent on old logs and for local kinds (their takeover truth is
        /// the workspace cwd — no SSH/remote fiction).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        takeover: Option<String>,
        /// Additive (ticket `workspace-remote-coder-provider`): previews as
        /// provisioned — the substrate-reported URLs name-matched to the
        /// contract's `previews[]`. Absent on old logs and for local kinds
        /// (their placeholders derive from the contract itself).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previews: Option<Vec<crate::types::ProvisionedPreview>>,
    },

    /// The provider's readiness outcome (D-E: readiness status is a mission
    /// artifact). Emitted only when a workspace contract drove a real
    /// bootstrap/readiness execution — never with `outcome = "ready"` for a
    /// contract-less run, which would imply a runnable environment that does
    /// not exist (D-H). `detail` carries the scrubbed block reason on
    /// failure.
    #[serde(rename = "workspace.readiness")]
    WorkspaceReadinessReport {
        #[serde(default)]
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },

    /// Workspace teardown recorded (D-E). v1 local-worktree only ever calls
    /// `keep` — `hibernate`/`destroy` are accepted and recorded but no-ops
    /// for the local provider; the integration worktree's filesystem
    /// lifecycle stays with the existing mission-branch/merge machinery.
    #[serde(rename = "workspace.teardown")]
    WorkspaceTeardown {
        #[serde(default)]
        mode: String,
    },

    /// The effective workspace provider identity pinned at plan approval
    /// (design D-B, ticket `workspace-provider-pin-at-approval`) — the consent
    /// artifact recording WHAT was approved: provider kind, template
    /// (isolation mode for local kinds; the configured substrate
    /// template/image id for `remote`), and version (the workspace contract's
    /// schemaVersion, `"none"` without a contract, or the remote adapter
    /// version). Emitted in
    /// `approve_plan` immediately before `plan.approved`, so the log reads:
    /// contract validated → provider pinned → plan approved. See
    /// [`crate::types::WorkspacePin`] for the per-kind field meanings.
    #[serde(rename = "workspace.provider.pinned")]
    WorkspaceProviderPinned {
        #[serde(default)]
        provider: String,
        #[serde(default)]
        template: String,
        #[serde(default)]
        version: String,
    },
}

impl EventKind {
    /// The dotted wire name of this event (matches the serde rename).
    pub fn type_name(&self) -> &'static str {
        match self {
            EventKind::MissionCreated { .. } => "mission.created",
            EventKind::PlanApproved { .. } => "plan.approved",
            EventKind::PlanRevisionProposed { .. } => "plan.revision.proposed",
            EventKind::PlanRevised { .. } => "plan.revised",
            EventKind::PlanRevisionRejected { .. } => "plan.revision.rejected",
            EventKind::GrantRequested { .. } => "grant.requested",
            EventKind::GrantApproved { .. } => "grant.approved",
            EventKind::GrantDenied { .. } => "grant.denied",
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
            EventKind::TierEscalated { .. } => "tier.escalated",
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
            EventKind::WorkspaceProvisioned { .. } => "workspace.provisioned",
            EventKind::WorkspaceReadinessReport { .. } => "workspace.readiness",
            EventKind::WorkspaceTeardown { .. } => "workspace.teardown",
            EventKind::WorkspaceProviderPinned { .. } => "workspace.provider.pinned",
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
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
        }
    }

    #[test]
    fn workspace_lifecycle_events_wire_names_and_payloads_round_trip() {
        let provisioned = EventKind::WorkspaceProvisioned {
            provider: "local-worktree".into(),
            cwd: "/tmp/m-1_integration".into(),
            detail: None,
            takeover: None,
            previews: None,
        };
        let json = serde_json::to_value(&provisioned).unwrap();
        assert_eq!(json["type"], "workspace.provisioned");
        assert_eq!(json["payload"]["provider"], "local-worktree");
        assert_eq!(json["payload"]["cwd"], "/tmp/m-1_integration");
        assert_eq!(provisioned.type_name(), "workspace.provisioned");
        let back: EventKind = serde_json::from_value(json).unwrap();
        assert!(matches!(back, EventKind::WorkspaceProvisioned { .. }));

        let readiness = EventKind::WorkspaceReadinessReport {
            outcome: "failed".into(),
            detail: Some("workspace gate: readiness check 1/1 failed".into()),
        };
        let json = serde_json::to_value(&readiness).unwrap();
        assert_eq!(json["type"], "workspace.readiness");
        assert_eq!(json["payload"]["outcome"], "failed");
        assert_eq!(readiness.type_name(), "workspace.readiness");
        // detail is omitted from the wire when None.
        let no_detail = EventKind::WorkspaceReadinessReport {
            outcome: "ready".into(),
            detail: None,
        };
        let json = serde_json::to_value(&no_detail).unwrap();
        assert!(
            !json["payload"].as_object().unwrap().contains_key("detail"),
            "payload must not contain detail when None: {json}"
        );

        let teardown = EventKind::WorkspaceTeardown {
            mode: "keep".into(),
        };
        let json = serde_json::to_value(&teardown).unwrap();
        assert_eq!(json["type"], "workspace.teardown");
        assert_eq!(json["payload"]["mode"], "keep");
        assert_eq!(teardown.type_name(), "workspace.teardown");

        let pinned = EventKind::WorkspaceProviderPinned {
            provider: "local-worktree".into(),
            template: "worktree".into(),
            version: "1".into(),
        };
        let json = serde_json::to_value(&pinned).unwrap();
        assert_eq!(json["type"], "workspace.provider.pinned");
        assert_eq!(json["payload"]["provider"], "local-worktree");
        assert_eq!(json["payload"]["template"], "worktree");
        assert_eq!(json["payload"]["version"], "1");
        assert_eq!(pinned.type_name(), "workspace.provider.pinned");
        let back: EventKind = serde_json::from_value(json).unwrap();
        assert!(matches!(back, EventKind::WorkspaceProviderPinned { .. }));

        // Backcompat: a payload missing fields (or the whole payload, as a
        // hand-written or future-trimmed log line might) folds with serde
        // defaults instead of failing the log read.
        let sparse: EventKind = serde_json::from_str(
            r#"{"type":"workspace.provider.pinned","payload":{"provider":"local-worktree"}}"#,
        )
        .unwrap();
        match sparse {
            EventKind::WorkspaceProviderPinned {
                provider,
                template,
                version,
            } => {
                assert_eq!(provider, "local-worktree");
                assert_eq!(template, "");
                assert_eq!(version, "");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// The additive `detail` on `workspace.provisioned` (ticket
    /// `local-container-workspace`): the container provider records its
    /// compose project name there; old log lines without it still fold with
    /// `detail = None`, and `None` never hits the wire.
    #[test]
    fn workspace_provisioned_detail_is_additive_and_old_logs_still_fold() {
        let with_detail = EventKind::WorkspaceProvisioned {
            provider: "container".into(),
            cwd: "/tmp/m-1_integration".into(),
            detail: Some("compose project kranz-ws-m-1".into()),
            takeover: None,
            previews: None,
        };
        let json = serde_json::to_value(&with_detail).unwrap();
        assert_eq!(json["payload"]["provider"], "container");
        assert_eq!(json["payload"]["detail"], "compose project kranz-ws-m-1");
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::WorkspaceProvisioned {
                provider, detail, ..
            } => {
                assert_eq!(provider, "container");
                assert_eq!(detail.as_deref(), Some("compose project kranz-ws-m-1"));
            }
            _ => panic!("wrong variant"),
        }

        // Old log line (pre-detail): folds with detail = None.
        let old: EventKind = serde_json::from_str(
            r#"{"type":"workspace.provisioned","payload":{"provider":"local-worktree","cwd":"/tmp/wt"}}"#,
        )
        .unwrap();
        match old {
            EventKind::WorkspaceProvisioned { detail, .. } => assert_eq!(detail, None),
            _ => panic!("wrong variant"),
        }

        // detail = None is omitted from the wire (additive, never breaks old
        // readers comparing payloads).
        let no_detail = EventKind::WorkspaceProvisioned {
            provider: "local-worktree".into(),
            cwd: "/tmp/wt".into(),
            detail: None,
            takeover: None,
            previews: None,
        };
        let json = serde_json::to_value(&no_detail).unwrap();
        assert!(
            !json["payload"].as_object().unwrap().contains_key("detail"),
            "payload must not contain detail when None: {json}"
        );
    }

    /// The additive remote-kind fields on `workspace.provisioned` (ticket
    /// `workspace-remote-coder-provider`): takeover + name-matched previews
    /// (with the substrate's auth report) round-trip, old log lines without
    /// them fold to None, and None never hits the wire.
    #[test]
    fn remote_workspace_provisioned_fields_are_additive_and_old_logs_still_fold() {
        let remote = EventKind::WorkspaceProvisioned {
            provider: "remote".into(),
            cwd: "/tmp/m-1_integration".into(),
            detail: Some("substrate workspace kranz-remote-m-1 (id ws-1)".into()),
            takeover: Some("https://coder.example.com/@me/ws-1".into()),
            previews: Some(vec![crate::types::ProvisionedPreview {
                name: "app".into(),
                url: "https://app.example.com".into(),
                auth: Some(true),
            }]),
        };
        let json = serde_json::to_value(&remote).unwrap();
        assert_eq!(
            json["payload"]["takeover"],
            "https://coder.example.com/@me/ws-1"
        );
        assert_eq!(
            json["payload"]["previews"],
            serde_json::json!([{"name": "app", "url": "https://app.example.com", "auth": true}])
        );
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::WorkspaceProvisioned {
                takeover, previews, ..
            } => {
                assert_eq!(
                    takeover.as_deref(),
                    Some("https://coder.example.com/@me/ws-1")
                );
                assert_eq!(
                    previews,
                    Some(vec![crate::types::ProvisionedPreview {
                        name: "app".into(),
                        url: "https://app.example.com".into(),
                        auth: Some(true),
                    }])
                );
            }
            _ => panic!("wrong variant"),
        }

        // Old log line (pre-remote): folds with takeover/previews = None…
        let old: EventKind = serde_json::from_str(
            r#"{"type":"workspace.provisioned","payload":{"provider":"local-worktree","cwd":"/tmp/wt"}}"#,
        )
        .unwrap();
        match old {
            EventKind::WorkspaceProvisioned {
                takeover, previews, ..
            } => {
                assert_eq!(takeover, None);
                assert_eq!(previews, None);
            }
            _ => panic!("wrong variant"),
        }

        // …and None stays off the wire (additive, never breaks old readers).
        let local = EventKind::WorkspaceProvisioned {
            provider: "local-worktree".into(),
            cwd: "/tmp/wt".into(),
            detail: None,
            takeover: None,
            previews: None,
        };
        let json = serde_json::to_value(&local).unwrap();
        let payload = json["payload"].as_object().unwrap();
        assert!(
            !payload.contains_key("takeover") && !payload.contains_key("previews"),
            "local kinds must not carry the remote fields: {json}"
        );

        // A preview whose substrate did not report auth omits the key (never
        // read as "no auth").
        let preview = serde_json::to_value(crate::types::ProvisionedPreview {
            name: "app".into(),
            url: "https://app.example.com".into(),
            auth: None,
        })
        .unwrap();
        assert!(
            !preview.as_object().unwrap().contains_key("auth"),
            "auth absent from the wire when the substrate did not say: {preview}"
        );
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
    fn milestone_unblocked_guidance_backcompat_and_round_trip() {
        // Pre-field wire shape (logs written before validatorGuidance
        // existed) must still parse, defaulting to None.
        let old_json = r#"{
            "seq": 4,
            "ts": "2026-01-02T03:04:05Z",
            "missionId": "m-1",
            "type": "milestone.unblocked",
            "payload": { "milestoneId": "ms-1", "reason": "cap raised" }
        }"#;
        let event: Event = serde_json::from_str(old_json).unwrap();
        match event.kind {
            EventKind::MilestoneUnblocked {
                validator_guidance, ..
            } => assert_eq!(validator_guidance, None),
            _ => panic!("wrong variant"),
        }

        // The new field serializes when present (camelCase wire name) and is
        // omitted when absent (byte-identical to old logs).
        let with = EventKind::MilestoneUnblocked {
            milestone_id: "ms-1".into(),
            reason: "r".into(),
            validator_guidance: Some("FMT FIRST".into()),
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["payload"]["validatorGuidance"], "FMT FIRST");
        let without = EventKind::MilestoneUnblocked {
            milestone_id: "ms-1".into(),
            reason: "r".into(),
            validator_guidance: None,
        };
        let json = serde_json::to_value(&without).unwrap();
        assert!(json["payload"].get("validatorGuidance").is_none());
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
