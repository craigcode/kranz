import { beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { HookStatusPanel } from './HookStatusPanel';
import { useKranzStore } from '../lib/store';
import type { MissionHookStatus } from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      hookStatus: vi.fn(),
    },
  };
});

import { api } from '../lib/api';

const INITIAL_STORE_STATE = useKranzStore.getState();

function projection(runs: MissionHookStatus['runs']): MissionHookStatus {
  return {
    missionId: 'm-1',
    authoritative: false,
    note: 'hook-derived lifecycle signals; observability only, never folded mission state',
    runs,
  };
}

const NEEDS_INPUT_RUN: MissionHookStatus['runs'][number] = {
  runId: 'r-12345678-abcd',
  registeredAt: '2026-08-06T00:00:00Z',
  signal: {
    signal: 'needs-input',
    detail: 'Shell was refused by the session’s permission posture',
    receivedAt: '2026-08-06T00:01:00Z',
  },
};

beforeEach(() => {
  cleanup();
  vi.mocked(api.hookStatus).mockReset().mockResolvedValue(projection([]));
  useKranzStore.setState({ ...INITIAL_STORE_STATE, missionId: 'm-1', state: null }, true);
});

describe('HookStatusPanel (hook_status_signal)', () => {
  it('does not render when no run carries a signal', async () => {
    render(<HookStatusPanel />);
    await waitFor(() => expect(api.hookStatus).toHaveBeenCalledWith('m-1'));
    expect(screen.queryByText(/hook-derived/)).toBeNull();
  });

  it('renders a needs-input signal labelled hook-derived, never mission state', async () => {
    vi.mocked(api.hookStatus).mockResolvedValue(projection([NEEDS_INPUT_RUN]));
    render(<HookStatusPanel />);

    await screen.findByText(/needs input/);
    expect(screen.getByText(/hook-derived, not mission state/)).toBeTruthy();
    expect(
      screen.getByText('Shell was refused by the session’s permission posture'),
    ).toBeTruthy();
    expect(screen.getByText(/r-123456/)).toBeTruthy();
  });

  it('renders registered-but-silent runs as nothing', async () => {
    vi.mocked(api.hookStatus).mockResolvedValue(
      projection([{ runId: 'r-quiet', registeredAt: '2026-08-06T00:00:00Z' }]),
    );
    render(<HookStatusPanel />);

    await waitFor(() => expect(api.hookStatus).toHaveBeenCalled());
    expect(screen.queryByText(/hook-derived/)).toBeNull();
  });

  it('renders nothing without a selected mission', () => {
    useKranzStore.setState({ missionId: null });
    render(<HookStatusPanel />);
    expect(api.hookStatus).not.toHaveBeenCalled();
    expect(screen.queryByText(/hook-derived/)).toBeNull();
  });
});
