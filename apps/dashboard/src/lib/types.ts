// Hand-written TypeScript mirrors of the engine's serde JSON shapes.
// Sources of truth: crates/engine/src/types.rs and events.rs (camelCase).
// CONTRACT MIRROR — keep in sync with the Rust types; do not invent fields.

// ---------------------------------------------------------------------------
// Enums (serde rename_all values)
// ---------------------------------------------------------------------------

export type MissionStatus =
  | 'planning'
  | 'approved'
  | 'running'
  | 'paused'
  | 'blocked'
  | 'validating'
  | 'complete'
  | 'failed'
  | 'abandoned';

export type MilestoneStatus =
  | 'pending'
  | 'active'
  | 'validating'
  | 'complete'
  | 'blocked';

export type FeatureOrigin = 'plan' | 'fix';

export type FeatureStatus =
  | 'pending'
  | 'active'
  | 'complete'
  | 'skipped'
  | 'failed';

export type Role =
  | 'orchestrator'
  | 'worker'
  | 'validator-scrutiny'
  | 'validator-functional';

export type RunResult = 'pass' | 'fail' | 'partial';

export type AssertionCheck = 'command' | 'agent-judgement' | 'pty-script';

export type ReasoningEffort = 'low' | 'medium' | 'high' | 'xhigh' | 'max';

export type WorkerMessageTag =
  | 'text'
  | 'tool-use'
  | 'tool-result'
  | 'denied'
  | 'system';

// ---------------------------------------------------------------------------
// Tickets (crates/server/src/tickets.rs, crates/engine/src/ticket.rs)
// ---------------------------------------------------------------------------

export type TicketState =
  | 'new'
  | 'drafting'
  | 'needs-context'
  | 'wrong-plan'
  | 'review'
  | 'queued'
  | 'running'
  | 'done'
  | 'failed'
  | 'parked';

/** Row shape from `GET /api/tickets` — enough to render a backlog table. */
export interface TicketSummary {
  slug: string;
  priority: number;
  state: TicketState;
  title: string;
  blockedBy: string[];
  isBlocked: boolean;
  /** Joined mission id. The server always emits the key (Rust
   *  `Option<String>`): null when the ticket has never been drafted. */
  missionId: string | null;
  /** Ancestry-probe bit: joined mission's branch tip is an ancestor of the
   *  live base branch tip. `null`/absent when there's no joined mission or
   *  the probe found nothing to merge yet. */
  merged?: boolean | null;
}

/** Full ticket shape from `GET /api/tickets/:slug`. */
export interface Ticket {
  slug: string;
  title: string;
  priority: number;
  schedule: 'once' | 'nightly' | 'weekly';
  blockedBy: string[];
  isBlocked: boolean;
  goal: string;
  context: string;
  scopingAnswers: string[];
  acceptanceHints: string[];
  taskClass?: string | null;
  reviewArtifact?: string | null;
  reviewOutput?: string | null;
  state: TicketState;
  needsContext: string[];
  /** Planner's draft-stage wrong-plan escalation reason (server always emits
   *  the key): null when the ticket carries no `## Wrong plan` section. */
  wrongPlan: string | null;
  /** Joined mission id. The server always emits the key (Rust
   *  `Option<String>`): null when the ticket has never been drafted. */
  missionId: string | null;
  merged?: boolean | null;
}

// ---------------------------------------------------------------------------
// Core model
// ---------------------------------------------------------------------------

export interface Assertion {
  id: string;
  statement: string;
  check: AssertionCheck;
  command?: string;
  /** The scripted terminal session when `check` is `pty-script` (engine
   * `pty_harness`; absent for every other check). */
  ptyScript?: PtyScript;
  /** Optional paired fixtures for the assertion's own command. Configuration
   *  alone is not evidence that either control has run or passed. */
  negativeControl?: NegativeControl;
}

export interface NegativeControlFile {
  path: string;
  content: string;
}

