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
    /// Worker deny rules (e.g. `Bash(git push*)`) an operator has LIFTED for
    /// this mission via a `WorkerDeny` grant — subtracted from the worker deny
    /// set by `permissions::for_role`. Extend-only, runtime-only (never plan-
    /// declared): a deliberate, logged erosion of a safety guardrail.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny_exceptions: Vec<String>,
    /// Egress destinations (`host:port`) an operator has GRANTED for this
    /// mission — folded into the egress proxy's allowlist for `fs+net`
    /// sessions (`crate::egress_proxy`). Extend-only, runtime-only (never
    /// plan-declared): the fold target a `GrantKind::Egress` approval extends;
    /// read into the proxy allowlist at spec build so that approval needs no
    /// plumbing change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress_grants: Vec<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_sha: Option<String>,
    /// Operator guidance folded from the latest milestone.unblocked event;
    /// injected verbatim into validator tasks until the milestone completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validator_guidance: Option<String>,
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

/// Default `quant` for worker runs predating provenance fields.
fn default_quant() -> String {
    "n/a".to_string()
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
    /// Quantization of the model weights used for this run (provenance).
    #[serde(default = "default_quant")]
    pub quant: String,
    /// Hash of the model weights used for this run, when known (provenance).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_hash: Option<String>,
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

/// The workspace provider identity pinned at plan approval (design D-B,
/// ticket `workspace-provider-pin-at-approval`) — the consent artifact naming
/// the environment the operator approved running against. Free-form strings
/// on purpose: the shape must not assume a provider kind. Per-kind meanings:
/// - `local-worktree` / `container`: `template` = the isolation mode
///   (`"worktree"` | `"checkout"` — source isolation, not a runnable
///   workspace, D-H), `version` = the workspace contract's schemaVersion
///   when a contract exists, else `"none"`.
/// - `remote` (ticket `workspace-remote-coder-provider`): `template` = the
///   configured substrate template/image id (as configured at approval —
///   the pin stays pure, no substrate contact), `version` = the adapter
///   version string (`"coder-v1"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspacePin {
    pub provider: String,
    pub template: String,
    pub version: String,
}

/// The last known workspace lifecycle transition (ticket
/// `workspace-idle-hibernate`), folded from `workspace.teardown` events
/// carrying a `state`: `"kept"` (mode keep), `"stopped"` (hibernate),
/// `"destroyed"` (destroy), or `"failed"` (the provider call failed — the
/// workspace may still be live). `state` is free-form on purpose so a
/// future substrate-reported transition (e.g. an idle hibernate the
/// substrate owns) folds into the same field without a schema change.
/// `ts` is the transition event's own timestamp — the workspace-hours
/// anchor for cost tooling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspaceLifecycle {
    pub state: String,
    pub ts: DateTime<Utc>,
}

/// A preview as provisioned by a remote workspace provider (ticket
/// `workspace-remote-coder-provider`), recorded on `workspace.provisioned`:
/// the URL the SUBSTRATE reported for a contract `previews[]` entry
/// (name-matched — never fabricated), plus whether the substrate reports
/// the URL is fronted with auth (design D-E: previews authenticated by
/// default — recorded, never disabled by the adapter).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ProvisionedPreview {
    pub name: String,
    pub url: String,
    /// Substrate-reported auth fronting; absent when the substrate did not
    /// say (never read as "no auth" — consumers must degrade on absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<bool>,
}

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
    /// A capability denial (today: a validator command outside its allow-set)
    /// awaiting an operator approve/deny decision. Like `pending_revision`, the
    /// run loop parks on this gate; approving extends `command_grants` and
    /// respawns, denying (or a timeout) fails the feature closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_grant_request: Option<PendingGrantRequest>,
    /// Seq of the last event folded in.
    pub last_seq: u64,
    /// Count of `tier.escalated` events folded (executor bumped from local to
    /// frontier after repeated failed local validations).
    #[serde(default)]
    pub escalated_milestones: u32,
    /// Count of milestones whose `milestone.started` folded while the
    /// executor tier was [`ExecutorTier::Local`] — the denominator for
    /// [`MissionState::escalation_rate`].
    #[serde(default)]
    pub local_executor_milestones: u32,
    /// Kind of the workspace provider that last provisioned this mission's
    /// workspace, folded from `workspace.provisioned` (design D-B/D-E).
    /// `None` in logs predating the provider seam.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_provider: Option<String>,
    /// The workspace provider identity pinned at plan approval, folded from
    /// `workspace.provider.pinned` (design D-B). `None` in logs predating
    /// the pin event (missions approved before pinning existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_pin: Option<WorkspacePin>,
    /// The last known workspace lifecycle transition (state + ts), folded
    /// from `workspace.teardown` events carrying a `state` (ticket
    /// `workspace-idle-hibernate`). `None` in logs predating the state
    /// field — v1 keep-only teardowns carried no outcome.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_lifecycle: Option<WorkspaceLifecycle>,
}

