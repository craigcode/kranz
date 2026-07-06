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
      draftTicket: vi.fn(),
      approveTicket: vi.fn(),
    },
  };
});

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
    ...overrides,
  };
}

const INITIAL_STORE_STATE = useKranzStore.getState();

beforeEach(() => {
  cleanup();
  vi.mocked(api.ticket).mockReset();
  vi.mocked(api.tickets).mockReset();
  vi.mocked(api.draftTicket).mockReset();
  vi.mocked(api.approveTicket).mockReset();
  vi.mocked(api.tickets).mockResolvedValue([]);
  useKranzStore.setState(
    { ...INITIAL_STORE_STATE, tickets: [], ticketsError: null, ticketError: null, missionId: null },
    true,
  );
});

describe('TicketDetail', () => {
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

  it('dispatches draftTicket when the Draft button is clicked', async () => {
    vi.mocked(api.ticket).mockResolvedValueOnce(makeTicket());
    vi.mocked(api.draftTicket).mockResolvedValueOnce({ missionId: 'm-1' });

    render(<TicketDetail slug="fix-b" />);

    const draft = await screen.findByRole('button', { name: 'Draft' });
    fireEvent.click(draft);

    await screen.findByText(/Draft progress/);
    expect(api.draftTicket).toHaveBeenCalledWith('fix-b');
  });
});
