import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useKranzStore } from '../lib/store';
import type { MissionState, WorkspaceSummary } from '../lib/types';
import { WorkspacePanel } from './WorkspacePanel';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      workspace: vi.fn(),
    },
  };
});

import { api } from '../lib/api';

function state(): MissionState {
  const role = { model: 'sonnet', reasoningEffort: 'medium' };
  return {
    mission: {
      id: 'm-workspace',
      goal: 'show workspace state',
      validationContract: [],
      milestones: [],
      status: 'running',
      createdAt: '2026-07-15T00:00:00Z',
      baseBranch: 'main',
      missionBranch: 'kranz/mission-m-workspace',
    },
    runs: {},
    totals: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    totalCostUsd: 0,
    localExecutorMilestones: 0,
    escalatedMilestones: 0,
    pendingUserMessages: [],
    recentDecisions: [],
    config: {
      orchestrator: role,
      worker: role,
      validatorScrutiny: role,
      validatorFunctional: role,
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
      workerIsolation: 'worktree',
    },
    latestPlanRevision: 0,
    lastSeq: 1,
  };
}

function summary(overrides: Partial<WorkspaceSummary> = {}): WorkspaceSummary {
  return {
    isolation: 'worktree',
    cwd: '/tmp/kranz-wt-m-workspace-_integration',
    lifecycle: 'active',
    worktreeActive: true,
    sandboxes: [
      { role: 'worker', enforce: 'fs', extraWriteCount: 1, egressCount: 0 },
      { role: 'scrutiny', enforce: 'off', extraWriteCount: 0, egressCount: 0 },
      { role: 'functional', enforce: 'off', extraWriteCount: 0, egressCount: 0 },
    ],
    preflight: {
      status: 'issues',
      summary: 'preflight: 1 issue(s): [warn] cargo missing',
      eventSeq: 2,
    },
    contract: { present: false, services: 0, previews: 0 },
    ...overrides,
  };
}

beforeEach(() => {
  cleanup();
  vi.mocked(api.workspace).mockReset();
  useKranzStore.setState({
    missionId: 'm-workspace',
    state: state(),
    events: [],
  });
});

describe('WorkspacePanel', () => {
  it('renders the effective worktree, sandbox tiers, and recorded preflight outcome', async () => {
    vi.mocked(api.workspace).mockResolvedValueOnce(summary());

    render(<WorkspacePanel />);

    expect(await screen.findByText('worktree · active')).toBeTruthy();
    expect(screen.getByText('/tmp/kranz-wt-m-workspace-_integration')).toBeTruthy();
    expect(screen.getByText('worker fs · +1 write')).toBeTruthy();
    expect(screen.getByText(/cargo missing/)).toBeTruthy();
  });

  it('labels checkout mode as the primary checkout rather than a missing worktree', async () => {
    vi.mocked(api.workspace).mockResolvedValueOnce(
      summary({
        isolation: 'checkout',
        cwd: '/repo',
        lifecycle: 'primary-checkout',
        worktreeActive: false,
      }),
    );

    render(<WorkspacePanel />);

    expect(await screen.findByText('checkout · primary checkout')).toBeTruthy();
    expect(screen.queryByText(/missing worktree/i)).toBeNull();
  });

  it('says when only source isolation is active versus a contract being present', async () => {
    vi.mocked(api.workspace).mockResolvedValueOnce(summary());
    const { unmount } = render(<WorkspacePanel />);
    expect(
      await screen.findByText('no workspace contract — source isolation only'),
    ).toBeTruthy();
    unmount();

    vi.mocked(api.workspace).mockResolvedValueOnce(
      summary({ contract: { present: true, services: 2, previews: 1 } }),
    );
    render(<WorkspacePanel />);
    expect(await screen.findByText('contract present (2 services, 1 previews)')).toBeTruthy();
  });

  it('refetches when sandbox grant counts change without an enforcement change', async () => {
    vi.mocked(api.workspace)
      .mockResolvedValueOnce(summary())
      .mockResolvedValueOnce(
        summary({
          sandboxes: [
            { role: 'worker', enforce: 'fs', extraWriteCount: 2, egressCount: 1 },
            { role: 'scrutiny', enforce: 'off', extraWriteCount: 0, egressCount: 0 },
            { role: 'functional', enforce: 'off', extraWriteCount: 0, egressCount: 0 },
          ],
        }),
      );
    const initial = state();
    initial.config.worker = {
      ...initial.config.worker,
      sandbox: { enforce: 'fs', extraWrite: ['/tmp/a'], egress: [] },
    };
    useKranzStore.setState({ state: initial });
    render(<WorkspacePanel />);
    expect(await screen.findByText('worker fs · +1 write')).toBeTruthy();

    const updated = state();
    updated.config.worker = {
      ...updated.config.worker,
      sandbox: {
        enforce: 'fs',
        extraWrite: ['/tmp/a', '/tmp/b'],
        egress: ['api.example.test'],
      },
    };
    useKranzStore.setState({ state: updated });

    await waitFor(() => expect(api.workspace).toHaveBeenCalledTimes(2));
    expect(await screen.findByText('worker fs · +2 write · +1 egress')).toBeTruthy();
  });
});