export interface NegativeControl {
  checkerFiles: NegativeControlFile[];
  validFiles: NegativeControlFile[];
  defectiveFiles: NegativeControlFile[];
  expectedFailure: string;
  /** Seconds per fixture: defaults to 60; the engine rejects values over 180. */
  timeoutSeconds?: number;
}

/** One scripted terminal session declared by a `pty-script` assertion. */
export interface PtyScript {
  command: string;
  steps: PtyStep[];
  timeoutSecs?: number;
}

export type PtyStep =
  | { op: 'send'; text: string }
  | { op: 'expect'; pattern: string; regex?: boolean; timeoutMs?: number };

export interface Feature {
  id: string;
  title: string;
  spec: string;
  validationCriteria: string[];
  origin: FeatureOrigin;
  status: FeatureStatus;
  workerRuns: string[];
  commits: string[];
  respawns: number;
}

export interface Milestone {
  id: string;
  title: string;
  features: Feature[];
  status: MilestoneStatus;
  fixCycles: number;
  startSha?: string;
}

export interface Mission {
  id: string;
  goal: string;
  validationContract: Assertion[];
  milestones: Milestone[];
  status: MissionStatus;
  createdAt: string; // ISO-8601
  baseBranch: string;
  missionBranch: string;
  standardsManifest?: StandardsPin;
}

export interface Plan {
  goal: string;
  validationContract: Assertion[];
  milestones: PlanMilestone[];
  consideredAlternatives?: ConsideredAlternatives;
  touchSet?: string[];
  standardsManifest?: StandardsPin;
  reviewerIndependence?: { scrutiny: boolean; functional: boolean };
}

export interface PinnedRule {
  id: string;
  revision: number;
  rfc: string;
  level: string;
  effectiveStatus: string;
  statement: string;
  domains: string[];
  stages: string[];
  whenPaths: string[];
  taskClasses: string[];
  checker?: string;
  waivable: boolean;
}

export interface PinnedGate {
  id: string;
  command: string;
  whenPaths?: string[];
}

export interface StandardsPin {
  packName: string;
  packDir: string;
  standardsRoot: string;
  digest: string;
  source: 'repo-tracked' | 'external-pinned';
  taskClass?: string;
  touchSet?: string[];
  contextPaths?: string[];
  gates?: PinnedGate[];
  rules: PinnedRule[];
}

export type RuleDisposition =
  | 'passed'
  | 'failed'
  | 'advisory'
  | 'waived'
  | 'not-evaluated'
  | 'not-applicable';

export interface WaiverJoin {
  seq: number;
  approver: string;
  surface: string;
  reason: string;
  expiresAt: string;
}

export interface CoverageEvidence {
  seq: number;
  event: string;
  mechanism: string;
  bearing: 'pass' | 'fail' | 'waived';
  reference: string;
  waiver?: WaiverJoin;
}

export interface RuleCoverage {
  id: string;
  revision: number;
  lifecycle: string;
  level: string;
  checker?: string;
  statement?: string;
  disposition: RuleDisposition;
  evidence?: CoverageEvidence[];
  note?: string;
}

export interface StandardsDriftRecord {
  seq: number;
  approvedDigest: string;
  currentDigest?: string;
  changedRules: string[];
}

export interface StandardsCoverage {
  packName: string;
  packDir: string;
  standardsRoot: string;
  digest: string;
  source: string;
  approvalSeq: number;
  resolutionSeq?: number;
  resolvedAt?: string;
  rules: RuleCoverage[];
  drift?: StandardsDriftRecord[];
}

export interface StandardsWaiverCandidate {
  rule: PinnedRule;
  findingSubject: string;
  findingEvidence: string;
  runId: string;
}

export interface MissionStandardsView {
  manifest?: StandardsPin;
  coverage?: StandardsCoverage;
  waiverCandidates?: StandardsWaiverCandidate[];
}

