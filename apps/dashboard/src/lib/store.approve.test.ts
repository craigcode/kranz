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

    await vi.waitFor(() => {
      expect(useKranzStore.getState().planning.approvedBranch).toBe('kranz/mission-m-test');
    });

    expect(api.approvePending).toHaveBeenCalledOnce();
    expect(api.approvePending).toHaveBeenCalledWith('m-test');
    expect(useKranzStore.getState().planning.approving).toBe(false);
  });

  it('sets planning.approving while the approve POST is in flight', async () => {
    let resolveApprove!: (v: { branch: string; started: boolean }) => void;
    vi.mocked(api.approvePending).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveApprove = resolve;
        }),
    );

    useKranzStore.getState().approvePlan();
    expect(useKranzStore.getState().planning.approving).toBe(true);

    // A second click while in flight is a no-op.
    useKranzStore.getState().approvePlan();
    expect(api.approvePending).toHaveBeenCalledTimes(1);

    resolveApprove({ branch: 'kranz/mission-m-test', started: false });
    await vi.waitFor(() => {
      expect(useKranzStore.getState().planning.approving).toBe(false);
    });
    expect(useKranzStore.getState().planning.approvedBranch).toBe('kranz/mission-m-test');
  });
});