impl MissionState {
    /// Share of local-tier milestones that were escalated to Frontier.
    /// `0.0` when no milestone has ever started under the Local tier (no
    /// divide-by-zero).
    pub fn escalation_rate(&self) -> f64 {
        if self.local_executor_milestones == 0 {
            0.0
        } else {
            self.escalated_milestones as f64 / self.local_executor_milestones as f64
        }
    }

    /// Which inference tier the Worker executes this mission on, derived
    /// from the current Worker `RoleConfig.backend` rather than stored:
    /// [`ExecutorTier::Local`] when the Worker backend is
    /// [`BackendKind::Local`] (applied at seed time by
    /// [`crate::config::apply_executor_routing`] or by any later
    /// `config.changed`), else [`ExecutorTier::Frontier`].
    pub fn executor_tier(&self) -> ExecutorTier {
        if self.config.backend_kind(Role::Worker) == BackendKind::Local {
            ExecutorTier::Local
        } else {
            ExecutorTier::Frontier
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRevision {
    pub revision: u32,
    pub plan: Plan,
    pub instructions: String,
}

/// What a grant would extend on approval. All kinds park through the SAME
/// operator approve/deny gate (and reuse its timeout + per-milestone cap); they
/// differ only in the boundary that triggered them and what the reducer
/// extends. `#[default]` = `Command` so pre-`kind` events (and the wire
/// default) fold as the original command-grant behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GrantKind {
    /// A validator command outside its allow-set → extend `command_grants`.
    #[default]
    Command,
    /// A worker write outside the `touch_set` → extend `touch_set`.
    TouchPath,
    /// A worker command blocked by a deny rule (deny-wins) → lift that rule by
    /// adding it to `deny_exceptions`. Unlike the others this SUBTRACTS from a
    /// safety guardrail, so it is per-mission, explicit, and logged.
    WorkerDeny,
    /// A sandboxed (`fs+net`) run whose egress proxy refused a destination →
    /// extend `egress_grants`, so the re-run's proxy allowlist covers it.
    Egress,
}

/// A parked capability-grant request (see [`MissionState::pending_grant_request`]).
/// Names the exact target a grant would unblock and the milestone whose
/// validation hit the boundary, so the operator's approve/deny decision — and
/// the reducer's cross-check on `grant.approved`/`grant.denied` — key off the
/// same target that was requested.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingGrantRequest {
    pub milestone_id: String,
    #[serde(default)]
    pub kind: GrantKind,
    /// The granted target: a command string (`Command`), a repo-relative path
    /// glob (`TouchPath`), a deny rule (`WorkerDeny`), or a `host:port`
    /// destination (`Egress`). Named `command` for wire back-compat with the
    /// original command-only grant events.
    pub command: String,
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
    /// [`crate::backend_codex::CodexBackend`], `"droid"` selects
    /// [`crate::backend_droid::DroidBackend`], `"kimi"` selects
    /// [`crate::backend_kimi::KimiBackend`], and `"local"` selects an
    /// OpenAI-compatible HTTP endpoint. `config::validate` checks that
    /// the selected backend/model pair is supported for the role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Base URL of the OpenAI-compatible HTTP endpoint for `backend = "local"`.
    /// Required and validated when a role selects the local backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Context window budget (tokens) for `backend = "local"`, used to guard
    /// against KV-cache blowout. Required and validated when a role selects
    /// the local backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_budget: Option<u32>,
    /// Sampling temperature for `backend = "local"`. Optional; validated when
    /// present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
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

/// Which sandbox mechanism wraps a role's sessions when `enforce` is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SandboxProvider {
    /// Tier-2 process sandboxing (Seatbelt on macOS, bubblewrap on Linux).
    #[default]
    Process,
    /// Tier-3 container sandboxing (see `crate::sandbox_container`).
    Container,
}

/// Per-role OS sandbox config.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SandboxConfig {
    pub enforce: SandboxEnforce,
    /// Sandbox provider: `process` (default, tier 2) or `container` (tier 3).
    /// `container` with `enforce = "off"` means no sandboxing, same as today.
    #[serde(default)]
    pub provider: SandboxProvider,
    /// Container image used when `provider = "container"`. Defaults to
    /// `sandbox_container::DEFAULT_IMAGE` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
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
    Kimi,
    /// OpenAI-compatible HTTP endpoint, configured via the role's `baseUrl`,
    /// `contextBudget`, and optional `temperature`.
    Local,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Claude => "claude",
            BackendKind::Codex => "codex",
            BackendKind::Droid => "droid",
            BackendKind::Kimi => "kimi",
            BackendKind::Local => "local",
        }
    }
}

