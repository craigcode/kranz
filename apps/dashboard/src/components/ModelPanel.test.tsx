import { beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { ModelPanel } from './ModelPanel';
import { useKranzStore } from '../lib/store';
import type { MissionConfig, MissionState, RoleConfig } from '../lib/types';

const INITIAL_STORE_STATE = useKranzStore.getState();

function role(model: string): RoleConfig {
  return { model, reasoningEffort: 'medium' };
}

function config(): MissionConfig {
  return {
    orchestrator: role('opus'),
    worker: role('sonnet'),
    validatorScrutiny: role('opus'),
    validatorFunctional: role('sonnet'),
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
  };
}

function missionState(): MissionState {
  return {
    mission: {
      id: 'm-1',
      goal: 'test backend UI',
      validationContract: [],
      milestones: [],
      status: 'running',
      createdAt: '2026-07-10T00:00:00Z',
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
    config: config(),
    latestPlanRevision: 0,
    lastSeq: 1,
  };
}

beforeEach(() => {
  cleanup();
  useKranzStore.setState(
    {
      ...INITIAL_STORE_STATE,
      missionId: 'm-1',
      state: missionState(),
    },
    true,
  );
});

describe('ModelPanel', () => {
  it('queues a backend-aware patch and keeps the worker opt-in explicit', async () => {
    const sendControl = vi.fn().mockResolvedValue(undefined);
    useKranzStore.setState({ sendControl });
    render(<ModelPanel />);

    fireEvent.click(screen.getByRole('button', { name: /Worker/ }));
    fireEvent.change(screen.getByLabelText('Worker backend'), { target: { value: 'codex' } });
    fireEvent.change(screen.getByLabelText('Worker model'), {
      target: { value: 'gpt-5-codex' },
    });
    fireEvent.change(screen.getByLabelText('Worker reasoning effort'), {
      target: { value: 'high' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));

    await waitFor(() => {
      expect(sendControl).toHaveBeenCalledWith({
        kind: 'config-change',
        patch: {
          worker: {
            backend: 'codex',
            model: 'gpt-5-codex',
            reasoningEffort: 'high',
          },
        },
      });
    });
    expect((await screen.findByRole('status')).textContent).toContain('queued');
  });

  it('shows a synchronous floor rejection as an alert and leaves the editor open', async () => {
    const sendControl = vi
      .fn()
      .mockRejectedValue(new Error('worker.model is below the default worker tier'));
    useKranzStore.setState({ sendControl });
    render(<ModelPanel />);

    fireEvent.click(screen.getByRole('button', { name: /Worker/ }));
    fireEvent.change(screen.getByLabelText('Worker backend'), { target: { value: 'droid' } });
    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));

    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('below the default worker tier');
    expect(screen.getByLabelText('Worker backend')).toBeTruthy();
  });

  it('does not overwrite backend or model changes received while the editor is open', async () => {
    const sendControl = vi.fn().mockResolvedValue(undefined);
    useKranzStore.setState({ sendControl });
    render(<ModelPanel />);

    fireEvent.click(screen.getByRole('button', { name: /Worker/ }));
    const updated = missionState();
    updated.config.worker = {
      backend: 'droid',
      model: 'fable',
      reasoningEffort: 'medium',
    };
    useKranzStore.setState({ state: updated });
    fireEvent.change(screen.getByLabelText('Worker reasoning effort'), {
      target: { value: 'high' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));

    await waitFor(() => {
      expect(sendControl).toHaveBeenCalledWith({
        kind: 'config-change',
        patch: { worker: { reasoningEffort: 'high' } },
      });
    });
  });

  it('clears editor and notice state when the selected mission changes', async () => {
    const sendControl = vi.fn().mockResolvedValue(undefined);
    useKranzStore.setState({ sendControl });
    render(<ModelPanel />);

    fireEvent.click(screen.getByRole('button', { name: /Worker/ }));
    fireEvent.change(screen.getByLabelText('Worker backend'), { target: { value: 'codex' } });
    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    await screen.findByRole('status');

    useKranzStore.setState({ missionId: 'm-2', state: missionState() });
    await waitFor(() => {
      expect(screen.queryByRole('status')).toBeNull();
      expect(screen.queryByLabelText('Worker backend')).toBeNull();
    });
  });

  it('lists kimi as a selectable worker backend', async () => {
    const sendControl = vi.fn().mockResolvedValue(undefined);
    useKranzStore.setState({ sendControl });
    render(<ModelPanel />);

    fireEvent.click(screen.getByRole('button', { name: /Worker/ }));
    const backendSelect = screen.getByLabelText('Worker backend') as HTMLSelectElement;
    const optionValues = Array.from(backendSelect.options).map((option) => option.value);
    expect(optionValues).toContain('kimi');

    fireEvent.change(backendSelect, { target: { value: 'kimi' } });
    expect(backendSelect.value).toBe('kimi');
  });
});
