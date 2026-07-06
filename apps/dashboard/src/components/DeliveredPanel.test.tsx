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
  vi.mocked(api.merge).mockReset();
  vi.mocked(api.reportMd).mockResolvedValue({ markdown: '# Report\n\nAll done.' });
  vi.mocked(api.diffStat).mockResolvedValue({
    diffStat: ' 1 file changed, 2 insertions(+)',
    baseSha: 'abc',
    tip: 'def',
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
});