/// Which inference tier executes a ticket, derived deterministically from its
/// `task-class` frontmatter via [`crate::config::task_class_to_tier`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutorTier {
    Local,
    Frontier,
}

impl Default for ExecutorTier {
    /// Frontier is the safe default when no task class is present or recognized.
    fn default() -> Self {
        ExecutorTier::Frontier
    }
}

/// How worker/validator sessions are isolated from the primary checkout.
///
/// Default is [`Worktree`]: the primary checkout must stay byte-untouched
/// across a mission (AGENTS.md). Operators may still opt into [`Checkout`]
/// for backends that cannot write into temp-dir worktrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WorkerIsolation {
    #[default]
    Worktree,
    Checkout,
}

/// Workspace provider seam config (design D-B, ticket
/// `workspace-provider-seam`): which
/// [`crate::workspace_provider::WorkspaceProvider`] supplies the mission's
/// runnable environment. A DIFFERENT config surface from
/// [`SandboxConfig`] — sandbox = process containment, workspace = the
/// runnable environment — and the two stay separate even where runtime code
/// could later be shared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct WorkspaceConfig {
    /// Workspace provider name. Absent (or `"local-worktree"`) selects
    /// today's isolation cwd (the default). Unknown names fail closed at run
    /// start — never a silent fallback to local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Remote-substrate settings (ticket `workspace-remote-coder-provider`),
    /// consulted only when `provider = "remote"`: `"remote"` resolves ONLY
    /// with these complete — a missing key fails closed at resolve (plan
    /// approval AND run start) with the key named, never a silent fallback
    /// to local.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteWorkspaceConfig>,
    /// The teardown mode the engine drives when a run reaches a TERMINAL
    /// state — Complete/Failed/Abandoned (ticket
    /// `workspace-idle-hibernate`): `"keep"` (the default) | `"hibernate"`
    /// | `"destroy"`. A non-terminal run end (Blocked/Paused) always Keeps
    /// so the mission can resume; local-worktree is always Keep regardless
    /// (its filesystem lifecycle belongs to the mission-branch/merge
    /// machinery). Unknown modes fail closed at run start with this key
    /// named — never a silent default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub teardown_mode: Option<String>,
}

