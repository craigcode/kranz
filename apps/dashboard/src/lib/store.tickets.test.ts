import { describe, it, expect, vi, beforeEach } from 'vitest';
import { useKranzStore } from './store';
import { ApiError } from './api';
import type { TicketSummary } from './types';

vi.mock('./api', async () => {
  const actual = await vi.importActual<typeof import('./api')>('./api');
  return {
    ...actual,
    api: {
      tickets: vi.fn(),
      ticket: vi.fn(),
      draftTicket: vi.fn(),
      approveTicket: vi.fn(),
    },
  };
});

import { api } from './api';

function makeSummary(slug: string): TicketSummary {
  return {
    slug,
    priority: 1,
    state: 'review',
    title: `Ticket ${slug}`,
    blockedBy: [],
    isBlocked: false,
  };
}

const INITIAL_STORE_STATE = useKranzStore.getState();

beforeEach(() => {
  vi.mocked(api.tickets).mockReset();
  vi.mocked(api.ticket).mockReset();
  vi.mocked(api.draftTicket).mockReset();
  vi.mocked(api.approveTicket).mockReset();
  useKranzStore.setState(
    {
      ...INITIAL_STORE_STATE,
      tickets: [],
      ticketsError: null,
      ticketError: null,
      missionId: null,
    },
    true,
  );
});

describe('loadTickets', () => {
  it('populates tickets on success', async () => {
    const rows = [makeSummary('a'), makeSummary('b')];
    vi.mocked(api.tickets).mockResolvedValueOnce(rows);

    await useKranzStore.getState().loadTickets();

    expect(useKranzStore.getState().tickets).toEqual(rows);
    expect(useKranzStore.getState().ticketsError).toBeNull();
  });

  it('records ticketsError on failure', async () => {
    vi.mocked(api.tickets).mockRejectedValueOnce(new Error('backlog unreachable'));

    await useKranzStore.getState().loadTickets();

    expect(useKranzStore.getState().ticketsError).toBe('backlog unreachable');
    expect(useKranzStore.getState().tickets).toEqual([]);
  });
});

describe('approveTicket', () => {
  it('propagates a rejected ApiError message into ticketError verbatim', async () => {
    const message = "ticket 'a' is blocked by 'b' (not yet complete)";
    vi.mocked(api.approveTicket).mockRejectedValueOnce(new ApiError(409, message));
    vi.mocked(api.tickets).mockResolvedValueOnce([]);

    await useKranzStore.getState().approveTicket('a', false);

    expect(api.approveTicket).toHaveBeenCalledWith('a', false);
    expect(useKranzStore.getState().ticketError).toBe(message);
  });

  it('reloads tickets on success and clears ticketError', async () => {
    vi.mocked(api.approveTicket).mockResolvedValueOnce({ approved: true, missionId: 'm-1' });
    const rows = [makeSummary('a')];
    vi.mocked(api.tickets).mockResolvedValueOnce(rows);

    await useKranzStore.getState().approveTicket('a', true);

    expect(api.approveTicket).toHaveBeenCalledWith('a', true);
    expect(useKranzStore.getState().tickets).toEqual(rows);
    expect(useKranzStore.getState().ticketError).toBeNull();
  });
});

describe('draftTicket', () => {
  it('connects the returned missionId via connectMission on success', async () => {
    vi.mocked(api.draftTicket).mockResolvedValueOnce({ missionId: 'm-42' });

    await useKranzStore.getState().draftTicket('a');

    expect(api.draftTicket).toHaveBeenCalledWith('a');
    expect(useKranzStore.getState().missionId).toBe('m-42');
  });

  it('sets ticketError on failure without touching missionId', async () => {
    vi.mocked(api.draftTicket).mockRejectedValueOnce(new Error('spend limit reached'));

    await useKranzStore.getState().draftTicket('a');

    expect(useKranzStore.getState().ticketError).toBe('spend limit reached');
    expect(useKranzStore.getState().missionId).toBeNull();
  });
});