export interface StandardsWaiverResult {
  recorded: true;
  seq: number;
  rule: PinnedRule;
  findingSubject: string;
  findingEvidence: string;
  runId: string;
  affectedPaths: string[];
  diffDigest: string;
  findingFingerprint: string;
}

export interface ConsideredAlternatives {
  chosen: string;
  rejected: RejectedAlternative[];
}

export interface RejectedAlternative {
  approach: string;
  tradeOff: string;
}

export interface PlanMilestone {
  title: string;
  features: PlanFeature[];
}

export interface PlanFeature {
  title: string;
  spec: string;
  validationCriteria: string[];
}

export interface PendingRevision {
  revision: number;
  plan: Plan;
  instructions: string;
}

export type GrantKind = 'command' | 'touch-path' | 'worker-deny' | 'egress';

export interface PendingGrantRequest {
  milestoneId: string;
  /** Defaults to 'command' for pre-`kind` snapshots. */
  kind?: GrantKind;
  /** The granted target: a command string, a path glob, a deny rule, or a host:port egress destination. */
  command: string;
}

/**
 * An open structured human question (the pending-decision projection's
 * second kind, rendered in the same "your move" area as the parked grant).
 * Absent on pre-field snapshots (`pendingQuestions` omitted then).
 */
export interface PendingQuestion {
  /** Engine-minted id (`q-<n>`) — the handle every answer path names. */
  questionId: string;
  /** Who asked — 'worker' in this pass. */
  role: Role;
  text: string;
  /** The structured choices offered (empty = free-text answer expected). */
  options?: string[];
  runId?: string;
  featureId?: string;
  milestoneId?: string;
}

export interface TokenUsage {
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
}

export interface WorkerReport {
  result: RunResult;
  summary: string;
  filesTouched?: string[];
  testsAdded?: string[];
  testEvidence?: string;
  dependenciesAdded?: string[];
  knownGaps?: string[];
  commits?: string[];
}

export interface Finding {
  subject: string;
  severity: string; // "critical" | "major" | "minor"
  evidence: string;
  suggestedFix?: string;
  class?: string;
  rule?: RuleCitation;
}

export interface RuleCitation {
  id: string;
  revision: number;
  source: string;
  digest: string;
  lifecycle: string;
  level: string;
  checker?: string;
}

export interface WorkerRun {
  id: string;
  role: Role;
  featureId?: string;
  milestoneId?: string;
  sdkSessionId: string;
  model: string;
  startedAt: string;
  endedAt?: string;
  tokens: TokenUsage;
  costUsd?: number;
  transcriptPath: string;
  result?: RunResult;
  report?: WorkerReport;
  promptHash: string;
}

export type AgentBackend = 'claude' | 'codex' | 'droid' | 'kimi';

export type SandboxEnforce = 'off' | 'fs' | 'fs+net';

export interface SandboxConfig {
  enforce: SandboxEnforce;
  extraWrite: string[];
  egress: string[];
}

export interface RoleConfig {
  model: string;
  reasoningEffort: string;
  maxTurns?: number;
  maxBudgetUsd?: number;
  backend?: AgentBackend;
  sandbox?: SandboxConfig;
}

export interface MissionConfig {
  orchestrator: RoleConfig;
  worker: RoleConfig;
  validatorScrutiny: RoleConfig;
  validatorFunctional: RoleConfig;
  skipScrutiny: boolean;
  skipFunctional: boolean;
  maxFixCyclesPerMilestone: number;
  maxRespawns: number;
  maxParallelWorkers: number;
  eventStreamThrottleMs: number;
  denyPatterns: string[];
  allowValidatorCommands: string[];
  dangerouslyAllowAll: boolean;
  allowBelowDefaultWorkerModel: boolean;
  claudeBinary?: string;
  workerIsolation?: 'worktree' | 'checkout';
}

