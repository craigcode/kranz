import { StrictMode } from 'react';
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor, act } from '@testing-library/react';
import { BoardLens } from './BoardLens';
import { api } from '../lib/api';
import { buildFleetModel } from '../lib/board/fleet';
import { useKranzStore } from '../lib/store';
import type {
  GateEvaluation,
  LivePermission,
  MissionState,
  MissionSummary,
  TicketSummary,
} from '../lib/types';

const NOW_MS = Date.parse('2026-10-08T12:00:00Z');

function makeTicket(overrides: Partial<TicketSummary> = {}): TicketSummary {
  return {
    slug: 't-1',
    priority: 2,
    state: 'new',
    title: 'Ticket 1',
    blockedBy: [],
    isBlocked: false,
    missionId: null,
    ...overrides,
  };
}

function makeMission(overrides: Partial<MissionSummary> = {}): MissionSummary {
  return {
    id: 'm-1',
    status: 'running',
    goal: 'Mission 1',
    createdAt: '2026-10-08T11:00:00Z',
    merged: null,
    ...overrides,
  };
}

function makeState(id: string, overrides: Partial<MissionState> = {}): MissionState {
  return {
    mission: {
      id,
      status: 'running',
      goal: `Goal for ${id}`,
      baseBranch: 'main',
      missionBranch: `kranz/mission-${id}`,
      createdAt: '2026-10-08T11:00:00Z',
      validationContract: [],
      milestones: [
        {
          id: 'ms-1',
          title: 'Milestone 1',
          status: 'active',
          features: [],
          fixCycles: 0,
        },
      ],
    },
    runs: {},
    totals: { input: 100, output: 50, cacheRead: 0, cacheWrite: 0 },
    totalCostUsd: 0.5,
    localExecutorMilestones: 0,
    escalatedMilestones: 0,
    pendingUserMessages: [],
    recentDecisions: [],
    config: {
      orchestrator: { model: 'opus', reasoningEffort: 'high', maxBudgetUsd: 10 },
      worker: { model: 'sonnet', reasoningEffort: 'medium', maxBudgetUsd: 5 },
      validatorScrutiny: { model: 'opus', reasoningEffort: 'high', maxBudgetUsd: 5 },
      validatorFunctional: { model: 'sonnet', reasoningEffort: 'medium', maxBudgetUsd: 5 },
      skipScrutiny: false,
      skipFunctional: false,
      maxFixCyclesPerMilestone: 2,
      maxRespawns: 2,
      maxParallelWorkers: 1,
      eventStreamThrottleMs: 250,
      denyPatterns: [],
      allowValidatorCommands: [],
      dangerouslyAllowAll: false,
      allowBelowDefaultWorkerModel: false,
    },
    latestPlanRevision: 1,
    lastSeq: 5,
    ...overrides,
  };
}

function makePermission(missionId: string, deadlineMs: number): LivePermission {
  return {
    request: {
      proposal: {
        id: 'perm-1',
        engineSessionId: 'eng-1',
        peerSessionId: 'peer-1',
        peerRequestId: 'req-1',
        toolCallId: 'tc-1',
        action: { tool: 'bash', command: 'cargo test' },
        options: [{ optionId: 'allow_once', kind: 'allow_once', name: 'Allow once' }],
        actionDigest: 'sha256:act',
        optionsDigest: 'sha256:opt',
        observedAt: new Date(deadlineMs - 10_000).toISOString(),
        deadline: new Date(deadlineMs).toISOString(),
        prohibition: null,
      },
      binding: {
        missionId,
        runId: 'r-1',
        workspace: '/tmp/ws',
        planDigest: 'sha256:plan',
        policyDigest: 'sha256:policy',
      },
      bindingDigest: 'sha256:bind',
    },
  };
}

function makeGate(missionId: string, deadlineMs: number): GateEvaluation {
  return {
    requested: {
      request: {
        params: {
          attemptId: 'att-1',
          gateId: 'security-review',
          stage: 'milestone',
          subject: { milestoneId: 'ms-1' },
          deadline: new Date(deadlineMs).toISOString(),
          binding: {
            missionId,
            planDigest: 'sha256:plan',
            subjectCommit: 'abc1234',
          },
        },
      },
    },
    requestedSeq: 2,
    resolution: {
      disposition: 'require-human',
      rationale: 'Operator sign-off required',
    },
    consumed: false,
  };
}

const INITIAL_STORE = useKranzStore.getState();

