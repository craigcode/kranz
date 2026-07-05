import { describe, it, expect } from 'vitest';
import type { Mission, MissionConfig, MissionState, MissionStatus, RoleConfig, WorkerRun } from './types';
import { isApprovedIdle } from './startAffordance';

function makeRoleConfig(): RoleConfig {
  return { model: 'claude-sonnet-5', reasoningEffort: 'medium' };
}

function makeConfig(): MissionConfig {
  return {
    orchestrator: makeRoleConfig(),
    worker: makeRoleConfig(),
    validatorScrutiny: makeRoleConfig(),
    validatorFunctional: makeRoleConfig(),
    skipScrutiny: false,
    skipFunctional: false,
    maxFixCyclesPerMilestone: 3,
    maxRespawns: 3,
    maxParallelWorkers: 1,
    eventStreamThrottleMs: 100,
    denyPatterns: [],
    allowValidatorCommands: [],
    dangerouslyAllowAll: false,
  };
}

function makeMission(status: MissionStatus): Mission {
  return {
    id: 'm-test',
    goal: 'test goal',
    validationContract: [],
    milestones: [],
    status,
    createdAt: '2026-01-01T00:00:00Z',
    baseBranch: 'main',
    missionBranch: 'kranz/mission-m-test',
  };
}

function makeWorkerRun(id: string): WorkerRun {
  return {
    id,
    role: 'worker',
    sdkSessionId: `session-${id}`,
    model: 'claude-sonnet-5',
    startedAt: '2026-01-01T00:00:01Z',
    tokens: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    transcriptPath: `/tmp/${id}.jsonl`,
    promptHash: 'hash',
  };
}

function makeState(status: MissionStatus, runs: Record<string, WorkerRun>): MissionState {
  return {
    mission: makeMission(status),
    runs,
    totals: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    totalCostUsd: 0,
    pendingUserMessages: [],
    recentDecisions: [],
    config: makeConfig(),
    lastSeq: 0,
  };
}

describe('isApprovedIdle', () => {
  it('is true when running with zero runs', () => {
    expect(isApprovedIdle(makeState('running', {}))).toBe(true);
  });

  it('is false when running with one or more runs', () => {
    expect(isApprovedIdle(makeState('running', { r1: makeWorkerRun('r1') }))).toBe(false);
    expect(
      isApprovedIdle(
        makeState('running', { r1: makeWorkerRun('r1'), r2: makeWorkerRun('r2') }),
      ),
    ).toBe(false);
  });

  const nonRunningStatuses: MissionStatus[] = [
    'planning',
    'paused',
    'blocked',
    'validating',
    'complete',
    'failed',
    'abandoned',
  ];

  for (const status of nonRunningStatuses) {
    it(`is false for status ${status} with zero runs`, () => {
      expect(isApprovedIdle(makeState(status, {}))).toBe(false);
    });
  }
});
