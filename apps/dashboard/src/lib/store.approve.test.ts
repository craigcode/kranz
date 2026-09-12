import { describe, it, expect, vi, beforeEach } from 'vitest';
import { useKranzStore } from './store';
import type { Plan, CostEstimate } from './types';

vi.mock('./api', async () => {
  const actual = await vi.importActual<typeof import('./api')>('./api');
  return {
    ...actual,
    api: {
      approvePending: vi.fn(),
      requestPlan: vi.fn(),
    },
  };
});

import { api, ApiError } from './api';

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
  vi.mocked(api.requestPlan).mockReset();
  useKranzStore.setState(
    {
      ...INITIAL_STORE_STATE,
      missionId: 'm-test',
      planning: {
        ...INITIAL_STORE_STATE.planning,
        review: { planIdentity: 'reviewed-plan-a', plan: makePlan(), estimate: makeEstimate() },
      },
    },
    true,
  );
});

describe('approvePlan', () => {
  it('retains the server preview identity and submits it unchanged', async () => {
    vi.mocked(api.requestPlan).mockResolvedValueOnce({
      ready: true, plan: makePlan(), planIdentity: 'server-plan-b', estimate: makeEstimate(),
    });
    useKranzStore.getState().requestPlan();
    await vi.waitFor(() => {
      expect(useKranzStore.getState().planning.review?.planIdentity).toBe('server-plan-b');
    });
    vi.mocked(api.approvePending).mockResolvedValueOnce({ branch: 'approved-b', started: false });
    useKranzStore.getState().approvePlan();
    await vi.waitFor(() => {
      expect(api.approvePending).toHaveBeenCalledWith('m-test', 'server-plan-b');
    });
  });

  it('discards a stale preview on conflict and requires another review', async () => {
    vi.mocked(api.approvePending).mockRejectedValueOnce(new ApiError(409, 'refresh the plan preview'));
    useKranzStore.getState().approvePlan();
    await vi.waitFor(() => {
      expect(useKranzStore.getState().planning.review).toBeNull();
    });
    expect(useKranzStore.getState().planning.approvedBranch).toBeNull();
    expect(useKranzStore.getState().planning.error).toContain('refresh');
    expect(useKranzStore.getState().planning.approving).toBe(false);
    useKranzStore.getState().approvePlan();
    expect(api.approvePending).toHaveBeenCalledOnce();
  });

  it('requires a fresh preview when its identity is missing', () => {
    useKranzStore.setState((s) => ({
      planning: { ...s.planning, review: { ...s.planning.review!, planIdentity: '' } },
    }));
    useKranzStore.getState().approvePlan();
    expect(api.approvePending).not.toHaveBeenCalled();
    expect(useKranzStore.getState().planning.review).toBeNull();
    expect(useKranzStore.getState().planning.error).toContain('Request the plan again');
  });

  it.each(['turn_in_flight', 'repository_busy', 'future_code'])('keeps the preview on coded %s refusal', async (code) => {
    vi.mocked(api.approvePending).mockRejectedValueOnce(new ApiError(409, 'retry shortly', code));
    useKranzStore.getState().approvePlan();
    await vi.waitFor(() => {
      expect(useKranzStore.getState().planning.approving).toBe(false);
    });
    expect(useKranzStore.getState().planning.review?.planIdentity).toBe('reviewed-plan-a');
    expect(useKranzStore.getState().planning.error).toBe('retry shortly');
  });

  it('clears the preview on stale_plan even with different wording', async () => {
    vi.mocked(api.approvePending).mockRejectedValueOnce(new ApiError(409, 'Review again.', 'stale_plan'));
    useKranzStore.getState().approvePlan();
    await vi.waitFor(() => {
      expect(useKranzStore.getState().planning.review).toBeNull();
    });
  });

  it('calls api.approvePending with the mission id and the displayed plan identity', async () => {
    vi.mocked(api.approvePending).mockResolvedValueOnce({
      branch: 'kranz/mission-m-test',
      started: false,
    });

    useKranzStore.getState().approvePlan();

    await vi.waitFor(() => {
      expect(useKranzStore.getState().planning.approvedBranch).toBe('kranz/mission-m-test');
    });

    expect(api.approvePending).toHaveBeenCalledOnce();
    expect(api.approvePending).toHaveBeenCalledWith('m-test', 'reviewed-plan-a');
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