beforeEach(() => {
  cleanup();
  window.location.hash = '#/board';
  Object.defineProperty(document, 'hidden', { configurable: true, value: false });
  useKranzStore.setState(
    {
      ...INITIAL_STORE,
      repoId: null,
      tickets: [],
      ticketsError: null,
      missions: [],
      missionsError: null,
      missionId: null,
      state: null,
    },
    true,
  );
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('BoardLens — slice 3 list, counts, escaping, and polling', () => {
  it('renders one list entry per ask in urgency order with fleet model hrefs and the three counts', async () => {
    const futureMs = Date.now() + 300_000;
    const tickets: TicketSummary[] = [
      makeTicket({ slug: 't-needs', state: 'needs-context', title: 'Needs context title' }),
      makeTicket({ slug: 't-wrong', state: 'wrong-plan', title: 'Wrong plan title' }),
      makeTicket({
        slug: 't-review',
        state: 'review',
        title: 'Reviewable title',
        missionId: 'm-review',
      }),
      makeTicket({
        slug: 't-delivered',
        state: 'done',
        title: 'Delivered title',
        missionId: 'm-delivered',
      }),
      makeTicket({ slug: 't-failed', state: 'failed', title: 'Failed title', missionId: 'm-failed' }),
      makeTicket({ slug: 't-landed', state: 'done', title: 'Landed title', missionId: 'm-landed' }),
    ];

    const missions: MissionSummary[] = [
      makeMission({ id: 'm-review', status: 'planning' }),
      makeMission({ id: 'm-delivered', status: 'complete', merged: false }),
      makeMission({ id: 'm-failed', status: 'failed' }),
      makeMission({ id: 'm-landed', status: 'complete', merged: true }),
      makeMission({ id: 'm-inflight', status: 'running' }),
    ];

    const inflightState: MissionState = {
      ...makeState('m-inflight', {
        permissions: { 'perm-1': makePermission('m-inflight', futureMs) },
        pendingGrantRequest: {
          milestoneId: 'ms-1',
          kind: 'egress',
          command: 'crates.io:443',
        },
        gateEvaluations: { 'att-1': makeGate('m-inflight', futureMs) },
        pendingRevision: {
          revision: 3,
          instructions: 'Add rollback test',
          plan: { goal: 'Goal', validationContract: [], milestones: [] },
        },
        pendingQuestions: [
          { questionId: 'q-1', role: 'worker', text: 'Use strict mode?' },
        ],
      }),
      mission: {
        ...makeState('m-inflight').mission,
        status: 'blocked',
      },
    };

    vi.spyOn(api, 'tickets').mockResolvedValue(tickets);
    vi.spyOn(api, 'missions').mockResolvedValue(missions);
    vi.spyOn(api, 'missionState').mockResolvedValue(inflightState);

    useKranzStore.setState({ tickets, missions });

    render(<BoardLens />);

    await waitFor(() => expect(api.missionState).toHaveBeenCalledWith('m-inflight'));

    const expectedModel = buildFleetModel(
      tickets,
      missions,
      new Map([['m-inflight', inflightState]]),
      Date.now(),
      null,
      '#/board',
    );

    expect(screen.getByTestId('board-count-waiting').textContent).toBe(
      String(expectedModel.counts.waiting),
    );
    expect(screen.getByTestId('board-count-inflight').textContent).toBe(
      String(expectedModel.counts.inFlight),
    );
    expect(screen.getByTestId('board-count-landed').textContent).toBe(
      String(expectedModel.counts.landed),
    );

    const items = Array.from(document.querySelectorAll('.trajectory-board-ask-item'));
    expect(items).toHaveLength(expectedModel.asks.length);
    expect(items.map((el) => el.getAttribute('data-ask-kind'))).toEqual([
      'permission',
      'grant',
      'blocked',
      'gate',
      'revision',
      'question',
      'needs-you',
      'wrong-plan',
      'reviewable',
      'delivered',
      'failed',
    ]);

    const nonActableKinds = new Set(['needs-you', 'wrong-plan', 'failed']);
    items.forEach((item, idx) => {
      const expected = expectedModel.asks[idx];
      if (nonActableKinds.has(expected.kind)) {
        const link = item.querySelector('a.trajectory-board-ask-link');
        expect(link?.getAttribute('href')).toBe(expected.href);
      } else {
        const btn = item.querySelector('button.trajectory-board-ask-link');
        expect(btn).not.toBeNull();
        expect(btn?.getAttribute('aria-expanded')).toBe('false');
      }
    });
  });

  it('renders a one-line empty state when nothing is waiting and escapes zero-width characters in titles', async () => {
    vi.spyOn(api, 'tickets').mockResolvedValue([]);
    vi.spyOn(api, 'missions').mockResolvedValue([]);

    const { unmount } = render(<BoardLens />);
    expect(screen.getByText('Nothing is waiting on you.')).toBeTruthy();
    unmount();

    // Now render with a zero-width space in a ticket title
    const zwTicket = makeTicket({
      slug: 't-zw',
      state: 'needs-context',
      title: 'Hidden\u200bZeroWidth',
    });
    vi.spyOn(api, 'tickets').mockResolvedValue([zwTicket]);
    useKranzStore.setState({ tickets: [zwTicket], missions: [] });

    render(<BoardLens />);
    expect(await screen.findByText('Hidden\\u200bZeroWidth')).toBeTruthy();
  });

  it('polls lists at most 7 times in 60s, calls missionState only for running missions, pauses while hidden, and refreshes on visibilitychange', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW_MS);

    const tickets = [
      makeTicket({ slug: 't-rev', state: 'review', missionId: 'm-planning' }),
    ];
    const missions = [
      makeMission({ id: 'm-planning', status: 'planning' }),
      makeMission({ id: 'm-running', status: 'running' }),
      makeMission({ id: 'm-complete', status: 'complete', merged: false }),
    ];

    const loadTicketsSpy = vi.fn().mockResolvedValue(undefined);
    const loadMissionsSpy = vi.fn().mockResolvedValue(undefined);
    const stateSpy = vi.spyOn(api, 'missionState').mockResolvedValue(makeState('m-running'));

    useKranzStore.setState({
      tickets,
      missions,
      loadTickets: loadTicketsSpy,
      loadMissions: loadMissionsSpy,
    });

    render(<BoardLens />);

    // Advance 60 seconds with document visible
    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
    });

    expect(loadTicketsSpy.mock.calls.length).toBeLessThanOrEqual(7);
    expect(loadMissionsSpy.mock.calls.length).toBeLessThanOrEqual(7);
    expect(loadTicketsSpy.mock.calls.length).toBeGreaterThanOrEqual(6);

    // missionState called ONLY for 'm-running', never for 'm-planning' or 'm-complete'
    expect(stateSpy).toHaveBeenCalledWith('m-running');
    expect(stateSpy).not.toHaveBeenCalledWith('m-planning');
    expect(stateSpy).not.toHaveBeenCalledWith('m-complete');
    expect(stateSpy.mock.calls.length).toBeLessThanOrEqual(13);

    // Hide document and advance 30 seconds -> zero new calls while hidden
    const ticketsBeforeHide = loadTicketsSpy.mock.calls.length;
    const missionsBeforeHide = loadMissionsSpy.mock.calls.length;
    const stateBeforeHide = stateSpy.mock.calls.length;

    Object.defineProperty(document, 'hidden', { configurable: true, value: true });
    document.dispatchEvent(new Event('visibilitychange'));

    await act(async () => {
      await vi.advanceTimersByTimeAsync(30_000);
    });

    expect(loadTicketsSpy.mock.calls.length).toBe(ticketsBeforeHide);
    expect(loadMissionsSpy.mock.calls.length).toBe(missionsBeforeHide);
    expect(stateSpy.mock.calls.length).toBe(stateBeforeHide);

    // Become visible again -> immediate refresh of lists and running mission state
    Object.defineProperty(document, 'hidden', { configurable: true, value: false });
    await act(async () => {
      document.dispatchEvent(new Event('visibilitychange'));
    });

    expect(loadTicketsSpy.mock.calls.length).toBe(ticketsBeforeHide + 1);
    expect(loadMissionsSpy.mock.calls.length).toBe(missionsBeforeHide + 1);
    expect(stateSpy.mock.calls.length).toBe(stateBeforeHide + 1);
  });

  it('uses only GET requests across 60 seconds of polling when globalThis.fetch is spied on', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW_MS);

    const fetchSpy = vi.spyOn(globalThis, 'fetch').mockImplementation(async (input) => {
      const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
      let body: unknown = [];
      if (url.endsWith('/state')) {
        body = makeState('m-1');
      } else if (url.endsWith('/missions')) {
        body = [makeMission({ id: 'm-1', status: 'running' })];
      } else if (url.endsWith('/tickets')) {
        body = [];
      }
      return new Response(JSON.stringify(body), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    });

    render(<BoardLens />);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(60_000);
    });

    expect(fetchSpy.mock.calls.length).toBeGreaterThan(0);
    for (const [, init] of fetchSpy.mock.calls) {
      const method = (init?.method ?? 'GET').toUpperCase();
      expect(method).toBe('GET');
    }
  });
});

