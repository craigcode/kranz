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

// Missing field means a legacy event; explicit null is not an escape hatch
// from typed ownership. Deserialize the present value as a context, not Option.
fn deserialize_block_context<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<BlockContext>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    BlockContext::deserialize(deserializer).map(Some)
}

/// Serialized as `"type": "<dotted.name>", "payload": { ... }`.
// large_enum_variant: MissionCreated carries the full MissionConfig (~456B).
// It occurs once per mission and events are I/O-bound; boxing would ripple
// through every construction/match site for no measurable win.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum EventKind {
    #[serde(rename = "gate.evaluation-requested")]
    GateEvaluationRequested {
        evaluation: Box<crate::gate_evaluation::lifecycle::Requested>,
    },
    #[serde(rename = "gate.evaluation-finished")]
    GateEvaluationFinished {
        evaluation: Box<crate::gate_evaluation::lifecycle::Finished>,
    },
    #[serde(rename = "gate.resolution-recorded")]
    GateResolutionRecorded {
        resolution: crate::gate_evaluation::lifecycle::Resolution,
    },
    #[serde(rename = "gate.resolution-consumed")]
    GateResolutionConsumed {
        consumption: crate::gate_evaluation::lifecycle::Consumed,
    },
    #[serde(rename = "permission.requested")]
    PermissionRequested {
        request: crate::live_permission::Request,
    },
    #[serde(rename = "permission.resolved")]
    PermissionResolved {
        resolution: crate::live_permission::Resolution,
    },
    #[serde(rename = "permission.response-recorded")]
    PermissionResponseRecorded {
        #[serde(rename = "requestId")]
        request_id: String,
        delivery: crate::live_permission::Delivery,
    },
    #[serde(rename = "permission.closed")]
    PermissionClosed {
        #[serde(rename = "requestId")]
        request_id: String,
        reason: String,
    },
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

    /// Engine-observed sequential feature baseline and cumulative commit receipts.
    /// Recorded before execution and after checkpoints, so retries and resume
    /// cannot turn retained work into an apparently commitless feature.
    #[serde(rename = "feature.progress")]
    FeatureProgress {
        #[serde(rename = "featureId")]
        feature_id: String,
        #[serde(rename = "baseSha")]
        base_sha: String,
        commits: Vec<String>,
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
        /// Sibling-candidate linkage when this run is one stream of a
        /// heterogeneous dispatch pool (KRZ-303): which unit it belongs to,
        /// the stream's index, N, and the backend it ran. Additive; absent on
        /// ordinary runs and in every pre-pool log — `None` never hits the
        /// wire. Carried on `worker.spawned` (not `worker.completed`) so the
        /// run is a labelled candidate from the moment it exists.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        candidate: Option<CandidateLink>,
        /// The effective executor route and the rule that decided it (ticket
        /// `routing-rules-config`): routing is provenance, not a hidden
        /// implementation detail, so it rides the same event that already
        /// records the model. Additive; present only on Worker-role spawns of
        /// missions whose seed carried a task class — absent everywhere else
        /// and in every pre-provenance log, where it folds to `None` and
        /// `None` never hits the wire.
        #[serde(
            rename = "executorRoute",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        executor_route: Option<crate::types::ExecutorRoute>,
        #[serde(rename = "sdkSessionId")]
        sdk_session_id: String,
        model: String,
        /// Actual dispatch backend after resolution/fallback; absent in old logs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backend: Option<crate::types::BackendKind>,
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

    /// Durable, run-attributed audit record for destinations refused by the
    /// filtering egress proxy. Record-only: grant handling still uses
    /// the in-memory [`crate::egress_proxy::EgressDenial`] returned by the
    /// session, while this event survives runtime-artifact cleanup and can
    /// be projected as bounded validator evidence.
    #[serde(rename = "worker.egress.denied")]
    WorkerEgressDenied {
        #[serde(rename = "runId")]
        run_id: String,
        denials: Vec<crate::egress_proxy::EgressDenial>,
        /// Repeated or over-cap denial records excluded from `denials`.
        /// Additive default keeps an early/pre-field event readable.
        #[serde(rename = "omittedCount", default)]
        omitted_count: u64,
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
        /// Commits the failed feature landed on the mission branch before the
        /// judgement (empty for a run that never committed — the m-eee81f
        /// auth-death class — and for parallel/dirty-tree paths where nothing
        /// reached the branch). Recorded so the supersession guard can tell
        /// "failed with real work" (started; re-proposal rejects) from
        /// "failed commitless" (re-proposable). Additive; old logs default
        /// to empty.
        #[serde(default)]
        commits: Vec<String>,
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

    /// Local-validator confirm-on-pass (ticket
    /// `local-inference-validator-guarded`, KRZ-206b; review addendum §4 of
    /// docs/scoping/local-inference-executor-tier.md): a LOCAL functional
    /// validator's PASS never greens a gate alone — a frontier functional
    /// session re-judged the same milestone and engine-captured
    /// contract-command evidence, and this event records the comparison.
    /// `confirmed` names the contract-command assertions both tiers pass;
    /// `disagreements` carries every frontier finding on a subject the local
    /// report passed (a local PASS vs frontier FAIL — the miss), each of
    /// which ALSO lands as a `validation.finding` and fails closed into the
    /// round as the frontier verdict. A local FAIL never triggers this
    /// event: failures are visible (they cost a fix cycle), misses are the
    /// danger — the asymmetry is deliberate.
    ///
    /// The confirmations ARE the local-vs-frontier miss-rate ground truth
    /// the ticket's start precondition demands: misses = disagreement
    /// subjects, opportunities = confirmed + disagreement command
    /// assertions + `judgmentOpportunity` (0/1), all computable from the
    /// log alone (join `localRunId` / `confirmRunId` against
    /// `worker.spawned` for the models). Additive event; absent in
    /// pre-field logs, which simply have no local-validator confirmations
    /// to measure.
    #[serde(rename = "validation.confirm")]
    ValidationConfirm {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        /// Run id of the LOCAL functional session whose PASS was confirmed.
        #[serde(rename = "localRunId")]
        local_run_id: String,
        /// Run id of the FRONTIER confirmation session.
        #[serde(rename = "confirmRunId")]
        confirm_run_id: String,
        /// Contract-command assertion ids both the local report and the
        /// frontier confirmation pass.
        confirmed: Vec<String>,
        /// Frontier findings on subjects the local report passed — the
        /// misses. Failed closed: each stands as the round's verdict.
        disagreements: Vec<Finding>,
        /// True when the confirmed PASS was JUDGMENT-only: a contract with
        /// no command assertions hands the local session pure judgment, and
        /// its all-clean report is confirmed exactly like a command-
        /// assertion PASS — but there are no assertion ids to list, so
        /// `confirmed`/`disagreements` alone would record ZERO opportunities
        /// for a confirmation that covered one, silently undercounting the
        /// miss-rate denominator (14th-pass review). Additive; absent
        /// (= false) in logs predating the field, which simply never
        /// recorded a judgment-only confirmation.
        #[serde(default, rename = "judgmentOpportunity")]
        judgment_opportunity: bool,
    },

    /// Pty-driven functional validation: one engine-run pty-script contract
    /// assertion (ticket `pty-functional-validation`, module
    /// [`crate::pty_harness`]) produced a bounded session transcript under
    /// the mission's gitignored `runs/pty-transcripts/`; this audit record
    /// names the milestone, the assertion, the stated verdict, and the
    /// transcript's `file:`-schemed mission-relative reference (the
    /// [`crate::gate_results`] ArtefactRef idiom — mission-relative, never
    /// an absolute host path, resolving to "unresolved" rather than erroring
    /// once the bytes are pruned). Record-only: the verdict reaches the
    /// round through the functional validator's evidence block, not through
    /// this event, so the reducer treats it as an audit record exactly like
    /// `validation.snapshot`. Additive event; absent in pre-field logs,
    /// which simply have no pty-driven validations.
    #[serde(rename = "validation.pty.transcript")]
    ValidationPtyTranscript {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        /// The contract assertion the session drove.
        #[serde(rename = "assertionId")]
        assertion_id: String,
        /// The verdict the harness stated — pass (every expect matched) or
        /// fail (the failing session's transcript is the evidence).
        verdict: crate::gate::GateVerdict,
        /// The `file:`-schemed mission-relative transcript path.
        #[serde(rename = "artefactRef")]
        artefact_ref: String,
        /// The per-step summary (contract-authored patterns and timings —
        /// no raw target output).
        #[serde(default, skip_serializing_if = "Option::is_none")]
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
        /// The stable Flight Rules standards rule ids this evaluation
        /// joined (KRZ-343, design D-H): the linkage the coverage matrix
        /// joins on, from [`crate::gate::GateOutcome::rule_ids`]. Additive
        /// and evidentiary only — a gate with no standards linkage carries
        /// an empty list, which never hits the wire, so a boolean-only
        /// gate's payload stays byte-identical.
        #[serde(rename = "ruleIds", default, skip_serializing_if = "Vec::is_empty")]
        rule_ids: Vec<String>,
    },

    /// One deterministic gate projected onto a Claude Code lifecycle hook
    /// fired IN-PROCESS inside a worker session (ticket
    /// `.kranz/tickets/claude-code-hook-gate-projection.md`, KRZ-302; module
    /// [`crate::hook_gates`]). The first projection is the out-of-contract
    /// write rule: a `PreToolUse` hook on the file-writing tools judges the
    /// target path against the mission's `touch_set` and blocks an
    /// out-of-contract write before it happens. The payload carries the
    /// gate identity, the hook event, the tool, the judged path, and the
    /// guard's verdict (`blocked` — refused in-process; `error` — the guard
    /// itself failed open, so only the engine-side sweep can judge it).
    ///
    /// Additive, RECORD-ONLY (the `gate.result` template): the engine-side
    /// gate ladder remains authoritative — hooks are defense-in-depth, never
    /// a replacement — so this event drives no state transition; it is the
    /// in-process layer's evidence landing in the log (folded from the
    /// per-session record file after the session stream closes, BEFORE
    /// `worker.completed`). `runId` is stamped from the run's metadata at
    /// fold time, never from the session-writable record file.
    #[serde(rename = "hook.gate.fired")]
    HookGateFired {
        #[serde(rename = "runId")]
        run_id: String,
        /// Gate identity (e.g. `out-of-contract-write`) — the same
        /// defect-class name the engine-side sweep reports, so one gate
        /// reads at two layers.
        gate: String,
        /// The lifecycle event that fired (`PreToolUse`).
        #[serde(rename = "hookEvent")]
        hook_event: String,
        /// The tool whose call was judged (`Write`, `Edit`, ...).
        tool: String,
        /// The judged target (repo-relative when it resolved inside the
        /// checkout, else the raw path).
        subject: String,
        /// `blocked` | `error` (see the variant docs).
        verdict: String,
        /// The guard's reason / error note, scrubbed and truncated at fold
        /// time. Absent when the record carried none; `None` never hits the
        /// wire.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },

    /// The candidate-comparison record of a heterogeneous dispatch pool
    /// (ticket `divergence-first-class-event`, KRZ-304; the follow-up the
    /// KRZ-303 pool parks for): when a unit's sibling streams have all
    /// recorded and the engine parks the milestone for judgement, the
    /// candidate branch TREES are compared and exactly one of these is
    /// appended, naming the unit, every compared candidate (run id, branch,
    /// backend, tree hash — see [`crate::types::DivergenceCandidate`]), and
    /// the verdict.
    ///
    /// **Agreement between models is a signal to log, never a criterion to
    /// trust.** Identical candidate trees produce THE SAME record kind with
    /// `diverged: false` — the agreement record: logged, never trusted. A
    /// unit is done when gates are green and no escalation is open, not
    /// when streams stop disagreeing; no gate, judgement, or park posture
    /// anywhere in the engine is keyed on this verdict (the
    /// agreement-record test pins that).
    ///
    /// TWO kinds, not one with a resolution field: the log is append-only,
    /// so a resolution that arrives later (or never) could only ever be a
    /// second event — mirroring `grant.requested` → `grant.approved` /
    /// `grant.denied`. Record-only in the reducer (the `gate.result`
    /// additive template, with reference validation as a corruption guard):
    /// the accompanying `milestone.blocked` drives the park, so old logs
    /// without any divergence.noted fold unchanged.
    ///
    /// WHY the tree hash travels on the event: it pins the exact bytes the
    /// verdict was computed from, so replay (provenance, the training
    /// corpus) never needs git — the branches stay for the judging human,
    /// the hash is the audit anchor. Only streams that produced a run
    /// record are compared (a stream that never started has no candidate
    /// diff; counting its untouched branch would fabricate agreement out of
    /// a failure), and with fewer than two recorded candidates NO event is
    /// appended at all — a one-stream "agreement" would be vacuous.
    #[serde(rename = "divergence.noted")]
    DivergenceNoted {
        /// The dispatch unit — the feature id fanned out to the pool
        /// ([`crate::types::CandidateLink::unit`] of every compared run).
        unit: String,
        /// Every compared candidate stream, in candidate-index order.
        candidates: Vec<DivergenceCandidate>,
        /// TRUE when at least two candidate branch trees differ (the
        /// streams diverged); FALSE = the agreement record (identical
        /// trees) — logged, never trusted (see the variant docs).
        diverged: bool,
    },

    /// The resolution of a unit's divergence record (ticket
    /// `divergence-first-class-event`, KRZ-304): WHICH candidate was chosen
    /// (or that none was), WHY, and decided by WHOM — today always the
    /// operator through the milestone unblock path the pool parks on; the
    /// string leaves room for a gate decider without a schema change.
    /// RECORD ONLY: the engine never merges a candidate (the KRZ-303
    /// freeze), so this changes nothing about the mission's course — it is
    /// the judgement landing in the log, feeding the escalation ledger and
    /// the provenance chain. At most one per unit: the first operator
    /// judgement stands (the reducer folds the unit set the engine dedupes
    /// against across restarts).
    #[serde(rename = "divergence.resolved")]
    DivergenceResolved {
        /// The dispatch unit whose divergence is resolved (the feature id).
        unit: String,
        /// The chosen candidate's zero-based stream index (the `-c<i>`
        /// branch suffix / [`crate::types::CandidateLink::index`]); `None`
        /// when no candidate was selected — a judged-and-abandoned unit is
        /// itself a recorded resolution, distinct from "not yet judged".
        /// `None` never hits the wire.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selected: Option<u32>,
        /// WHY, verbatim from the decider (the operator's note, or the
        /// unblock action when no note was given).
        reason: String,
        /// WHO or WHAT decided: `"operator"` for the unblock path; a gate
        /// identity when a gate ever resolves (none does today).
        #[serde(rename = "decidedBy")]
        decided_by: String,
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

    /// Worker-initiated escalation to the frontier advisor (ticket
    /// `backend-routing-abstraction`, KRZ-331): a worker whose report carried
    /// an `escalation` reason judged its task beyond its route's confidence
    /// and asked for frontier-tier advice. Distinct from `tier.escalated` —
    /// that is the ORCHESTRATOR's fix-cycle-cap valve, which flips the
    /// executor tier and resets the milestone; THIS is the WORKER's request,
    /// layered on top of the deterministic routing floor and never replacing
    /// it.
    ///
    /// RECORD-ONLY (the `gate.result` additive template): the fold validates
    /// the run reference as a corruption guard and changes NO state — the
    /// validator route, the executor tier, the respawn budget, and every
    /// milestone status are all untouched, so a worker escalation can never
    /// bypass the floor's validator requirements. The judgement turn that
    /// already reads the worker's report IS the frontier advisor act
    /// consuming the request (the orchestrator role's model/endpoint,
    /// frontier-floor enforced by `config::validate`); this event is the
    /// provenance that the request was made, feeding the escalation record
    /// the ticket requires of every escalation. Old logs without any
    /// worker.escalated fold unchanged.
    ///
    /// Routes are capability classes ([`ExecutorTier`]), never model ids —
    /// the same discipline as the routing table itself.
    #[serde(rename = "worker.escalated")]
    WorkerEscalated {
        #[serde(rename = "runId")]
        run_id: String,
        /// The feature whose worker asked (denormalized onto the event so
        /// the log reads without a join; the run record is the join of
        /// record).
        #[serde(rename = "featureId")]
        feature_id: String,
        /// Source route: the executor capability class the escalating worker
        /// session ran on.
        from: ExecutorTier,
        /// Target route: the advisor capability class requested — always
        /// `frontier` in this pass (see the variant docs).
        to: ExecutorTier,
        /// WHY the worker asked, verbatim from its report (already
        /// credential-scrubbed with the report text it was parsed from).
        reason: String,
    },

    /// A worker asked the human a structured question (ticket
    /// `structured-human-question-events`): the report's `questions` payload
    /// (the "ask the human" tool shape — text plus capped structured choices)
    /// opened as ONE entry of the pending-decision projection
    /// ([`crate::types::MissionState::pending_questions`]) that the dashboard
    /// and Slack render beside grants — the D-X channel-unification ruling:
    /// permission prompts stay on the grant flow, ticket underspecification
    /// stays on NeedsContext, and ONLY orchestrator/worker structured asks
    /// land here, so this is not a third competing human-input inbox.
    ///
    /// Unlike `grant.requested`, opening a question parks NOTHING: the
    /// worker's own run result drives the mission's course exactly as before
    /// (a prose-only report opens no question at all — the prose fallback),
    /// and an answer reaches the running mission through the existing
    /// user-message consult fold (see `question.answered`). The id is
    /// engine-minted (`q-<n>` from the folded
    /// [`crate::types::MissionState::question_count`] — restart-safe, never
    /// reused), never model-supplied. Text and options are credential-
    /// scrubbed and size-capped at write (orchestrator.rs caps); `role`
    /// names who asked (`worker` today — an orchestrator ask path can land
    /// without a schema change). The run/feature/milestone refs are
    /// denormalized context so surfaces render without a join.
    #[serde(rename = "question.opened")]
    QuestionOpened {
        /// Engine-minted id (`q-<n>`, per-mission monotonic).
        #[serde(rename = "questionId")]
        question_id: String,
        /// Who asked — `worker` in this pass.
        role: Role,
        /// The question text (scrubbed, capped at write).
        text: String,
        /// Structured choices the asker offered (each scrubbed + capped, the
        /// list capped at write). EMPTY means a free-text answer is expected.
        /// Absent in pre-field logs and omitted from the wire when empty.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        options: Vec<String>,
        /// The run whose report carried the ask. `None` never hits the wire.
        #[serde(rename = "runId", default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        /// Feature the asking run worked on (context ref). `None` never hits
        /// the wire.
        #[serde(rename = "featureId", default, skip_serializing_if = "Option::is_none")]
        feature_id: Option<String>,
        /// Milestone the asking run worked under (context ref; the clear-on-
        /// complete sweep keys on it). `None` never hits the wire.
        #[serde(
            rename = "milestoneId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        milestone_id: Option<String>,
    },

    /// The operator answered an open question (ticket
    /// `structured-human-question-events`), mirroring
    /// `grant.requested` → `grant.approved`: the reducer cross-checks the id
    /// against the parked projection (a stale or forged answer for a question
    /// that is not open fails the fold), removes it from
    /// [`crate::types::MissionState::pending_questions`], and folds the answer
    /// onto `pending_user_messages` — the EXISTING consult path, so the
    /// answer reaches the running mission (and replays after restart) with no
    /// new delivery mechanism. `answer` is the chosen option's text verbatim
    /// or the operator's free text (scrubbed + capped at write — an operator
    /// can paste a token into an answer box, and the log is corpus-exported);
    /// `option` records the 0-based index when an offered option was picked,
    /// `None` for free text. `via` names the control path that delivered it
    /// (the `answer-question` control kind today; a free-form string so a
    /// future `msg`-carried answer needs no schema change).
    #[serde(rename = "question.answered")]
    QuestionAnswered {
        #[serde(rename = "questionId")]
        question_id: String,
        answer: String,
        via: String,
        /// 0-based index into the question's `options` when an offered option
        /// was picked; absent for free-text answers. `None` never hits the
        /// wire.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option: Option<u32>,
    },

    /// An open question stopped being actionable WITHOUT an answer (ticket
    /// `structured-human-question-events`) — its milestone completed, or the
    /// mission ended with the ask still open. Third kind rather than a
    /// resolution field on `question.opened` for the same reason grants are
    /// two kinds: the log is append-only, so a later resolution can only ever
    /// be a second event. `why` is the engine's reason verbatim
    /// ("milestone completed", "mission completed", ...).
    #[serde(rename = "question.cleared")]
    QuestionCleared {
        #[serde(rename = "questionId")]
        question_id: String,
        why: String,
    },

    #[serde(rename = "milestone.blocked")]
    MilestoneBlocked {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        reason: String,
        /// Absent only in legacy logs; present unknown values fail closed.
        #[serde(
            rename = "blockContext",
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_block_context"
        )]
        block_context: Option<BlockContext>,
    },

    #[serde(rename = "milestone.unblocked")]
    MilestoneUnblocked {
        #[serde(rename = "milestoneId")]
        milestone_id: String,
        /// e.g. "raised fix-cycle cap", "user skipped findings"
        reason: String,
        /// Absent only in legacy logs; present unknown values fail closed.
        #[serde(
            rename = "blockContext",
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_block_context"
        )]
        block_context: Option<BlockContext>,
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

    /// The Flight Rules resolution record (KRZ-342, design D-D/D-E/D-H):
    /// emitted at plan approval, immediately after `plan.approved`, when a
    /// standards-configured pack governed the approval. Records the source
    /// identity + digest, the selection inputs (stage, task class, touch
    /// set), the selected rule revisions, and the `plan.approved` seq the
    /// pin attaches to — the queryable provenance for the consent artifact
    /// the plan's `standardsManifest` carries in full.
    ///
    /// D-H's record list, verified for KRZ-343: the source identity/digest,
    /// selection inputs, stage, rule revisions, and approval sequence all
    /// ride in this payload; the effective-time evaluation instant is the
    /// event envelope's own `ts` — resolution runs in the same approve_plan
    /// call as the emission, so the append stamp IS the instant the
    /// effective statuses were judged (payloads never duplicate the envelope
    /// clock anywhere in this schema). The RFC `effective_at` absorption
    /// window itself stays unevaluated in this slice: KRZ-341 parses and
    /// carries the field, and the stage-projection slice that evaluates it
    /// (KRZ-345) records its own surfaces.
    #[serde(rename = "standards.resolved")]
    StandardsResolved {
        /// `repo-tracked` or `external-pinned` ([`StandardsPinSource`]).
        source: String,
        #[serde(rename = "packName")]
        pack_name: String,
        #[serde(rename = "standardsRoot")]
        standards_root: String,
        /// sha256 over the pack's normalized canonical manifest text.
        digest: String,
        /// The resolution surface: `approval` for the pinning resolution
        /// (stage-specific projections are KRZ-345's emitters).
        stage: String,
        #[serde(rename = "taskClass", default, skip_serializing_if = "Option::is_none")]
        task_class: Option<String>,
        #[serde(rename = "touchSet", default, skip_serializing_if = "Vec::is_empty")]
        touch_set: Vec<String>,
        #[serde(
            rename = "contextPaths",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        context_paths: Vec<String>,
        /// The selected rules, stable-sorted by id.
        rules: Vec<StandardsRuleRef>,
        /// The seq of the `plan.approved` event this resolution pins.
        #[serde(rename = "approvalSeq")]
        approval_seq: u64,
    },

    /// The Flight Rules policy-drift refusal (KRZ-342, design D-E/D-H):
    /// emitted when merge re-resolves the LIVE base policy against the exact
    /// scratch integration diff and the applicable ENFORCED set differs from
    /// the approved pin's — the merge is refused and the mission requires
    /// explicit revalidation/reapproval. `currentDigest` is `None` when the
    /// live base no longer yields a readable standards manifest at all (a
    /// removed or malformed pack — the ultimate drift, failed closed).
    /// Audit-only in the reducer: the refusal already happened; the event is
    /// the evidence.
    #[serde(rename = "standards.drifted")]
    StandardsDrifted {
        /// The digest pinned at approval.
        #[serde(rename = "approvedDigest")]
        approved_digest: String,
        /// The digest resolved from the live base, when one resolved.
        #[serde(
            rename = "currentDigest",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        current_digest: Option<String>,
        /// The surface that detected the drift (`merge` in this slice).
        surface: String,
        /// Id-level descriptions of the changed applicable enforced rules
        /// (added / removed / changed), stable-sorted.
        #[serde(rename = "changedRules")]
        changed_rules: Vec<String>,
    },

    /// The Flight Rules human waiver decision (ticket
    /// `.kranz/tickets/flight-rules-waiver-decisions.md`, KRZ-344; design
    /// D-I — "waivers are narrow human decisions"): the ONE authorized
    /// exception path for a standards failure. Only an authenticated human
    /// surface records it (`kranz standards waive` in this slice) — a model
    /// may request a waiver or propose a fix but can NEVER approve one, so
    /// no engine or backend code path emits this event. The binding is
    /// deliberately narrow enough that the waiver cannot survive a
    /// meaningful rule/finding/scope/diff change: it names the pinned rule
    /// id + revision + manifest digest + approval sequence, the fingerprint
    /// of the EXACT finding it subtracts, the affected paths, and the
    /// sha256 over the affected-path diff (the whole diff for an unscoped
    /// rule), plus the reason, the approver, and the expiry. A change to
    /// the affected-path diff, the rule revision, the finding fingerprint,
    /// or the pin — or the expiry passing — invalidates the waiver and
    /// restores the block; unrelated paths receive no authority. It
    /// subtracts EXACTLY ONE matching standards failure: it never disables
    /// a checker, an RFC, a domain, or a class, and engine floor gates have
    /// no waiver slot at all. Audit-only in the reducer: the coverage fold
    /// joins it straight from the log.
    #[serde(rename = "standards.waiver.approved")]
    StandardsWaiverApproved {
        /// The pinned rule id the waiver excepts (frontmatter `id:`).
        #[serde(rename = "ruleId")]
        rule_id: String,
        /// The pinned rule revision — a waiver naming any other revision
        /// joins nothing.
        #[serde(rename = "ruleRevision")]
        rule_revision: u64,
        /// sha256 of the approved manifest the waiver binds to
        /// ([`StandardsPin::digest`]).
        #[serde(rename = "manifestDigest")]
        manifest_digest: String,
        /// The seq of the `plan.approved` event whose pin the waiver binds
        /// — a re-approval supersedes every earlier waiver.
        #[serde(rename = "approvalSeq")]
        approval_seq: u64,
        /// sha256 fingerprint of the ONE finding this waiver subtracts
        /// ([`crate::standards_waiver::finding_fingerprint`]).
        #[serde(rename = "findingFingerprint")]
        finding_fingerprint: String,
        /// The affected paths the bound diff covers: the rule's
        /// `when-paths` intersected with the mission diff, or the whole
        /// changed set for an unscoped rule. Recorded so the audit names
        /// exactly what the digest covers; empty when a scoped rule
        /// matched no changed path (the waiver then binds the empty
        /// scoped diff).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        paths: Vec<String>,
        /// sha256 over the affected-path diff bytes at approval time — a
        /// later change to any affected path digests differently and
        /// invalidates the waiver.
        #[serde(rename = "diffDigest")]
        diff_digest: String,
        /// The human's reason, verbatim (scrubbed at write like every
        /// payload string).
        reason: String,
        /// The approver principal: the authenticated identity where the
        /// local authority model can name one, else honestly
        /// `local-operator` (D-I — never invent a real-world identity).
        approver: String,
        /// The authenticated invocation surface (`cli` in this slice).
        /// The coverage fold honors only recognized human surfaces — a
        /// hand-cut event claiming a model surface carries no authority.
        surface: String,
        /// The expiry instant. The fold judges it against the log's own
        /// frontier (the latest event instant — never a wall clock, so
        /// replays stay byte-identical); an enforcement decision re-judges
        /// it against its own clock.
        #[serde(rename = "expiresAt")]
        expires_at: DateTime<Utc>,
    },

    /// Positive human verdict for a rule whose typed checker is
    /// `manual-attestation` (KRZ-346 D-F). Like a waiver, authority is narrow:
    /// exact mission pin, rule revision, affected paths, and current diff.
    /// Unlike a waiver it does not except a failing checker; it IS the
    /// checker and therefore carries no finding fingerprint or expiry.
    #[serde(rename = "standards.attestation.approved")]
    StandardsAttestationApproved {
        #[serde(rename = "ruleId")]
        rule_id: String,
        #[serde(rename = "ruleRevision")]
        rule_revision: u64,
        #[serde(rename = "manifestDigest")]
        manifest_digest: String,
        #[serde(rename = "approvalSeq")]
        approval_seq: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        paths: Vec<String>,
        #[serde(rename = "diffDigest")]
        diff_digest: String,
        reason: String,
        approver: String,
        surface: String,
    },
}

