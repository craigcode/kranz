import { describe, it, expect, vi, beforeEach } from 'vitest';
import { useKranzStore } from './store';
import { ApiError } from './api';

const { sockets } = vi.hoisted(() => ({
  sockets: [] as Array<{ closed: boolean }>,
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

    constructor() {
      sockets.push(this);
    }

    connect() {}

    close() {
      this.closed = true;
    }
  },
}));

import { api } from './api';

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

    useKranzStore.getState().connectMission('m-1');

    await useKranzStore.getState().deleteMission('m-1', false);

    // Disconnect only follows a successful delete — the mission dir still
    // exists, so the live feed stays useful. (loadMissions clears the error
    // it recorded; that reset is pre-existing behaviour.)
    expect(useKranzStore.getState().missionId).toBe('m-1');
    expect(sockets[0].closed).toBe(false);
  });
});
