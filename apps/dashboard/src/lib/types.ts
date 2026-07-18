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

export type AssertionCheck = 'command' | 'agent-judgement';

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
  state: TicketState;
  needsContext: string[];
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
}

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
}

export interface Plan {
  goal: string;
  validationContract: Assertion[];
  milestones: PlanMilestone[];
  consideredAlternatives?: ConsideredAlternatives;
  touchSet?: string[];
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

export type GrantKind = 'command' | 'touch-path' | 'worker-deny';

export interface PendingGrantRequest {
  milestoneId: string;
  /** Defaults to 'command' for pre-`kind` snapshots. */
  kind?: GrantKind;
  /** The granted target: a command string, or a path glob for a touch grant. */
  command: string;
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

export type AgentBackend = 'claude' | 'codex' | 'droid';

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
  runs: Record<string, WorkerRun>;
  totals: TokenUsage;
  totalCostUsd: number;
  pendingUserMessages: string[];
  recentDecisions: string[];
  config: MissionConfig;
  latestPlanRevision: number;
  pendingRevision?: PendingRevision;
  pendingGrantRequest?: PendingGrantRequest;
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
  | { ready: true; plan: Plan; estimate: CostEstimate }
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
  | { type: 'milestone.started'; payload: { milestoneId: string; startSha: string } }
  | { type: 'feature.started'; payload: { featureId: string } }
  | { type: 'worker.spawned'; payload: { runId: string; role: Role; featureId?: string; milestoneId?: string; sdkSessionId: string; model: string; promptHash: string; transcriptPath: string } }
  | { type: 'worker.message'; payload: { runId: string; tag: WorkerMessageTag; content: string } }
  | { type: 'worker.completed'; payload: { runId: string; result: RunResult; tokens: TokenUsage; costUsd?: number; report?: WorkerReport } }
  | { type: 'feature.completed'; payload: { featureId: string; commits: string[] } }
  | { type: 'feature.failed'; payload: { featureId: string; reason: string } }
  | { type: 'feature.skipped'; payload: { featureId: string; reason: string } }
  | { type: 'milestone.validating'; payload: { milestoneId: string } }
  | { type: 'validation.finding'; payload: { milestoneId: string; runId: string; finding: Finding } }
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
