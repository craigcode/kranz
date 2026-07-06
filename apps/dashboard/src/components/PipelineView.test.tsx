import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { PipelineView } from './PipelineView';
import { useKranzStore } from '../lib/store';
import type { MissionSummary, TicketSummary } from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      ...actual.api,
      tickets: vi.fn(),
      missions: vi.fn(),
      queue: vi.fn().mockResolvedValue({ entries: [], busyWith: null, drain: { live: false, currentMissionId: null, ran: [] } }),
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
  vi.mocked(api.missions).mockReset();
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
});

describe('App default route', () => {
  it('renders the pipeline view (not the old MissionPicker landing) at the default hash', async () => {
    window.location.hash = '';
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    vi.mocked(api.missions).mockResolvedValueOnce([]);

    const { default: App } = await import('../App');
    render(<App />);

    expect(await screen.findByText('Pipeline')).toBeTruthy();
    expect(screen.queryByText('Missions')).toBeNull();
    expect(screen.queryByText('+ new mission')).toBeTruthy();
  });
});
