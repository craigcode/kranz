import { describe, expect, it } from 'vitest';
import {
  isMissionBlocked,
  pendingAsks,
  pendingGateReviews,
  pendingGrant,
  pendingPermissions,
  pendingQuestions,
  pendingRevision,
} from './pendingAsks';
import type {
  GateEvaluation,
  LivePermission,
  MissionState,
  MissionStatus,
  PendingGrantRequest,
  PendingQuestion,
  PendingRevision,
} from './types';

const NOW = Date.UTC(2026, 10, 6, 20, 0, 0);

function baseState(status: MissionStatus = 'running'): MissionState {
  return {
    mission: {
      id: 'm-1',
      goal: 'Ship fleet model',
      validationContract: [],
      milestones: [{ id: 'ms-1', title: 'Core', features: [], status: 'active', fixCycles: 0 }],
      status,
      createdAt: '2026-11-06T19:00:00Z',
      baseBranch: 'main',
      missionBranch: 'kranz/mission-m-1',
    },
    runs: {},
    totals: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    totalCostUsd: 0,
    localExecutorMilestones: 0,
    escalatedMilestones: 0,
    pendingUserMessages: [],
    recentDecisions: [],
    config: {} as MissionState['config'],
    latestPlanRevision: 0,
    lastSeq: 1,
  };
}

function makePermission(overrides: Partial<LivePermission> = {}, deadlineOffsetMs = 60_000): LivePermission {
  return {
    request: {
      proposal: {
        id: 'perm-1',
        engineSessionId: 's-1',
        peerSessionId: 'peer-1',
        peerRequestId: 1,
        toolCallId: 'tc-1',
        action: { command: 'npm test' },
        options: [{ optionId: 'allow', kind: 'allow_once' }],
        actionDigest: 'ad',
        optionsDigest: 'od',
        observedAt: new Date(NOW - 5_000).toISOString(),
        deadline: new Date(NOW + deadlineOffsetMs).toISOString(),
        prohibition: null,
      },
      binding: {
        missionId: 'm-1',
        runId: 'r-1',
        workspace: '/tmp/ws',
        planDigest: 'pd',
        policyDigest: 'pol',
      },
      bindingDigest: 'bd',
    },
    ...overrides,
  };
}

function makeGate(overrides: Partial<GateEvaluation> = {}): GateEvaluation {
  return {
    requested: {
      request: {
        params: {
          attemptId: 'att-1',
          gateId: 'scrutiny-gate',
          stage: 'final-gate',
          deadline: new Date(NOW + 60_000).toISOString(),
          subject: { kind: 'deliverable' },
          binding: {},
        },
      },
    },
    requestedSeq: 4,
    resolution: { disposition: 'require-human', rationale: 'check diff' },
    ...overrides,
  };
}

describe('pendingAsks six in-flight conditions', () => {
  it('matches PermissionRequestPanel for present, resolved, closed, and expired permissions', () => {
    const active = baseState();
    active.permissions = { p1: makePermission() };
    expect(pendingPermissions(active, NOW)).toHaveLength(1);

    const resolved = baseState();
    resolved.permissions = {
      p1: makePermission({
        resolution: {
          requestId: 'perm-1',
          bindingDigest: 'bd',
          allow: true,
          actor: { kind: 'human' },
          reason: 'ok',
        },
      }),
    };
    expect(pendingPermissions(resolved, NOW)).toHaveLength(0);

    const closed = baseState();
    closed.permissions = { p1: makePermission({ closed: 'peer disconnected' }) };
    expect(pendingPermissions(closed, NOW)).toHaveLength(0);

    const expired = baseState();
    expired.permissions = { p1: makePermission({}, -1) };
    expect(pendingPermissions(expired, NOW)).toHaveLength(0);
  });

  it('matches GrantRequestPanel for present and absent grants', () => {
    expect(pendingGrant(baseState())).toBeNull();
    const s = baseState('validating');
    const grant: PendingGrantRequest = {
      milestoneId: 'ms-1',
      kind: 'egress',
      command: 'registry.npmjs.org:443',
    };
    s.pendingGrantRequest = grant;
    expect(pendingGrant(s)).toEqual(grant);
  });

  it('matches StatusStrip blocked condition', () => {
    expect(isMissionBlocked(baseState('running'))).toBe(false);
    expect(isMissionBlocked(baseState('paused'))).toBe(false);
    expect(isMissionBlocked(baseState('blocked'))).toBe(true);
  });

  it('matches GateReviewPanel for require-human/block vs proceed, consumed, and closed', () => {
    const s = baseState();
    s.gateEvaluations = {
      late: makeGate({ requestedSeq: 9, requested: { request: { params: { attemptId: 'att-2', gateId: 'g-2', stage: 's', deadline: '2026-11-06T21:00:00Z', subject: {}, binding: {} } } } }),
      early: makeGate({ requestedSeq: 2 }),
      proceed: makeGate({ resolution: { disposition: 'proceed', rationale: 'ok' } }),
      consumed: makeGate({ consumed: true }),
      closed: makeGate({ closed: 'superseded' }),
    };
    const gates = pendingGateReviews(s);
    expect(gates.map((g) => g.requestedSeq)).toEqual([2, 9]);
  });

  it('matches RevisionPanel for revisable vs terminal mission statuses', () => {
    const rev: PendingRevision = {
      revision: 2,
      plan: { goal: 'g', validationContract: [], milestones: [] },
      instructions: 'Split milestone 1',
    };
    const active = baseState('running');
    active.pendingRevision = rev;
    expect(pendingRevision(active)).toEqual(rev);

    const complete = baseState('complete');
    complete.pendingRevision = rev;
    expect(pendingRevision(complete)).toBeNull();
  });

  it('matches QuestionRequestPanel for open vs empty questions', () => {
    expect(pendingQuestions(baseState())).toEqual([]);
    const q: PendingQuestion = {
      questionId: 'q-1',
      role: 'worker',
      text: 'Use SQLite or memory?',
      options: ['sqlite', 'memory'],
    };
    const s = baseState();
    s.pendingQuestions = [q];
    expect(pendingQuestions(s)).toEqual([q]);
  });

  it('orders combined asks as permission, grant, blocked, gate, revision, question', () => {
    const s = baseState('blocked');
    s.permissions = { p1: makePermission() };
    s.pendingGrantRequest = { milestoneId: 'ms-1', command: 'cargo test' };
    s.gateEvaluations = { g1: makeGate() };
    s.pendingRevision = {
      revision: 1,
      plan: { goal: 'g', validationContract: [], milestones: [] },
      instructions: 'Tighten assertion',
    };
    s.pendingQuestions = [{ questionId: 'q-1', role: 'worker', text: 'Retry?' }];

    const asks = pendingAsks(s, NOW);
    expect(asks.map((a) => a.kind)).toEqual([
      'permission',
      'grant',
      'blocked',
      'gate',
      'revision',
      'question',
    ]);
  });
});