impl EventKind {
    /// The dotted wire name of this event (matches the serde rename).
    pub fn type_name(&self) -> &'static str {
        match self {
            EventKind::GateEvaluationRequested { .. } => "gate.evaluation-requested",
            EventKind::GateEvaluationFinished { .. } => "gate.evaluation-finished",
            EventKind::GateResolutionRecorded { .. } => "gate.resolution-recorded",
            EventKind::GateResolutionConsumed { .. } => "gate.resolution-consumed",
            EventKind::PermissionRequested { .. } => "permission.requested",
            EventKind::PermissionResolved { .. } => "permission.resolved",
            EventKind::PermissionResponseRecorded { .. } => "permission.response-recorded",
            EventKind::PermissionClosed { .. } => "permission.closed",
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
            EventKind::FeatureProgress { .. } => "feature.progress",
            EventKind::WorkerSpawned { .. } => "worker.spawned",
            EventKind::WorkerMessage { .. } => "worker.message",
            EventKind::WorkerEgressDenied { .. } => "worker.egress.denied",
            EventKind::WorkerCompleted { .. } => "worker.completed",
            EventKind::FeatureCompleted { .. } => "feature.completed",
            EventKind::FeatureFailed { .. } => "feature.failed",
            EventKind::FeatureSkipped { .. } => "feature.skipped",
            EventKind::MilestoneValidating { .. } => "milestone.validating",
            EventKind::ValidationFinding { .. } => "validation.finding",
            EventKind::ValidatorTamper { .. } => "validator.tamper",
            EventKind::ValidationSnapshot { .. } => "validation.snapshot",
            EventKind::ValidationConfirm { .. } => "validation.confirm",
            EventKind::ValidationPtyTranscript { .. } => "validation.pty.transcript",
            EventKind::GateResult { .. } => "gate.result",
            EventKind::HookGateFired { .. } => "hook.gate.fired",
            EventKind::DivergenceNoted { .. } => "divergence.noted",
            EventKind::DivergenceResolved { .. } => "divergence.resolved",
            EventKind::FixFeatureCreated { .. } => "fixfeature.created",
            EventKind::TierEscalated { .. } => "tier.escalated",
            EventKind::WorkerEscalated { .. } => "worker.escalated",
            EventKind::QuestionOpened { .. } => "question.opened",
            EventKind::QuestionAnswered { .. } => "question.answered",
            EventKind::QuestionCleared { .. } => "question.cleared",
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
            EventKind::StandardsResolved { .. } => "standards.resolved",
            EventKind::StandardsDrifted { .. } => "standards.drifted",
            EventKind::StandardsWaiverApproved { .. } => "standards.waiver.approved",
            EventKind::StandardsAttestationApproved { .. } => "standards.attestation.approved",
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
            standards_manifest: None,
            reviewer_independence: None,
        }
    }

    /// Runtime egress evidence is an additive event, not a new required field
    /// on worker.completed: legacy logs remain byte-compatible simply by not
    /// containing this record, while new records preserve exact run
    /// attribution and destination data.
    #[test]
    fn worker_egress_denied_round_trips() {
        let event = EventKind::WorkerEgressDenied {
            run_id: "run-1".to_string(),
            denials: vec![crate::egress_proxy::EgressDenial {
                host: "example.com".to_string(),
                port: 443,
            }],
            omitted_count: 3,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "worker.egress.denied");
        assert_eq!(json["payload"]["runId"], "run-1");
        assert_eq!(json["payload"]["denials"][0]["host"], "example.com");
        assert_eq!(json["payload"]["denials"][0]["port"], 443);
        assert_eq!(json["payload"]["omittedCount"], 3);
        assert_eq!(event.type_name(), "worker.egress.denied");

        let mut legacy = json.clone();
        legacy["payload"]
            .as_object_mut()
            .unwrap()
            .remove("omittedCount");
        match serde_json::from_value::<EventKind>(legacy).unwrap() {
            EventKind::WorkerEgressDenied { omitted_count, .. } => assert_eq!(omitted_count, 0),
            _ => panic!("wrong variant"),
        }

        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::WorkerEgressDenied {
                run_id,
                denials,
                omitted_count,
            } => {
                assert_eq!(run_id, "run-1");
                assert_eq!(denials.len(), 1);
                assert_eq!(denials[0].host, "example.com");
                assert_eq!(denials[0].port, 443);
                assert_eq!(omitted_count, 3);
            }
            _ => panic!("wrong variant"),
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
            block_context: None,
            milestone_id: "ms-1".into(),
            reason: "r".into(),
            validator_guidance: Some("FMT FIRST".into()),
        };
        let json = serde_json::to_value(&with).unwrap();
        assert_eq!(json["payload"]["validatorGuidance"], "FMT FIRST");
        let without = EventKind::MilestoneUnblocked {
            block_context: None,
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

    /// The additive `validation.confirm` event (ticket
    /// `local-inference-validator-guarded`, KRZ-206b): wire name, payload
    /// shape, and round-trip — the miss-rate ground truth must survive serde
    /// verbatim, because the local-vs-frontier miss rate is computed from
    /// these bytes alone (misses = disagreement subjects; opportunities =
    /// confirmed + disagreement command assertions + judgmentOpportunity).
    #[test]
    fn guarded_local_validator_confirm_event_wire_shape_and_round_trip() {
        let event = EventKind::ValidationConfirm {
            milestone_id: "ms-1".to_string(),
            local_run_id: "run-local".to_string(),
            confirm_run_id: "run-frontier".to_string(),
            confirmed: vec!["a1".to_string()],
            disagreements: vec![Finding {
                subject: "a2".to_string(),
                severity: "major".to_string(),
                evidence: "frontier sees a failure the local pass missed".to_string(),
                suggested_fix: "fix a2".to_string(),
                class: String::new(),
                rule: None,
            }],
            judgment_opportunity: false,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "validation.confirm");
        assert_eq!(json["payload"]["milestoneId"], "ms-1");
        assert_eq!(json["payload"]["localRunId"], "run-local");
        assert_eq!(json["payload"]["confirmRunId"], "run-frontier");
        assert_eq!(json["payload"]["confirmed"], serde_json::json!(["a1"]));
        assert_eq!(
            json["payload"]["disagreements"][0]["subject"],
            serde_json::json!("a2")
        );
        assert_eq!(
            json["payload"]["judgmentOpportunity"],
            serde_json::json!(false)
        );
        assert_eq!(event.type_name(), "validation.confirm");
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::ValidationConfirm {
                milestone_id,
                local_run_id,
                confirm_run_id,
                confirmed,
                disagreements,
                judgment_opportunity,
            } => {
                assert_eq!(milestone_id, "ms-1");
                assert_eq!(local_run_id, "run-local");
                assert_eq!(confirm_run_id, "run-frontier");
                assert_eq!(confirmed, vec!["a1".to_string()]);
                assert_eq!(disagreements.len(), 1);
                assert_eq!(disagreements[0].subject, "a2");
                assert!(!judgment_opportunity);
            }
            _ => panic!("wrong variant"),
        }

        // A legacy line (the field predated) decodes with the additive
        // default — pre-field logs simply never recorded a judgment-only
        // confirmation.
        let mut legacy = serde_json::to_value(&event).unwrap();
        legacy["payload"]
            .as_object_mut()
            .unwrap()
            .remove("judgmentOpportunity");
        let back: EventKind = serde_json::from_value(legacy).unwrap();
        match back {
            EventKind::ValidationConfirm {
                judgment_opportunity,
                ..
            } => assert!(!judgment_opportunity, "absent reads as false"),
            _ => panic!("wrong variant"),
        }
    }

    /// The additive `validation.pty.transcript` event (ticket
    /// `pty-functional-validation`): wire name, payload shape, and
    /// round-trip — the audit record binding a pty-script assertion's
    /// verdict to its transcript artifact must survive serde verbatim, and
    /// a legacy line without `detail` still decodes (serde default).
    #[test]
    fn validation_pty_transcript_round_trips() {
        let event = EventKind::ValidationPtyTranscript {
            milestone_id: "ms-1".to_string(),
            assertion_id: "a-pty".to_string(),
            verdict: crate::gate::GateVerdict::Fail,
            artefact_ref: "file:runs/pty-transcripts/a-pty-0123abcd.log".to_string(),
            detail: Some("step 1 ok step 2 FAILED (expect `echo:hello` timed out)".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "validation.pty.transcript");
        assert_eq!(json["payload"]["milestoneId"], "ms-1");
        assert_eq!(json["payload"]["assertionId"], "a-pty");
        assert_eq!(
            json["payload"]["artefactRef"],
            "file:runs/pty-transcripts/a-pty-0123abcd.log"
        );
        assert_eq!(event.type_name(), "validation.pty.transcript");
        let back: EventKind = serde_json::from_value(json.clone()).unwrap();
        match back {
            EventKind::ValidationPtyTranscript {
                milestone_id,
                assertion_id,
                verdict,
                artefact_ref,
                detail,
            } => {
                assert_eq!(milestone_id, "ms-1");
                assert_eq!(assertion_id, "a-pty");
                assert_eq!(verdict, crate::gate::GateVerdict::Fail);
                assert_eq!(artefact_ref, "file:runs/pty-transcripts/a-pty-0123abcd.log");
                assert!(detail.unwrap().contains("FAILED"));
            }
            _ => panic!("wrong variant"),
        }
        // A legacy line without `detail` still decodes (serde default).
        let mut legacy = json;
        legacy["payload"].as_object_mut().unwrap().remove("detail");
        let back: EventKind = serde_json::from_value(legacy).unwrap();
        match back {
            EventKind::ValidationPtyTranscript { detail, .. } => assert_eq!(detail, None),
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
            rule_ids: Vec::new(),
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
                rule_ids,
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
                assert!(rule_ids.is_empty());
            }
            _ => panic!("wrong variant"),
        }
    }

    /// Boolean-only gates carry no score, and a reference without captured
    /// content carries no detail: all three are additive-optional — `None`
    /// stays OFF the wire (byte-identical to a payload that never had them)
    /// and a line without them parses back to `None` (serde default), so
    /// hand-written or future-trimmed logs fold like engine-written ones.
    /// KRZ-343's `ruleIds` follows the same rule: a gate with no standards
    /// linkage carries an empty list, which serializes as NO key.
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
            rule_ids: Vec::new(),
        };
        let json = serde_json::to_value(&sparse).unwrap();
        assert_eq!(json["payload"]["surface"], "final-gate");
        assert_eq!(json["payload"]["kind"], "model-judged");
        assert_eq!(json["payload"]["verdict"], "pass");
        let payload = json["payload"].as_object().unwrap();
        for absent in ["artefactDetail", "score", "threshold", "ruleIds"] {
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

    /// The additive `worker.escalated` event (ticket
    /// `backend-routing-abstraction`, KRZ-331): wire name, exact payload
    /// shape, and round-trip — the gate.result template. The payload names
    /// the source and target routes as capability classes (ExecutorTier's
    /// lowercase wire form), never model ids.
    #[test]
    fn routing_abstraction_worker_escalated_wire_shape_and_round_trip() {
        let kind = EventKind::WorkerEscalated {
            run_id: "r-1".to_string(),
            feature_id: "f-1-1".to_string(),
            from: ExecutorTier::Local,
            to: ExecutorTier::Frontier,
            reason: "spec ambiguity beyond my confidence".to_string(),
        };
        let json = serde_json::to_value(&kind).unwrap();
        assert_eq!(json["type"], "worker.escalated");
        assert_eq!(json["payload"]["runId"], "r-1");
        assert_eq!(json["payload"]["featureId"], "f-1-1");
        assert_eq!(json["payload"]["from"], "local");
        assert_eq!(json["payload"]["to"], "frontier");
        assert_eq!(
            json["payload"]["reason"],
            "spec ambiguity beyond my confidence"
        );
        assert_eq!(kind.type_name(), "worker.escalated");
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::WorkerEscalated {
                run_id,
                feature_id,
                from,
                to,
                reason,
            } => {
                assert_eq!(run_id, "r-1");
                assert_eq!(feature_id, "f-1-1");
                assert_eq!(from, ExecutorTier::Local);
                assert_eq!(to, ExecutorTier::Frontier);
                assert_eq!(reason, "spec ambiguity beyond my confidence");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// The additive `candidate` on `worker.spawned` (ticket
    /// heterogeneous-dispatch-pool, KRZ-303): the sibling linkage round-trips
    /// when present, old log lines without it fold to None, and None never
    /// hits the wire.
    #[test]
    fn dispatch_pool_worker_spawned_candidate_is_additive() {
        fn spawned(candidate: Option<CandidateLink>) -> EventKind {
            EventKind::WorkerSpawned {
                backend: None,
                run_id: "r-1".into(),
                role: Role::Worker,
                feature_id: Some("f-1-1".into()),
                milestone_id: None,
                candidate,
                executor_route: None,
                sdk_session_id: "s".into(),
                model: "sonnet".into(),
                quant: "n/a".into(),
                weight_hash: None,
                prompt_hash: "h".into(),
                transcript_path: "t".into(),
            }
        }
        let link = CandidateLink {
            unit: "f-1-1".into(),
            index: 1,
            count: 2,
            backend: "codex".into(),
        };

        // Some: camelCase wire shape, full round-trip.
        let json = serde_json::to_value(spawned(Some(link.clone()))).unwrap();
        assert_eq!(json["payload"]["candidate"]["unit"], "f-1-1");
        assert_eq!(json["payload"]["candidate"]["index"], 1);
        assert_eq!(json["payload"]["candidate"]["count"], 2);
        assert_eq!(json["payload"]["candidate"]["backend"], "codex");
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::WorkerSpawned { candidate, .. } => {
                assert_eq!(candidate, Some(link))
            }
            _ => panic!("wrong variant"),
        }

        // None: omitted from the wire (byte-identical to pre-pool logs).
        let json = serde_json::to_value(spawned(None)).unwrap();
        assert!(
            !json["payload"]
                .as_object()
                .unwrap()
                .contains_key("candidate"),
            "candidate must not serialize when None: {json}"
        );

        // Old log line (pre-candidate): folds with candidate = None.
        let old: EventKind = serde_json::from_str(
            r#"{"type":"worker.spawned","payload":{"runId":"r-1","role":"worker","featureId":"f-1-1","sdkSessionId":"s","model":"sonnet","promptHash":"h","transcriptPath":"t"}}"#,
        )
        .unwrap();
        match old {
            EventKind::WorkerSpawned { candidate, .. } => assert_eq!(candidate, None),
            _ => panic!("wrong variant"),
        }
    }

    /// The additive `executorRoute` field (ticket `routing-rules-config`):
    /// the effective route + deciding rule round-trips in camelCase when
    /// present, old log lines without it fold to None, and None never hits
    /// the wire (byte-identical to pre-provenance logs).
    #[test]
    fn routing_rules_config_worker_spawned_executor_route_is_additive() {
        fn spawned(executor_route: Option<crate::types::ExecutorRoute>) -> EventKind {
            EventKind::WorkerSpawned {
                backend: None,
                run_id: "r-1".into(),
                role: Role::Worker,
                feature_id: Some("f-1-1".into()),
                milestone_id: None,
                candidate: None,
                executor_route,
                sdk_session_id: "s".into(),
                model: "sonnet".into(),
                quant: "n/a".into(),
                weight_hash: None,
                prompt_hash: "h".into(),
                transcript_path: "t".into(),
            }
        }

        // Some: camelCase wire shape, full round-trip — rule omitted when
        // the fall-through decided (None never serializes).
        let route = crate::types::ExecutorRoute {
            tier: crate::types::ExecutorTier::Local,
            rule: Some("taskClassRules[0]".to_string()),
        };
        let json = serde_json::to_value(spawned(Some(route.clone()))).unwrap();
        assert_eq!(json["payload"]["executorRoute"]["tier"], "local");
        assert_eq!(
            json["payload"]["executorRoute"]["rule"],
            "taskClassRules[0]"
        );
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::WorkerSpawned { executor_route, .. } => {
                assert_eq!(executor_route, Some(route))
            }
            _ => panic!("wrong variant"),
        }
        let fall_through = crate::types::ExecutorRoute {
            tier: crate::types::ExecutorTier::Frontier,
            rule: None,
        };
        let json = serde_json::to_value(spawned(Some(fall_through))).unwrap();
        assert_eq!(json["payload"]["executorRoute"]["tier"], "frontier");
        assert!(
            !json["payload"]["executorRoute"]
                .as_object()
                .unwrap()
                .contains_key("rule"),
            "a fall-through route must not serialize a rule key: {json}"
        );

        // None: omitted from the wire (byte-identical to pre-provenance logs).
        let json = serde_json::to_value(spawned(None)).unwrap();
        assert!(
            !json["payload"]
                .as_object()
                .unwrap()
                .contains_key("executorRoute"),
            "executorRoute must not serialize when None: {json}"
        );

        // Old log line (pre-provenance): folds with executor_route = None.
        let old: EventKind = serde_json::from_str(
            r#"{"type":"worker.spawned","payload":{"runId":"r-1","role":"worker","featureId":"f-1-1","sdkSessionId":"s","model":"sonnet","promptHash":"h","transcriptPath":"t"}}"#,
        )
        .unwrap();
        match old {
            EventKind::WorkerSpawned { executor_route, .. } => {
                assert_eq!(executor_route, None)
            }
            _ => panic!("wrong variant"),
        }
    }

    /// The additive `divergence.noted` event (ticket
    /// `divergence-first-class-event`, KRZ-304): wire name, payload shape,
    /// and round-trip — the comparison record naming every candidate ref
    /// (run id + branch + backend + tree hash) and the verdict. The
    /// `diverged: false` form IS the agreement record: logged, never
    /// trusted.
    #[test]
    fn divergence_event_noted_wire_shape_and_round_trip() {
        let noted = EventKind::DivergenceNoted {
            unit: "f-1-1".into(),
            candidates: vec![
                DivergenceCandidate {
                    run_id: "r-1".into(),
                    branch: "kranz/pool/m-1/f-1-1-c0".into(),
                    backend: "claude".into(),
                    tree: "aaa".into(),
                },
                DivergenceCandidate {
                    run_id: "r-2".into(),
                    branch: "kranz/pool/m-1/f-1-1-c1".into(),
                    backend: "codex".into(),
                    tree: "bbb".into(),
                },
            ],
            diverged: true,
        };
        let json = serde_json::to_value(&noted).unwrap();
        assert_eq!(json["type"], "divergence.noted");
        assert_eq!(json["payload"]["unit"], "f-1-1");
        assert_eq!(json["payload"]["diverged"], true);
        assert_eq!(json["payload"]["candidates"][0]["runId"], "r-1");
        assert_eq!(
            json["payload"]["candidates"][1]["branch"],
            "kranz/pool/m-1/f-1-1-c1"
        );
        assert_eq!(json["payload"]["candidates"][1]["backend"], "codex");
        assert_eq!(json["payload"]["candidates"][1]["tree"], "bbb");
        assert_eq!(noted.type_name(), "divergence.noted");
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::DivergenceNoted {
                unit,
                candidates,
                diverged,
            } => {
                assert_eq!(unit, "f-1-1");
                assert!(diverged);
                assert_eq!(candidates.len(), 2);
                assert_eq!(candidates[0].run_id, "r-1");
                assert_eq!(candidates[1].tree, "bbb");
            }
            _ => panic!("wrong variant"),
        }

        // The agreement record is the SAME kind with diverged = false —
        // there is no separate, trustable "agreement" event shape.
        let agreed = EventKind::DivergenceNoted {
            unit: "f-1-1".into(),
            candidates: vec![
                DivergenceCandidate {
                    run_id: "r-1".into(),
                    branch: "kranz/pool/m-1/f-1-1-c0".into(),
                    backend: "claude".into(),
                    tree: "aaa".into(),
                },
                DivergenceCandidate {
                    run_id: "r-2".into(),
                    branch: "kranz/pool/m-1/f-1-1-c1".into(),
                    backend: "codex".into(),
                    tree: "aaa".into(),
                },
            ],
            diverged: false,
        };
        let json = serde_json::to_value(&agreed).unwrap();
        assert_eq!(json["type"], "divergence.noted");
        assert_eq!(json["payload"]["diverged"], false);
    }

    /// The additive `divergence.resolved` event (KRZ-304): the resolution
    /// naming WHICH candidate (or none), WHY, and decided by WHOM.
    /// `selected: None` means judged-and-abandoned, is omitted from the
    /// wire, and a wire line without it parses back to None (serde default)
    /// — so hand-written or future-trimmed logs fold like engine-written
    /// ones.
    #[test]
    fn divergence_event_resolved_wire_shape_and_round_trip() {
        let resolved = EventKind::DivergenceResolved {
            unit: "f-1-1".into(),
            selected: Some(1),
            reason: "the codex candidate keeps the parser total".into(),
            decided_by: "operator".into(),
        };
        let json = serde_json::to_value(&resolved).unwrap();
        assert_eq!(json["type"], "divergence.resolved");
        assert_eq!(json["payload"]["unit"], "f-1-1");
        assert_eq!(json["payload"]["selected"], 1);
        assert_eq!(
            json["payload"]["reason"],
            "the codex candidate keeps the parser total"
        );
        assert_eq!(json["payload"]["decidedBy"], "operator");
        assert_eq!(resolved.type_name(), "divergence.resolved");
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::DivergenceResolved {
                unit,
                selected,
                reason,
                decided_by,
            } => {
                assert_eq!(unit, "f-1-1");
                assert_eq!(selected, Some(1));
                assert!(reason.contains("codex"));
                assert_eq!(decided_by, "operator");
            }
            _ => panic!("wrong variant"),
        }

        // None = judged-and-abandoned: off the wire, and a wire line
        // without the key parses back to None.
        let none = EventKind::DivergenceResolved {
            unit: "f-1-1".into(),
            selected: None,
            reason: "neither candidate survives review".into(),
            decided_by: "operator".into(),
        };
        let json = serde_json::to_value(&none).unwrap();
        assert!(
            !json["payload"]
                .as_object()
                .unwrap()
                .contains_key("selected"),
            "selected must not serialize when None: {json}"
        );
        let line = r#"{
            "seq": 9,
            "ts": "2026-01-02T03:04:05Z",
            "missionId": "m-1",
            "type": "divergence.resolved",
            "payload": {
                "unit": "f-1-1",
                "reason": "milestone skipped by operator",
                "decidedBy": "operator"
            }
        }"#;
        let event: Event = serde_json::from_str(line).unwrap();
        match event.kind {
            EventKind::DivergenceResolved {
                selected,
                decided_by,
                ..
            } => {
                assert_eq!(selected, None);
                assert_eq!(decided_by, "operator");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// The additive `hook.gate.fired` event (ticket
    /// claude-code-hook-gate-projection, KRZ-302): wire name and payload
    /// round-trip, `detail` is omitted when None, and a sparse wire line
    /// folds with serde defaults — the gate.result additive template.
    #[test]
    fn hook_gate_projection_event_wire_shape_round_trips() {
        let fired = EventKind::HookGateFired {
            run_id: "r-1".into(),
            gate: "out-of-contract-write".into(),
            hook_event: "PreToolUse".into(),
            tool: "Write".into(),
            subject: "docs/oops.md".into(),
            verdict: "blocked".into(),
            detail: Some("matches none of the declared touch-set globs".into()),
        };
        let json = serde_json::to_value(&fired).unwrap();
        assert_eq!(json["type"], "hook.gate.fired");
        assert_eq!(json["payload"]["runId"], "r-1");
        assert_eq!(json["payload"]["gate"], "out-of-contract-write");
        assert_eq!(json["payload"]["hookEvent"], "PreToolUse");
        assert_eq!(json["payload"]["tool"], "Write");
        assert_eq!(json["payload"]["subject"], "docs/oops.md");
        assert_eq!(json["payload"]["verdict"], "blocked");
        assert_eq!(fired.type_name(), "hook.gate.fired");
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::HookGateFired {
                run_id,
                gate,
                verdict,
                detail,
                ..
            } => {
                assert_eq!(run_id, "r-1");
                assert_eq!(gate, "out-of-contract-write");
                assert_eq!(verdict, "blocked");
                assert_eq!(
                    detail.as_deref(),
                    Some("matches none of the declared touch-set globs")
                );
            }
            _ => panic!("wrong variant"),
        }

        // detail = None stays off the wire (additive; old readers never see
        // the key), and a wire line without it folds to None.
        let no_detail = EventKind::HookGateFired {
            run_id: "r-2".into(),
            gate: "out-of-contract-write".into(),
            hook_event: "PreToolUse".into(),
            tool: "Edit".into(),
            subject: "x".into(),
            verdict: "error".into(),
            detail: None,
        };
        let json = serde_json::to_value(&no_detail).unwrap();
        assert!(
            !json["payload"].as_object().unwrap().contains_key("detail"),
            "detail must not serialize when None: {json}"
        );
        let sparse: EventKind = serde_json::from_str(
            r#"{"type":"hook.gate.fired","payload":{"runId":"r-3","gate":"out-of-contract-write","hookEvent":"PreToolUse","tool":"Write","subject":"y","verdict":"blocked"}}"#,
        )
        .unwrap();
        match sparse {
            EventKind::HookGateFired { detail, .. } => assert_eq!(detail, None),
            _ => panic!("wrong variant"),
        }
    }

    /// The additive question events (ticket
    /// `structured-human-question-events`): wire names, exact payload shapes,
    /// and round-trips. Every optional field stays OFF the wire when absent,
    /// and sparse wire lines (hand-written or future-trimmed logs) fold with
    /// serde defaults — the gate.result additive template.
    #[test]
    fn question_events_wire_shapes_and_round_trips() {
        let opened = EventKind::QuestionOpened {
            question_id: "q-1".into(),
            role: Role::Worker,
            text: "Which storage engine should the cache use?".into(),
            options: vec!["sqlite".into(), "in-memory".into()],
            run_id: Some("r-1".into()),
            feature_id: Some("f-1-1".into()),
            milestone_id: Some("ms-1".into()),
        };
        let json = serde_json::to_value(&opened).unwrap();
        assert_eq!(json["type"], "question.opened");
        assert_eq!(json["payload"]["questionId"], "q-1");
        assert_eq!(json["payload"]["role"], "worker");
        assert_eq!(
            json["payload"]["text"],
            "Which storage engine should the cache use?"
        );
        assert_eq!(
            json["payload"]["options"],
            serde_json::json!(["sqlite", "in-memory"])
        );
        assert_eq!(json["payload"]["runId"], "r-1");
        assert_eq!(json["payload"]["featureId"], "f-1-1");
        assert_eq!(json["payload"]["milestoneId"], "ms-1");
        assert_eq!(opened.type_name(), "question.opened");
        let back: EventKind = serde_json::from_value(json).unwrap();
        match back {
            EventKind::QuestionOpened {
                question_id,
                role,
                options,
                milestone_id,
                ..
            } => {
                assert_eq!(question_id, "q-1");
                assert_eq!(role, Role::Worker);
                assert_eq!(options.len(), 2);
                assert_eq!(milestone_id.as_deref(), Some("ms-1"));
            }
            _ => panic!("wrong variant"),
        }

        // Empty options (a free-text ask) and absent context refs stay off
        // the wire, and a sparse line folds them to the defaults.
        let free_text = EventKind::QuestionOpened {
            question_id: "q-2".into(),
            role: Role::Worker,
            text: "What should the flag be called?".into(),
            options: vec![],
            run_id: None,
            feature_id: None,
            milestone_id: None,
        };
        let json = serde_json::to_value(&free_text).unwrap();
        let payload = json["payload"].as_object().unwrap();
        for absent in ["options", "runId", "featureId", "milestoneId"] {
            assert!(
                !payload.contains_key(absent),
                "payload must not contain {absent} when empty/None: {json}"
            );
        }
        let sparse: EventKind = serde_json::from_str(
            r#"{"type":"question.opened","payload":{"questionId":"q-2","role":"worker","text":"What should the flag be called?"}}"#,
        )
        .unwrap();
        match sparse {
            EventKind::QuestionOpened {
                options,
                run_id,
                feature_id,
                milestone_id,
                ..
            } => {
                assert!(options.is_empty());
                assert_eq!(run_id, None);
                assert_eq!(feature_id, None);
                assert_eq!(milestone_id, None);
            }
            _ => panic!("wrong variant"),
        }

        let answered = EventKind::QuestionAnswered {
            question_id: "q-1".into(),
            answer: "sqlite".into(),
            via: "answer-question".into(),
            option: Some(0),
        };
        let json = serde_json::to_value(&answered).unwrap();
        assert_eq!(json["type"], "question.answered");
        assert_eq!(json["payload"]["questionId"], "q-1");
        assert_eq!(json["payload"]["answer"], "sqlite");
        assert_eq!(json["payload"]["via"], "answer-question");
        assert_eq!(json["payload"]["option"], 0);
        assert_eq!(answered.type_name(), "question.answered");
        let back: EventKind = serde_json::from_value(json).unwrap();
        assert!(matches!(back, EventKind::QuestionAnswered { .. }));

        // option = None (free-text answer) stays off the wire; a sparse line
        // folds it to None.
        let free_answer = EventKind::QuestionAnswered {
            question_id: "q-2".into(),
            answer: "call it --cache-dir".into(),
            via: "answer-question".into(),
            option: None,
        };
        let json = serde_json::to_value(&free_answer).unwrap();
        assert!(
            !json["payload"].as_object().unwrap().contains_key("option"),
            "option must not serialize when None: {json}"
        );
        let sparse: EventKind = serde_json::from_str(
            r#"{"type":"question.answered","payload":{"questionId":"q-2","answer":"call it --cache-dir","via":"answer-question"}}"#,
        )
        .unwrap();
        match sparse {
            EventKind::QuestionAnswered { option, .. } => assert_eq!(option, None),
            _ => panic!("wrong variant"),
        }

        let cleared = EventKind::QuestionCleared {
            question_id: "q-1".into(),
            why: "milestone completed".into(),
        };
        let json = serde_json::to_value(&cleared).unwrap();
        assert_eq!(json["type"], "question.cleared");
        assert_eq!(json["payload"]["questionId"], "q-1");
        assert_eq!(json["payload"]["why"], "milestone completed");
        assert_eq!(cleared.type_name(), "question.cleared");
        let back: EventKind = serde_json::from_value(json).unwrap();
        assert!(matches!(back, EventKind::QuestionCleared { .. }));
    }
}
