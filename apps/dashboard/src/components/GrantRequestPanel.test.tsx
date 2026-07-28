import { beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { GrantRequestPanel } from './GrantRequestPanel';
import { useKranzStore } from '../lib/store';
import type { MissionState, PendingGrantRequest } from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      approveGrant: vi.fn(),
      denyGrant: vi.fn(),
    },
  };
});

import { api, ApiError } from '../lib/api';

const INITIAL_STORE_STATE = useKranzStore.getState();

function missionState(pending?: PendingGrantRequest): MissionState {
  return {
    mission: {
      id: 'm-1',
      goal: 'test grant UI',
      validationContract: [],
      milestones: [],
      status: 'validating',
      createdAt: '2026-07-13T00:00:00Z',
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
    // config is not read by this panel; a bare cast keeps the fixture small.
    config: {} as MissionState['config'],
    latestPlanRevision: 0,
    pendingGrantRequest: pending,
    lastSeq: 1,
  };
}

beforeEach(() => {
  cleanup();
  vi.mocked(api.approveGrant).mockReset().mockResolvedValue(undefined);
  vi.mocked(api.denyGrant).mockReset().mockResolvedValue(undefined);
  useKranzStore.setState(
    { ...INITIAL_STORE_STATE, missionId: 'm-1', state: missionState() },
    true,
  );
});

describe('GrantRequestPanel', () => {
  it('does not render when no grant is parked', () => {
    render(<GrantRequestPanel />);
    expect(screen.queryByRole('button', { name: /Approve grant/ })).toBeNull();
  });

  it('shows the parked command and approves it', async () => {
    useKranzStore.setState({
      state: missionState({ milestoneId: 'ms-1', command: 'gc audit --deep' }),
    });
    render(<GrantRequestPanel />);

    expect(screen.getByText('gc audit --deep')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Approve grant' }));

    await waitFor(() =>
      expect(api.approveGrant).toHaveBeenCalledWith('m-1', 'gc audit --deep'),
    );
    expect(api.denyGrant).not.toHaveBeenCalled();
  });

  it('denies the parked command', async () => {
    useKranzStore.setState({
      state: missionState({ milestoneId: 'ms-1', command: 'gc audit --deep' }),
    });
    render(<GrantRequestPanel />);

    fireEvent.click(screen.getByRole('button', { name: 'Deny' }));

    await waitFor(() => expect(api.denyGrant).toHaveBeenCalledWith('m-1', 'gc audit --deep'));
    expect(api.approveGrant).not.toHaveBeenCalled();
  });

  it('renders an egress grant distinctly and approves the host:port target', async () => {
    useKranzStore.setState({
      state: missionState({
        milestoneId: 'ms-1',
        kind: 'egress',
        command: 'registry.npmjs.org:443',
      }),
    });
    render(<GrantRequestPanel />);

    expect(screen.getByText('registry.npmjs.org:443')).toBeTruthy();
    expect(screen.getByText(/egress allowlist/)).toBeTruthy();
    expect(screen.queryByText(/outside its allow-set/)).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: 'Approve grant' }));
    await waitFor(() =>
      expect(api.approveGrant).toHaveBeenCalledWith('m-1', 'registry.npmjs.org:443'),
    );
  });

  it('surfaces an error verbatim in a role="alert" block', async () => {
    vi.mocked(api.approveGrant).mockRejectedValueOnce(new ApiError(409, 'no pending grant request'));
    useKranzStore.setState({
      state: missionState({ milestoneId: 'ms-1', command: 'gc audit --deep' }),
    });
    render(<GrantRequestPanel />);

    fireEvent.click(screen.getByRole('button', { name: 'Approve grant' }));

    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toBe('no pending grant request');
  });
});
