import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';
import { PipelineView } from './PipelineView';
import { useKranzStore } from '../lib/store';
import type { MissionSummary, TicketSummary } from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      ...actual.api,
      repos: vi.fn(),
      tickets: vi.fn(),
      missions: vi.fn(),
      queue: vi.fn().mockResolvedValue({ entries: [], busyWith: null, drain: { live: false, currentMissionId: null, ran: [] } }),
      planMd: vi.fn(),
      reportMd: vi.fn(),
      diffStat: vi.fn(),
      createTicket: vi.fn(),
      deleteMission: vi.fn(),
    },
  };
});

import { api } from '../lib/api';

function makeTicket(overrides: Partial<TicketSummary> = {}): TicketSummary {
  return {
    slug: 'fix-a',
    priority: 2,
    state: 'new',
    title: 'Fix the thing',
    blockedBy: [],
    isBlocked: false,
    missionId: null,
    ...overrides,
  };
}

function makeMission(overrides: Partial<MissionSummary> = {}): MissionSummary {
  return {
    id: 'm-1',
    status: 'planning',
    goal: 'Some mission goal',
    createdAt: '2026-01-01T00:00:00Z',
    ...overrides,
  };
}

const INITIAL_STORE_STATE = useKranzStore.getState();

beforeEach(() => {
  cleanup();
  vi.mocked(api.tickets).mockReset();
  vi.mocked(api.repos).mockReset();
  vi.mocked(api.missions).mockReset();
  vi.mocked(api.planMd).mockReset();
  vi.mocked(api.reportMd).mockReset();
  vi.mocked(api.diffStat).mockReset();
  vi.mocked(api.createTicket).mockReset();
  vi.mocked(api.deleteMission).mockReset();
  useKranzStore.setState(
    { ...INITIAL_STORE_STATE, tickets: [], ticketsError: null, missions: [], missionsError: null },
    true,
  );
});