export interface MissionState {
  mission: Mission;
  featureBaseShas?: Record<string, string>;
  runs: Record<string, WorkerRun>;
  totals: TokenUsage;
  totalCostUsd: number;
  localExecutorMilestones: number;
  escalatedMilestones: number;
  pendingUserMessages: string[];
  recentDecisions: string[];
  config: MissionConfig;
  latestPlanRevision: number;
  pendingRevision?: PendingRevision;
  pendingGrantRequest?: PendingGrantRequest;
  /** Open structured human questions, in open order. Omitted when empty. */
  pendingQuestions?: PendingQuestion[];
  lastSeq: number;
}

/** Row of GET /api/missions. */
export interface MissionSummary {
  id: string;
  status: string;
  goal: string;
  createdAt: string;
  error?: string;
  /** Ancestry-probe bit: mission branch tip is an ancestor of the live base
   *  branch tip. `null`/absent when there's no branch or the probe failed. */
  merged?: boolean | null;
}

export interface RepoActivity {
  queued: number;
  running: number;
  needsInput: number;
  completeUnmerged: number;
  failed: number;
}

/** Operator-owned row from `GET /api/repos`. */
export interface RepoSummary {
  id: string;
  root: string;
  displayName: string;
  group?: string;
  pinned: boolean;
  isDefault: boolean;
  status: 'healthy' | 'unavailable';
  error?: string;
  activity: RepoActivity;
}

// ---------------------------------------------------------------------------
// Mission lifecycle (server-hosted engine; M2.5) — docs/protocol.md
// ---------------------------------------------------------------------------

/** Mirror of crates/engine/src/cost.rs CostEstimate (camelCase). */
export interface CostEstimate {
  workerRuns: number;
  validatorRuns: number;
  lowUsd: number;
  expectedUsd: number;
  highUsd: number;
}

/** POST /api/missions/:id/planning/request-plan response. */
export type PlanRequestResponse =
  | { ready: true; plan: Plan; planIdentity: string; estimate: CostEstimate }
  | { ready: false; reply: string };

// ---------------------------------------------------------------------------
// Events (envelope flattens the kind: { seq, ts, missionId, type, payload })
// ---------------------------------------------------------------------------

