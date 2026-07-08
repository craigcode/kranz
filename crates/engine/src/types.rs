//! Core data model for Kranz missions (plan §4.2).
//!
//! CONTRACT FILE — do not modify in implementation phases. If a change seems
//! necessary, report it instead of editing.
//!
//! All types serialize camelCase to match the plan document's JSON shapes.
//! `plan.json`, `state.json`, and event payloads are built from these types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Mission
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MissionStatus {
    Planning,
    /// Plan approved; run loop has not yet started (folded on `plan.approved`,
    /// transitions to `Running` on the first `milestone.started` or
    /// `worker.spawned`).
    Approved,
    Running,
    Paused,
    Blocked,
    Validating,
    Complete,
    Failed,
    /// Explicitly retired by the operator (`kranz abandon`) — a terminal state
    /// distinct from Failed (the mission didn't fail, it was called off).
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Mission {
    pub id: String,
    pub goal: String,
    /// Defined BEFORE features (plan §2.3).
    pub validation_contract: Vec<Assertion>,
    pub milestones: Vec<Milestone>,
    pub status: MissionStatus,
    pub created_at: DateTime<Utc>,
    /// e.g. "main"
    pub base_branch: String,
    /// Base-branch commit SHA pinned at plan approval; `None` until approved
    /// or for missions created before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    /// e.g. "kranz/mission-<id>"
    pub mission_branch: String,
    /// Read-only shell commands granted mission-wide to worker AND validator
    /// sessions; single source of truth carried from the approved `Plan`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_grants: Vec<String>,
    /// Gitignore/glob-style repo-relative path patterns the mission is
    /// allowed to touch; single source of truth carried from the approved
    /// `Plan`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touch_set: Vec<String>,
}

// ---------------------------------------------------------------------------
// Plan (the orchestrator's structured output; committed as plan.json)
// ---------------------------------------------------------------------------

/// The approved plan as emitted by the orchestrator and committed by the
/// engine as the first commit on the mission branch (plan §4.4).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub goal: String,
    pub validation_contract: Vec<Assertion>,
    pub milestones: Vec<PlanMilestone>,
    /// Review material required for broad/expensive plans: the approach the
    /// planner chose and at least two rejected shapes with their trade-offs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub considered_alternatives: Option<ConsideredAlternatives>,
    /// Read-only shell commands the plan declares as runnable by BOTH worker
    /// and validator sessions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_grants: Vec<String>,
    /// Gitignore/glob-style repo-relative path patterns the mission is
    /// allowed to touch.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touch_set: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsideredAlternatives {
    pub chosen: String,
    pub rejected: Vec<RejectedAlternative>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RejectedAlternative {
    pub approach: String,
    pub trade_off: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanMilestone {
    pub title: String,
    pub features: Vec<PlanFeature>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanFeature {
    pub title: String,
    /// What to build.
    pub spec: String,
    /// How we know it's done.
    pub validation_criteria: Vec<String>,
}

// ---------------------------------------------------------------------------
// Milestone / Feature
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MilestoneStatus {
    Pending,
    Active,
    Validating,
    Complete,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Milestone {
    pub id: String,
    pub title: String,
    pub features: Vec<Feature>,
    pub status: MilestoneStatus,
    /// Loop-guard counter (plan §4.5). Incremented per validation round that
    /// produced findings; milestone blocks when it exceeds the configured cap.
    pub fix_cycles: u32,
    /// Recorded at milestone.started so validators diff start..HEAD (§4.4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeatureOrigin {
    /// Part of the approved plan.
    Plan,
    /// Created by the orchestrator from a validation finding.
    Fix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FeatureStatus {
    Pending,
    Active,
    Complete,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Feature {
    pub id: String,
    pub title: String,
    pub spec: String,
    pub validation_criteria: Vec<String>,
    pub origin: FeatureOrigin,
    pub status: FeatureStatus,
    /// Run ids (full WorkerRun records live in MissionState.runs).
    pub worker_runs: Vec<String>,
    pub commits: Vec<String>,
    /// Times this feature's worker was respawned (bounded by config.max_respawns).
    pub respawns: u32,
}

// ---------------------------------------------------------------------------
// Worker runs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    Orchestrator,
    Worker,
    ValidatorScrutiny,
    ValidatorFunctional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunResult {
    Pass,
    Fail,
    Partial,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl TokenUsage {
    pub fn add(&mut self, other: &TokenUsage) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerRun {
    pub id: String,
    pub role: Role,
    /// Feature this run worked on (workers) — validators have milestone_id instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feature_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milestone_id: Option<String>,
    /// Claude Code session id (UUID chosen by the engine, used for --resume).
    pub sdk_session_id: String,
    pub model: String,
    pub started_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    pub tokens: TokenUsage,
    /// Cost as reported by the CLI result message when available, else estimated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// Relative path (under the mission dir) of the run transcript JSONL.
    pub transcript_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<RunResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<WorkerReport>,
    /// Hash of the role prompt file used (traceability, plan §4.6).
    pub prompt_hash: String,
}

// ---------------------------------------------------------------------------
// Validation contract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssertionCheck {
    /// Verified by running `command` (hard gate at mission completion).
    Command,
    /// Verified by orchestrator judgement against the full mission diff.
    AgentJudgement,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Assertion {
    pub id: String,
    /// Behavioural, testable statement.
    pub statement: String,
    pub check: AssertionCheck,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

// ---------------------------------------------------------------------------
// Worker report (plan §4.6) — the enforced final message of every worker run
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerReport {
    pub result: RunResult,
    pub summary: String,
    #[serde(default)]
    pub files_touched: Vec<String>,
    #[serde(default)]
    pub tests_added: Vec<String>,
    #[serde(default)]
    pub test_evidence: String,
    #[serde(default)]
    pub dependencies_added: Vec<String>,
    #[serde(default)]
    pub known_gaps: Vec<String>,
    #[serde(default)]
    pub commits: Vec<String>,
    #[serde(default)]
    pub commands_run: Vec<String>,
}

/// A finding emitted by a validator (scrutiny or functional).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// Assertion id or feature criterion this finding is about.
    pub subject: String,
    /// "critical" | "major" | "minor"
    pub severity: String,
    pub evidence: String,
    #[serde(default)]
    pub suggested_fix: String,
    /// Free-form finding class, e.g. "out-of-contract-write"; default "" for
    /// existing scrutiny/functional/gate findings.
    #[serde(default)]
    pub class: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidatorReport {
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub summary: String,
}

// ---------------------------------------------------------------------------
// Derived state (pure fold over the event log; state.json is a cache of this)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MissionState {
    pub mission: Mission,
    /// All runs keyed by run id (BTreeMap for deterministic serialization).
    pub runs: BTreeMap<String, WorkerRun>,
    pub totals: TokenUsage,
    pub total_cost_usd: f64,
    /// Queued user messages not yet consumed by the orchestrator.
    pub pending_user_messages: Vec<String>,
    /// Recent orchestrator decision summaries (newest last; capped by reducer).
    pub recent_decisions: Vec<String>,
    /// Per-role config overrides applied mid-mission via config.changed.
    pub config: MissionConfig,
    /// Latest revision number observed in the durable log. `0` means the
    /// original approved plan is still the only plan of record.
    #[serde(default)]
    pub latest_plan_revision: u32,
    /// A proposed revised plan awaiting human approve/reject. The mission
    /// status does not change while this is set; the run loop parks on this
    /// gate and the repo stays busy until consent arrives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_revision: Option<PendingRevision>,
    /// Seq of the last event folded in.
    pub last_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRevision {
    pub revision: u32,
    pub plan: Plan,
    pub instructions: String,
}

// ---------------------------------------------------------------------------
// Configuration (plan §6)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleConfig {
    /// Model alias or full id passed to `claude --model` (e.g. "opus", "sonnet").
    pub model: String,
    /// Passed to `claude --effort`: low | medium | high | xhigh | max.
    pub reasoning_effort: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_budget_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// Backend override for this role. `None` or `"claude"` keeps the default
    /// Claude Code backend, `"codex"` selects
    /// [`crate::backend_codex::CodexBackend`], and `"droid"` selects
    /// [`crate::backend_droid::DroidBackend`]. `config::validate` checks that
    /// the selected backend/model pair is supported for the role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Per-role OS sandbox opt-in.
    #[serde(default)]
    pub sandbox: SandboxConfig,
}

/// OS sandbox enforcement level for a role's sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SandboxEnforce {
    #[default]
    Off,
    Fs,
    #[serde(rename = "fs+net")]
    FsNet,
}

/// Per-role OS sandbox config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SandboxConfig {
    pub enforce: SandboxEnforce,
    /// Extra paths the operator opts into as writable (e.g. "~/.cargo").
    /// Stored as raw strings; not expanded or canonicalized here.
    pub extra_write: Vec<String>,
    /// Extra network destinations allowed under `enforce = "fs+net"` (for
    /// example package registries). Stored as `host:port` strings.
    pub egress: Vec<String>,
}

/// Which [`AgentBackend`](crate::backend::AgentBackend) drives a role's sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Claude,
    Codex,
    Droid,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Claude => "claude",
            BackendKind::Codex => "codex",
            BackendKind::Droid => "droid",
        }
    }
}

/// How worker/validator sessions are isolated from the primary checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WorkerIsolation {
    Worktree,
    #[default]
    Checkout,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MissionConfig {
    pub orchestrator: RoleConfig,
    pub worker: RoleConfig,
    pub validator_scrutiny: RoleConfig,
    pub validator_functional: RoleConfig,
    pub skip_scrutiny: bool,
    pub skip_functional: bool,
    pub max_fix_cycles_per_milestone: u32,
    pub max_respawns: u32,
    pub max_parallel_workers: u32,
    pub event_stream_throttle_ms: u64,
    /// An in-planning mission whose hosted engine sits idle this many minutes
    /// is released (its events.jsonl lock freed); 0 disables auto-release.
    pub planning_idle_release_minutes: u64,
    /// When true, the serve process drains the queue automatically whenever
    /// entries are waiting. Default off.
    pub auto_work: bool,
    /// Require `Plan.consideredAlternatives` when feature count reaches this
    /// threshold. `0` disables this trigger.
    pub considered_alternatives_feature_threshold: usize,
    /// Require `Plan.consideredAlternatives` when `touchSet` breadth reaches
    /// this threshold. `0` disables this trigger.
    pub considered_alternatives_touch_set_threshold: usize,
    /// Require `Plan.consideredAlternatives` when the estimated high cost
    /// reaches this threshold. `0.0` disables this trigger.
    pub considered_alternatives_high_usd_threshold: f64,
    /// Extra Bash deny patterns beyond the built-in list (§4.7).
    pub deny_patterns: Vec<String>,
    /// Commands validators may run, in addition to contract `command`s.
    pub allow_validator_commands: Vec<String>,
    /// Loud, never-default escape hatch.
    pub dangerously_allow_all: bool,
    /// Explicit mission opt-in for worker models below the default worker tier.
    ///
    /// Default false: cheap/lower-tier workers must be chosen deliberately on
    /// the mission config rather than becoming a silent global default.
    pub allow_below_default_worker_model: bool,
    /// Path to the claude binary (auto-discovered when None).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_binary: Option<String>,
    /// How worker/validator sessions are isolated (§M7 tier 1).
    pub worker_isolation: WorkerIsolation,
}

impl Default for MissionConfig {
    fn default() -> Self {
        MissionConfig {
            orchestrator: RoleConfig {
                model: "opus".into(),
                reasoning_effort: "high".into(),
                max_turns: None,
                max_budget_usd: Some(20.0),
                tools: vec![],
                backend: None,
                sandbox: SandboxConfig::default(),
            },
            worker: RoleConfig {
                model: "sonnet".into(),
                reasoning_effort: "medium".into(),
                max_turns: Some(50),
                max_budget_usd: Some(10.0),
                tools: vec![],
                backend: None,
                sandbox: SandboxConfig::default(),
            },
            validator_scrutiny: RoleConfig {
                model: "opus".into(),
                reasoning_effort: "high".into(),
                max_turns: Some(40),
                max_budget_usd: Some(10.0),
                tools: vec![],
                backend: None,
                sandbox: SandboxConfig::default(),
            },
            validator_functional: RoleConfig {
                model: "sonnet".into(),
                reasoning_effort: "medium".into(),
                max_turns: Some(40),
                max_budget_usd: Some(5.0),
                tools: vec![],
                backend: None,
                sandbox: SandboxConfig::default(),
            },
            skip_scrutiny: false,
            skip_functional: false,
            max_fix_cycles_per_milestone: 2,
            max_respawns: 2,
            max_parallel_workers: 1,
            event_stream_throttle_ms: 250,
            planning_idle_release_minutes: 30,
            auto_work: false,
            considered_alternatives_feature_threshold: 4,
            considered_alternatives_touch_set_threshold: 4,
            considered_alternatives_high_usd_threshold: 0.0,
            deny_patterns: vec![],
            allow_validator_commands: vec![],
            dangerously_allow_all: false,
            allow_below_default_worker_model: false,
            claude_binary: None,
            worker_isolation: WorkerIsolation::Checkout,
        }
    }
}

impl MissionConfig {
    pub fn isolation(&self) -> WorkerIsolation {
        self.worker_isolation
    }

    pub fn role(&self, role: Role) -> &RoleConfig {
        match role {
            Role::Orchestrator => &self.orchestrator,
            Role::Worker => &self.worker,
            Role::ValidatorScrutiny => &self.validator_scrutiny,
            Role::ValidatorFunctional => &self.validator_functional,
        }
    }

    /// Which backend drives a role's sessions. `config::validate` rejects
    /// unknown backend strings before runtime; this accessor treats any
    /// unexpected value as Claude as a conservative fallback for callers that
    /// operate on already-validated config.
    pub fn backend_kind(&self, role: Role) -> BackendKind {
        match self.role(role).backend.as_deref() {
            Some("codex") => BackendKind::Codex,
            Some("droid") => BackendKind::Droid,
            _ => BackendKind::Claude,
        }
    }
}

// ---------------------------------------------------------------------------
// Control commands (cross-process: CLI/server -> engine, via control dir)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ControlCommand {
    Pause,
    Resume,
    Msg { text: String, interrupt: bool },
    ConfigChange { patch: serde_json::Value },
    RequestRevision { instructions: String },
    ApproveRevision { revision: u32 },
    RejectRevision { revision: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_run_backcompat_defaults_empty() {
        let json = r#"{"result": "pass", "summary": "did the thing"}"#;
        let report: WorkerReport = serde_json::from_str(json).unwrap();
        assert!(report.commands_run.is_empty());
    }

    #[test]
    fn finding_class_round_trips_through_serde() {
        let finding = Finding {
            subject: "a-1".to_string(),
            severity: "major".to_string(),
            evidence: "wrote outside touch-set".to_string(),
            suggested_fix: String::new(),
            class: "out-of-contract-write".to_string(),
        };
        let json = serde_json::to_value(&finding).unwrap();
        assert_eq!(json["class"], "out-of-contract-write");
        let round_tripped: Finding = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped.class, "out-of-contract-write");
    }

    #[test]
    fn finding_class_backcompat_defaults_empty() {
        let json = r#"{
            "subject": "a-1",
            "severity": "major",
            "evidence": "it broke"
        }"#;
        let finding: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(finding.class, "");
    }
}