describe('PipelineView', () => {
  it('renders one row per work item, including a ticketless mission', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeTicket({ slug: 'fix-a', title: 'Fix A', state: 'new' }),
      makeTicket({ slug: 'fix-b', title: 'Fix B', state: 'review' }),
    ]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-orphan', goal: 'Ticketless mission work', status: 'running' }),
    ]);

    render(<PipelineView />);

    await screen.findByText('fix-a');
    fireEvent.click(screen.getByText('All'));

    expect(await screen.findByText('fix-a')).toBeTruthy();
    expect(screen.getByText('Fix A')).toBeTruthy();
    expect(screen.getByText('fix-b')).toBeTruthy();
    expect(screen.getByText('Fix B')).toBeTruthy();
    expect(screen.getByText('m-orphan')).toBeTruthy();
    expect(screen.getByText('Ticketless mission work')).toBeTruthy();

    const list = document.querySelector('.picker-list');
    expect(list?.querySelectorAll('.picker-item').length).toBe(3);
  });

  it('renders exactly one primary action per row appropriate to its stage', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeTicket({ slug: 'fix-a', title: 'Fix A', state: 'new' }),
      makeTicket({ slug: 'fix-b', title: 'Fix B', state: 'review' }),
      makeTicket({ slug: 'fix-c', title: 'Fix C', state: 'queued' }),
    ]);
    vi.mocked(api.missions).mockResolvedValueOnce([]);

    render(<PipelineView />);

    await screen.findByText('fix-a');
    fireEvent.click(screen.getByText('All'));

    const rowA = (await screen.findByText('fix-a')).closest('li');
    const rowB = (await screen.findByText('fix-b')).closest('li');
    const rowC = (await screen.findByText('fix-c')).closest('li');

    expect(rowA?.querySelectorAll('.pipeline-primary-action').length).toBe(1);
    expect(rowA?.querySelector('.pipeline-primary-action')?.textContent).toBe('Draft');

    expect(rowB?.querySelectorAll('.pipeline-primary-action').length).toBe(1);

    // queued has no primary action per the nine-stage model.
    expect(rowC?.querySelectorAll('.pipeline-primary-action').length).toBe(0);
  });

  it('reads the Reviewable queueing action as Queue / Queue for run, never Approve', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeTicket({ slug: 'fix-b', title: 'Fix B', state: 'review' }),
    ]);
    vi.mocked(api.missions).mockResolvedValueOnce([]);

    render(<PipelineView />);

    const row = (await screen.findByText('fix-b')).closest('li');
    const action = row?.querySelector('.pipeline-primary-action');
    expect(action?.textContent).toMatch(/^Queue( for run)?$/);
    expect(screen.queryByText('Approve')).toBeNull();
    expect(document.querySelectorAll('*')).not.toContainEqual(
      expect.objectContaining({ textContent: 'Approve' }),
    );
  });

  it('renders a plan-approval link for a ticketless Reviewable mission, and keeps Queue for run for the ticket-backed one', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeTicket({ slug: 'fix-b', title: 'Fix B', state: 'review' }),
    ]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-orphan-plan', goal: 'Ticketless awaiting review', status: 'planning' }),
    ]);
    vi.mocked(api.planMd).mockResolvedValue({ markdown: 'Plan body.' });

    render(<PipelineView />);

    const orphanRow = (await screen.findByText('Ticketless awaiting review')).closest('li');
    const orphanAction = orphanRow?.querySelector('.pipeline-primary-action');
    expect(orphanAction?.tagName).toBe('A');
    expect(orphanAction?.getAttribute('href')).toBe('#/m/m-orphan-plan');
    expect(orphanAction?.textContent).toBe('Approve plan');

    const ticketRow = (await screen.findByText('fix-b')).closest('li');
    const ticketAction = ticketRow?.querySelector('.pipeline-primary-action');
    expect(ticketAction?.tagName).toBe('BUTTON');
    expect(ticketAction?.textContent).toBe('Queue for run');
  });

  it('disables the Reviewable action and shows the blocked badge when isBlocked', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeTicket({
        slug: 'fix-b',
        title: 'Fix B',
        state: 'review',
        blockedBy: ['fix-a'],
        isBlocked: true,
      }),
    ]);
    vi.mocked(api.missions).mockResolvedValueOnce([]);

    render(<PipelineView />);

    const row = (await screen.findByText('fix-b')).closest('li');
    const action = row?.querySelector('.pipeline-primary-action') as HTMLButtonElement;
    expect(action.disabled).toBe(true);
    expect(screen.getByText('blocked by fix-a')).toBeTruthy();
  });

  it('shows the UNMERGED badge for a complete, unmerged mission and hides it once merged', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-unmerged', goal: 'Delivered but unmerged', status: 'complete', merged: false }),
      makeMission({ id: 'm-landed', goal: 'Delivered and landed', status: 'complete', merged: true }),
    ]);
    vi.mocked(api.reportMd).mockResolvedValue({ markdown: 'The report body.' });
    vi.mocked(api.diffStat).mockResolvedValue({ diffStat: '1 file changed', baseSha: 'a', tip: 'b' });

    render(<PipelineView />);

    await screen.findByText('m-unmerged');
    fireEvent.click(screen.getByText('All'));

    const unmergedRow = (await screen.findByText('m-unmerged')).closest('li');
    expect(unmergedRow?.querySelector('.unmerged-badge')?.textContent).toBe('UNMERGED');

    const landedRow = (await screen.findByText('m-landed')).closest('li');
    expect(landedRow?.querySelector('.unmerged-badge')).toBeNull();
  });

  it('renders an abandoned mission row with review and delete, but no work actions', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    vi.mocked(api.missions)
      .mockResolvedValueOnce([
        makeMission({ id: 'm-dead', goal: 'Abandoned mission work', status: 'abandoned' }),
      ])
      .mockResolvedValueOnce([]);
    vi.mocked(api.deleteMission).mockResolvedValueOnce({ deleted: true });

    render(<PipelineView />);

    await screen.findByText('Pipeline');
    fireEvent.click(screen.getByText('All'));

    const row = (await screen.findByText('Abandoned mission work')).closest('li') as HTMLElement;
    const detailLink = row.querySelector('a.picker-id[href="#/m/m-dead"]');
    expect(detailLink?.textContent).toBe('m-dead');
    expect(row.querySelector('.pipeline-delete-action')?.textContent).toBe('Delete');
    expect(row.querySelectorAll('.pipeline-primary-action').length).toBe(0);
    expect(row.querySelectorAll('.pipeline-secondary-action').length).toBe(0);
    expect(row.querySelector('.pill-abandoned')?.textContent).toContain('abandoned');
    expect(row.querySelector('.unmerged-badge')).toBeNull();
    expect(row.textContent).not.toContain('Redraft');
    expect(row.textContent).not.toContain('Merge');
    expect(row.textContent).not.toContain('Iterate');

    fireEvent.click(row.querySelector('.pipeline-delete-action') as HTMLButtonElement);
    expect(row.querySelector('.pipeline-delete-action')?.textContent).toBe('confirm delete');
    expect(api.deleteMission).not.toHaveBeenCalled();

    fireEvent.click(row.querySelector('.pipeline-delete-action') as HTMLButtonElement);
    await waitFor(() => expect(api.deleteMission).toHaveBeenCalledWith('m-dead', false));
    await waitFor(() => expect(screen.queryByText('Abandoned mission work')).toBeNull());
  });

  it('renders a direct-fixed done ticket (no mission) as landed, not delivered', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeTicket({ slug: 'fix-work-branch-isolation', title: 'Fix branch isolation', state: 'done' }),
    ]);
    vi.mocked(api.missions).mockResolvedValueOnce([]);

    render(<PipelineView />);

    await screen.findByText('Pipeline');
    fireEvent.click(screen.getByText('All'));

    const row = (await screen.findByText('fix-work-branch-isolation')).closest('li') as HTMLElement;
    expect(row.querySelector('.pill-landed')?.textContent).toContain('landed');
    expect(row.querySelector('.unmerged-badge')).toBeNull();
    expect(row.textContent).not.toContain('Draft');
    expect(row.textContent).not.toContain('Merge');
  });

  it('still offers Redraft for a genuinely failed mission/ticket', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeTicket({ slug: 'fix-failed', title: 'Failed ticket', state: 'failed' }),
    ]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-failed', goal: 'Failed mission work', status: 'failed' }),
    ]);

    render(<PipelineView />);

    const ticketRow = (await screen.findByText('fix-failed')).closest('li') as HTMLElement;
    expect(ticketRow.querySelector('.pipeline-primary-action')?.textContent).toBe('Redraft');

    const missionRow = (await screen.findByText('Failed mission work')).closest('li') as HTMLElement;
    expect(missionRow.querySelector('.pipeline-primary-action')?.textContent).toBe('Redraft');
  });

  it('fetches plan.md and shows the persisted estimate for a Reviewable row', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-plan', goal: 'Awaiting review', status: 'planning' }),
    ]);
    vi.mocked(api.planMd).mockResolvedValueOnce({
      markdown:
        '# Mission plan — m-plan\n\n## Cost estimate\n\nEstimated **$1.20 – $3.40** (expected ~$2.10). Rough estimate.\n',
    });

    render(<PipelineView />);

    await screen.findByText('m-plan');
    expect(api.planMd).toHaveBeenCalledWith('m-plan');
    const estimate = await waitFor(() => {
      const el = document.querySelector('.pipeline-estimate');
      if (el === null) throw new Error('estimate not rendered yet');
      return el;
    });
    expect(estimate.textContent).toContain('1.20');
    expect(estimate.textContent).toContain('3.40');
    expect(estimate.textContent).toContain('2.10');
  });

  it('fetches report.md and diff-stat and renders them inline for a Delivered row', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-delivered', goal: 'Done, unmerged', status: 'complete', merged: false }),
    ]);
    vi.mocked(api.reportMd).mockResolvedValueOnce({ markdown: 'Shipped the widget.' });
    vi.mocked(api.diffStat).mockResolvedValueOnce({
      diffStat: '2 files changed, 10 insertions(+)',
      baseSha: 'a',
      tip: 'b',
    });

    render(<PipelineView />);

    await screen.findByText('m-delivered');
    await waitFor(() => expect(api.reportMd).toHaveBeenCalledWith('m-delivered'));
    expect(api.diffStat).toHaveBeenCalledWith('m-delivered');
    expect(await screen.findByText('Shipped the widget.')).toBeTruthy();
    expect(screen.getByText('2 files changed, 10 insertions(+)')).toBeTruthy();
  });

  it('creates a follow-up ticket seeded with the report when Iterate is used', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-landed', goal: 'Landed work', status: 'complete', merged: true }),
    ]);
    vi.mocked(api.reportMd).mockResolvedValue({ markdown: 'Report: shipped the thing.' });
    vi.mocked(api.createTicket).mockResolvedValueOnce({
      slug: 'iterate-m-landed-abc123',
      priority: 2,
      state: 'new',
      title: 'Polish the thing',
      blockedBy: [],
      isBlocked: false,
      missionId: null,
    });

    render(<PipelineView />);

    await screen.findByText('Pipeline');
    fireEvent.click(screen.getByText('All'));

    const row = (await screen.findByText('m-landed')).closest('li') as HTMLElement;
    fireEvent.click(row.querySelector('.pipeline-primary-action') as HTMLButtonElement);

    const input = row.querySelector('.pipeline-iterate-input') as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'Polish the thing' } });
    fireEvent.click(row.querySelector('.pipeline-iterate-submit') as HTMLButtonElement);

    await waitFor(() => expect(api.createTicket).toHaveBeenCalledTimes(1));
    const call = vi.mocked(api.createTicket).mock.calls[0][0];
    expect(call.title).toBe('Polish the thing');
    expect(call.goal).toBe('Polish the thing');
    expect(call.context).toContain('Polish the thing');
    expect(call.context).toContain('Report: shipped the thing.');
  });
  it('renders a Delivered row report-fetch failure in a dedicated slot outside the row header, not overlapping the UNMERGED badge', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-broken-report', goal: 'Fetch will fail', status: 'complete', merged: false }),
    ]);
    vi.mocked(api.reportMd).mockRejectedValueOnce(new Error('network error'));
    vi.mocked(api.diffStat).mockResolvedValueOnce({ diffStat: '1 file changed', baseSha: 'a', tip: 'b' });

    render(<PipelineView />);

    const row = (await screen.findByText('m-broken-report')).closest('li') as HTMLElement;
    const errorEl = await waitFor(() => {
      const el = row.querySelector('.pipeline-inline-error');
      if (el === null) throw new Error('error slot not rendered yet');
      return el;
    });

    expect(errorEl.textContent).toContain('Could not load report');
    const pickerRow = row.querySelector('.picker-row') as HTMLElement;
    expect(pickerRow.contains(errorEl)).toBe(false);
    expect(errorEl.querySelector('.unmerged-badge')).toBeNull();
    expect(pickerRow.contains(row.querySelector('.unmerged-badge'))).toBe(true);
    // the error slot and the badge must not sit in the same inline row container
    expect(errorEl.closest('.picker-row')).toBeNull();
  });

  it('renders the full mission id with no characters dropped when the title is absent', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    const longId = 'm-2026-07-06-abcdef1234567890-extra-long-suffix-keeps-going';
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: longId, goal: '', status: 'running' }),
    ]);

    render(<PipelineView />);

    await screen.findByText('Pipeline');
    fireEvent.click(screen.getByText('All'));

    const idEl = await screen.findByText(longId);
    expect(idEl.className).toContain('picker-id');
    expect(idEl.textContent).toBe(longId);
  });

  it('default lens is Actionable', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeTicket({ slug: 'fix-a', title: 'Fix A', state: 'new' }),
    ]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-landed', goal: 'Landed work', status: 'complete', merged: true }),
      makeMission({ id: 'm-dead', goal: 'Abandoned mission work', status: 'abandoned' }),
    ]);
    vi.mocked(api.reportMd).mockResolvedValue({ markdown: 'Report body.' });
    vi.mocked(api.diffStat).mockResolvedValue({ diffStat: '1 file changed', baseSha: 'a', tip: 'b' });

    render(<PipelineView />);

    expect(await screen.findByText('fix-a')).toBeTruthy();
    expect(screen.queryByText('m-landed')).toBeNull();
    expect(screen.queryByText('Abandoned mission work')).toBeNull();
  });

  it('All lens shows every row', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeTicket({ slug: 'fix-a', title: 'Fix A', state: 'new' }),
    ]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-landed', goal: 'Landed work', status: 'complete', merged: true }),
      makeMission({ id: 'm-dead', goal: 'Abandoned mission work', status: 'abandoned' }),
    ]);
    vi.mocked(api.reportMd).mockResolvedValue({ markdown: 'Report body.' });
    vi.mocked(api.diffStat).mockResolvedValue({ diffStat: '1 file changed', baseSha: 'a', tip: 'b' });

    render(<PipelineView />);

    await screen.findByText('fix-a');
    fireEvent.click(screen.getByText('All'));

    expect(await screen.findByText('m-landed')).toBeTruthy();
    expect(screen.getByText('Abandoned mission work')).toBeTruthy();
  });

  it('Missions lens row links to mission detail', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-delivered', goal: 'Done, unmerged', status: 'complete', merged: false }),
    ]);
    vi.mocked(api.reportMd).mockResolvedValueOnce({ markdown: 'Shipped the widget.' });
    vi.mocked(api.diffStat).mockResolvedValueOnce({
      diffStat: '2 files changed, 10 insertions(+)',
      baseSha: 'a',
      tip: 'b',
    });

    render(<PipelineView />);

    await screen.findByText('m-delivered');
    fireEvent.click(screen.getByText('Missions'));

    const row = (await screen.findByText('m-delivered')).closest('li') as HTMLElement;
    const link = row.querySelector('a[href="#/m/m-delivered"]');
    expect(link).toBeTruthy();
  });

  it('does not render a second Backlog nav link beside the Backlog lens', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    vi.mocked(api.missions).mockResolvedValueOnce([]);

    render(<PipelineView />);

    await screen.findByText('Pipeline');
    const backlogLink = document.querySelector('a[href="#/backlog"]');
    expect(backlogLink).toBeNull();
  });

  it('shows a Landed count hint that switches to the All lens', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeTicket({ slug: 'fix-a', title: 'Fix A', state: 'new' }),
    ]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-landed', goal: 'Landed work', status: 'complete', merged: true }),
    ]);
    vi.mocked(api.reportMd).mockResolvedValue({ markdown: 'Report body.' });
    vi.mocked(api.diffStat).mockResolvedValue({ diffStat: '1 file changed', baseSha: 'a', tip: 'b' });

    render(<PipelineView />);

    expect(await screen.findByText('fix-a')).toBeTruthy();
    expect(screen.queryByText('m-landed')).toBeNull();

    const hint = document.querySelector('.lens-landed-hint') as HTMLElement;
    expect(hint).toBeTruthy();
    expect(hint.textContent).toContain('Landed (1)');

    fireEvent.click(hint);

    expect(await screen.findByText('m-landed')).toBeTruthy();
  });

  it('Backlog lens shows only ticket rows', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeTicket({ slug: 'fix-a', title: 'Fix A', state: 'new' }),
    ]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-orphan', goal: 'Ticketless mission work', status: 'running' }),
    ]);

    render(<PipelineView />);

    await screen.findByText('fix-a');
    fireEvent.click(screen.getByText('Backlog'));

    expect(await screen.findByText('fix-a')).toBeTruthy();
    expect(screen.queryByText('Ticketless mission work')).toBeNull();
  });
});