export type EventKind =
  | { type: 'mission.created'; payload: { goal: string; baseBranch: string; missionBranch: string; config: MissionConfig } }
  | { type: 'plan.approved'; payload: { plan: Plan } }
  | { type: 'plan.revision.proposed'; payload: { revision: number; plan: Plan; instructions: string } }
  | { type: 'plan.revised'; payload: { revision: number; plan: Plan } }
  | { type: 'plan.revision.rejected'; payload: { revision: number; reason: string } }
  | { type: 'grant.requested'; payload: { milestoneId: string; command: string } }
  | { type: 'grant.approved'; payload: { command: string } }
  | { type: 'grant.denied'; payload: { command: string; reason: string } }
  | { type: 'question.opened'; payload: { questionId: string; role: Role; text: string; options?: string[]; runId?: string; featureId?: string; milestoneId?: string } }
  | { type: 'question.answered'; payload: { questionId: string; answer: string; via: string; option?: number } }
  | { type: 'question.cleared'; payload: { questionId: string; why: string } }
  | { type: 'milestone.started'; payload: { milestoneId: string; startSha: string } }
  | { type: 'feature.started'; payload: { featureId: string } }
  | { type: 'feature.progress'; payload: { featureId: string; baseSha: string; commits: string[] } }
  | { type: 'worker.spawned'; payload: { runId: string; role: Role; featureId?: string; milestoneId?: string; sdkSessionId: string; model: string; promptHash: string; transcriptPath: string } }
  | { type: 'worker.message'; payload: { runId: string; tag: WorkerMessageTag; content: string } }
  | { type: 'worker.completed'; payload: { runId: string; result: RunResult; tokens: TokenUsage; costUsd?: number; report?: WorkerReport } }
  | { type: 'feature.completed'; payload: { featureId: string; commits: string[] } }
  | { type: 'feature.failed'; payload: { featureId: string; reason: string } }
  | { type: 'feature.skipped'; payload: { featureId: string; reason: string } }
  | { type: 'milestone.validating'; payload: { milestoneId: string } }
  | { type: 'validation.finding'; payload: { milestoneId: string; runId: string; finding: Finding } }
  | { type: 'gate.result'; payload: { gate: string; surface: string; kind: string; index: number; verdict: 'pass' | 'fail'; artefactRef: string; artefactDetail?: string; score?: number; threshold?: number; ruleIds?: string[] } }
  | { type: 'standards.resolved'; payload: { source: string; packName: string; standardsRoot: string; digest: string; stage: string; taskClass?: string; touchSet: string[]; contextPaths?: string[]; rules: Array<{ id: string; revision: number; effectiveStatus: string }>; approvalSeq: number } }
  | { type: 'standards.drifted'; payload: { approvedDigest: string; currentDigest?: string; surface: string; changedRules: string[] } }
  | { type: 'standards.waiver.approved'; payload: { ruleId: string; ruleRevision: number; manifestDigest: string; approvalSeq: number; findingFingerprint: string; paths?: string[]; diffDigest: string; reason: string; approver: string; surface: string; expiresAt: string } }
  | { type: 'standards.attestation.approved'; payload: { ruleId: string; ruleRevision: number; manifestDigest: string; approvalSeq: number; paths?: string[]; diffDigest: string; reason: string; approver: string; surface: string } }
  | { type: 'validator.tamper'; payload: { milestoneId: string; runId: string; role: Role; headBefore: string; headAfter: string; appeared: string[]; resolved: string[] } }
  | { type: 'fixfeature.created'; payload: { milestoneId: string; feature: Feature } }
  | { type: 'milestone.blocked'; payload: { milestoneId: string; reason: string } }
  | { type: 'milestone.unblocked'; payload: { milestoneId: string; reason: string } }
  | { type: 'milestone.completed'; payload: { milestoneId: string; tag?: string } }
  | { type: 'mission.validating'; payload: Record<string, never> }
  | { type: 'mission.paused'; payload: Record<string, never> }
  | { type: 'mission.resumed'; payload: Record<string, never> }
  | { type: 'user.message'; payload: { text: string; interrupt: boolean } }
  | { type: 'orchestrator.decision'; payload: { summary: string; detail?: string } }
  | { type: 'config.changed'; payload: { patch: unknown } }
  | { type: 'mission.completed'; payload: Record<string, never> }
  | { type: 'mission.failed'; payload: { reason: string } };

export type MissionEvent = {
  seq: number;
  ts: string;
  missionId: string;
} & EventKind;

export type EventType = EventKind['type'];

// ---------------------------------------------------------------------------
// Control commands (POST /api/missions/:id/control)
// ---------------------------------------------------------------------------

export type ControlCommand =
  | { kind: 'pause' }
  | { kind: 'resume' }
  | { kind: 'msg'; text: string; interrupt: boolean }
  | { kind: 'config-change'; patch: Record<string, unknown> }
  | { kind: 'request-revision'; instructions: string }
  | { kind: 'approve-revision'; revision: number }
  | { kind: 'reject-revision'; revision: number };

// ---------------------------------------------------------------------------
// WebSocket frames (GET /api/missions/:id/ws)
// ---------------------------------------------------------------------------

export type WsFrame =
  | { type: 'snapshot'; seq: number; state: MissionState }
  | { type: 'event'; seq: number; event: MissionEvent }
  | { type: 'state'; seq: number; state: MissionState }
  | { type: 'pong' };

// ---------------------------------------------------------------------------
// Run transcripts: JSONL of Claude Code stream-json values, parsed to an array.
// Loosely typed — we only pick out the fields we render.
// ---------------------------------------------------------------------------

export interface TranscriptBlock {
  type: string; // "text" | "thinking" | "tool_use" | "tool_result"
  text?: string;
  thinking?: string;
  id?: string;
  name?: string;
  input?: unknown;
  tool_use_id?: string;
  content?: unknown;
  is_error?: boolean;
}

