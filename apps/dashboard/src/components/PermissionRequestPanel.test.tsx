import { beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { PermissionRequestPanel } from './PermissionRequestPanel';
import { useKranzStore } from '../lib/store';
import type { LivePermission, MissionState } from '../lib/types';

vi.mock('../lib/api', () => ({ api: { answerPermission: vi.fn() } }));
import { api } from '../lib/api';

function pending(): LivePermission {
  return {
    request: {
      proposal: {
        id: 'permission-1', engineSessionId: 's-1', peerSessionId: 'peer-1',
        peerRequestId: 9, toolCallId: 'call-1',
        action: { kind: 'execute', rawInput: { command: 'npm test' } },
        options: [{ optionId: 'one', kind: 'allow_once' }],
        actionDigest: 'action', optionsDigest: 'options',
        observedAt: new Date().toISOString(), deadline: new Date(Date.now() + 60_000).toISOString(),
        prohibition: null,
      },
      binding: { missionId: 'm-1', runId: 'run-1', workspace: '/workspace/feature', planDigest: 'plan', policyDigest: 'policy' },
      bindingDigest: 'exact-binding',
    },
  };
}

function show(record = pending()) {
  useKranzStore.setState({ state: { permissions: { 'permission-1': record } } as unknown as MissionState });
  render(<PermissionRequestPanel />);
}

beforeEach(() => {
  cleanup();
  vi.mocked(api.answerPermission).mockReset().mockResolvedValue(undefined);
});

describe('PermissionRequestPanel', () => {
  it('shows the complete invocation and queues one answer with its exact binding', async () => {
    show();
    expect(screen.getByText(/"command": "npm test"/)).toBeTruthy();
    expect(screen.getByText('/workspace/feature')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Allow once' }));
    await waitFor(() => expect(api.answerPermission).toHaveBeenCalledWith('m-1', 'permission-1', 'exact-binding', true));
    await screen.findByText(/Answer queued/);
    fireEvent.click(screen.getByRole('button', { name: 'Allow once' }));
    fireEvent.click(screen.getByRole('button', { name: 'Deny once' }));
    expect(api.answerPermission).toHaveBeenCalledTimes(1);
  });

  it('keeps policy prohibitions denied and permits a one-call refusal', async () => {
    const record = pending();
    record.request.proposal.prohibition = 'protected path';
    show(record);
    fireEvent.click(screen.getByRole('button', { name: 'Allow once' }));
    expect(api.answerPermission).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Deny once' }));
    await waitFor(() => expect(api.answerPermission).toHaveBeenCalledWith('m-1', 'permission-1', 'exact-binding', false));
  });

  it.each(['expired', 'resolved', 'closed'])('hides a %s request', (kind) => {
    const record = pending();
    if (kind === 'expired') record.request.proposal.deadline = new Date(Date.now() - 1).toISOString();
    if (kind === 'closed') record.closed = 'peer ended';
    if (kind === 'resolved') record.resolution = { requestId: 'permission-1', bindingDigest: 'exact-binding', allow: false, actor: { kind: 'policy' }, reason: 'denied' };
    show(record);
    expect(screen.queryByRole('button', { name: 'Allow once' })).toBeNull();
  });

  it('shows a stale-answer rejection without claiming delivery', async () => {
    vi.mocked(api.answerPermission).mockRejectedValueOnce(new Error('permission expired'));
    show();
    fireEvent.click(screen.getByRole('button', { name: 'Allow once' }));
    expect((await screen.findByRole('alert')).textContent).toBe('permission expired');
    expect(screen.queryByText(/Answer queued/)).toBeNull();
  });
});
