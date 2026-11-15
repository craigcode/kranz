import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { GateReviewPanel } from './GateReviewPanel';
import { RevisionPanel } from './RevisionPanel';
import { api } from '../lib/api';
import { useKranzStore } from '../lib/store';
import type { GateEvaluation, MissionState } from '../lib/types';

afterEach(cleanup);
const pending = (): GateEvaluation => ({
  requested: { request: { params: { attemptId: 'attempt-1', gateId: 'reviewer', stage: 'final-gate',
    deadline: '2000-01-01T00:00:00Z', subject: { kind: 'deliverable' }, binding: {} } } },
  requestedSeq: 7,
  finished: { outcome: { status: 'evaluated', result: { rationale: '<script>untrusted finding</script>' } } },
  resolution: { disposition: 'require-human', rationale: 'review needed' },
});
function show(records?: Record<string, GateEvaluation>) {
  useKranzStore.setState({ state: { gateEvaluations: records } as MissionState });
  return render(<GateReviewPanel />);
}
describe('gate review obligations', () => {
  it('shows escaped evidence and requires a fresh attempt after expiry', () => {
    show({ a: pending() });
    expect(screen.getByText('<script>untrusted finding</script>')).toBeTruthy();
    expect(screen.getByRole('status').textContent).toContain('expired');
    expect(document.querySelector('script')).toBeNull();
    expect(screen.queryByRole('button', { name: /approve/i })).toBeNull();
  });
  it('omits closed, consumed and proceeding attempts', () => {
    show({ closed: { ...pending(), closed: 'superseded' }, consumed: { ...pending(), consumed: {} },
      proceeded: { ...pending(), resolution: { disposition: 'proceed', rationale: 'done' } } });
    expect(screen.queryByText('Gate review needed')).toBeNull();
  });
  it('preserves the empty view for legacy logs', () => {
    const { container } = show();
    expect(container.textContent).toBe('');
  });
  it('escapes BiDi and invisible control characters in gate review and revision surfaces (KRZ-M11)', async () => {
    const base = pending();
    show({
      bidi: {
        ...base,
        requested: {
          request: {
            params: {
              ...base.requested.request.params,
              gateId: 'gate-\u202echek',
              stage: 'stage-\u200b1',
              binding: { ref: 'refs/heads/\u202emain' },
            },
          },
        },
        finished: {
          outcome: {
            status: 'evaluated',
            result: { rationale: 'spoofed \u202erationale' },
          },
        },
      },
    });
    expect(screen.getByText('gate-\\u202echek')).toBeTruthy();
    expect(screen.getByText(/stage-\\u200b1/)).toBeTruthy();
    expect(screen.getByText('spoofed \\u202erationale')).toBeTruthy();
    expect(screen.getByText(/refs\/heads\/\\u202emain/)).toBeTruthy();
    cleanup();

    const diffSpy = vi.spyOn(api, 'revisionDiff').mockResolvedValueOnce({
      revision: 2,
      instructions: 'rev',
      markdown: '# Plan',
      diff: '+allow \u202ehidden',
    });
    try {
      useKranzStore.setState({
        state: {
          mission: {
            id: 'm-rev',
            goal: 'goal',
            validationContract: [],
            milestones: [],
            status: 'running',
            createdAt: '2026-01-01T00:00:00Z',
            baseBranch: 'main',
            missionBranch: 'kranz/mission-m-rev',
          },
          pendingRevision: {
            revision: 2,
            instructions: 'Narrow \u200bscope',
            plan: { goal: 'goal', validationContract: [], milestones: [] },
          },
        } as unknown as MissionState,
      });
      render(<RevisionPanel />);
      expect(screen.getByText('Narrow \\u200bscope')).toBeTruthy();
      await waitFor(() => expect(screen.getByText('+allow \\u202ehidden')).toBeTruthy());
    } finally {
      diffSpy.mockRestore();
    }
  });

  it('holds Approve until the revision diff is shown, and keeps it off when the diff fails (UI-5)', async () => {
    const pendingState = {
      mission: {
        id: 'm-rev', goal: 'goal', validationContract: [], milestones: [], status: 'running',
        createdAt: '2026-01-01T00:00:00Z', baseBranch: 'main', missionBranch: 'kranz/mission-m-rev',
      },
      pendingRevision: {
        revision: 2, instructions: 'Narrow scope',
        plan: { goal: 'goal', validationContract: [], milestones: [] },
      },
    } as unknown as MissionState;
    const approve = () => screen.getByRole('button', { name: 'Approve' }) as HTMLButtonElement;
    const reject = () => screen.getByRole('button', { name: 'Reject' }) as HTMLButtonElement;

    let release: (value: { revision: number; instructions: string; markdown: string; diff: string }) => void = () => {};
    const slow = vi.spyOn(api, 'revisionDiff').mockReturnValueOnce(
      new Promise((resolve) => { release = resolve; }),
    );
    try {
      useKranzStore.setState({ state: pendingState });
      render(<RevisionPanel />);
      expect(approve().disabled).toBe(true);
      expect(reject().disabled).toBe(false);
      release({ revision: 2, instructions: 'rev', markdown: '# Plan', diff: '+one line' });
      await waitFor(() => expect(approve().disabled).toBe(false));
      const text = document.body.textContent ?? '';
      expect(text.indexOf('+one line')).toBeLessThan(text.indexOf('Approve'));
    } finally {
      slow.mockRestore();
      cleanup();
    }

    const failing = vi.spyOn(api, 'revisionDiff').mockRejectedValueOnce(new Error('boom'));
    try {
      useKranzStore.setState({ state: { ...pendingState, pendingRevision: { ...pendingState.pendingRevision!, revision: 3 } } as MissionState });
      render(<RevisionPanel />);
      await waitFor(() => expect(screen.getByRole('status').textContent).toContain('could not be loaded'));
      expect(approve().disabled).toBe(true);
      expect(reject().disabled).toBe(false);
    } finally {
      failing.mockRestore();
    }
  });

  it('clears the revision instructions draft when switching missions and never sends mission A\'s text to B (DD2)', async () => {
    const draftState = (id: string) =>
      ({
        mission: {
          id,
          goal: 'goal',
          validationContract: [],
          milestones: [],
          status: 'running',
          createdAt: '2026-01-01T00:00:00Z',
          baseBranch: 'main',
          missionBranch: `kranz/mission-${id}`,
        },
      }) as unknown as MissionState;

    useKranzStore.setState({ missionId: 'm-a', state: draftState('m-a') });
    const { rerender } = render(<RevisionPanel />);

    fireEvent.change(screen.getByRole('textbox'), {
      target: { value: 'instructions drafted for mission A' },
    });
    expect((screen.getByRole('textbox') as HTMLTextAreaElement).value).toBe(
      'instructions drafted for mission A',
    );

    useKranzStore.setState({ missionId: 'm-b', state: draftState('m-b') });
    rerender(<RevisionPanel />);

    const textarea = screen.getByRole('textbox') as HTMLTextAreaElement;
    expect(textarea.value).toBe('');
    expect(
      (screen.getByRole('button', { name: 'Request revision' }) as HTMLButtonElement).disabled,
    ).toBe(true);

    const requestSpy = vi.spyOn(api, 'requestRevision').mockResolvedValue(undefined as never);
    try {
      fireEvent.click(screen.getByRole('button', { name: 'Request revision' }));
      expect(requestSpy).not.toHaveBeenCalled();
    } finally {
      requestSpy.mockRestore();
    }
  });
});
