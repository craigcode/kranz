import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';
import { DeliveredPanel } from './DeliveredPanel';
import type { MissionSummary } from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      missions: vi.fn(),
      reportMd: vi.fn(),
      diffStat: vi.fn(),
      prHandoff: vi.fn(),
      createPr: vi.fn(),
      merge: vi.fn(),
    },
  };
});

import { api, ApiError } from '../lib/api';

function makeSummary(overrides: Partial<MissionSummary> = {}): MissionSummary {
  return {
    id: 'm-1',
    status: 'complete',
    goal: 'ship it',
    createdAt: '2026-01-01T00:00:00Z',
    merged: false,
    ...overrides,
  };
}

beforeEach(() => {
  cleanup();
  vi.mocked(api.missions).mockReset();
  vi.mocked(api.reportMd).mockReset();
  vi.mocked(api.diffStat).mockReset();
  vi.mocked(api.prHandoff).mockReset();
  vi.mocked(api.createPr).mockReset();
  vi.mocked(api.merge).mockReset();
  vi.mocked(api.reportMd).mockResolvedValue({ markdown: '# Report\n\nAll done.' });
  vi.mocked(api.diffStat).mockResolvedValue({
    diffStat: ' 1 file changed, 2 insertions(+)',
    baseSha: 'abc',
    tip: 'def',
  });
  vi.mocked(api.prHandoff).mockResolvedValue({
    kind: 'unavailable',
    reason: 'no git remote named "origin"',
  });
});

afterEach(() => {
  vi.useRealTimers();
});

