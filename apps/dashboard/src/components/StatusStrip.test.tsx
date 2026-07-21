import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { StatusStrip } from './StatusStrip';
import { useKranzStore } from '../lib/store';
import { ApiError } from '../lib/api';
import type {
  Mission,
  MissionConfig,
  MissionState,
  MissionStatus,
  RoleConfig,
  WorkerRun,
} from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      startMission: vi.fn(),
      control: vi.fn(),
    },
  };
});

import { api } from '../lib/api';

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
    localExecutorMilestones: 0,
    escalatedMilestones: 0,
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
  vi.mocked(api.startMission).mockReset();
  vi.mocked(api.control).mockReset();
  useKranzStore.setState(
    {
      ...INITIAL_STORE_STATE,
      missionId: 'm-test',
      events: [],
      startingRun: false,
      startRunError: null,
    },
    true,
  );
});

describe('StatusStrip Start action', () => {
  it('shows Start and no pause when approved-idle', () => {
    useKranzStore.setState({ state: makeState('approved', {}) });
    render(<StatusStrip />);
    expect(screen.queryByText('Start')).toBeTruthy();
    expect(screen.queryByText(/pause/i)).toBeFalsy();
  });

  it('shows pause and no Start when running with worker runs', () => {
    useKranzStore.setState({ state: makeState('running', { r1: makeWorkerRun('r1') }) });
    render(<StatusStrip />);
    expect(screen.queryByText(/pause/i)).toBeTruthy();
    expect(screen.queryByText('Start')).toBeFalsy();
  });

  it('calls api.startMission and surfaces a 409 conflict message', async () => {
    useKranzStore.setState({ state: makeState('approved', {}) });
    vi.mocked(api.startMission).mockRejectedValueOnce(
      new ApiError(409, "mission 'm-test' is already running — observe it via GET .../state ..."),
    );
    render(<StatusStrip />);

    fireEvent.click(screen.getByText('Start'));

    expect(api.startMission).toHaveBeenCalledTimes(1);
    expect(api.startMission).toHaveBeenCalledWith('m-test');

    const alert = await screen.findByText(/already running/);
    expect(alert).toBeTruthy();
    expect(alert.getAttribute('role')).toBe('alert');
  });
});

describe('StatusStrip pause/resume errors', () => {
  it('surfaces a failed pause control as an alert', async () => {
    useKranzStore.setState({ state: makeState('running', { r1: makeWorkerRun('r1') }) });
    vi.mocked(api.control).mockRejectedValueOnce(new ApiError(503, 'control channel down'));
    render(<StatusStrip />);

    fireEvent.click(screen.getByText(/pause/i));

    const alert = await screen.findByText('control channel down');
    expect(alert.getAttribute('role')).toBe('alert');
  });

  it('surfaces a failed resume control as an alert', async () => {
    useKranzStore.setState({ state: makeState('paused', {}) });
    vi.mocked(api.control).mockRejectedValueOnce(new ApiError(503, 'resume refused'));
    render(<StatusStrip />);

    fireEvent.click(screen.getByText(/resume/i));

    const alert = await screen.findByText('resume refused');
    expect(alert.getAttribute('role')).toBe('alert');
  });
});
