//! Core data model for Kranz missions (plan §4.2).
//!
//! CONTRACT FILE — do not modify in implementation phases. If a change seems
//! necessary, report it instead of editing.
//!
//! All types serialize camelCase to match the plan document's JSON shapes.
//! `plan.json`, `state.json`, and event payloads are built from these types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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
    /// e.g. `kranz/mission-<id>`
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
    /// The executor route this mission was seeded with (ticket
    /// `routing-rules-config`): derived at fold time from `mission.created`'s
    /// original folded goal + routed config ([`crate::routing::seed_executor_route`])
    /// — `plan.approved` overwrites `goal` with the plan's own, so the task
    /// class exists only on that first event and the decision is folded here
    /// once, then replayed onto every `worker.spawned`. `None` when the seed
    /// carried no task class; additive (absent in pre-existing state
    /// snapshots). A mid-mission `config.changed` backend flip moves the
    /// LIVE tier ([`MissionState::executor_tier`]), not this seed-time record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor_route: Option<ExecutorRoute>,
    /// The Flight Rules standards pin folded from the approved plan
    /// (`plan.approved` / `plan.revised`; KRZ-342 D-E) — the single source of
    /// truth every later mission stage resolves against. `None` for missions
    /// without a standards-configured pack and in every pre-KRZ-342 state
    /// snapshot; additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standards_manifest: Option<StandardsPin>,
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
    /// The Flight Rules standards manifest pinned at approval (ticket
    /// `flight-rules-resolution-pin`, KRZ-342; design D-E): the
    /// engine-resolved applicable-rule snapshot — pack identity + digest,
    /// selection inputs, and every applicable rule's id, revision, effective
    /// status, statement, scopes, and checker binding. The ENGINE resolves
    /// and writes it from the trusted source at `approve_plan`; a plan
    /// carrying a stale or substituted manifest is rejected there. Additive:
    /// `None` in every pre-KRZ-342 plan and whenever no standards-configured
    /// pack governs, and `skip_serializing_if` keeps those plans
    /// byte-identical. Boxed: the pin is a rare, sizable field, and an
    /// inline `StandardsPin` would push `Plan` past the
    /// `large_enum_variant` budget on `PlanRequest::Ready`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standards_manifest: Option<Box<StandardsPin>>,
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

// ---------------------------------------------------------------------------
// Flight Rules approval pin (KRZ-342, design D-D/D-E)
// ---------------------------------------------------------------------------

/// Where the pinned standards bytes came from (KRZ-342 D-A/D-E) — the trust
/// posture approval resolved under, recorded so later stages know whether a
/// live-base re-read exists at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StandardsPinSource {
    /// A tracked, repo-relative pack read from the pinned base git tree
    /// (`pack_dir` is its repo-relative slash path). Enforced rules may be
    /// active; merge re-reads the LIVE base for the policy-drift check.
    RepoTracked,
    /// An external/untracked pack capability-read once at approval: the
    /// pinned bytes are the only authority (advisory rules only — the loader
    /// refuses enforced ones), and a later filesystem edit cannot change the
    /// run. `pack_dir` is informational (the as-configured path).
    ExternalPinned,
}

impl StandardsPinSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RepoTracked => "repo-tracked",
            Self::ExternalPinned => "external-pinned",
        }
    }
}

/// One rule's approval-pinned snapshot inside [`StandardsPin`] — the consent
/// surface (D-E): what the operator accepted, verbatim. Strings carry the
/// pack contract's canonical spellings (`must`/`should`,
/// `approved`/`enforced`, stage names, the rendered checker binding) so an
/// old log folds even if the pack vocabulary later grows additively.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedRule {
    pub id: String,
    pub revision: u64,
    /// Parent RFC id (lifecycle grouping; the identity findings join on).
    pub rfc: String,
    pub level: String,
    /// The rule's EFFECTIVE lifecycle at resolution time (`approved` or
    /// `enforced` — the resolver never pins retired or draft rules).
    pub effective_status: String,
    /// The one-line normative statement — the canonical machine/human text.
    pub statement: String,
    /// Browsing/reporting labels (D-D: never a selection input). Kept in the
    /// pin so review and reports render them without a corpus read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domains: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_classes: Vec<String>,
    /// The rendered checker binding (`gate:<id>`, `agent-judgement`,
    /// `manual-attestation`); pinned so evaluation never re-reads it from a
    /// moved source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checker: Option<String>,
    #[serde(default)]
    pub waivable: bool,
}

/// One pack gate declaration copied into the approval pin. Flight Rules
/// checker execution consumes this snapshot, never a later mission-worktree
/// lookup, so changing `pack.toml` on the mission branch cannot rewrite the
/// command that judges that same mission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedGate {
    pub id: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub when_paths: Vec<String>,
}

/// The approval-pinned standards manifest (KRZ-342, design D-E), carried on
/// the plan contract as `standardsManifest`: pack identity + content digest,
/// the selection inputs resolution ran with, and the applicable rule
/// snapshots. The engine resolves and writes it at approval from the trusted
/// source; every later mission stage consumes THIS snapshot — a mission
/// branch edit or an external pack edit cannot reshape it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandardsPin {
    pub pack_name: String,
    /// The pack directory: the repo-relative slash path for `repo-tracked`
    /// (merge/final-validation re-reads address it against a git ref), the
    /// as-configured path for `external-pinned` (display only — external
    /// pins never re-read it).
    pub pack_dir: String,
    /// The normalized `[standards] root` inside the pack.
    pub standards_root: String,
    /// Lowercase hex sha256 over the pack's normalized canonical manifest
    /// text ([`crate::pack::standards::StandardsManifest::digest`]).
    pub digest: String,
    pub source: StandardsPinSource,
    /// The mission task class resolution ran with; `None` when the goal
    /// carried no class (task-class-scoped rules then never apply).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_class: Option<String>,
    /// The approved touch-set globs resolution ran against (D-D input 3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touch_set: Vec<String>,
    /// Read-only paths that participate in applicability without granting
    /// write authority. Review-artifact missions use this for the immutable
    /// spec/incident input; additive for pre-KRZ-349 plans and logs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_paths: Vec<String>,
    /// Every pack gate declaration from the trusted approval source. Rules
    /// reference these by stable id; non-rule pack gates also retain their
    /// pre-Flight-Rules advisory behavior without a live worktree re-read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<PinnedGate>,
    /// The applicable rules (D-D's mission-wide set — the union over the
    /// four workflow stages), stable-sorted by id.
    pub rules: Vec<PinnedRule>,
}

