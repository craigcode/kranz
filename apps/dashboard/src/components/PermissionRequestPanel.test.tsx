import { beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { PermissionRequestPanel } from './PermissionRequestPanel';
import { useKranzStore } from '../lib/store';
import type { LivePermission, MissionState } from '../lib/types';

vi.mock('../lib/api', () => ({ api: { answerPermission: vi.fn() } }));
import { api } from '../lib/api';

vi.mock('../lib/useNow', () => ({ useNow: vi.fn(() => Date.now()) }));
import { useNow } from '../lib/useNow';

function pending(id = 'permission-1', deadlineMs = 60_000): LivePermission {
  return {
    request: {
      proposal: {
        id, engineSessionId: 's-1', peerSessionId: 'peer-1',
        peerRequestId: 9, toolCallId: 'call-1',
        action: { kind: 'execute', rawInput: { command: 'npm test' } },
        options: [{ optionId: 'one', kind: 'allow_once' }],
        actionDigest: 'action', optionsDigest: 'options',
        observedAt: new Date().toISOString(), deadline: new Date(Date.now() + deadlineMs).toISOString(),
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
  vi.mocked(useNow).mockReset().mockImplementation(() => Date.now());
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

  it.each(['\u202e', '\u200b', '\u061c', '\r', '\u001b'])('makes unsafe approval text visible and refuses allow (%j)', async (control) => {
    const record = pending();
    record.request.proposal.action = { rawInput: { command: `npm ${control}test` } };
    record.request.binding.workspace += control;
    show(record);
    expect(screen.getByText(/invisible or control characters/)).toBeTruthy();
    expect(document.body.textContent).not.toContain(control);
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

  it('DD4 keeps an expired card mounted in place with disabled buttons and an Expired status, leaving its sibling untouched', () => {
    const first = pending('permission-1', 1000);
    const second = pending('permission-2', 60_000);
    useKranzStore.setState({
      state: { permissions: { 'permission-1': first, 'permission-2': second } } as unknown as MissionState,
    });
    const startedAt = Date.now();
    vi.mocked(useNow).mockReturnValue(startedAt);

    const { rerender, container } = render(<PermissionRequestPanel />);
    const sections = () => Array.from(container.querySelectorAll('section.panel-grant'));

    expect(sections()).toHaveLength(2);
    const [firstSection, secondSection] = sections();
    const firstStatus = firstSection.querySelector('.permission-status') as HTMLElement;
    const firstAllow = within(firstSection as HTMLElement).getByRole('button', { name: 'Allow once' }) as HTMLButtonElement;
    const firstDeny = within(firstSection as HTMLElement).getByRole('button', { name: 'Deny once' }) as HTMLButtonElement;
    const secondAllow = within(secondSection as HTMLElement).getByRole('button', { name: 'Allow once' }) as HTMLButtonElement;
    const secondDeny = within(secondSection as HTMLElement).getByRole('button', { name: 'Deny once' }) as HTMLButtonElement;

    expect(firstStatus.textContent).toBe(' ');
    expect(firstAllow.disabled).toBe(false);
    expect(secondAllow.disabled).toBe(false);

    vi.mocked(useNow).mockReturnValue(Date.parse(first.request.proposal.deadline) + 1000);
    rerender(<PermissionRequestPanel />);

    const sectionsAfter = sections();
    expect(sectionsAfter).toHaveLength(2);
    expect(sectionsAfter[0]).toBe(firstSection);
    expect(sectionsAfter[1]).toBe(secondSection);

    const statusAfter = firstSection.querySelector('.permission-status') as HTMLElement;
    expect(statusAfter).toBe(firstStatus);
    expect(statusAfter.textContent).toContain('Expired');

    const firstAllowAfter = within(firstSection as HTMLElement).getByRole('button', { name: 'Allow once' }) as HTMLButtonElement;
    const firstDenyAfter = within(firstSection as HTMLElement).getByRole('button', { name: 'Deny once' }) as HTMLButtonElement;
    expect(firstAllowAfter).toBe(firstAllow);
    expect(firstDenyAfter).toBe(firstDeny);
    expect(firstAllowAfter.disabled).toBe(true);
    expect(firstDenyAfter.disabled).toBe(true);

    const secondAllowAfter = within(secondSection as HTMLElement).getByRole('button', { name: 'Allow once' }) as HTMLButtonElement;
    const secondDenyAfter = within(secondSection as HTMLElement).getByRole('button', { name: 'Deny once' }) as HTMLButtonElement;
    expect(secondAllowAfter).toBe(secondAllow);
    expect(secondDenyAfter).toBe(secondDeny);
    expect(secondAllowAfter.disabled).toBe(false);
    expect(secondDenyAfter.disabled).toBe(false);
  });

  it('DD4 sends no answer when Allow is clicked on an already-expired card', () => {
    const first = pending('permission-1', 1000);
    useKranzStore.setState({ state: { permissions: { 'permission-1': first } } as unknown as MissionState });
    vi.mocked(useNow).mockReturnValue(Date.now());

    const { rerender, container } = render(<PermissionRequestPanel />);
    vi.mocked(useNow).mockReturnValue(Date.parse(first.request.proposal.deadline) + 1000);
    rerender(<PermissionRequestPanel />);

    const section = container.querySelector('section.panel-grant') as HTMLElement;
    const allow = within(section).getByRole('button', { name: 'Allow once' }) as HTMLButtonElement;
    fireEvent.click(allow);
    expect(api.answerPermission).not.toHaveBeenCalled();
  });

  it('DD4 does not mark an answered card as Expired once its resolution arrives before its deadline', async () => {
    const record = pending('permission-1', 60_000);
    useKranzStore.setState({ state: { permissions: { 'permission-1': record } } as unknown as MissionState });
    vi.mocked(useNow).mockReturnValue(Date.now());

    const { rerender, container } = render(<PermissionRequestPanel />);
    fireEvent.click(screen.getByRole('button', { name: 'Allow once' }));
    await waitFor(() => expect(api.answerPermission).toHaveBeenCalledTimes(1));
    await screen.findByText(/Answer queued/);

    useKranzStore.setState({
      state: {
        permissions: {
          'permission-1': {
            ...record,
            resolution: { requestId: 'permission-1', bindingDigest: 'exact-binding', allow: true, actor: { kind: 'operator' }, reason: 'allowed' },
          },
        },
      } as unknown as MissionState,
    });
    rerender(<PermissionRequestPanel />);

    expect(container.textContent).not.toContain('Expired');
    expect(container.querySelector('section.panel-grant')).toBeNull();
  });
});