export interface TranscriptEntry {
  type: string; // "system" | "assistant" | "user" | "result" | ...
  subtype?: string;
  message?: {
    role?: string;
    model?: string;
    content?: TranscriptBlock[] | string;
  };
  model?: string;
  result?: string;
  total_cost_usd?: number;
  num_turns?: number;
  is_error?: boolean;
  [key: string]: unknown;
}

export type ConnectionStatus = 'connecting' | 'live' | 'lost';

// ---------------------------------------------------------------------------
// Queue (crates/engine/src/queue.rs, crates/server/src/host.rs — GET /api/queue,
// POST /api/queue/drain)
// ---------------------------------------------------------------------------

/** One queued mission (`kranz_engine::queue::QueueEntry`). */
export interface QueueEntry {
  missionId: string;
  ticketSlug?: string;
  priority: number;
  seq: number;
  /** Best-effort readiness probe (same shape as GET /readiness). */
  readiness?: ReadinessReport;
}

/** `GET /api/missions/:id/readiness` */
export interface ReadinessReport {
  missionId: string;
  roles: Array<{
    role: string;
    backend: string;
    status: string;
    detail: string;
    nextAction: string;
  }>;
  overall: string;
  warnings: string[];
}

/** `GET /api/missions/:id/workspace` — derived local execution context. */
export interface WorkspaceSummary {
  isolation: 'worktree' | 'checkout';
  cwd: string;
  lifecycle: 'active' | 'pending' | 'removed' | 'primary-checkout';
  worktreeActive: boolean;
  sandboxes: Array<{
    role: 'worker' | 'scrutiny' | 'functional';
    enforce: SandboxEnforce;
    extraWriteCount: number;
    egressCount: number;
  }>;
  preflight: {
    status: 'pending' | 'clear' | 'issues';
    summary: string;
    eventSeq: number | null;
  };
  /** `.kranz/workspace.json` presence (D-H): services/previews are 0 when
   *  no contract is present. */
  contract: {
    present: boolean;
    services: number;
    previews: number;
  };
}

/** The hook-status lane's signal vocabulary (ticket
 *  `agent-hooks-status-signals`) — deliberately much smaller than mission
 *  status, so no hook payload can spell a state transition. */
export type HookStatusSignalKind = 'running' | 'needs-input' | 'interrupted' | 'turn-finished';

/** One accepted hook-derived signal occurrence. */
export interface HookStatusSignalRecord {
  signal: HookStatusSignalKind;
  detail?: string;
  receivedAt: string;
}

/** One run's ephemeral projection entry (latest signal wins). */
export interface RunHookStatusView {
  runId: string;
  registeredAt: string;
  signal?: HookStatusSignalRecord;
}

/** `GET /api/missions/:id/hook-status` — the ephemeral hook-signal
 *  projection. `authoritative` is always false: hook-derived signals are
 *  observability, never folded mission state. */
export interface MissionHookStatus {
  missionId: string;
  authoritative: false;
  note: string;
  runs: RunHookStatusView[];
}


/** This host's queue-drain tracker (`MissionHost::drain` / `drain_state_json`). */
export interface DrainState {
  live: boolean;
  currentMissionId: string | null;
  ran: string[];
  /** Missions removed before claim due to backend readiness failure. */
  parked?: string[];
}

/** `GET /api/queue` response shape. */
export interface QueueState {
  entries: QueueEntry[];
  busyWith: string | null;
  drain: DrainState;
  /** Run slots left under `host.maxConcurrentRepos` (multi-repo hosts only). */
  maxConcurrentReposAvailable?: number;
  /** True when the process-wide run budget is exhausted. */
  maxConcurrentReposSaturated?: boolean;
}

// ---------------------------------------------------------------------------
// Outcomes (crates/engine/src/outcomes.rs) — flight-surgeon fold
// ---------------------------------------------------------------------------