/// One rule reference on a `standards.resolved` event (D-H): the compact,
/// queryable form of a selection. The full snapshots ride in the plan's
/// [`StandardsPin`]; the event stays replay-cheap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandardsRuleRef {
    pub id: String,
    pub revision: u64,
    pub effective_status: String,
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

/// Sibling-candidate linkage for one stream of a heterogeneous dispatch pool
/// (ticket `heterogeneous-dispatch-pool`, KRZ-303; the positioning ADR's
/// 2026-07-31 boundary gloss): one unit of work (a feature) fanned out to N
/// configured backends concurrently, every output recorded as a CANDIDATE FOR
/// JUDGEMENT tied to the same unit — never auto-merged into a winner.
///
/// The sibling set is every run sharing `unit` (the feature id, duplicated
/// here so the linkage is first-class on the record rather than implied by
/// `feature_id`). `index` is the stream's zero-based position in the
/// mission's `workerCandidates` config list; `count` is N. Purely
/// evidentiary: no code path ranks, selects, or merges candidates — selection
/// is a later human judgement act (the divergence follow-up ticket), and the
/// claimed value is divergence for scrutiny, never throughput.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateLink {
    /// The dispatch unit — the feature id fanned out to the pool.
    pub unit: String,
    /// Zero-based position of this stream's backend in `workerCandidates`.
    pub index: u32,
    /// Total streams dispatched for the unit (N).
    pub count: u32,
    /// Backend this stream ran (the run's `model` field alone cannot name
    /// the harness — e.g. a claude-routed and a codex-routed stream may both
    /// record a gpt-family model name after alias normalization).
    pub backend: String,
}

/// One compared candidate stream on a `divergence.noted` event (ticket
/// `divergence-first-class-event`, KRZ-304): the reference to the candidate
/// DIFF the judgement act inspects — the run that produced it, the branch
/// that carries it (KEPT: `kranz/pool/<mission>/<unit>-c<index>` is the
/// deliverable), the backend that ran it, and the tree hash of the branch
/// HEAD at record time. The hash pins the exact bytes the `diverged`
/// verdict was computed from, so replay (provenance, the training corpus)
/// re-reads the record without git; only streams that produced a run
/// record appear (a stream that never started has no diff to compare).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DivergenceCandidate {
    /// The candidate stream's run id — its `worker.spawned` carries the
    /// [`CandidateLink`] for the same unit and index.
    pub run_id: String,
    /// The candidate branch — the deliverable the judging human inspects.
    pub branch: String,
    /// Backend the stream ran ([`CandidateLink::backend`] verbatim).
    pub backend: String,
    /// Tree hash of the branch HEAD at record time.
    pub tree: String,
}

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

/// `skip_serializing_if` for counters whose zero means "predates the field"
/// (e.g. [`MissionState::question_count`]) — zero stays off the wire so
/// pre-field snapshots compare byte-identical.
fn is_zero(value: &u32) -> bool {
    *value == 0
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
    /// Sibling-candidate linkage when this run is one stream of a
    /// heterogeneous dispatch pool (KRZ-303). Absent on ordinary runs (and in
    /// every pre-pool state snapshot); `None` never hits the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<CandidateLink>,
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
    /// Verified by driving `pty_script`'s interactive target through its
    /// scripted terminal session (ticket `pty-functional-validation`):
    /// the engine runs the script in the validation round's evidence pass
    /// (the same pass that executes command assertions, under the same
    /// gate-sandbox wrap), captures the bounded session transcript as a
    /// validation artifact, and hands the functional validator the
    /// per-step verdicts as authoritative evidence — the M5 lane extended
    /// to terminal-native deliverables (REPLs, TUIs, interactive CLIs).
    PtyScript,
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
    /// The scripted terminal session when `check` is `pty-script`
    /// ([`AssertionCheck::PtyScript`]); ignored for every other check.
    /// Additive: `None` in every pre-field contract, and
    /// `skip_serializing_if` keeps old plans byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pty_script: Option<PtyScript>,
}

/// One scripted terminal session against an interactive target — the
/// contract-side declaration a `pty-script` assertion carries (ticket
/// `pty-functional-validation`). This is VALIDATOR tooling: the script
/// judges what the delivered software DOES on a terminal, it never feeds
/// work back into the mission (positioning ADR's retained list).
///
/// WHY inline in the assertion (not a referenced script file): the
/// validation contract is drafted and approved as ONE self-contained
/// plan.json, committed on the mission branch — a script living at a
/// repo path could be edited by the very worker the validation judges,
/// while the contract itself is approval-locked.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PtyScript {
    /// The interactive target, as a shell command line. Executed exactly
    /// like a contract command: the cleared contract env, the resolved
    /// gate-sandbox wrap, and the gate tree as cwd — a pty session never
    /// widens the posture the validator's other evidence runs under.
    pub command: String,
    /// The steps to drive, in order. Every `expect` is one assertion
    /// verdict; the script FAILS at the first unmatched `expect`.
    #[serde(default)]
    pub steps: Vec<PtyStep>,
    /// Overall session cap in seconds (default
    /// [`crate::pty_harness::DEFAULT_SESSION_TIMEOUT_SECS`]): the
    /// whole-script wall-clock bound regardless of per-step timeouts, so
    /// a script of generous expects still cannot hang the round.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

