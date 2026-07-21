import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { TopBar } from './TopBar';
import { useKranzStore } from '../lib/store';
import type { Mission, MissionConfig, MissionState, RoleConfig } from '../lib/types';

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
    allowBelowDefaultWorkerModel: false,
  };
}

function makeMission(): Mission {
  return {
    id: 'm-test',
    goal: 'test goal',
    validationContract: [],
    milestones: [],
    status: 'running',
    createdAt: '2026-01-01T00:00:00Z',
    baseBranch: 'main',
    missionBranch: 'kranz/mission-m-test',
  };
}

function makeState(localExecutorMilestones: number, escalatedMilestones: number): MissionState {
  return {
    mission: makeMission(),
    runs: {},
    totals: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    totalCostUsd: 0,
    localExecutorMilestones,
    escalatedMilestones,
    pendingUserMessages: [],
    recentDecisions: [],
    config: makeConfig(),
    latestPlanRevision: 0,
    lastSeq: 0,
  };
}

const INITIAL_STORE_STATE = useKranzStore.getState();

beforeEach(() => {
  cleanup();
  useKranzStore.setState(
    {
      ...INITIAL_STORE_STATE,
      missionId: 'm-test',
      events: [],
      pauseEvents: [],
    },
    true,
  );
});

describe('TopBar escalation stat', () => {
  it('renders 50% for a mission with 2 local milestones and 1 escalation', () => {
    useKranzStore.setState({ state: makeState(2, 1) });
    render(<TopBar />);
    expect(screen.getByText('Escalation')).toBeTruthy();
    expect(screen.getByText('50%')).toBeTruthy();
  });

  it('renders an em-dash for a never-local mission with no divide-by-zero', () => {
    useKranzStore.setState({ state: makeState(0, 0) });
    render(<TopBar />);
    expect(screen.getByText('Escalation')).toBeTruthy();
    expect(screen.queryByText(/NaN/)).toBeFalsy();
    expect(screen.getByText('—')).toBeTruthy();
  });
});