describe('App routes', () => {
  it('renders the project picker at the default hash', async () => {
    window.location.hash = '';
    vi.mocked(api.repos).mockResolvedValueOnce([]);

    const { default: App } = await import('../App');
    render(<App />);

    expect(await screen.findByRole('heading', { name: 'Projects' })).toBeTruthy();
    expect(screen.queryByText('Pipeline')).toBeNull();
  });

  it('routes #/backlog to the pipeline with the Backlog lens selected', async () => {
    window.location.hash = '#/backlog';
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeTicket({ slug: 'fix-a', title: 'Fix A', state: 'new' }),
    ]);
    vi.mocked(api.missions).mockResolvedValueOnce([
      makeMission({ id: 'm-orphan', goal: 'Ticketless mission work', status: 'running' }),
    ]);

    const { default: App } = await import('../App');
    render(<App />);

    expect(await screen.findByText('Pipeline')).toBeTruthy();
    expect(await screen.findByText('fix-a')).toBeTruthy();
    expect(screen.queryByText('Ticketless mission work')).toBeNull();
    const backlogTab = Array.from(document.querySelectorAll('.lens-tab')).find(
      (el) => el.textContent === 'Backlog',
    );
    expect(backlogTab?.getAttribute('aria-pressed')).toBe('true');
  });
});