/// One step of a [`PtyScript`], serde-tagged on `op`:
/// `{"op":"send","text":"…"}` / `{"op":"expect","pattern":"…",…}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum PtyStep {
    /// Write `text` to the pty verbatim — JSON string escapes carry the
    /// control bytes (`\n` submits a line to a canonical-mode REPL, `\r`
    /// for raw-mode TUIs, `\u001b` for escape sequences), so no separate
    /// key-name vocabulary is needed.
    Send { text: String },
    /// Block until the accumulated session output contains `pattern`
    /// (a literal substring; a regular expression when `regex` is true)
    /// or the step's timeout elapses. A timeout — or the target exiting
    /// unmatched — FAILS the assertion at this step.
    Expect {
        pattern: String,
        #[serde(default)]
        regex: bool,
        /// Per-step timeout in milliseconds (default
        /// [`crate::pty_harness::DEFAULT_EXPECT_TIMEOUT_MS`]).
        #[serde(rename = "timeoutMs")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
    },
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
    /// Worker-initiated escalation to the frontier advisor (ticket
    /// `backend-routing-abstraction`, KRZ-331): when set, the worker judged
    /// the task beyond its route's confidence and asked for frontier-tier
    /// advice — the VALUE is the worker's reason, verbatim. The engine folds
    /// the request into a record-only `worker.escalated` event naming the
    /// source and target routes; the judgement turn (the frontier advisor)
    /// reads the request from this same report. Never a way to skip
    /// validation: the floor's validator requirements are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<String>,
    /// Structured "ask the human" questions (ticket
    /// `structured-human-question-events`): the worker's structured choices
    /// for a decision only a human can make. The engine opens each as a
    /// `question.opened` event feeding the ONE pending-decision projection
    /// the dashboard and Slack render beside grants (the D-X channel
    /// unification — never a parallel inbox to grants, NeedsContext, or
    /// blocked prose). Absent on prose-only reports (the fallback every
    /// backend without a structured ask keeps), capped at write
    /// (orchestrator.rs), and never a park: the report's own `result` drives
    /// the mission's course exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub questions: Option<Vec<ReportQuestion>>,
}

/// One structured human question inside a [`WorkerReport`] (ticket
/// `structured-human-question-events`). Deliberately id-less: the engine
/// mints the question id at emit time (`q-<n>`, per-mission monotonic from
/// the folded count), so a model-supplied id can never collide with or
/// shadow another question's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportQuestion {
    /// The question text (size-capped + scrubbed at event write).
    pub text: String,
    /// Structured choices the worker offered; EMPTY asks for free text.
    /// Capped per-option and per-list at event write.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
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
    /// The Flight Rules standards rule this finding cites (ticket
    /// `flight-rules-finding-provenance`, KRZ-343; design D-H): the stable
    /// rule/revision join key plus the pinned source identity, carried as
    /// structured data so provenance joins never parse `subject` or
    /// `evidence` prose. Additive: `None` on every pre-KRZ-343 finding and
    /// on every finding that does not cite a rule, and
    /// `skip_serializing_if` keeps those findings byte-identical on the
    /// wire. `subject` stays the human/assertion handle for old consumers;
    /// rule provenance is never smuggled into it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<RuleCitation>,
}

