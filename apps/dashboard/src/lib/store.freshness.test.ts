import { beforeEach, describe, expect, it, vi } from 'vitest';
import { useKranzStore } from './store';
import type { MissionSocketOptions } from './ws';
import type { MissionEvent, MissionState, MissionSummary, PlanRequestResponse, TicketSummary } from './types';

const { sockets } = vi.hoisted(() => ({ sockets: [] as MissionSocketOptions[] }));

vi.mock('./api', async () => {
  const actual = await vi.importActual<typeof import('./api')>('./api');
  return {
    ...actual,
    api: {
      events: vi.fn(),
      missionState: vi.fn(),
      missions: vi.fn(),
      tickets: vi.fn(),
      requestPlan: vi.fn(),
      planningTurn: vi.fn(),
      approvePending: vi.fn(),
      startMission: vi.fn(),
    },
  };
});

vi.mock('./ws', () => ({
  MissionSocket: class {
    constructor(opts: MissionSocketOptions) { sockets.push(opts); }
    connect() {}
    close() {}
  },
}));

import { api, ApiError } from './api';

const INITIAL = useKranzStore.getState();

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

async function settle() {
  // Flush chained then/catch callbacks without timers or transport timing.
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

function preview(goal: string): Extract<PlanRequestResponse, { ready: true }> {
  return {
    ready: true,
    plan: { goal, validationContract: [], milestones: [] },
    planIdentity: goal,
    estimate: { workerRuns: 1, validatorRuns: 1, lowUsd: 1, expectedUsd: 2, highUsd: 3 },
  };
}

function row(id: string): MissionSummary & TicketSummary {
  return {
    id, status: 'planning', goal: id, createdAt: '2026-09-12T00:00:00Z',
    slug: id, title: id, priority: 1, state: 'review', blockedBy: [],
    isBlocked: false, missionId: null,
  };
}

function revisit() {
  useKranzStore.getState().connectMission('B');
  useKranzStore.getState().connectMission('A');
}

beforeEach(() => {
  useKranzStore.getState().disconnect();
  useKranzStore.setState(INITIAL, true);
  sockets.length = 0;
  vi.resetAllMocks();
  vi.mocked(api.events).mockResolvedValue([]);
});

describe('mission connection freshness', () => {
  it('keeps the newer preview when an A → B → A request finishes last', async () => {
    const old = deferred<PlanRequestResponse>();
    const fresh = deferred<PlanRequestResponse>();
    vi.mocked(api.requestPlan).mockReturnValueOnce(old.promise).mockReturnValueOnce(fresh.promise);
    useKranzStore.getState().connectMission('A');
    useKranzStore.getState().requestPlan();
    revisit();
    useKranzStore.getState().requestPlan();
    fresh.resolve(preview('newer'));
    await settle();
    old.resolve(preview('older'));
    await settle();
    expect(useKranzStore.getState().planning.review?.plan.goal).toBe('newer');
  });

  it('ignores an earlier visit’s error without clearing the current busy state or queue', async () => {
    const old = deferred<PlanRequestResponse>();
    const fresh = deferred<PlanRequestResponse>();
    vi.mocked(api.requestPlan).mockReturnValueOnce(old.promise).mockReturnValueOnce(fresh.promise);
    useKranzStore.getState().connectMission('A');
    useKranzStore.getState().requestPlan();
    revisit();
    useKranzStore.getState().requestPlan();
    useKranzStore.getState().sendPlanningMessage('next question');
    old.reject(new ApiError(409, 'mission is not hosted', 'mission_not_hosted'));
    await settle();
    expect(useKranzStore.getState().planning).toMatchObject({
      busy: 'plan-request', notHosted: false, error: null, queued: ['next question'],
    });
    fresh.resolve(preview('current'));
    await settle();
    expect(useKranzStore.getState().planning.review?.plan.goal).toBe('current');
  });

  it('ignores a previous planning turn without draining the new visit’s queue', async () => {
    const old = deferred<{ reply: string }>();
    const fresh = deferred<{ reply: string }>();
    vi.mocked(api.planningTurn).mockReturnValueOnce(old.promise).mockReturnValueOnce(fresh.promise);
    useKranzStore.getState().connectMission('A');
    useKranzStore.getState().sendPlanningMessage('old question');
    revisit();
    useKranzStore.getState().sendPlanningMessage('new question');
    useKranzStore.getState().sendPlanningMessage('queued question');
    old.resolve({ reply: 'old reply' });
    await settle();
    expect(useKranzStore.getState().planning.localItems.map(item => item.text)).toEqual(['new question']);
    expect(useKranzStore.getState().planning.queued).toEqual(['queued question']);
    expect(api.planningTurn).toHaveBeenCalledTimes(2);
    vi.mocked(api.planningTurn).mockResolvedValueOnce({ reply: 'queue reply' });
    fresh.resolve({ reply: 'new reply' });
    await settle();
    expect(api.planningTurn).toHaveBeenLastCalledWith('A', 'queued question');
  });

  it('does not reuse an approval response from before disconnect and reconnect', async () => {
    const old = deferred<{ branch: string; started: boolean }>();
    vi.mocked(api.approvePending).mockReturnValueOnce(old.promise);
    useKranzStore.getState().connectMission('A');
    useKranzStore.setState(s => ({ planning: { ...s.planning, review: preview('old') } }));
    useKranzStore.getState().approvePlan();
    useKranzStore.getState().disconnect();
    useKranzStore.getState().connectMission('A');
    useKranzStore.setState(s => ({ planning: { ...s.planning, review: preview('new') } }));
    old.resolve({ branch: 'old-branch', started: false });
    await settle();
    expect(useKranzStore.getState().planning.approvedBranch).toBeNull();
    expect(useKranzStore.getState().planning.review?.plan.goal).toBe('new');
  });

  it('does not apply an earlier start failure to the current in-flight start', async () => {
    const old = deferred<{ running: boolean }>();
    const fresh = deferred<{ running: boolean }>();
    vi.mocked(api.startMission).mockReturnValueOnce(old.promise).mockReturnValueOnce(fresh.promise);
    useKranzStore.getState().connectMission('A');
    const oldStart = useKranzStore.getState().startRun();
    revisit();
    const freshStart = useKranzStore.getState().startRun();
    old.reject(new ApiError(409, 'already running'));
    await oldStart;
    expect(useKranzStore.getState().startingRun).toBe(true);
    expect(useKranzStore.getState().startRunError).toBeNull();
    fresh.resolve({ running: true });
    await freshStart;
    expect(useKranzStore.getState().startingRun).toBe(false);
  });

  it('ignores old history seeds and socket callbacks after revisiting a mission', async () => {
    const old = deferred<MissionEvent[]>();
    vi.mocked(api.events).mockReturnValueOnce(old.promise);
    useKranzStore.getState().connectMission('A');
    const previousSocket = sockets[0];
    revisit();
    old.resolve([{ seq: 5, type: 'mission.paused', payload: {} } as MissionEvent]);
    previousSocket.onFrame({ type: 'snapshot', seq: 5, state: { lastSeq: 5 } as MissionState });
    previousSocket.onStatus('gone');
    await settle();
    expect(useKranzStore.getState().missionId).toBe('A');
    expect(useKranzStore.getState().events).toEqual([]);
    expect(useKranzStore.getState().pauseEvents).toEqual([]);
    expect(useKranzStore.getState().state).toBeNull();
  });

  it('does not let a late websocket snapshot roll back a newer REST state', async () => {
    useKranzStore.getState().connectMission('A');
    const current = sockets[0];
    current.onFrame({ type: 'snapshot', seq: 5, state: { lastSeq: 5 } as MissionState });
    vi.mocked(api.missionState).mockResolvedValueOnce({ lastSeq: 12 } as MissionState);
    current.onStatus('live');
    await settle();
    current.onFrame({ type: 'snapshot', seq: 9, state: { lastSeq: 9 } as MissionState });
    expect(useKranzStore.getState().state?.lastSeq).toBe(12);
    expect(useKranzStore.getState().lastSeq).toBe(12);
  });

  it('does not disconnect a new visit based on a list requested in the old visit', async () => {
    const old = deferred<MissionSummary[]>();
    vi.mocked(api.missions).mockReturnValueOnce(old.promise);
    useKranzStore.getState().connectMission('A');
    const load = useKranzStore.getState().loadMissions();
    revisit();
    old.resolve([]);
    await load;
    expect(useKranzStore.getState().missionId).toBe('A');
  });
});

describe.each(['missions', 'tickets'] as const)('%s request freshness', (resource) => {
  const load = () => resource === 'missions'
    ? useKranzStore.getState().loadMissions() : useKranzStore.getState().loadTickets();
  const error = () => resource === 'missions'
    ? useKranzStore.getState().missionsError : useKranzStore.getState().ticketsError;

  it('keeps newer results when requests finish in reverse order', async () => {
    const old = deferred<ReturnType<typeof row>[]>();
    vi.mocked(api[resource]).mockReturnValueOnce(old.promise).mockResolvedValueOnce([row('new')]);
    const previous = load();
    await load();
    old.resolve([row('old')]);
    await previous;
    expect(useKranzStore.getState()[resource]).toEqual([row('new')]);
  });

  it('ignores a late error after the newer request succeeds', async () => {
    const old = deferred<ReturnType<typeof row>[]>();
    vi.mocked(api[resource]).mockReturnValueOnce(old.promise).mockResolvedValueOnce([row('new')]);
    const previous = load();
    await load();
    old.reject(new Error('old request failed'));
    await previous;
    expect(error()).toBeNull();
    expect(useKranzStore.getState()[resource]).toEqual([row('new')]);
  });

  it('does not replace a newer failure with stale success', async () => {
    const old = deferred<ReturnType<typeof row>[]>();
    vi.mocked(api[resource]).mockReturnValueOnce(old.promise).mockRejectedValueOnce(new Error('new failure'));
    const previous = load();
    await load();
    old.resolve([row('old')]);
    await previous;
    expect(error()).toBe('new failure');
    expect(useKranzStore.getState()[resource]).toEqual([]);
  });
});