describe('DeliveredPanel', () => {
  it('renders the Merge button for a Complete, unmerged mission', async () => {
    vi.mocked(api.missions).mockResolvedValueOnce([makeSummary({ merged: false })]);

    render(<DeliveredPanel missionId="m-1" status="complete" />);

    expect(await screen.findByRole('button', { name: 'Merge' })).toBeTruthy();
    expect(screen.getByText('UNMERGED')).toBeTruthy();
  });

  it('does not render for a mission that is not Complete', () => {
    render(<DeliveredPanel missionId="m-1" status="running" />);
    expect(screen.queryByRole('button', { name: 'Merge' })).toBeNull();
  });

  it('does not render for a mission that is already merged', async () => {
    vi.mocked(api.missions).mockResolvedValueOnce([makeSummary({ merged: true })]);

    render(<DeliveredPanel missionId="m-1" status="complete" />);

    await waitFor(() => expect(api.missions).toHaveBeenCalled());
    expect(screen.queryByRole('button', { name: 'Merge' })).toBeNull();
    expect(screen.queryByText('UNMERGED')).toBeNull();
  });

  it('renders the fetched report.md and diff-stat inline', async () => {
    vi.mocked(api.missions).mockResolvedValueOnce([makeSummary({ merged: false })]);

    render(<DeliveredPanel missionId="m-1" status="complete" />);

    expect(await screen.findByText('All done.')).toBeTruthy();
    expect(screen.getByText(/insertions/)).toBeTruthy();
  });

  it('clears the UNMERGED state and disables the button on a successful merge', async () => {
    vi.mocked(api.missions).mockResolvedValueOnce([makeSummary({ merged: false })]);
    vi.mocked(api.merge).mockResolvedValueOnce({ merged: true, commit: 'abc123' });

    render(<DeliveredPanel missionId="m-1" status="complete" />);

    const button = await screen.findByRole('button', { name: 'Merge' });
    fireEvent.click(button);

    expect(api.merge).toHaveBeenCalledWith('m-1');
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Merge' })).toBeNull());
    expect(screen.queryByText('UNMERGED')).toBeNull();
  });

  it('offers to accept a changed base policy only after that refusal, and sends its digest back', async () => {
    vi.mocked(api.missions).mockResolvedValueOnce([makeSummary({ merged: false })]);
    const digest = 'ab'.repeat(32);
    const refusal = `merge refused: .kranz/merge-gates.json changed on the base branch. Retry the merge with acceptBasePolicy "${digest}".`;
    vi.mocked(api.merge)
      .mockRejectedValueOnce(new ApiError(409, refusal, 'base_policy_changed'))
      .mockResolvedValueOnce({ merged: true, commit: 'abc123' });

    render(<DeliveredPanel missionId="m-1" status="complete" />);

    const accept = /Accept base policy change and merge/;
    const merge = await screen.findByRole('button', { name: 'Merge' });
    expect(screen.queryByRole('button', { name: accept })).toBeFalsy();
    fireEvent.click(merge);

    expect((await screen.findByRole('alert')).textContent).toBe(refusal);
    fireEvent.click(await screen.findByRole('button', { name: accept }));

    await waitFor(() => expect(api.merge).toHaveBeenLastCalledWith('m-1', digest));
    expect(api.merge).toHaveBeenNthCalledWith(1, 'm-1');
  });

  it('does not offer the accept step for any other merge refusal', async () => {
    vi.mocked(api.missions).mockResolvedValueOnce([makeSummary({ merged: false })]);
    const digest = 'cd'.repeat(32);
    // Gate output can contain anything, including the marker text.
    vi.mocked(api.merge).mockRejectedValueOnce(
      new ApiError(422, `lint failed: acceptBasePolicy "${digest}"`),
    );

    render(<DeliveredPanel missionId="m-1" status="complete" />);
    fireEvent.click(await screen.findByRole('button', { name: 'Merge' }));

    await screen.findByRole('alert');
    expect(screen.queryByRole('button', { name: /Accept base policy change and merge/ })).toBeFalsy();
  });

  it('renders the ApiError message verbatim in a role="alert" block on a failed merge', async () => {
    vi.mocked(api.missions).mockResolvedValueOnce([makeSummary({ merged: false })]);
    const gateOutput = 'gate "lint" failed:\nline 1 error\nline 2 error';
    vi.mocked(api.merge).mockRejectedValueOnce(new ApiError(409, gateOutput));

    render(<DeliveredPanel missionId="m-1" status="complete" />);

    const button = await screen.findByRole('button', { name: 'Merge' });
    fireEvent.click(button);

    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toBe(gateOutput);
    // still unmerged: button remains present and enabled
    expect(screen.getByRole('button', { name: 'Merge' })).toBeTruthy();
  });

  it('shows PR handoff needsPush copy affordance', async () => {
    vi.mocked(api.missions).mockResolvedValueOnce([makeSummary({ merged: false })]);
    vi.mocked(api.prHandoff).mockResolvedValueOnce({
      kind: 'needsPush',
      command: 'git push origin kranz/mission-m-1',
      remote: 'origin',
      branch: 'kranz/mission-m-1',
    });

    render(<DeliveredPanel missionId="m-1" status="complete" />);

    expect(await screen.findByText(/kranz never pushes/i)).toBeTruthy();
    expect(screen.getByText('git push origin kranz/mission-m-1')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Copy push command' })).toBeTruthy();
  });

  it('shows Create PR when readyToCreate', async () => {
    vi.mocked(api.missions).mockResolvedValueOnce([makeSummary({ merged: false })]);
    vi.mocked(api.prHandoff).mockResolvedValueOnce({
      kind: 'readyToCreate',
      command: 'gh pr create --base main --head kranz/mission-m-1 --title "x" --body "y"',
      title: 'x',
      body: 'y',
      remote: 'origin',
      branch: 'kranz/mission-m-1',
      base: 'main',
    });
    vi.mocked(api.createPr).mockResolvedValueOnce({ url: 'https://github.com/o/r/pull/1' });

    render(<DeliveredPanel missionId="m-1" status="complete" />);

    const create = await screen.findByRole('button', { name: 'Create PR' });
    fireEvent.click(create);
    expect(api.createPr).toHaveBeenCalledWith('m-1');
    expect(await screen.findByText('https://github.com/o/r/pull/1')).toBeTruthy();
  });

  it('guards against rejected missions(), unsafe non-http(s) PR URLs, and stale async completions across mission switches (KRZ-M13)', async () => {
    // 1) Rejected api.missions() does not cause unhandled rejection
    vi.mocked(api.missions).mockRejectedValueOnce(new Error('offline'));
    const { unmount: unmountOffline } = render(<DeliveredPanel missionId="m-1" status="complete" />);
    await waitFor(() => expect(api.missions).toHaveBeenCalledTimes(1));
    unmountOffline();

    // 2) Non-http(s) PR URL renders as inert <span> instead of <a href="javascript:...">
    vi.mocked(api.missions).mockResolvedValueOnce([makeSummary({ merged: false })]);
    vi.mocked(api.prHandoff).mockResolvedValueOnce({
      kind: 'readyToCreate',
      command: 'gh pr create',
      title: 'x',
      body: 'y',
      remote: 'origin',
      branch: 'kranz/mission-m-1',
      base: 'main',
    });
    vi.mocked(api.createPr).mockResolvedValueOnce({ url: 'javascript:alert(1)' });

    const { container, unmount: unmountUnsafe } = render(
      <DeliveredPanel missionId="m-1" status="complete" />,
    );
    fireEvent.click(await screen.findByRole('button', { name: 'Create PR' }));
    expect(await screen.findByText('javascript:alert(1)')).toBeTruthy();
    expect(container.querySelector('.delivered-pr-url a')).toBeNull();
    expect(container.querySelector('.delivered-pr-url span')?.textContent).toBe(
      'javascript:alert(1)',
    );
    unmountUnsafe();

    // 3) Stale merge completion from m-1 must not mark m-2 as merged after switching missionId
    let resolveMergeM1!: (value: { merged: boolean; commit: string }) => void;
    vi.mocked(api.merge).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveMergeM1 = resolve;
        }),
    );
    vi.mocked(api.missions).mockResolvedValue([
      makeSummary({ id: 'm-1', merged: false }),
      makeSummary({ id: 'm-2', merged: false }),
    ]);

    const { rerender } = render(<DeliveredPanel missionId="m-1" status="complete" />);
    fireEvent.click(await screen.findByRole('button', { name: 'Merge' }));
    expect(screen.getByRole('button', { name: 'Merging…' })).toBeTruthy();

    // Switch to m-2 before m-1's merge resolves
    rerender(<DeliveredPanel missionId="m-2" status="complete" />);
    expect(await screen.findByRole('button', { name: 'Merge' })).toBeTruthy();

    resolveMergeM1({ merged: true, commit: 'deadbeef' });
    await new Promise((r) => setTimeout(r, 20));

    // m-2 remains unmerged with its Merge button intact!
    expect(screen.getByRole('button', { name: 'Merge' })).toBeTruthy();
    expect(screen.getByText('UNMERGED')).toBeTruthy();
  });
});