/// The standards rule a [`Finding`] cites (KRZ-343, design D-H): the join
/// key that makes a checker verdict answerable to the approved manifest pin
/// without parsing prose. Every field is the PINNED spelling (the consent
/// snapshot [`StandardsPin`] carries), so a citation joins the mission's
/// approved policy even after the live pack moves on.
///
/// WHY a group and not loose optional fields: a citation is only joinable
/// whole — a rule id without its revision names a moving target (revisions
/// are the semantic-change unit, D-C), and either without the source digest
/// cannot say WHICH approved manifest it answered to. The group is all-or-
/// nothing: `Some` carries the full join key, `None` cites nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleCitation {
    /// The stable rule id (frontmatter `id:`), matching
    /// [`StandardsRuleRef`]'s naming so log-wide joins use one spelling.
    pub id: String,
    /// The pinned revision the verdict was rendered against. A citation at
    /// any other revision does not join the pin — the coverage fold renders
    /// it `not-applicable` rather than joining stale policy.
    pub revision: u64,
    /// The pinned pack identity: pack name + standards root, the display
    /// spelling [`crate::pack::resolution::render_pin_section`] uses.
    pub source: String,
    /// Lowercase hex sha256 of the pinned normalized manifest
    /// ([`StandardsPin::digest`]) — the content binding of the citation.
    pub digest: String,
    /// The rule's pinned EFFECTIVE lifecycle (`approved` or `enforced`):
    /// whether the cited verdict could block (D-B).
    pub lifecycle: String,
    /// The rule's RFC-2119 level (`must` or `should`).
    pub level: String,
    /// The rule's pinned checker binding (`gate:<id>`, `agent-judgement`,
    /// `manual-attestation`) — the mechanism the verdict came from; `None`
    /// when the pinned rule declared none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checker: Option<String>,
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
    /// The pending-decision projection (ticket
    /// `structured-human-question-events`): open structured human questions,
    /// folded from `question.opened`, in open order; `question.answered` /
    /// `question.cleared` remove their entry. Rendered by the dashboard and
    /// Slack in the SAME "your move" area as the parked grant (distinct kind,
    /// shared chrome — the D-X channel unification). Unlike
    /// `pending_grant_request` the run loop never gates on this list; empty
    /// on pre-field logs and omitted from the wire then.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_questions: Vec<PendingQuestion>,
    /// Total `question.opened` events folded — the per-mission monotonic
    /// counter the engine mints the next question id (`q-<n+1>`) from.
    /// Restart-safe by construction (derived from the log, never reset by
    /// answers or clears), so an id is never reused after a process restart.
    /// `0` on pre-field logs and omitted from the wire then.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub question_count: u32,
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
    /// Dispatch-pool units with a recorded resolution, folded from
    /// `divergence.resolved` (ticket `divergence-first-class-event`,
    /// KRZ-304). The engine emits at most one resolution per unit — the
    /// FIRST operator judgement stands — and this set is how the unblock
    /// path knows, across a process restart, that a unit's judgement
    /// already landed. Derived at fold time (state.json is only a cache of
    /// the fold); empty on pre-pool logs and omitted from the wire then.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub resolved_divergence_units: BTreeSet<String>,
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
    /// `config.changed`), else [`ExecutorTier::Frontier`]. A configured
    /// dispatch pool (`worker_candidates`) is always Frontier: pool
    /// candidates are never local-backed (validation rejects `local`
    /// entries), so a stray local `worker.backend` alongside a pool must not
    /// classify the mission's spend as $0-marginal.
    pub fn executor_tier(&self) -> ExecutorTier {
        self.config.executor_tier()
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

/// An open structured human question (ticket
/// `structured-human-question-events`), one entry of the pending-decision
/// projection folded from `question.opened` (see
/// [`MissionState::pending_questions`]). Carries the full context the
/// dashboard and Slack need to render the decision without a join: the ask,
/// its structured choices (empty = free text), and who/where it came from.
/// Unlike [`PendingGrantRequest`] this parks NOTHING — the run loop does not
/// gate on it; the question rides alongside the mission until the operator
/// answers (`question.answered`) or it stops being actionable
/// (`question.cleared`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingQuestion {
    /// Engine-minted id (`q-<n>`, per-mission monotonic) — the handle every
    /// answer path (REST/Slack/CLI) names.
    pub question_id: String,
    /// Who asked — `worker` in this pass.
    pub role: Role,
    /// The question text (scrubbed + capped at write).
    pub text: String,
    /// The structured choices offered (empty = free-text answer expected).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// The run whose report carried the ask (context ref).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Feature the asking run worked on (context ref).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_id: Option<String>,
    /// Milestone the asking run worked under (context ref; the
    /// clear-on-complete sweep keys on it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone_id: Option<String>,
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
    /// [`crate::backend_kimi::KimiBackend`], `"local"` selects an
    /// OpenAI-compatible HTTP endpoint, `"acp"` selects
    /// [`crate::backend_acp::AcpBackend`] (worker role only), and `"cursor"`
    /// selects [`crate::backend_cursor::CursorBackend`] (opt-in,
    /// validator-first; docs/scoping/cursor-cli-backend.md).
    /// `config::validate` checks that
    /// the selected backend/model pair is supported for the role.
    ///
    /// The guarded local-validator boundary (KRZ-206b): on the
    /// `validatorFunctional` role `"local"` is allowed for DETERMINISTIC
    /// mechanical checks only (contract-command pass/fail against
    /// engine-captured exit codes) — every local PASS is frontier-confirmed
    /// before it greens a gate, and a local FAIL is trusted unconfirmed
    /// (failures are visible; misses are the danger). On the
    /// `validatorScrutiny` role `"local"` is rejected outright: scrutiny is
    /// judgment, and a local judgment PASS is the silent-green failure mode
    /// the split exists to prevent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,
    /// Base URL of the OpenAI-compatible HTTP endpoint for `backend = "local"`.
    /// Required and validated when a role selects the local backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Executable of the ACP (Agent Client Protocol) agent for
    /// `backend = "acp"`. Required and validated when a role selects the acp
    /// backend (KRZ-301; worker role only for now).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_command: Option<String>,
    /// Extra argv for `acpCommand` (model flags, agent-specific options —
    /// ACP itself has no standard model-selection parameter in v1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acp_args: Vec<String>,
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

impl SandboxEnforce {
    /// The config-file spelling of this mode (`"off"` / `"fs"` / `"fs+net"`),
    /// for errors and reports that name the requested enforcement.
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxEnforce::Off => "off",
            SandboxEnforce::Fs => "fs",
            SandboxEnforce::FsNet => "fs+net",
        }
    }
}

/// Which sandbox mechanism wraps a role's sessions when `enforce` is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SandboxProvider {
    /// Tier-2 process sandboxing (Seatbelt on macOS, bubblewrap on Linux).
    #[default]
    Process,
    /// Tier-3 container sandboxing on live-proven macOS/Linux hosts (see
    /// `crate::sandbox_container`). Windows refuses until its mount and
    /// authority-mask contract has a real hostile-host receipt.
    Container,
}

