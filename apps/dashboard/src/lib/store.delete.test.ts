import { describe, it, expect, vi, beforeEach } from 'vitest';
import { useKranzStore } from './store';
import { ApiError } from './api';
import type { MissionSummary } from './types';

const { sockets } = vi.hoisted(() => ({
  sockets: [] as Array<{
    closed: boolean;
    onStatus: (status: 'connecting' | 'live' | 'lost' | 'gone') => void;
  }>,
}));

vi.mock('./api', async () => {
  const actual = await vi.importActual<typeof import('./api')>('./api');
  return {
    ...actual,
    api: {
      missions: vi.fn(),
      events: vi.fn().mockResolvedValue([]),
      deleteMission: vi.fn(),
    },
  };
});

vi.mock('./ws', () => ({
  MissionSocket: class {
    closed = false;
    onStatus: (status: 'connecting' | 'live' | 'lost' | 'gone') => void;

    constructor(opts: { onStatus: (status: 'connecting' | 'live' | 'lost' | 'gone') => void }) {
      this.onStatus = opts.onStatus;
      sockets.push(this);
    }

    connect() {}

    close() {
      this.closed = true;
    }
  },
}));

import { api } from './api';

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
  sockets.length = 0;
  vi.mocked(api.missions).mockReset().mockResolvedValue([]);
  vi.mocked(api.events).mockReset().mockResolvedValue([]);
  vi.mocked(api.deleteMission).mockReset();
  useKranzStore.setState(
    {
      ...INITIAL_STORE_STATE,
      missions: [],
      missionsError: null,
      missionId: null,
    },
    true,
  );
});

describe('deleteMission', () => {
  it('disconnects when the CONNECTED mission is deleted (no 404 reconnect loop)', async () => {
    vi.mocked(api.deleteMission).mockResolvedValueOnce({ deleted: true });

    useKranzStore.getState().connectMission('m-1');
    expect(useKranzStore.getState().missionId).toBe('m-1');
    expect(sockets).toHaveLength(1);

    await useKranzStore.getState().deleteMission('m-1', false);

    expect(api.deleteMission).toHaveBeenCalledWith('m-1', false);
    expect(useKranzStore.getState().missionId).toBeNull();
    expect(sockets[0].closed).toBe(true);
    expect(api.missions).toHaveBeenCalledOnce();
  });

  it('leaves the connection alone when a NON-connected mission is deleted', async () => {
    vi.mocked(api.deleteMission).mockResolvedValueOnce({ deleted: true });
    // The refresh after deleting m-2 still lists the connected m-1.
    vi.mocked(api.missions).mockResolvedValue([makeMission('m-1')]);

    useKranzStore.getState().connectMission('m-1');
    expect(sockets).toHaveLength(1);

    await useKranzStore.getState().deleteMission('m-2', true);

    expect(api.deleteMission).toHaveBeenCalledWith('m-2', true);
    expect(useKranzStore.getState().missionId).toBe('m-1');
    expect(sockets[0].closed).toBe(false);
    expect(api.missions).toHaveBeenCalledOnce();
  });

  it('keeps the connection when the server refuses the delete', async () => {
    vi.mocked(api.deleteMission).mockRejectedValueOnce(
      new ApiError(409, 'mission is running — pause or abandon first'),
    );
    // A refused delete means m-1 still exists — the refresh lists it.
    vi.mocked(api.missions).mockResolvedValue([makeMission('m-1')]);

    useKranzStore.getState().connectMission('m-1');

    await useKranzStore.getState().deleteMission('m-1', false);

    // Disconnect only follows a successful delete — the mission dir still
    // exists, so the live feed stays useful. (loadMissions clears the error
    // it recorded; that reset is pre-existing behaviour.)
    expect(useKranzStore.getState().missionId).toBe('m-1');
    expect(sockets[0].closed).toBe(false);
  });
});

describe('loadMissions vanished-mission teardown', () => {
  it('disconnects when the connected mission is missing from the fresh list', async () => {
    useKranzStore.getState().connectMission('m-1');
    vi.mocked(api.missions).mockResolvedValue([makeMission('m-2')]);

    await useKranzStore.getState().loadMissions();

    expect(useKranzStore.getState().missionId).toBeNull();
    expect(sockets[0].closed).toBe(true);
  });

  it('keeps the connection when the fresh list still contains it', async () => {
    useKranzStore.getState().connectMission('m-1');
    vi.mocked(api.missions).mockResolvedValue([makeMission('m-1'), makeMission('m-2')]);

    await useKranzStore.getState().loadMissions();

    expect(useKranzStore.getState().missionId).toBe('m-1');
    expect(sockets[0].closed).toBe(false);
  });

  it('keeps the connection when the load FAILS (no fresh list to trust)', async () => {
    useKranzStore.getState().connectMission('m-1');
    vi.mocked(api.missions).mockRejectedValueOnce(new Error('network down'));

    await useKranzStore.getState().loadMissions();

    expect(useKranzStore.getState().missionId).toBe('m-1');
    expect(sockets[0].closed).toBe(false);
  });

  it('spares a mission connected mid-flight (the stale list predates it)', async () => {
    let resolveList!: (missions: MissionSummary[]) => void;
    vi.mocked(api.missions).mockImplementationOnce(
      () =>
        new Promise((resolve) => {
          resolveList = resolve;
        }),
    );

    const pending = useKranzStore.getState().loadMissions();
    useKranzStore.getState().connectMission('m-new');
    resolveList([]); // fetched before m-new existed — must not tear it down
    await pending;

    expect(useKranzStore.getState().missionId).toBe('m-new');
    expect(sockets[0].closed).toBe(false);
  });

  it("maps the socket's terminal 'gone' status to a clean disconnect", () => {
    useKranzStore.getState().connectMission('m-1');

    // MissionSocket reports 'gone' after its existence probe saw a 404
    // (mission deleted out-of-band while this tab sat on the mission page).
    sockets[0].onStatus('gone');

    expect(useKranzStore.getState().missionId).toBeNull();
    expect(sockets[0].closed).toBe(true);
  });

  it('delete response lost: the follow-up refresh still tears the connection down', async () => {
    // The delete POST landed server-side but its response never arrived, so
    // deleteMission's success-path disconnect never ran. The refresh it
    // always issues proves m-1 is gone and disconnects instead.
    vi.mocked(api.deleteMission).mockRejectedValueOnce(new Error('socket hang up'));
    vi.mocked(api.missions).mockResolvedValue([makeMission('m-2')]);

    useKranzStore.getState().connectMission('m-1');
    await useKranzStore.getState().deleteMission('m-1', false);

    expect(useKranzStore.getState().missionId).toBeNull();
    expect(sockets[0].closed).toBe(true);
  });
});