/// Remote substrate (Coder-shaped) connection config (ticket
/// `workspace-remote-coder-provider`). Additive and serde-defaulted like the
/// rest of the config contract. The substrate token comes from the
/// environment variable NAMED by `tokenEnv`, read lazily at provision —
/// never a value in config, logs, or events. VPN/SSH reachability of the
/// substrate is an operator/network concern: no public IP is required, and
/// the adapter never opens one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RemoteWorkspaceConfig {
    /// Substrate API base URL (e.g. `"https://coder.internal.example.com"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// The template/image id workspaces are provisioned from (pinned at
    /// approval into `workspace.provider.pinned` as the remote `template`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// NAME of the environment variable holding the substrate API token
    /// (e.g. `"CODER_SESSION_TOKEN"`) — the name only, never the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
    /// Substrate-side idle policy VALUE (ticket `workspace-idle-hibernate`):
    /// hours of inactivity after which the SUBSTRATE hibernates the
    /// workspace. kranz never schedules: the value is passed through to the
    /// substrate at provision (when the substrate accepts an idle policy)
    /// and recorded in `workspace.provisioned`'s detail; the substrate owns
    /// the policy's execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_after_hours: Option<f64>,
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
    /// Escape hatch for the cleared contract-command environment (ticket
    /// `agent-env-clear`): NAMES of ambient env vars copied verbatim into
    /// the env of contract `command` assertions (validation round, final
    /// gate, approval-time lint). This is the sanctioned way to give a
    /// contract command one credential (e.g. a private-registry token the
    /// toolchain-cache passthrough does not cover). Values are never
    /// logged — decision records list names only. Names colliding with the
    /// contract env's own managed keys (`PATH`/`HOME`/`TMPDIR`/
    /// `KRANZ_BASE_SHA`/toolchain caches) are refused. Everything else
    /// ambient is cleared before the command runs.
    #[serde(default)]
    pub contract_env_passthrough: Vec<String>,
    /// Workspace provider seam (design D-B). `provider` absent =
    /// local-worktree; unknown names fail closed at run start.
    #[serde(default)]
    pub workspace: WorkspaceConfig,
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
                base_url: None,
                context_budget: None,
                temperature: None,
                sandbox: SandboxConfig::default(),
            },
            worker: RoleConfig {
                model: "sonnet".into(),
                reasoning_effort: "medium".into(),
                max_turns: Some(50),
                max_budget_usd: Some(10.0),
                tools: vec![],
                backend: None,
                base_url: None,
                context_budget: None,
                temperature: None,
                sandbox: SandboxConfig::default(),
            },
            validator_scrutiny: RoleConfig {
                model: "opus".into(),
                reasoning_effort: "high".into(),
                max_turns: Some(40),
                max_budget_usd: Some(10.0),
                tools: vec![],
                backend: None,
                base_url: None,
                context_budget: None,
                temperature: None,
                sandbox: SandboxConfig::default(),
            },
            validator_functional: RoleConfig {
                model: "sonnet".into(),
                reasoning_effort: "medium".into(),
                max_turns: Some(40),
                max_budget_usd: Some(5.0),
                tools: vec![],
                backend: None,
                base_url: None,
                context_budget: None,
                temperature: None,
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
            worker_isolation: WorkerIsolation::Worktree,
            contract_env_passthrough: vec![],
            workspace: WorkspaceConfig::default(),
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
            Some("kimi") => BackendKind::Kimi,
            Some("local") => BackendKind::Local,
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
    Msg {
        text: String,
        interrupt: bool,
    },
    ConfigChange {
        patch: serde_json::Value,
    },
    RequestRevision {
        instructions: String,
    },
    ApproveRevision {
        revision: u32,
    },
    RejectRevision {
        revision: u32,
    },
    /// Approve the parked grant request for `command` (extend `command_grants`
    /// and respawn). The command is echoed back so a stale approval can't apply
    /// to a different pending request than the operator saw.
    ApproveGrant {
        command: String,
    },
    /// Deny the parked grant request for `command` (fail the feature closed).
    DenyGrant {
        command: String,
        reason: String,
    },
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