impl SandboxProvider {
    /// The config-file spelling of this provider (`"process"` / `"container"`),
    /// for errors and reports that name it.
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxProvider::Process => "process",
            SandboxProvider::Container => "container",
        }
    }

    /// Whether `enforce = "fs+net"` under this provider is backed by a HARD
    /// network boundary for the given egress list — a session that ignores the
    /// run's egress-proxy env vars still cannot open a direct socket. The
    /// process provider qualifies on both supported platforms (macOS Seatbelt
    /// cuts outbound TCP to loopback; Linux bwrap `--unshare-net` removes the
    /// network entirely), so its proxy hop is the only reachable way out. The
    /// container provider qualifies only on its supported macOS/Linux hosts
    /// and with an EMPTY egress list
    /// (`--network none`); a non-empty list keeps the runtime's default
    /// bridge, where the proxy env vars are advisory and a direct socket
    /// bypasses the filter — `config::validate` rejects that pair (fail
    /// closed) until the internal-network/sidecar boundary exists
    /// (docs/scoping/worker-sandboxing.md tier 3). The match is deliberately
    /// exhaustive: a future provider must declare itself here.
    pub fn enforces_hard_net_boundary(self, egress: &[String]) -> bool {
        match self {
            SandboxProvider::Process => true,
            SandboxProvider::Container => egress.is_empty(),
        }
    }
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
    /// example package registries). Stored as `host:port` strings. With
    /// `provider = "container"` a non-empty list is refused by
    /// `config::validate` (proxy-env advisory only — see
    /// [`SandboxProvider::enforces_hard_net_boundary`]); an empty list keeps
    /// the hard `--network none` boundary.
    pub egress: Vec<String>,
}

/// One backend+model pairing in the heterogeneous dispatch pool
/// (`MissionConfig::worker_candidates`, ticket `heterogeneous-dispatch-pool`
/// / KRZ-303). Structured rather than a `"backend/model"` string so
/// `config::validate` can apply the exact same backend/model pair checks as a
/// role selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateSpec {
    /// Backend name, same vocabulary as [`RoleConfig::backend`]. `local` and
    /// `acp` are rejected at validation in this pass: their per-role
    /// endpoint/command config (`baseUrl`/`acpCommand`) has no per-candidate
    /// home yet — a deliberate widening, never a silent share.
    pub backend: String,
    /// Model alias or id, interpreted against `backend`'s table by the same
    /// `effective_model` / `model_tier` rules as a role selection.
    pub model: String,
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
    /// ACP (Agent Client Protocol) agent executable, configured via the
    /// role's `acpCommand`/`acpArgs` (KRZ-301; worker role only).
    Acp,
    /// Cursor CLI (`agent --print --output-format stream-json`), the decided
    /// `direct-parser` route (docs/scoping/cursor-cli-backend.md). Opt-in
    /// only, validator-first; never a default.
    Cursor,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Claude => "claude",
            BackendKind::Codex => "codex",
            BackendKind::Droid => "droid",
            BackendKind::Kimi => "kimi",
            BackendKind::Local => "local",
            BackendKind::Acp => "acp",
            BackendKind::Cursor => "cursor",
        }
    }

    /// Whether this backend applies the engine-resolved OS sandbox
    /// ([`crate::backend::SessionSpec::sandbox`]) to its sessions. Only the
    /// claude backend wraps its spawned CLI in the resolved sandbox today;
    /// the other CLI backends spawn their binaries directly (and `local`
    /// runs in the engine process), so an enforced sandbox on them would be
    /// silently unenforced — `config::validate` rejects that pair (fail
    /// closed). Cursor's own `--sandbox` flag is observed to be no isolation
    /// boundary (docs/scoping/cursor-cli-backend.md), so it does not count
    /// either. The match is deliberately exhaustive: a future backend must
    /// declare itself here.
    pub fn supports_sandbox_enforcement(self) -> bool {
        match self {
            BackendKind::Claude => true,
            BackendKind::Codex
            | BackendKind::Droid
            | BackendKind::Kimi
            | BackendKind::Local
            | BackendKind::Acp
            | BackendKind::Cursor => false,
        }
    }

    /// Whether this backend's wire reports cache-READ input tokens the
    /// parser records (outcomes-report context-reuse split, ticket
    /// `outcomes-report-task-class`). Claude/droid read
    /// `cache_read_input_tokens`; codex reads `cached_input_tokens`; cursor
    /// reads `cacheReadTokens` off the terminal result event (present in the
    /// committed fixture). Kimi's
    /// wire carries no usage at all, local hardcodes zeros, and ACP v1's
    /// `usage_update` reports context-window state rather than a token
    /// split — for all three, a zero would be fabricated, so they report
    /// nothing (absent, never 0%).
    /// The match is deliberately exhaustive: a future backend must declare
    /// itself here.
    pub fn reports_cache_read_tokens(self) -> bool {
        match self {
            BackendKind::Claude | BackendKind::Codex | BackendKind::Droid | BackendKind::Cursor => {
                true
            }
            BackendKind::Kimi | BackendKind::Local | BackendKind::Acp => false,
        }
    }

    /// Whether this backend's wire reports cache-WRITE (creation) input
    /// tokens. Claude/droid carry `cache_creation_input_tokens` and cursor
    /// carries `cacheWriteTokens` on its terminal result event; codex
    /// has no such field (its cache write side is never recorded, so the
    /// split's cache-write column is absent for codex, never zero-filled).
    pub fn reports_cache_write_tokens(self) -> bool {
        match self {
            BackendKind::Claude | BackendKind::Droid | BackendKind::Cursor => true,
            BackendKind::Codex | BackendKind::Kimi | BackendKind::Local | BackendKind::Acp => false,
        }
    }

    /// Whether this backend exposes a lifecycle-hook surface the
    /// hook-status lane can project onto (ticket
    /// `agent-hooks-status-signals`, [`crate::hook_status`]). Only the
    /// cursor CLI's documented `hooks.json` lifecycle events qualify today;
    /// every other backend IGNORES [`crate::backend::SessionSpec::hook_status`]
    /// exactly like `settings_json`, so an enabled lane is a byte-identical
    /// no-op there (the hooks-disabled regression). The match is
    /// deliberately exhaustive: a future backend must declare itself here.
    pub fn supports_hook_status_signals(self) -> bool {
        match self {
            BackendKind::Cursor => true,
            BackendKind::Claude
            | BackendKind::Codex
            | BackendKind::Droid
            | BackendKind::Kimi
            | BackendKind::Local
            | BackendKind::Acp => false,
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

/// The backend routing table (ticket `backend-routing-abstraction`, KRZ-331):
/// the declarative form of "task class → executor route", making local
/// endpoints, hosted frontier models, and hosted fine-tunes peers behind one
/// routing interface. Resolved deterministically by [`crate::routing`]:
/// the FIRST matching rule wins, no match falls through to
/// [`ExecutorTier::Frontier`], and an EMPTY table keeps the hardcoded literal
/// floor ([`crate::config::task_class_to_tier`]) byte-for-byte.
///
/// Rules name CAPABILITY CLASSES ([`ExecutorTier`]), never model ids
/// (docs/reviews/local-llm-and-triumvirate.md §1): a local endpoint, a
/// hosted OpenAI-compatible frontier endpoint, and a hosted fine-tune are
/// all the `local` class — which concrete endpoint the class resolves to is
/// ordinary local-backend role config (`baseUrl` + `model`), not routing
/// table content and not a new backend kind. The tracked, base-branch-owned
/// rules FILE surface ([`crate::routing_rules`], ticket
/// `routing-rules-config`) is the tracked way to populate this table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RoutingConfig {
    /// Ordered routing rules; the first rule whose `taskClass` matches the
    /// ticket's task class (case- and whitespace-insensitively, the same
    /// normalization as the hardcoded floor) decides the executor tier.
    pub task_class_rules: Vec<TaskClassRoute>,
    /// Ordered PATTERN rules (ticket `routing-rules-config`), consulted only
    /// when no exact `taskClassRules` entry matched: the first pattern that
    /// matches the normalized task class decides the executor tier. An exact
    /// class rule always beats a pattern (specific over general); within this
    /// list, order is the only precedence knob. Additive: absent in every
    /// pre-pattern config and every pre-pattern `mission.created` payload,
    /// where it deserializes to empty.
    #[serde(default)]
    pub pattern_rules: Vec<PatternRoute>,
}

impl RoutingConfig {
    /// No rules of either form — the empty table that keeps the hardcoded
    /// literal floor ([`crate::config::task_class_to_tier`]) byte-for-byte.
    pub fn is_empty(&self) -> bool {
        self.task_class_rules.is_empty() && self.pattern_rules.is_empty()
    }
}

/// One routing rule: a task class routed to an executor capability class.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskClassRoute {
    /// The ticket `task-class` frontmatter value this rule matches, compared
    /// trimmed and case-insensitively. Must be non-empty and unique within
    /// the table after normalization — `config::validate` fails closed
    /// otherwise (a duplicate is dead config under first-match-wins).
    pub task_class: String,
    /// The capability class the matched task class routes to.
    pub tier: ExecutorTier,
}