describe('BoardLens — slice 4 jsdom degradation and DEMO route marker', () => {
  it('renders its header and list under jsdom (where canvas has no context) and logs no error', () => {
    const errorSpy = vi.spyOn(console, 'error');
    vi.spyOn(api, 'tickets').mockResolvedValue([]);
    vi.spyOn(api, 'missions').mockResolvedValue([]);

    const { container } = render(<BoardLens />);

    expect(container.querySelector('.trajectory-board-lens')).not.toBeNull();
    expect(screen.getByText('Waiting on you', { selector: 'dt' })).toBeTruthy();
    expect(screen.getByText('Nothing is waiting on you.')).toBeTruthy();
    expect(errorSpy).not.toHaveBeenCalled();
  });

  it('shows DEMO in the header on the demo route and not on the board route', () => {
    vi.spyOn(api, 'tickets').mockResolvedValue([]);
    vi.spyOn(api, 'missions').mockResolvedValue([]);

    const { unmount } = render(<BoardLens demo={false} />);
    expect(screen.queryByText('DEMO')).toBeNull();
    unmount();

    render(<BoardLens demo={true} />);
    expect(screen.getByText('DEMO')).toBeTruthy();
  });
});

describe('trajectory act in place — slice 6 reusing existing panels', () => {
  it('connects the mission and renders the owning panel beside the board for each in-flight ask kind', async () => {
    const futureMs = Date.now() + 300_000;
    const connectSpy = vi.fn((id: string) => {
      useKranzStore.setState({ missionId: id, state: null, lastSeq: null });
    });

    const missions = [
      makeMission({ id: 'm-perm', status: 'running', createdAt: '2026-10-08T10:00:00Z' }),
      makeMission({ id: 'm-grant', status: 'running', createdAt: '2026-10-08T10:01:00Z' }),
      makeMission({ id: 'm-block', status: 'blocked', createdAt: '2026-10-08T10:02:00Z' }),
      makeMission({ id: 'm-gate', status: 'running', createdAt: '2026-10-08T10:03:00Z' }),
      makeMission({ id: 'm-rev', status: 'running', createdAt: '2026-10-08T10:04:00Z' }),
      makeMission({ id: 'm-q', status: 'running', createdAt: '2026-10-08T10:05:00Z' }),
    ];

    const stateMap: Record<string, MissionState> = {
      'm-perm': makeState('m-perm', {
        permissions: { 'perm-1': makePermission('m-perm', futureMs) },
      }),
      'm-grant': makeState('m-grant', {
        pendingGrantRequest: {
          milestoneId: 'ms-1',
          kind: 'command',
          command: 'cargo test --workspace',
        },
      }),
      'm-block': {
        ...makeState('m-block'),
        mission: { ...makeState('m-block').mission, status: 'blocked' },
      },
      'm-gate': makeState('m-gate', {
        gateEvaluations: { 'att-1': makeGate('m-gate', futureMs) },
      }),
      'm-rev': makeState('m-rev', {
        pendingRevision: {
          revision: 2,
          instructions: 'Narrow touch set',
          plan: { goal: 'Goal', validationContract: [], milestones: [] },
        },
      }),
      'm-q': makeState('m-q', {
        pendingQuestions: [
          { questionId: 'q-1', role: 'worker', text: 'Which port should serve bind?' },
        ],
      }),
    };

    vi.spyOn(api, 'tickets').mockResolvedValue([]);
    vi.spyOn(api, 'missions').mockResolvedValue(missions);
    vi.spyOn(api, 'missionState').mockImplementation(async (id) => stateMap[id]);
    vi.spyOn(api, 'revisionDiff').mockResolvedValue({
      revision: 2,
      instructions: 'Narrow touch set',
      markdown: '# Plan',
      diff: '+milestone',
    });

    useKranzStore.setState({
      tickets: [],
      missions,
      connectMission: connectSpy,
    });

    const { container } = render(<BoardLens />);

    await waitFor(() => expect(api.missionState).toHaveBeenCalledWith('m-q'));

    const cases: Array<{ kind: string; missionId: string; heading: string }> = [
      { kind: 'permission', missionId: 'm-perm', heading: 'One-call permission' },
      { kind: 'grant', missionId: 'm-grant', heading: 'Grant request' },
      { kind: 'blocked', missionId: 'm-block', heading: 'Blocked' },
      { kind: 'gate', missionId: 'm-gate', heading: 'Gate review needed' },
      { kind: 'revision', missionId: 'm-rev', heading: 'Revision' },
      { kind: 'question', missionId: 'm-q', heading: 'Questions' },
    ];

    for (const c of cases) {
      const entry = container.querySelector(
        `.trajectory-board-ask-item[data-ask-kind="${c.kind}"]`,
      ) as HTMLElement;
      expect(entry).not.toBeNull();
      fireEvent.click(entry);

      expect(connectSpy).toHaveBeenCalledWith(c.missionId);
      const actCol = container.querySelector('.trajectory-board-act') as HTMLElement;
      expect(actCol).not.toBeNull();
      expect(actCol.textContent).toContain(c.heading);
      const openLink = actCol.querySelector('a.trajectory-board-open-mission');
      expect(openLink?.getAttribute('href')).toBe(`#/m/${c.missionId}`);
      if (c.kind !== 'revision') {
        expect(actCol.querySelector('.panel-revision')).toBeNull();
      } else {
        expect(actCol.querySelector('.panel-revision')).not.toBeNull();
      }
    }

    // Close via button and via Escape key
    fireEvent.click(container.querySelector('button.trajectory-board-act-close')!);
    expect(container.querySelector('.trajectory-board-act')).toBeNull();

    fireEvent.click(
      container.querySelector('.trajectory-board-ask-item[data-ask-kind="grant"]')!,
    );
    expect(container.querySelector('.trajectory-board-act')).not.toBeNull();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(container.querySelector('.trajectory-board-act')).toBeNull();
  });

  it('prevents cross-mission panel bleed during rapid/polled transitions and never rewinds lastSeq (KRZ-H6)', async () => {
    const missions = [
      makeMission({ id: 'm-a', status: 'running', createdAt: '2026-10-08T10:00:00Z' }),
      makeMission({ id: 'm-b', status: 'running', createdAt: '2026-10-08T10:01:00Z' }),
    ];
    const stateA = {
      ...makeState('m-a', {
        pendingGrantRequest: {
          milestoneId: 'ms-a',
          kind: 'command',
          command: 'secret-cmd-mission-a',
        },
      }),
      lastSeq: 10,
    };
    const stateB = {
      ...makeState('m-b', {
        pendingGrantRequest: {
          milestoneId: 'ms-b',
          kind: 'command',
          command: 'cmd-mission-b',
        },
      }),
      lastSeq: 5,
    };

    // Simulate connectMission lagging for m-b (store still holds m-a state while user clicks m-b)
    let lagConnectForB = true;
    const connectSpy = vi.fn((id: string) => {
      if (id === 'm-b' && lagConnectForB) return;
      if (useKranzStore.getState().missionId === id && useKranzStore.getState().state !== null) {
        return;
      }
      useKranzStore.setState({ missionId: id, state: null, lastSeq: null });
    });

    vi.spyOn(api, 'tickets').mockResolvedValue([]);
    vi.spyOn(api, 'missions').mockResolvedValue(missions);
    vi.spyOn(api, 'missionState').mockImplementation(async (id) =>
      id === 'm-a' ? stateA : stateB,
    );

    useKranzStore.setState({
      tickets: [],
      missions,
      connectMission: connectSpy,
    });

    const { container } = render(<BoardLens />);
    await waitFor(() => expect(api.missionState).toHaveBeenCalledWith('m-b'));

    const items = container.querySelectorAll('.trajectory-board-ask-item[data-ask-kind="grant"]');
    expect(items).toHaveLength(2);

    // 1) Select Mission A -> renders Mission A's grant panel
    fireEvent.click(items[0]);
    expect(container.querySelector('.trajectory-board-act')?.textContent).toContain(
      'secret-cmd-mission-a',
    );

    // Advance live WS seq on Mission A to 42; re-clicking Mission A must NOT rewind lastSeq to 10!
    useKranzStore.setState({
      state: { ...stateA, lastSeq: 42 },
      lastSeq: 42,
    });
    fireEvent.click(items[0]);
    expect(useKranzStore.getState().lastSeq).toBe(42);
    expect(useKranzStore.getState().state?.lastSeq).toBe(42);

    // 2) Rapidly click Mission B while store still holds Mission A -> Mission A's panel MUST NOT render!
    fireEvent.click(items[1]);
    expect(connectSpy).toHaveBeenCalledWith('m-b');
    const actWhileSwitching = container.querySelector('.trajectory-board-act') as HTMLElement;
    expect(actWhileSwitching.textContent).toContain('Connecting to mission…');
    expect(actWhileSwitching.textContent).not.toContain('secret-cmd-mission-a');
    expect(actWhileSwitching.querySelector('button[class*="strip-btn"]')).toBeNull();

    // Once Mission B finishes connecting, Mission B's panel renders cleanly
    lagConnectForB = false;
    act(() => {
      useKranzStore.setState({ missionId: 'm-b', state: stateB, lastSeq: stateB.lastSeq });
    });
    expect(container.querySelector('.trajectory-board-act')?.textContent).toContain(
      'cmd-mission-b',
    );
    expect(container.querySelector('.trajectory-board-act')?.textContent).not.toContain(
      'secret-cmd-mission-a',
    );
  });

  it('calls connectMission via effect when polled state transitions selected craft to a new in-flight ask (KRZ-H6)', async () => {
    const connectSpy = vi.fn((id: string) => {
      useKranzStore.setState({ missionId: id, state: null, lastSeq: null });
    });
    const missions = [
      makeMission({ id: 'm-polled', status: 'running', createdAt: '2026-10-08T10:00:00Z' }),
    ];
    let currentPolledState = makeState('m-polled');
    vi.spyOn(api, 'tickets').mockResolvedValue([]);
    vi.spyOn(api, 'missions').mockResolvedValue(missions);
    vi.spyOn(api, 'missionState').mockImplementation(async () => currentPolledState);
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(
      () =>
        ({
          fillStyle: '#000',
          strokeStyle: '#000',
          lineWidth: 1,
          lineCap: 'butt',
          lineJoin: 'miter',
          globalAlpha: 1,
          globalCompositeOperation: 'source-over',
          setTransform() {},
          clearRect() {},
          beginPath() {},
          closePath() {},
          moveTo() {},
          lineTo() {},
          arc() {},
          ellipse() {},
          fill() {},
          stroke() {},
          fillRect() {},
          setLineDash() {},
          createRadialGradient: () => ({ addColorStop() {} }),
          drawImage() {},
          save() {},
          restore() {},
          translate() {},
          rotate() {},
        }) as unknown as RenderingContext,
    );

    vi.useFakeTimers();
    vi.setSystemTime(NOW_MS);

    useKranzStore.setState({ tickets: [], missions, connectMission: connectSpy });
    render(<BoardLens />);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(100);
    });

    // Select craft m-polled on the plot before it has any ask
    fireEvent.click(screen.getByText('m-polled').closest('button')!);
    expect(connectSpy).not.toHaveBeenCalled();

    // Next 5s state poll surfaces a parked grant on m-polled -> effect connects m-polled automatically
    currentPolledState = makeState('m-polled', {
      pendingGrantRequest: { milestoneId: 'ms-1', kind: 'command', command: 'echo ok' },
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(5_000);
    });

    expect(connectSpy).toHaveBeenCalledWith('m-polled');
    expect(screen.getByText('echo ok')).toBeTruthy();
  });

  it('advances nowMs during list refreshes even when zero missions are running (KRZ-H9)', async () => {
    vi.useFakeTimers();
    vi.setSystemTime(NOW_MS);

    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(
      () =>
        ({
          fillStyle: '#000',
          strokeStyle: '#000',
          lineWidth: 1,
          lineCap: 'butt',
          lineJoin: 'miter',
          globalAlpha: 1,
          globalCompositeOperation: 'source-over',
          setTransform() {},
          clearRect() {},
          beginPath() {},
          closePath() {},
          moveTo() {},
          lineTo() {},
          arc() {},
          ellipse() {},
          fill() {},
          stroke() {},
          fillRect() {},
          setLineDash() {},
          createRadialGradient: () => ({ addColorStop() {} }),
          drawImage() {},
          save() {},
          restore() {},
          translate() {},
          rotate() {},
        }) as unknown as RenderingContext,
    );

    // Zero running missions; one delivered mission that merges on the first 10s poll
    let currentMissions = [
      makeMission({
        id: 'm-landed-fresh',
        status: 'complete',
        merged: false,
        createdAt: new Date(NOW_MS - 60_000).toISOString(),
      }),
    ];
    const loadMissionsSpy = vi.fn(async () => {
      useKranzStore.setState({ missions: currentMissions });
    });
    useKranzStore.setState({
      tickets: [],
      missions: currentMissions,
      loadTickets: vi.fn().mockResolvedValue(undefined),
      loadMissions: loadMissionsSpy,
    });

    const { container } = render(<BoardLens />);
    // At mount (t = 0), m-landed-fresh is delivered and visible on the board
    expect(
      container.querySelector('.trajectory-board-chip[data-craft-key="mission-m-landed-fresh"]'),
    ).not.toBeNull();

    // Mark m-landed-fresh as merged before the 10s list poll; zero running missions throughout!
    currentMissions = [
      makeMission({
        id: 'm-landed-fresh',
        status: 'complete',
        merged: true,
        createdAt: new Date(NOW_MS - 60_000).toISOString(),
      }),
    ];

    // Advance 100s across 10 list-poll ticks with zero running missions -> nowMs advances, craft finishes lift
    await act(async () => {
      await vi.advanceTimersByTimeAsync(100_000);
    });
    expect(
      container.querySelector('.trajectory-board-chip[data-craft-key="mission-m-landed-fresh"]'),
    ).toBeNull();
  });

  it('isolates demo mode asks from live navigation and mutations, and preserves transitions under StrictMode (KRZ-M12, KRZ-M13, KRZ-L8)', () => {
    window.location.hash = '#/board/demo';
    const connectSpy = vi.fn();
    useKranzStore.setState({ connectMission: connectSpy });

    const { container, unmount } = render(<BoardLens demo={true} />);
    expect(container.querySelectorAll('a.trajectory-board-ask-link')).toHaveLength(0);
    const demoButtons = container.querySelectorAll('button.trajectory-board-ask-link');
    expect(demoButtons.length).toBeGreaterThan(0);
    expect(demoButtons[0].getAttribute('aria-expanded')).toBe('false');

    fireEvent.click(demoButtons[0]);
    expect(demoButtons[0].getAttribute('aria-expanded')).toBe('true');
    expect(window.location.hash).toBe('#/board/demo');
    expect(connectSpy).not.toHaveBeenCalled();
    expect(container.querySelector('.trajectory-board-act')).toBeNull();
    unmount();

    // Verify StrictMode double-render does not wipe model.transitions
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(
      () =>
        ({
          fillStyle: '#000',
          strokeStyle: '#000',
          lineWidth: 1,
          lineCap: 'butt',
          lineJoin: 'miter',
          globalAlpha: 1,
          globalCompositeOperation: 'source-over',
          setTransform() {},
          clearRect() {},
          beginPath() {},
          closePath() {},
          moveTo() {},
          lineTo() {},
          arc() {},
          ellipse() {},
          fill() {},
          stroke() {},
          fillRect() {},
          setLineDash() {},
          createRadialGradient: () => ({ addColorStop() {} }),
          drawImage() {},
          save() {},
          restore() {},
          translate() {},
          rotate() {},
        }) as unknown as RenderingContext,
    );
    const rafSpy = vi.spyOn(window, 'requestAnimationFrame').mockImplementation(() => 1);
    vi.spyOn(api, 'tickets').mockResolvedValue([]);
    vi.spyOn(api, 'missions').mockResolvedValue([]);

    useKranzStore.setState({
      tickets: [],
      missions: [makeMission({ id: 'm-strict', status: 'complete', merged: false })],
      loadTickets: vi.fn().mockResolvedValue(undefined),
      loadMissions: vi.fn().mockResolvedValue(undefined),
    });

    render(
      <StrictMode>
        <BoardLens />
      </StrictMode>,
    );
    // Stationary delivered craft -> zero rAF scheduled initially
    expect(rafSpy).not.toHaveBeenCalled();

    // Single-step transition delivered -> landed (running count remains 0, so rAF ONLY fires if transitions survived StrictMode!)
    act(() => {
      useKranzStore.setState({
        missions: [makeMission({ id: 'm-strict', status: 'complete', merged: true })],
      });
    });
    expect(rafSpy).toHaveBeenCalledTimes(1);
  });

  it('renders ReviewableTicketRow (plan, estimate, Queue for run) for a reviewable ticket and calls approveTicket', async () => {
    const approveSpy = vi.fn().mockResolvedValue(undefined);
    const tickets = [
      makeTicket({
        slug: 't-ready',
        state: 'review',
        title: 'Ready ticket',
        missionId: 'm-ready',
      }),
    ];
    const missions = [makeMission({ id: 'm-ready', status: 'planning', goal: 'Ready ticket' })];

    vi.spyOn(api, 'tickets').mockResolvedValue(tickets);
    vi.spyOn(api, 'missions').mockResolvedValue(missions);
    vi.spyOn(api, 'planMd').mockResolvedValue({
      markdown:
        '# Plan for t-ready\n\n## Cost estimate\n\nEstimated **$1.10 – $2.90** (expected ~$1.75).\n\nShip the feature.',
    });

    useKranzStore.setState({
      tickets,
      missions,
      approveTicket: approveSpy,
      ticketBusySlug: null,
    });

    const { container } = render(<BoardLens />);

    const entry = container.querySelector(
      '.trajectory-board-ask-item[data-ask-kind="reviewable"]',
    ) as HTMLElement;
    fireEvent.click(entry);

    const actCol = container.querySelector('.trajectory-board-act') as HTMLElement;
    expect(actCol).not.toBeNull();
    expect(await screen.findByText(/Ship the feature\./)).toBeTruthy();
    expect(actCol.querySelector('.pipeline-estimate')?.textContent).toContain('$1.10 – $2.90');

    const queueBtn = actCol.querySelector('button.pipeline-primary-action') as HTMLButtonElement;
    expect(queueBtn.textContent).toBe('Queue for run');
    fireEvent.click(queueBtn);

    expect(approveSpy).toHaveBeenCalledWith('t-ready', false);
  });

  it('renders DeliveredPanel (report, diff stat, and Merge control) when a delivered mission is selected', async () => {
    const missions = [
      makeMission({
        id: 'm-del',
        status: 'complete',
        merged: false,
        goal: 'Delivered work',
      }),
    ];

    vi.spyOn(api, 'tickets').mockResolvedValue([]);
    vi.spyOn(api, 'missions').mockResolvedValue(missions);
    vi.spyOn(api, 'reportMd').mockResolvedValue({ markdown: 'Delivered report body.' });
    vi.spyOn(api, 'diffStat').mockResolvedValue({
      diffStat: '3 files changed, 42 insertions(+)',
      baseSha: 'aaa',
      tip: 'bbb',
    });
    vi.spyOn(api, 'prHandoff').mockResolvedValue({
      kind: 'unavailable',
      reason: 'local only',
    });

    useKranzStore.setState({ tickets: [], missions });

    const { container } = render(<BoardLens />);

    const entry = container.querySelector(
      '.trajectory-board-ask-item[data-ask-kind="delivered"]',
    ) as HTMLElement;
    fireEvent.click(entry);

    const actCol = container.querySelector('.trajectory-board-act') as HTMLElement;
    expect(actCol).not.toBeNull();
    expect(await screen.findByText('Delivered report body.')).toBeTruthy();
    expect(screen.getByText('3 files changed, 42 insertions(+)')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Merge' })).toBeTruthy();
  });

  it('keeps needs-you, wrong-plan, failed, and ticketless reviewable missions as plain links with slice 3 hrefs', () => {
    const tickets = [
      makeTicket({ slug: 't-needs', state: 'needs-context', title: 'Needs context' }),
      makeTicket({ slug: 't-wrong', state: 'wrong-plan', title: 'Wrong plan' }),
      makeTicket({ slug: 't-fail', state: 'failed', title: 'Failed ticket', missionId: 'm-fail' }),
    ];
    const missions = [
      makeMission({ id: 'm-orphan-plan', status: 'planning', goal: 'Orphan reviewable' }),
      makeMission({ id: 'm-fail', status: 'failed', goal: 'Failed ticket' }),
    ];

    vi.spyOn(api, 'tickets').mockResolvedValue(tickets);
    vi.spyOn(api, 'missions').mockResolvedValue(missions);

    useKranzStore.setState({ tickets, missions });

    const { container } = render(<BoardLens />);

    const expectedHrefs: Array<[string, string]> = [
      ['needs-you', '#/backlog/t-needs'],
      ['wrong-plan', '#/backlog/t-wrong'],
      ['reviewable', '#/m/m-orphan-plan'],
      ['failed', '#/backlog/t-fail'],
    ];

    for (const [kind, href] of expectedHrefs) {
      const entry = container.querySelector(
        `.trajectory-board-ask-item[data-ask-kind="${kind}"]`,
      ) as HTMLElement;
      expect(entry).not.toBeNull();
      const link = entry.querySelector('a.trajectory-board-ask-link') as HTMLAnchorElement;
      expect(link.getAttribute('href')).toBe(href);

      fireEvent.click(entry);
      expect(container.querySelector('.trajectory-board-act')).toBeNull();
    }
  });

  it('source files src/components/Board*.tsx and src/lib/board/*.ts never mention postJson and only reference api.missionState', () => {
    const rawModules = import.meta.glob<string>(
      ['./Board*.tsx', '!./*.test.tsx', '../lib/board/*.ts', '!../lib/board/*.test.ts'],
      { query: '?raw', import: 'default', eager: true },
    );

    const entries = Object.entries(rawModules);
    expect(entries.length).toBeGreaterThanOrEqual(6);

    for (const [filePath, content] of entries) {
      expect(content, `${filePath} must not mention postJson`).not.toContain('postJson');

      const apiMatches = [...content.matchAll(/\bapi\.([A-Za-z0-9_]+)/g)].map((m) => m[1]);
      for (const member of apiMatches) {
        expect(member, `${filePath} referenced forbidden api.${member}`).toBe(
          'missionState',
        );
      }
    }
  });
});