export interface AutonomyRatio {
  closedMissions: number;
  totalInterventions: number;
  interventionsPerClosedMission: number;
  zeroInterventionMissions: number;
  zeroInterventionShare: number;
}

export interface LatencyBucket {
  label: string;
  count: number;
}

export interface GrantLatency {
  buckets: LatencyBucket[];
  totalDecided: number;
}

export type EscalationKind = 'block' | 'grant' | 'revision';

export interface EscalationRow {
  ts: string;
  missionId: string;
  kind: EscalationKind;
  summary: string;
  decision: string;
  latencyMs: number | null;
  // DEFERRED (false-green linkage): not wired — see flight-surgeon-console ticket
  defectOf?: string;
}

/** `GET /api/missions/outcomes` — flight-surgeon outcomes fold. */
export interface Outcomes {
  autonomyRatio: AutonomyRatio;
  grantLatency: GrantLatency;
  escalations: EscalationRow[];
  costPerChange: CostPerChange;
  cycleTime: CycleTime;
}

// ---------------------------------------------------------------------------
// Escalation metrics (crates/engine/src/escalation_metrics.rs) — the
// flight-surgeon console: autonomy split by outcome, rubber-stamp signal,
// false greens joined from traced-from-mission tickets, and the ledger.
// ---------------------------------------------------------------------------

export interface AutonomyOutcomeSplit {
  missions: number;
  zeroIntervention: number;
  /** null when the arm has no missions. */
  zeroInterventionShare: number | null;
}

export interface AutonomyMetric {
  closedMissions: number;
  zeroInterventionMissions: number;
  /** null when nothing closed. */
  zeroInterventionShare: number | null;
  completed: AutonomyOutcomeSplit;
  failed: AutonomyOutcomeSplit;
}

export interface RubberStamp {
  decidedGrants: number;
  /** Nearest-rank percentiles (ms); null when nothing was decided. */
  p50Ms: number | null;
  p90Ms: number | null;
  underTenSeconds: number;
}

export interface FalseGreenSplit {
  completedMissions: number;
  falseGreens: number;
  /** null when the arm has no completed missions. */
  rate: number | null;
}

export interface TracedDefect {
  ticket: string;
  missionId: string;
}

export interface FalseGreens {
  completedMissions: number;
  falseGreens: number;
  /** null when nothing completed. */
  falseGreenRate: number | null;
  withInterventions: FalseGreenSplit;
  zeroIntervention: FalseGreenSplit;
  tracedDefects: TracedDefect[];
}

export type LedgerKind = 'grant' | 'steer';

export interface LedgerRow {
  ts: string;
  missionId: string;
  kind: LedgerKind;
  milestoneId: string | null;
  ask: string;
  decision: string;
  latencyMs: number | null;
}

/** `GET /api/escalation-metrics` — the flight-surgeon console fold. */
export interface EscalationMetrics {
  autonomy: AutonomyMetric;
  rubberStamp: RubberStamp;
  falseGreens: FalseGreens;
  ledger: LedgerRow[];
}

/** costUsd per merged non-meta commit (outcomes-view lagging metric). */
export interface CostPerChange {
  totalCostUsd: number;
  nonMetaCommits: number;
  /** totalCostUsd / nonMetaCommits; null when no non-meta commits. */
  usdPerCommit: number | null;
}

/** mission.created → terminal, minus paused spans. */
export interface CycleTime {
  closedMissions: number;
  totalMs: number;
  /** totalMs / closedMissions; null when nothing closed yet. */
  meanMs: number | null;
}

/** `GET /api/missions/:id/pr-handoff` — never auto-pushes. */
export type PrHandoff =
  | { kind: 'needsPush'; command: string; remote: string; branch: string }
  | {
      kind: 'readyToCreate';
      command: string;
      title: string;
      body: string;
      remote: string;
      branch: string;
      base: string;
    }
  | { kind: 'unavailable'; reason: string };
