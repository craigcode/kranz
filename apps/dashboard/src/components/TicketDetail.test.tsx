import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import { TicketDetail } from './TicketDetail';
import { useKranzStore } from '../lib/store';
import type { Ticket, TicketSummary } from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      ticket: vi.fn(),
      tickets: vi.fn(),
      missions: vi.fn(),
      events: vi.fn(),
      draftTicket: vi.fn(),
      approveTicket: vi.fn(),
    },
  };
});

vi.mock('../lib/ws', () => ({
  MissionSocket: class {
    connect() {}
    close() {}
  },
}));

import { api } from '../lib/api';

function makeTicket(overrides: Partial<Ticket> = {}): Ticket {
  return {
    slug: 'fix-b',
    title: 'Fix the second thing',
    priority: 2,
    schedule: 'once',
    blockedBy: [],
    isBlocked: false,
    goal: 'Ship the fix.',
    context: 'Some context.',
    scopingAnswers: [],
    acceptanceHints: [],
    state: 'review',
    needsContext: [],
    wrongPlan: null,
    // Real wire shape: the server always emits the key, null when undrafted.
    missionId: null,
    ...overrides,
  };
}

function makeSummary(overrides: Partial<TicketSummary> = {}): TicketSummary {
  return {
    slug: 'dep-a',
    priority: 1,
    state: 'new',
    title: 'Dependency',
    blockedBy: [],
    isBlocked: false,
    missionId: null,
    ...overrides,
  };
}

const INITIAL_STORE_STATE = useKranzStore.getState();

beforeEach(() => {
  cleanup();
  vi.mocked(api.ticket).mockReset();
  vi.mocked(api.tickets).mockReset();
  vi.mocked(api.missions).mockReset();
  vi.mocked(api.events).mockReset();
  vi.mocked(api.draftTicket).mockReset();
  vi.mocked(api.approveTicket).mockReset();
  vi.mocked(api.tickets).mockResolvedValue([]);
  vi.mocked(api.missions).mockResolvedValue([]);
  vi.mocked(api.events).mockResolvedValue([]);
  useKranzStore.setState(
    {
      ...INITIAL_STORE_STATE,
      tickets: [],
      ticketsError: null,
      ticketError: null,
      ticketBusySlug: null,
      draftingSlug: null,
      missionId: null,
    },
    true,
  );
});

