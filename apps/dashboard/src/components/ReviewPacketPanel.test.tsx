import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { api } from '../lib/api';
import { useKranzStore } from '../lib/store';
import type { MissionState } from '../lib/types';
import { ReviewPacketPanel } from './ReviewPacketPanel';

vi.mock('../lib/api', () => ({ api: { reviewPacket: vi.fn() } }));
const mock = vi.mocked(api.reviewPacket);
const response = (text = 'Gate attempt attempt-1 — event #7') => ({
  packet: { missionId: 'm-1', throughSeq: 9, observedAt: '2026-09-21T12:00:00Z' }, markdown: text,
});
beforeEach(() => {
  mock.mockReset();
  useKranzStore.setState({ repoId: 'repo-a', missionId: 'm-1', state: { lastSeq: 9 } as MissionState });
});
afterEach(cleanup);

describe('human review packet', () => {
  it('loads explicitly, renders the shared decision identity and exposes no consent action', async () => {
    mock.mockResolvedValue(response('<script>untrusted</script>\n\nGate attempt attempt-1 — event #7'));
    render(<ReviewPacketPanel />);
    expect(mock).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Read review packet' }));
    expect(await screen.findByText('Gate attempt attempt-1 — event #7')).toBeTruthy();
    expect(screen.getByText('<script>untrusted</script>')).toBeTruthy();
    expect(document.querySelector('script')).toBeNull();
    expect(screen.queryByRole('button', { name: /approve|merge|grant/i })).toBeNull();
  });

  it('hides obsolete evidence after new events and can refresh unchanged event sequences', async () => {
    mock.mockResolvedValue(response());
    render(<ReviewPacketPanel />);
    fireEvent.click(screen.getByRole('button', { name: 'Read review packet' }));
    await screen.findByText(/Gate attempt attempt-1/);
    act(() => useKranzStore.setState({ state: { lastSeq: 10 } as MissionState }));
    expect(screen.queryByText(/Gate attempt attempt-1/)).toBeNull();
    expect(screen.getByRole('status').textContent).toContain('Mission events changed');
    mock.mockResolvedValue({ ...response('Historical after dirty edit'), packet: { ...response().packet, throughSeq: 10 } });
    fireEvent.click(screen.getByRole('button', { name: 'Refresh review packet' }));
    await screen.findByText('Historical after dirty edit');
    fireEvent.click(screen.getByRole('button', { name: 'Refresh review packet' }));
    await waitFor(() => expect(mock).toHaveBeenCalledTimes(3));
  });

  it('discards a late response when switching repositories with the same mission id', async () => {
    let finish!: (value: ReturnType<typeof response>) => void;
    mock.mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
    mock.mockResolvedValueOnce(response('Repo B packet'));
    render(<ReviewPacketPanel />);
    fireEvent.click(screen.getByRole('button', { name: 'Read review packet' }));
    act(() => useKranzStore.setState({ repoId: 'repo-b' }));
    fireEvent.click(screen.getByRole('button', { name: 'Read review packet' }));
    await screen.findByText('Repo B packet');
    await act(async () => finish(response('Repo A stale evidence')));
    expect(screen.queryByText('Repo A stale evidence')).toBeNull();
    expect(screen.getByText('Repo B packet')).toBeTruthy();
  });

  it('shows unavailable rather than silently retaining a prior pass after read failure', async () => {
    mock.mockRejectedValue(new Error('evidence read failed'));
    render(<ReviewPacketPanel />);
    fireEvent.click(screen.getByRole('button', { name: 'Read review packet' }));
    expect((await screen.findByRole('alert')).textContent).toContain('Review packet unavailable');
  });
});
