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

    /// A validator session altered its checkout (validator immutability
    /// proof, ticket `validator-immutability-proof`): the HEAD/index/worktree
    /// identity assertion around every validator session found drift, so the
    /// round failed honestly — the milestone blocks, with no retry and no
    /// waivable finding. The payload records WHAT changed: HEAD before/after
    /// and the `git status --porcelain` entries gained/lost across the
    /// session. Additive event; absent in pre-field logs.
    #[serde(rename = "validator.tamper")]
    ValidatorTamper {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        #[serde(rename = "runId")]
        run_id: String,
        role: Role,
        /// HEAD when the session started.
        #[serde(rename = "headBefore")]
        head_before: String,
        /// HEAD when the session ended (== headBefore unless the session
        /// moved it, e.g. a validator-run `git commit`).
        #[serde(rename = "headAfter")]
        head_after: String,
        /// Porcelain entries present after but not before (the session's
        /// writes: ` M <path>`, `A  <path>`, `?? <path>`, …).
        appeared: Vec<String>,
        /// Porcelain entries present before but not after (the session
        /// reverted or hid a pre-existing dirty state — equally a mutation).
        resolved: Vec<String>,
        /// Whether `.git` metadata (config/hooks/refs) changed across the
        /// session — the checkout can look identical while the plumbing was
        /// weaponized (`core.fsmonitor`/`core.hooksPath` execute on the
        /// ENGINE's own git invocations; a moved ref retargets later merges).
        #[serde(default, rename = "gitMetadataChanged")]
        git_metadata_changed: bool,
        /// WHICH metadata surfaces changed (additive): `config` / `hooks` /
        /// `refs` / `index-flags` / `info-exclude` — a tripwire fire is
        /// diagnosable from the event alone.
        #[serde(default, rename = "gitMetadataFields")]
        git_metadata_fields: Vec<String>,
    },

    /// A validator session ran in a throwaway snapshot of the session
    /// checkout (copy-on-write immutable validator snapshot, the follow-up
    /// to ticket `validator-immutability-proof`; module
    /// [`crate::validator_snapshot`]): HEAD plus the worker's uncommitted
    /// diff and untracked files, a warmed `target/` copy, discarded after
    /// the session regardless of outcome. The payload records the snapshot
    /// path, which target-copy tier warmed it, and the creation cost.
    /// Additive event; absent in pre-field logs.
    #[serde(rename = "validation.snapshot")]
    ValidationSnapshot {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        role: Role,
        /// Absolute path of the (already discarded) snapshot worktree,
        /// under the mission's gitignored `runs/` scratch.
        path: String,
        /// How the snapshot's `target/` was warmed: "clonefile", "reflink",
        /// "copy", "fresh" (empty target — the cost is named in `detail`),
        /// or "absent" (no target/ in the session checkout).
        #[serde(rename = "targetTier")]
        target_tier: String,
        /// Wall-clock cost of building the snapshot (worktree add + diff
        /// apply + untracked copy + target warm), in milliseconds.
        #[serde(rename = "creationMs")]
        creation_ms: u64,
        /// Extra context — notably the named cost when `targetTier` is
        /// "fresh".
        #[serde(default)]
        detail: Option<String>,
    },

    /// One gate evaluation, recorded as a first-class event (ticket
    /// `.kranz/tickets/gate-results-first-class-events`, KRZ-312 — the
    /// governance evidence layer's last substrate gap before
    /// provenance-replay). Every [`crate::gate::GatePipeline`] evaluation
    /// emits one of these per gate, in pipeline order, carrying the gate id,
    /// its ladder position, the stated verdict, and the artefact handle —
    /// so the mission's full gate ladder replays from the log alone, with no
    /// dependency on external state that may have moved. Record-only: the
    /// reducer treats it as an audit record (like `secret.redacted`), never
    /// a state transition, so old logs without any gate.result fold
    /// unchanged.
    ///
    /// WHY `surface` is a first-class field: the same gate id is evaluated
    /// more than once per mission (approval and final gate), so gate id +
    /// ladder position cannot name ONE evaluation — and reconstructing the
    /// surface from neighbouring events would couple replay to emission
    /// order, exactly the external-state fragility this event abolishes.
    ///
    /// WHY there is no separate `section` field: [`crate::gate::GateKind`]
    /// selects the pipeline section one-to-one (gate.rs), so `kind` doubles
    /// as the section discriminator; `index` is the zero-based evaluation
    /// position WITHIN that section.
    ///
    /// Artefact discipline (ticket text): `artefactRef` is mission-relative
    /// or content-addressed, NEVER an absolute host path — a `file:`-schemed
    /// mission-relative path when the evidence is a file
    /// ([`crate::gate_results`]), or the gate-local handle verbatim (a
    /// command line, a description) when the evidence is inherently textual.
    /// A reference whose bytes are gone resolves to "unresolved", never to
    /// an error that blocks replay.
    #[serde(rename = "gate.result")]
    GateResult {
        /// Gate identity: the registered `Gate::name()` — e.g. a defect-class
        /// name (`vacuous-filter`), a pack gate name, `merge-gate-suite`.
        gate: String,
        /// Which evaluation surface ran the pipeline (see
        /// [`crate::gate::GateSurface`]).
        surface: crate::gate::GateSurface,
        /// The gate's kind — doubling as the ladder section (see the variant
        /// docs).
        kind: crate::gate::GateKind,
        /// Zero-based evaluation position within the section: registration
        /// order is evaluation order (gate.rs), so index order within
        /// (surface, kind) IS the pipeline order.
        index: u32,
        /// The verdict the gate stated — never derived from `score`.
        verdict: crate::gate::GateVerdict,
        /// The artefact handle, verbatim from the outcome's
        /// [`crate::gate::ArtefactRef::reference`].
        #[serde(rename = "artefactRef")]
        artefact_ref: String,
        /// Evidence captured verbatim by the gate (a failing command's
        /// output tail, per-assertion findings), from
        /// [`crate::gate::ArtefactRef::detail`]. Absent when the reference
        /// alone is the evidence; `None` never hits the wire.
        #[serde(
            rename = "artefactDetail",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        artefact_detail: Option<String>,
        /// Gate-supplied confidence score (KRZ-315), purely evidentiary —
        /// absent for boolean-only gates, never consulted to compute
        /// `verdict`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        score: Option<f64>,
        /// The threshold the gate judged `score` against; present exactly
        /// when `score` is (the two travel as a pair from
        /// [`crate::gate::GateScore`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threshold: Option<f64>,
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

    /// Workspace teardown recorded (D-E). The engine drives the configured
    /// `workspace.teardownMode` when a run reaches a terminal state
    /// (ticket `workspace-idle-hibernate`) and `keep` otherwise;
    /// local-worktree is always `keep` — the integration worktree's
    /// filesystem lifecycle stays with the existing mission-branch/merge
    /// machinery.
    #[serde(rename = "workspace.teardown")]
    WorkspaceTeardown {
        #[serde(default)]
        mode: String,
        /// Additive (ticket `workspace-idle-hibernate`): the teardown
        /// OUTCOME — `"kept"` (mode keep), `"stopped"` (hibernate),
        /// `"destroyed"` (destroy), `"failed"` (the provider call failed;
        /// the run's outcome stands — see the accompanying
        /// `orchestrator.decision`). Absent on old logs (v1 keep-only
        /// teardowns recorded no outcome); folds into
        /// [`crate::types::MissionState::workspace_lifecycle`] with the
        /// event's own `ts` as the workspace-hours anchor.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<String>,
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
            EventKind::ValidatorTamper { .. } => "validator.tamper",
            EventKind::ValidationSnapshot { .. } => "validation.snapshot",
            EventKind::GateResult { .. } => "gate.result",
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
            state: None,
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

    /// The additive `state` on `workspace.teardown` (ticket
    /// `workspace-idle-hibernate`): the outcome round-trips, old log lines
    /// without it fold to None, and None never hits the wire.
    #[test]
    fn workspace_teardown_state_is_additive_and_old_logs_still_fold() {
        let stopped = EventKind::WorkspaceTeardown {
            mode: "hibernate".into(),
            state: Some("stopped".into()),
        };
        let json = serde_json::to_value(&stopped).unwrap();
        assert_eq!(json["payload"]["mode"], "hibernate");
        assert_eq!(json["payload"]["state"], "stopped");
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::WorkspaceTeardown { mode, state } => {
                assert_eq!(mode, "hibernate");
                assert_eq!(state.as_deref(), Some("stopped"));
            }
            _ => panic!("wrong variant"),
        }

        // Old log line (v1 keep-only, no outcome): folds with state = None.
        let old: EventKind =
            serde_json::from_str(r#"{"type":"workspace.teardown","payload":{"mode":"keep"}}"#)
                .unwrap();
        match old {
            EventKind::WorkspaceTeardown { mode, state } => {
                assert_eq!(mode, "keep");
                assert_eq!(state, None);
            }
            _ => panic!("wrong variant"),
        }

        // state = None is omitted from the wire (additive, never breaks old
        // readers comparing payloads).
        let no_state = EventKind::WorkspaceTeardown {
            mode: "keep".into(),
            state: None,
        };
        let json = serde_json::to_value(&no_state).unwrap();
        assert!(
            !json["payload"].as_object().unwrap().contains_key("state"),
            "payload must not contain state when None: {json}"
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

    /// The additive `validator.tamper` event (ticket
    /// `validator-immutability-proof`): wire name, payload shape, and
    /// round-trip — the audit record of a failed immutability assertion.
    #[test]
    fn validator_tamper_round_trips() {
        let tamper = EventKind::ValidatorTamper {
            milestone_id: "ms-1".to_string(),
            run_id: "r-1".to_string(),
            role: Role::ValidatorScrutiny,
            head_before: "abc1234".to_string(),
            head_after: "def5678".to_string(),
            appeared: vec![" M README.md".to_string(), "?? sneaky.rs".to_string()],
            resolved: vec![],
            git_metadata_changed: false,
            git_metadata_fields: Vec::new(),
        };
        let json = serde_json::to_value(&tamper).unwrap();
        assert_eq!(json["type"], "validator.tamper");
        assert_eq!(json["payload"]["milestoneId"], "ms-1");
        assert_eq!(json["payload"]["headBefore"], "abc1234");
        assert_eq!(json["payload"]["headAfter"], "def5678");
        assert_eq!(json["payload"]["gitMetadataChanged"], false);
        // Back-compat: a pre-field log line (no gitMetadataChanged) still
        // parses, defaulting to false.
        let mut legacy = json.clone();
        legacy["payload"]
            .as_object_mut()
            .unwrap()
            .remove("gitMetadataChanged");
        let legacy_back: EventKind = serde_json::from_value(legacy).unwrap();
        match legacy_back {
            EventKind::ValidatorTamper {
                git_metadata_changed,
                ..
            } => assert!(!git_metadata_changed),
            _ => panic!("wrong variant"),
        }
        assert_eq!(
            json["payload"]["appeared"],
            serde_json::json!([" M README.md", "?? sneaky.rs"])
        );
        assert_eq!(tamper.type_name(), "validator.tamper");
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::ValidatorTamper {
                milestone_id,
                role,
                appeared,
                resolved,
                ..
            } => {
                assert_eq!(milestone_id, "ms-1");
                assert_eq!(role, Role::ValidatorScrutiny);
                assert_eq!(appeared.len(), 2);
                assert!(resolved.is_empty());
            }
            _ => panic!("wrong variant"),
        }
    }

    /// The additive `validation.snapshot` event (copy-on-write immutable
    /// validator snapshot, the follow-up to ticket
    /// `validator-immutability-proof`): wire name, payload shape, and
    /// round-trip — the audit record of which throwaway checkout a
    /// validator ran in and what warming it cost.
    #[test]
    fn validation_snapshot_round_trips() {
        let snap = EventKind::ValidationSnapshot {
            milestone_id: "ms-1".to_string(),
            role: Role::ValidatorFunctional,
            path: "/repo/.kranz/missions/m-1/runs/validator-snapshot-functional".to_string(),
            target_tier: "clonefile".to_string(),
            creation_ms: 42,
            detail: None,
        };
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["type"], "validation.snapshot");
        assert_eq!(json["payload"]["milestoneId"], "ms-1");
        assert_eq!(json["payload"]["targetTier"], "clonefile");
        assert_eq!(json["payload"]["creationMs"], 42);
        assert_eq!(snap.type_name(), "validation.snapshot");
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::ValidationSnapshot {
                milestone_id,
                role,
                target_tier,
                detail,
                ..
            } => {
                assert_eq!(milestone_id, "ms-1");
                assert_eq!(role, Role::ValidatorFunctional);
                assert_eq!(target_tier, "clonefile");
                assert_eq!(detail, None);
            }
            _ => panic!("wrong variant"),
        }

        // The `fresh` tier's named cost rides `detail`, and a legacy line
        // without the field still decodes (serde default).
        let with_cost = EventKind::ValidationSnapshot {
            milestone_id: "ms-1".to_string(),
            role: Role::ValidatorScrutiny,
            path: "/snap".to_string(),
            target_tier: "fresh".to_string(),
            creation_ms: 7,
            detail: Some("target/ is 31 GiB; snapshot pays a cold rebuild".to_string()),
        };
        let mut json = serde_json::to_value(&with_cost).unwrap();
        assert!(json["payload"]["detail"].as_str().unwrap().contains("GiB"));
        json["payload"].as_object_mut().unwrap().remove("detail");
        let legacy_back: EventKind = serde_json::from_value(json).unwrap();
        match legacy_back {
            EventKind::ValidationSnapshot { detail, .. } => assert_eq!(detail, None),
            _ => panic!("wrong variant"),
        }
    }

    /// The additive `gate.result` event (ticket
    /// `gate-results-first-class-events`, KRZ-312): wire name, exact payload
    /// shape, and round-trip. A full record — surface, ladder section +
    /// index, stated verdict, artefact handle with captured detail, and the
    /// optional score pair — survives serde verbatim, because replay
    /// reconstructs the ladder from these bytes alone.
    #[test]
    fn gate_result_event_wire_shape_and_round_trip() {
        let result = EventKind::GateResult {
            gate: "vacuous-filter".to_string(),
            surface: crate::gate::GateSurface::Approval,
            kind: crate::gate::GateKind::Deterministic,
            index: 0,
            verdict: crate::gate::GateVerdict::Fail,
            artefact_ref: "contract gate vacuous-filter".to_string(),
            artefact_detail: Some("[a-1] test-runner pipeline's grep anchors no nonzero count: `cargo test | grep ok`".to_string()),
            score: Some(0.42),
            threshold: Some(0.75),
        };
        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["type"], "gate.result");
        assert_eq!(json["payload"]["gate"], "vacuous-filter");
        assert_eq!(json["payload"]["surface"], "approval");
        assert_eq!(json["payload"]["kind"], "deterministic");
        assert_eq!(json["payload"]["index"], 0);
        assert_eq!(json["payload"]["verdict"], "fail");
        assert_eq!(
            json["payload"]["artefactRef"],
            "contract gate vacuous-filter"
        );
        assert_eq!(json["payload"]["score"], 0.42);
        assert_eq!(json["payload"]["threshold"], 0.75);
        assert_eq!(result.type_name(), "gate.result");
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::GateResult {
                gate,
                surface,
                kind,
                index,
                verdict,
                artefact_ref,
                artefact_detail,
                score,
                threshold,
            } => {
                assert_eq!(gate, "vacuous-filter");
                assert_eq!(surface, crate::gate::GateSurface::Approval);
                assert_eq!(kind, crate::gate::GateKind::Deterministic);
                assert_eq!(index, 0);
                assert_eq!(verdict, crate::gate::GateVerdict::Fail);
                assert_eq!(artefact_ref, "contract gate vacuous-filter");
                assert!(artefact_detail.as_deref().unwrap().contains("[a-1]"));
                assert_eq!(score, Some(0.42));
                assert_eq!(threshold, Some(0.75));
            }
            _ => panic!("wrong variant"),
        }
    }

    /// Boolean-only gates carry no score, and a reference without captured
    /// content carries no detail: all three are additive-optional — `None`
    /// stays OFF the wire (byte-identical to a payload that never had them)
    /// and a line without them parses back to `None` (serde default), so
    /// hand-written or future-trimmed logs fold like engine-written ones.
    #[test]
    fn gate_result_event_optional_fields_are_additive() {
        let sparse = EventKind::GateResult {
            gate: "env-sensitive".to_string(),
            surface: crate::gate::GateSurface::FinalGate,
            kind: crate::gate::GateKind::ModelJudged,
            index: 2,
            verdict: crate::gate::GateVerdict::Pass,
            artefact_ref: "contract gate env-sensitive".to_string(),
            artefact_detail: None,
            score: None,
            threshold: None,
        };
        let json = serde_json::to_value(&sparse).unwrap();
        assert_eq!(json["payload"]["surface"], "final-gate");
        assert_eq!(json["payload"]["kind"], "model-judged");
        assert_eq!(json["payload"]["verdict"], "pass");
        let payload = json["payload"].as_object().unwrap();
        for absent in ["artefactDetail", "score", "threshold"] {
            assert!(
                !payload.contains_key(absent),
                "payload must not contain {absent} when None: {json}"
            );
        }

        // A wire line naming only the required fields folds with the
        // optional ones defaulted to None.
        let line = r#"{
            "seq": 7,
            "ts": "2026-01-02T03:04:05Z",
            "missionId": "m-1",
            "type": "gate.result",
            "payload": {
                "gate": "merge-gate-suite",
                "surface": "final-gate",
                "kind": "deterministic",
                "index": 1,
                "verdict": "pass",
                "artefactRef": ".kranz/merge-gates.json"
            }
        }"#;
        let event: Event = serde_json::from_str(line).unwrap();
        match event.kind {
            EventKind::GateResult {
                gate,
                artefact_detail,
                score,
                threshold,
                ..
            } => {
                assert_eq!(gate, "merge-gate-suite");
                assert_eq!(artefact_detail, None);
                assert_eq!(score, None);
                assert_eq!(threshold, None);
            }
            _ => panic!("wrong variant"),
        }
    }
}