/// One PATTERN routing rule (ticket `routing-rules-config`): a task-class
/// pattern routed to an executor capability class. The pattern language is
/// deliberately tiny and deterministic — `*` matches any (possibly empty)
/// run of characters, every other character is literal, comparison happens
/// after the floor's normalization (trim + ASCII-lowercase). Must be
/// non-empty and unique within the pattern list after normalization —
/// `config::validate` fails closed otherwise (a duplicate is dead config
/// under first-match-wins).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatternRoute {
    /// The glob-style pattern matched against the normalized task class.
    pub pattern: String,
    /// The capability class a matching task class routes to.
    pub tier: ExecutorTier,
}

/// The effective executor route of one worker session (ticket
/// `routing-rules-config`), recorded additively on `worker.spawned`: routing
/// is provenance, not a hidden implementation detail. Derived once at fold
/// time from `mission.created`'s original folded goal and routed config
/// ([`crate::routing::seed_executor_route`]) — the determinism contract
/// guarantees the recomputation equals the seed-time decision, so no new
/// event payload is needed — then replayed onto each worker spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutorRoute {
    /// The EFFECTIVE capability class the session runs on — after the
    /// fail-safes (a `local` route with no configured endpoint lands the
    /// worker back on `frontier`), derived from the routed config exactly as
    /// [`MissionState::executor_tier`] derives it.
    pub tier: ExecutorTier,
    /// The rule that decided the route, named by its position in the table
    /// (`taskClassRules[i]` / `patternRules[i]`). `None` when no rule
    /// decided it: the fall-through to `frontier`, or the legacy literal
    /// floor with no table configured at all. Never serialized when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
}

/// How worker/validator sessions are isolated from the primary checkout.
///
/// Default is [`WorkerIsolation::Worktree`]: the primary checkout must stay
/// byte-untouched across a mission (AGENTS.md). Operators may still opt into
/// [`WorkerIsolation::Checkout`]
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

/// Default for [`MissionConfig::rubber_stamp_threshold_ms`] (ticket
/// `rubber-stamp-grant-flag`): the docs/metrics.md §2 sub-ten-second bucket,
/// made configurable. A grant APPROVED in under this latency is flagged as a
/// rubber-stamp signal in the outcomes report — a flag, never an enforcement.
pub const DEFAULT_RUBBER_STAMP_THRESHOLD_MS: u64 = 10_000;

