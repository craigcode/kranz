import { describe, it, expect, vi, beforeEach } from 'vitest';
import { useKranzStore } from './store';
import { ApiError } from './api';
import type { MissionSummary, TicketSummary } from './types';

vi.mock('./api', async () => {
  const actual = await vi.importActual<typeof import('./api')>('./api');
  return {
    ...actual,
    api: {
      tickets: vi.fn(),
      ticket: vi.fn(),
      missions: vi.fn(),
      events: vi.fn().mockResolvedValue([]),
      draftTicket: vi.fn(),
      approveTicket: vi.fn(),
    },
  };
});

vi.mock('./ws', () => ({
  MissionSocket: class {
    connect() {}
    close() {}
  },
}));

import { api } from './api';

function makeSummary(slug: string): TicketSummary {
  return {
    slug,
    priority: 1,
    state: 'review',
    title: `Ticket ${slug}`,
    blockedBy: [],
    isBlocked: false,
    missionId: null,
  };
}

function makeMission(id: string): MissionSummary {
  return {
    id,
    status: 'planning',
    goal: `goal for ${id}`,
    createdAt: '2026-01-01T00:00:00Z',
  };
}

const INITIAL_STORE_STATE = useKranzStore.getState();

beforeEach(() => {
  vi.mocked(api.tickets).mockReset();
  vi.mocked(api.ticket).mockReset();
  vi.mocked(api.missions).mockReset();
  vi.mocked(api.events).mockReset().mockResolvedValue([]);
  vi.mocked(api.draftTicket).mockReset();
  vi.mocked(api.approveTicket).mockReset();
  useKranzStore.setState(
    {
      ...INITIAL_STORE_STATE,
      tickets: [],
      ticketsError: null,
      ticketError: null,
      ticketBusySlug: null,
      missions: [],
      missionsError: null,
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

    await useKranzStore.getState().approveTicket('a', false);

    expect(api.approveTicket).toHaveBeenCalledWith('a', false);
    expect(useKranzStore.getState().ticketError).toBe(message);
    expect(useKranzStore.getState().ticketBusySlug).toBeNull();
  });

  it('reloads tickets and missions on success and clears ticketError', async () => {
    vi.mocked(api.approveTicket).mockResolvedValueOnce({ approved: true, missionId: 'm-1' });
    const rows = [makeSummary('a')];
    const missions = [makeMission('m-1')];
    vi.mocked(api.tickets).mockResolvedValueOnce(rows);
    vi.mocked(api.missions).mockResolvedValueOnce(missions);

    await useKranzStore.getState().approveTicket('a', true);

    expect(api.approveTicket).toHaveBeenCalledWith('a', true);
    expect(api.tickets).toHaveBeenCalledOnce();
    expect(api.missions).toHaveBeenCalledOnce();
    expect(useKranzStore.getState().tickets).toEqual(rows);
    expect(useKranzStore.getState().missions).toEqual(missions);
    expect(useKranzStore.getState().ticketError).toBeNull();
    expect(useKranzStore.getState().ticketBusySlug).toBeNull();
  });

  it('sets ticketBusySlug while the approve POST is in flight', async () => {
    let resolveApprove!: (v: { approved: boolean; missionId: string }) => void;
    vi.mocked(api.approveTicket).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveApprove = resolve;
        }),
    );
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    vi.mocked(api.missions).mockResolvedValueOnce([]);

    const pending = useKranzStore.getState().approveTicket('a', false);
    expect(useKranzStore.getState().ticketBusySlug).toBe('a');

    resolveApprove({ approved: true, missionId: 'm-1' });
    await pending;
    expect(useKranzStore.getState().ticketBusySlug).toBeNull();
  });
});

describe('draftTicket', () => {
  it('connects the returned missionId and reloads tickets and missions', async () => {
    vi.mocked(api.draftTicket).mockResolvedValueOnce({ missionId: 'm-42' });
    const rows = [makeSummary('a')];
    const missions = [makeMission('m-42')];
    vi.mocked(api.tickets).mockResolvedValueOnce(rows);
    vi.mocked(api.missions).mockResolvedValueOnce(missions);

    await useKranzStore.getState().draftTicket('a');

    expect(api.draftTicket).toHaveBeenCalledWith('a');
    expect(useKranzStore.getState().missionId).toBe('m-42');
    // The drafting slug survives draftTicket's own connectMission, so
    // TicketDetail can attribute the fresh feed to this ticket.
    expect(useKranzStore.getState().draftingSlug).toBe('a');
    expect(api.tickets).toHaveBeenCalledOnce();
    expect(api.missions).toHaveBeenCalledOnce();
    expect(useKranzStore.getState().tickets).toEqual(rows);
    expect(useKranzStore.getState().missions).toEqual(missions);
    expect(useKranzStore.getState().ticketBusySlug).toBeNull();
  });

  it('clears draftingSlug when an unrelated mission connects or on disconnect', async () => {
    vi.mocked(api.draftTicket).mockResolvedValueOnce({ missionId: 'm-42' });
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    vi.mocked(api.missions).mockResolvedValueOnce([]);
    await useKranzStore.getState().draftTicket('a');
    expect(useKranzStore.getState().draftingSlug).toBe('a');

    useKranzStore.getState().connectMission('m-other');
    expect(useKranzStore.getState().draftingSlug).toBeNull();

    useKranzStore.getState().disconnect();
    expect(useKranzStore.getState().draftingSlug).toBeNull();
  });

  it('sets ticketError on failure without touching missionId', async () => {
    vi.mocked(api.draftTicket).mockRejectedValueOnce(new Error('spend limit reached'));

    await useKranzStore.getState().draftTicket('a');

    expect(useKranzStore.getState().ticketError).toBe('spend limit reached');
    expect(useKranzStore.getState().missionId).toBeNull();
    expect(useKranzStore.getState().ticketBusySlug).toBeNull();
  });

  it('sets ticketBusySlug while the draft POST is in flight', async () => {
    let resolveDraft!: (v: { missionId: string }) => void;
    vi.mocked(api.draftTicket).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveDraft = resolve;
        }),
    );
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    vi.mocked(api.missions).mockResolvedValueOnce([]);

    const pending = useKranzStore.getState().draftTicket('a');
    expect(useKranzStore.getState().ticketBusySlug).toBe('a');

    resolveDraft({ missionId: 'm-42' });
    await pending;
    expect(useKranzStore.getState().ticketBusySlug).toBeNull();
  });

  it('ignores a second draft while one is already in flight', async () => {
    let resolveDraft!: (v: { missionId: string }) => void;
    vi.mocked(api.draftTicket).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveDraft = resolve;
        }),
    );
    vi.mocked(api.tickets).mockResolvedValueOnce([]);
    vi.mocked(api.missions).mockResolvedValueOnce([]);

    const first = useKranzStore.getState().draftTicket('a');
    await useKranzStore.getState().draftTicket('b');
    expect(api.draftTicket).toHaveBeenCalledTimes(1);

    resolveDraft({ missionId: 'm-42' });
    await first;
  });
});
