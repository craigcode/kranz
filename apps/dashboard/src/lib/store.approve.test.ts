import { describe, it, expect, vi, beforeEach } from 'vitest';
import { useKranzStore } from './store';
import type { Plan, CostEstimate } from './types';

vi.mock('./api', async () => {
  const actual = await vi.importActual<typeof import('./api')>('./api');
  return {
    ...actual,
    api: {
      approvePending: vi.fn(),
    },
  };
});

import { api } from './api';

const INITIAL_STORE_STATE = useKranzStore.getState();

function makePlan(): Plan {
  return {
    goal: 'ship the thing',
    validationContract: [],
    milestones: [],
  };
}

function makeEstimate(): CostEstimate {
  return {
    workerRuns: 1,
    validatorRuns: 1,
    lowUsd: 1,
    expectedUsd: 2,
    highUsd: 3,
  };
}

beforeEach(() => {
  vi.mocked(api.approvePending).mockReset();
  useKranzStore.setState(
    {
      ...INITIAL_STORE_STATE,
      missionId: 'm-test',
      planning: {
        ...INITIAL_STORE_STATE.planning,
        review: { plan: makePlan(), estimate: makeEstimate() },
      },
    },
    true,
  );
});

describe('approvePlan', () => {
  it('calls api.approvePending with the mission id and not the legacy approve method', async () => {
    vi.mocked(api.approvePending).mockResolvedValueOnce({
      branch: 'kranz/mission-m-test',
      started: false,
    });

    useKranzStore.getState().approvePlan();
    await Promise.resolve();
    await Promise.resolve();

    expect(api.approvePending).toHaveBeenCalledOnce();
    expect(api.approvePending).toHaveBeenCalledWith('m-test');
    expect(useKranzStore.getState().planning.approvedBranch).toBe('kranz/mission-m-test');
  });
});