/// Hook-derived status-signal lane config (ticket
/// `agent-hooks-status-signals`, [`crate::hook_status`]): an OPTIONAL,
/// off-by-default observability lane for backends with a lifecycle-hook
/// surface ([`BackendKind::supports_hook_status_signals`] — cursor only
/// today). When enabled, worker sessions on hook-capable backends get a
/// per-run capability token + hook install that reports coarse signals
/// ("running" / "needs input" / "interrupted" / "turn finished") to
/// `endpoint`; the signals land ONLY in the ephemeral `.kranz/hook-status/`
/// projection, never in mission state. When disabled (the default) every
/// session is byte-identical to today — no hook config anywhere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct HookStatusConfig {
    /// Master switch, off by default.
    pub enabled: bool,
    /// The full loopback signal POST URL the per-session relay
    /// (`kranz hook-status`) delivers to — e.g.
    /// `http://127.0.0.1:4560/api/hook-status`. Required when `enabled`;
    /// `config::validate` refuses non-loopback or non-HTTP(S) values
    /// (the per-run capability token rides this URL).
    pub endpoint: String,
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
    /// Pack contract (ticket `pack-contract-gates-prompts`): directory of the
    /// pack this mission runs with — a `pack.toml` declaring deterministic
    /// gates, role prompts, checklists, and artefact stores
    /// (docs/pack-contract.md). Relative paths resolve against the repo
    /// root. Loaded and validated (fail-closed, naming the offending field)
    /// at run start and at each consuming surface; absent ⇒ byte-identical
    /// pack-less behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_dir: Option<String>,
    /// Outcomes-report flag threshold (ticket `rubber-stamp-grant-flag`):
    /// grants APPROVED in under this many milliseconds are flagged as
    /// rubber-stamp signals — a flag on a report row, never an enforcement.
    /// The default (10s) is the docs/metrics.md §2 bucket made configurable.
    pub rubber_stamp_threshold_ms: u64,
    /// Heterogeneous dispatch pool (ticket `heterogeneous-dispatch-pool`,
    /// KRZ-303; the positioning ADR's 2026-07-31 boundary gloss). Empty (the
    /// default) is today's single-backend worker behavior EXACTLY. With ≥2
    /// candidates, every worker feature — the unit of work — is dispatched to
    /// ALL of them concurrently, one git worktree per stream, and every
    /// output is recorded as a sibling [`CandidateLink`]ed run: a candidate
    /// for judgement, never auto-merged into a winner (no code path selects
    /// or merges one), and the mission then parks for the human judgement act
    /// the follow-up divergence ticket surfaces. The claimed value is
    /// divergence for scrutiny, never throughput; cost multiplies by N and
    /// the approval-time estimate prices the SUM. `config::validate` rejects
    /// a 1-entry list (use `worker.backend`), `local`/`acp` entries (no
    /// per-candidate endpoint config in this pass), and combining the pool
    /// with `maxParallelWorkers > 1` (a different fan-out model).
    #[serde(default)]
    pub worker_candidates: Vec<CandidateSpec>,
    /// The backend routing table (ticket `backend-routing-abstraction`,
    /// KRZ-331): ordered task-class → executor-tier rules, resolved
    /// deterministically by [`crate::routing`] at mission seed time
    /// ([`crate::config::route_task_class_executor`]). Empty (the default)
    /// keeps today's hardcoded literal floor byte-for-byte. Capability
    /// classes only — a rule names an [`ExecutorTier`], never a model id;
    /// which endpoint a `local` route resolves to is ordinary local-backend
    /// role config, so a hosted fine-tune needs no new kind here.
    #[serde(default)]
    pub routing: RoutingConfig,
    /// Hook-derived status-signal lane (ticket
    /// `agent-hooks-status-signals`). Absent/disabled = byte-identical
    /// pre-lane behavior; enabled installs hook config ONLY into
    /// session-private scratch HOMEs on hook-capable backends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_status: Option<HookStatusConfig>,
    /// EXPLICIT per-repo opt-in for the uncontained-validator degrade
    /// (ticket `validator-containment-degrade-fail-closed`, 14th-pass
    /// review): when the mandatory validator containment wrap cannot apply
    /// (an uncontainable platform, linux without `bwrap`, a validator
    /// backend that does not honor the resolved sandbox), validation now
    /// FAILS CLOSED by default — the degrade reopens the modify→use→restore
    /// path the mandatory-containment work was built to close. This reverses
    /// the recorded 224fa73 decision (loud-degrade-by-default); setting this
    /// true restores that posture: the validator runs uncontained with the
    /// loud per-round degradation decision, snapshot isolation, and the
    /// after-fingerprint tripwire as the only remaining layers.
    #[serde(default)]
    pub validator_allow_uncontained_degrade: bool,
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
                acp_command: None,
                acp_args: vec![],
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
                acp_command: None,
                acp_args: vec![],
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
                acp_command: None,
                acp_args: vec![],
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
                acp_command: None,
                acp_args: vec![],
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
            pack_dir: None,
            rubber_stamp_threshold_ms: DEFAULT_RUBBER_STAMP_THRESHOLD_MS,
            worker_candidates: vec![],
            routing: RoutingConfig::default(),
            hook_status: None,
            validator_allow_uncontained_degrade: false,
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
            Some("acp") => BackendKind::Acp,
            Some("cursor") => BackendKind::Cursor,
            _ => BackendKind::Claude,
        }
    }

    /// Which inference tier the Worker executes on under this config, derived
    /// from the Worker `RoleConfig.backend` rather than stored:
    /// [`ExecutorTier::Local`] when the Worker backend is
    /// [`BackendKind::Local`], else [`ExecutorTier::Frontier`]. A configured
    /// dispatch pool (`worker_candidates`) is always Frontier: pool
    /// candidates are never local-backed (validation rejects `local`
    /// entries), so a stray local `worker.backend` alongside a pool must not
    /// classify the spend as $0-marginal. The config-level home of the
    /// derivation — [`MissionState::executor_tier`] delegates here, and the
    /// per-session route record ([`ExecutorRoute`]) reads the same source.
    pub fn executor_tier(&self) -> ExecutorTier {
        if self.worker_candidates.is_empty()
            && self.backend_kind(Role::Worker) == BackendKind::Local
        {
            ExecutorTier::Local
        } else {
            ExecutorTier::Frontier
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
    /// Answer an open structured question (ticket
    /// `structured-human-question-events`) — the pending-decision
    /// projection's input edge, submitted through this EXISTING control path
    /// (the D-X ruling: no new server). `answer` is the chosen option's text
    /// verbatim or the operator's free text (scrubbed + capped when the
    /// engine lands it as `question.answered`); `option` records the 0-based
    /// index when an offered option was picked, and the engine cross-checks
    /// it against the parked question exactly like the grant approve/deny
    /// commands echo their target — a stale answer can't land on a different
    /// question than the operator saw.
    AnswerQuestion {
        #[serde(rename = "questionId")]
        question_id: String,
        answer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option: Option<u32>,
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
    fn routing_abstraction_worker_report_escalation_is_additive() {
        // KRZ-331: old reports (no escalation key) parse with no request…
        let json = r#"{"result": "pass", "summary": "did the thing"}"#;
        let report: WorkerReport = serde_json::from_str(json).unwrap();
        assert_eq!(report.escalation, None);
        // …None never hits the wire…
        let value = serde_json::to_value(&report).unwrap();
        assert!(value.get("escalation").is_none());
        // …and a request round-trips camelCase verbatim.
        let json = r#"{"result": "partial", "summary": "s", "escalation": "spec ambiguity beyond my confidence"}"#;
        let report: WorkerReport = serde_json::from_str(json).unwrap();
        assert_eq!(
            report.escalation.as_deref(),
            Some("spec ambiguity beyond my confidence")
        );
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["escalation"], "spec ambiguity beyond my confidence");
    }

    /// The additive `questions` on the worker report (ticket
    /// `structured-human-question-events`): prose-only reports (the fallback)
    /// parse with no questions, None never hits the wire, and a structured
    /// ask round-trips — options included, or absent for a free-text ask.
    #[test]
    fn question_events_worker_report_questions_are_additive() {
        // Old/prose-only report (no questions key): parses with None…
        let json = r#"{"result": "pass", "summary": "did the thing"}"#;
        let report: WorkerReport = serde_json::from_str(json).unwrap();
        assert_eq!(report.questions, None);
        // …and None stays off the wire.
        let value = serde_json::to_value(&report).unwrap();
        assert!(value.get("questions").is_none());

        // A structured ask (with options and without) round-trips verbatim.
        let json = r#"{
            "result": "partial",
            "summary": "blocked on a human choice",
            "questions": [
                { "text": "Which storage engine?", "options": ["sqlite", "in-memory"] },
                { "text": "What should the flag be called?" }
            ]
        }"#;
        let report: WorkerReport = serde_json::from_str(json).unwrap();
        let questions = report.questions.as_ref().expect("questions parsed");
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].text, "Which storage engine?");
        assert_eq!(questions[0].options, vec!["sqlite", "in-memory"]);
        assert_eq!(questions[1].options, Vec::<String>::new());
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["questions"][0]["options"][1], "in-memory");
        // Empty options stay off the wire (a free-text ask carries no key).
        assert!(value["questions"][1].get("options").is_none());
    }

    /// The `answer-question` control kind (ticket
    /// `structured-human-question-events`): kebab-case wire name, camelCase
    /// `questionId` (matching the event payload + REST body convention), and
    /// `option` additive — absent for free-text answers and never on the
    /// wire then.
    #[test]
    fn question_events_answer_control_kind_wire_shape() {
        let cmd = ControlCommand::AnswerQuestion {
            question_id: "q-1".into(),
            answer: "sqlite".into(),
            option: Some(0),
        };
        let json = serde_json::to_value(&cmd).unwrap();
        assert_eq!(json["kind"], "answer-question");
        assert_eq!(json["questionId"], "q-1");
        assert_eq!(json["answer"], "sqlite");
        assert_eq!(json["option"], 0);
        let back: ControlCommand = serde_json::from_value(json).unwrap();
        match back {
            ControlCommand::AnswerQuestion {
                question_id,
                answer,
                option,
            } => {
                assert_eq!(question_id, "q-1");
                assert_eq!(answer, "sqlite");
                assert_eq!(option, Some(0));
            }
            _ => panic!("wrong variant"),
        }

        // A free-text answer (no option index) omits the key, and a wire
        // line without it parses back to None (serde default).
        let cmd = ControlCommand::AnswerQuestion {
            question_id: "q-2".into(),
            answer: "call it --cache-dir".into(),
            option: None,
        };
        let json = serde_json::to_value(&cmd).unwrap();
        assert!(
            !json.as_object().unwrap().contains_key("option"),
            "option must not serialize when None: {json}"
        );
        let sparse: ControlCommand = serde_json::from_str(
            r#"{"kind":"answer-question","questionId":"q-2","answer":"call it --cache-dir"}"#,
        )
        .unwrap();
        match sparse {
            ControlCommand::AnswerQuestion { option, .. } => assert_eq!(option, None),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn finding_class_round_trips_through_serde() {
        let finding = Finding {
            subject: "a-1".to_string(),
            severity: "major".to_string(),
            evidence: "wrote outside touch-set".to_string(),
            suggested_fix: String::new(),
            class: "out-of-contract-write".to_string(),
            rule: None,
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