describe('TicketDetail', () => {
  it('shows the immutable input and required review output', async () => {
    vi.mocked(api.ticket).mockResolvedValueOnce(
      makeTicket({
        taskClass: 'spec-review',
        reviewArtifact: 'docs/api.md',
        reviewOutput: 'reviews/api.md',
      }),
    );

    render(<TicketDetail slug="fix-b" />);

    expect(await screen.findByText('Review artifact contract')).toBeTruthy();
    expect(screen.getByText('docs/api.md')).toBeTruthy();
    expect(screen.getByText('reviews/api.md')).toBeTruthy();
  });

  it('disables Queue for run and names the blocker for a blocked review ticket', async () => {
    vi.mocked(api.ticket).mockResolvedValueOnce(
      makeTicket({ blockedBy: ['dep-a'], isBlocked: true }),
    );
    vi.mocked(api.tickets).mockResolvedValue([makeSummary({ slug: 'dep-a', state: 'new' })]);

    render(<TicketDetail slug="fix-b" />);

    const queue = await screen.findByRole('button', { name: 'Queue for run' });
    expect(queue.hasAttribute('disabled')).toBe(true);
    expect(queue.getAttribute('title')).toContain('dep-a');
    expect(screen.queryByRole('button', { name: /Approve/ })).toBeFalsy();
  });

  it('enables Queue for run when the blocker is done', async () => {
    vi.mocked(api.ticket).mockResolvedValueOnce(
      makeTicket({ blockedBy: ['dep-a'], isBlocked: false }),
    );
    vi.mocked(api.tickets).mockResolvedValue([makeSummary({ slug: 'dep-a', state: 'done' })]);

    render(<TicketDetail slug="fix-b" />);

    const queue = await screen.findByRole('button', { name: 'Queue for run' });
    expect(queue.hasAttribute('disabled')).toBe(false);
    expect(queue.getAttribute('title')).toBe('queue for run');
  });

  it('enables Queue for run when isBlocked is false even if blockedBy is non-empty', async () => {
    vi.mocked(api.ticket).mockResolvedValueOnce(
      makeTicket({ blockedBy: ['dep-a'], isBlocked: false }),
    );
    vi.mocked(api.tickets).mockResolvedValue([makeSummary({ slug: 'dep-a', state: 'new' })]);

    render(<TicketDetail slug="fix-b" />);

    const queue = await screen.findByRole('button', { name: 'Queue for run' });
    expect(queue.hasAttribute('disabled')).toBe(false);
  });

  it('renders goal/context via markdown and lists needs-context questions', async () => {
    vi.mocked(api.ticket).mockResolvedValueOnce(
      makeTicket({ state: 'needs-context', needsContext: ['What auth scheme?'] }),
    );

    render(<TicketDetail slug="fix-b" />);

    expect(await screen.findByText('Ship the fix.')).toBeTruthy();
    expect(screen.getByText('Some context.')).toBeTruthy();
    expect(screen.getByText('What auth scheme?')).toBeTruthy();
    // needs-context tickets are not yet in review — no Approve button.
    expect(screen.queryByRole('button', { name: /Approve/ })).toBeFalsy();
  });

  it('renders the wrong-plan escalation reason and no Queue button', async () => {
    vi.mocked(api.ticket).mockResolvedValueOnce(
      makeTicket({
        state: 'wrong-plan',
        wrongPlan: 'The store is SQLite — any plan on the Postgres premise is wrong.',
      }),
    );

    render(<TicketDetail slug="fix-b" />);

    expect(
      await screen.findByText('The store is SQLite — any plan on the Postgres premise is wrong.'),
    ).toBeTruthy();
    expect(screen.getByText('wrong-plan')).toBeTruthy();
    // wrong-plan tickets are parked, not reviewable — no Queue button.
    expect(screen.queryByRole('button', { name: 'Queue for run' })).toBeFalsy();
    // …but re-drafting stays available, exactly like needs-context.
    expect(screen.getByRole('button', { name: 'Draft' })).toBeTruthy();
  });

  it('shows draft progress after Draft is clicked on an undrafted ticket (missionId null)', async () => {
    // The server serializes undrafted tickets as missionId:null; the local
    // ticket record keeps that until re-fetched, so the feed must appear via
    // the store's draftingSlug, not the ticket's own missionId.
    vi.mocked(api.ticket).mockResolvedValueOnce(makeTicket({ missionId: null }));
    vi.mocked(api.draftTicket).mockResolvedValueOnce({ missionId: 'm-1' });
    // The refresh must list the freshly drafted mission — loadMissions treats
    // a connected mission missing from a fresh list as deleted out-of-band.
    vi.mocked(api.missions).mockResolvedValue([
      { id: 'm-1', status: 'planning', goal: 'Ship the fix.', createdAt: '2026-01-01T00:00:00Z' },
    ]);

    render(<TicketDetail slug="fix-b" />);

    const draft = await screen.findByRole('button', { name: 'Draft' });
    fireEvent.click(draft);

    await screen.findByText(/Draft progress/);
    expect(api.draftTicket).toHaveBeenCalledWith('fix-b');
    expect(useKranzStore.getState().draftingSlug).toBe('fix-b');
  });

  it('hides draft progress on an undrafted ticket when the live feed is an unrelated mission', async () => {
    // Leak regression: a previously viewed mission's feed must not surface
    // on a ticket page it does not belong to.
    vi.mocked(api.ticket).mockResolvedValueOnce(makeTicket({ missionId: null }));
    useKranzStore.setState({ missionId: 'm-other', draftingSlug: null });

    render(<TicketDetail slug="fix-b" />);

    await screen.findByText('Fix the second thing');
    expect(screen.queryByText(/Draft progress/)).toBeFalsy();
  });

  it('hides draft progress when the current draft belongs to a different ticket', async () => {
    vi.mocked(api.ticket).mockResolvedValueOnce(makeTicket({ missionId: null }));
    useKranzStore.setState({ missionId: 'm-other', draftingSlug: 'other-ticket' });

    render(<TicketDetail slug="fix-b" />);

    await screen.findByText('Fix the second thing');
    expect(screen.queryByText(/Draft progress/)).toBeFalsy();
  });

  it("shows draft progress when the ticket's missionId matches the connected mission", async () => {
    vi.mocked(api.ticket).mockResolvedValueOnce(makeTicket({ missionId: 'm-9' }));
    useKranzStore.setState({ missionId: 'm-9', draftingSlug: null });

    render(<TicketDetail slug="fix-b" />);

    await screen.findByText(/Draft progress/);
  });
});
