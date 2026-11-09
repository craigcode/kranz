// Deterministic scripted fleet for `#/board/demo` and `#/r/<repo>/board/demo` —
// docs/scoping/trajectory-board.md (slice 4).
// Produces successive fleet-model inputs (tickets, missions, missionStates) with
// zero network calls, cycling a fixed fleet through all 11 pipeline stages and
// all 11 ask kinds (including a free-return failure and an abandonment) while
// keeping all three header counts non-zero on every beat.

import type {
  GateEvaluation,
  LivePermission,
  MissionConfig,
  MissionState,
  MissionSummary,
  TicketSummary,
} from '../types';

export const DEMO_STEP_MS = 10_000;
export const DEMO_STEP_COUNT = 15;
export const DEMO_LOOP_MS = DEMO_STEP_MS * DEMO_STEP_COUNT;

export interface DemoSnapshot {
  step: number;
  nowMs: number;
  tickets: TicketSummary[];
  missions: MissionSummary[];
  missionStates: ReadonlyMap<string, MissionState>;
}

const DEMO_CONFIG: MissionConfig = {
  orchestrator: { model: 'opus', reasoningEffort: 'high', maxBudgetUsd: 20 },
  worker: { model: 'sonnet', reasoningEffort: 'medium', maxTurns: 50, maxBudgetUsd: 10 },
  validatorScrutiny: { model: 'opus', reasoningEffort: 'high', maxTurns: 40, maxBudgetUsd: 10 },
  validatorFunctional: { model: 'sonnet', reasoningEffort: 'medium', maxTurns: 40, maxBudgetUsd: 5 },
  skipScrutiny: false,
  skipFunctional: false,
  maxFixCyclesPerMilestone: 2,
  maxRespawns: 2,
  maxParallelWorkers: 1,
  eventStreamThrottleMs: 250,
  denyPatterns: ['git push'],
  allowValidatorCommands: ['npm test'],
  dangerouslyAllowAll: false,
  allowBelowDefaultWorkerModel: false,
};

function makeDemoState(
  id: string,
  goal: string,
  createdAt: string,
  doneFeatures: number,
  totalFeatures: number,
  status: MissionState['mission']['status'] = 'running',
  overrides: Partial<MissionState> = {},
): MissionState {
  const features = Array.from({ length: totalFeatures }, (_, i) => ({
    id: `f-1-${i + 1}`,
    title: `Feature ${i + 1}`,
    spec: `Implement feature ${i + 1}`,
    validationCriteria: ['Passes tests'],
    origin: 'plan' as const,
    status: i < doneFeatures ? ('complete' as const) : ('pending' as const),
    workerRuns: [],
    commits: i < doneFeatures ? [`c0ffee${i}`] : [],
    respawns: 0,
  }));

  return {
    mission: {
      id,
      status,
      goal,
      baseBranch: 'main',
      missionBranch: `kranz/mission-${id}`,
      createdAt,
      validationContract: [],
      milestones: [
        {
          id: 'ms-1',
          title: 'Core implementation',
          status: status === 'blocked' ? 'blocked' : 'active',
          features,
          fixCycles: 0,
        },
      ],
    },
    runs: {},
    totals: { input: 34_000, output: 6_200, cacheRead: 120_000, cacheWrite: 5_000 },
    totalCostUsd: 1.85,
    localExecutorMilestones: 0,
    escalatedMilestones: 0,
    pendingUserMessages: [],
    recentDecisions: [],
    config: DEMO_CONFIG,
    latestPlanRevision: 1,
    lastSeq: 12,
    ...overrides,
  };
}

function demoPermission(missionId: string, deadlineIso: string): LivePermission {
  return {
    request: {
      proposal: {
        id: 'perm-demo-1',
        engineSessionId: 'eng-demo-1',
        peerSessionId: 'peer-demo-1',
        peerRequestId: 'req-demo-1',
        toolCallId: 'tc-demo-1',
        action: { tool: 'bash', command: 'npx vitest run src/lib/board/plot.test.ts' },
        options: [{ optionId: 'allow_once', kind: 'allow_once', name: 'Allow once' }],
        actionDigest: 'sha256:demo-act',
        optionsDigest: 'sha256:demo-opts',
        observedAt: deadlineIso,
        deadline: deadlineIso,
        prohibition: null,
      },
      binding: {
        missionId,
        runId: 'r-2',
        workspace: '/workspace/kranz',
        planDigest: 'sha256:demo-plan',
        policyDigest: 'sha256:demo-policy',
      },
      bindingDigest: 'sha256:demo-bind-1',
    },
  };
}

function demoGate(missionId: string, deadlineIso: string): GateEvaluation {
  return {
    requested: {
      request: {
        params: {
          attemptId: 'att-demo-1',
          gateId: 'bundle-budget',
          stage: 'milestone',
          subject: { milestoneId: 'ms-1' },
          deadline: deadlineIso,
          binding: {
            missionId,
            planDigest: 'sha256:demo-plan',
            subjectCommit: '47cc3a8',
          },
        },
      },
    },
    requestedSeq: 9,
    resolution: {
      disposition: 'require-human',
      rationale: 'Verify lazy chunk boundary before proceeding',
    },
    consumed: false,
  };
}

/**
 * Pure snapshot generator for step `stepIndex` at timestamp `nowMs`.
 * Calling with `step = 0..DEMO_STEP_COUNT-1` steps the fleet through one full loop.
 */
export function demoSnapshotAt(stepIndex: number, nowMs: number): DemoSnapshot {
  const step = ((stepIndex % DEMO_STEP_COUNT) + DEMO_STEP_COUNT) % DEMO_STEP_COUNT;
  const iso = (offsetSec: number) => new Date(nowMs + offsetSec * 1000).toISOString();
  const futureDeadline = iso(300);

  // Anchor items present on every step so all 3 counts (Waiting, In flight, Landed)
  // are always > 0 and at least one orbital + one pad craft wears a hold ring.
  const tLanded1: TicketSummary = {
    slug: 'board-flight-plan',
    priority: 1,
    state: 'done',
    title: 'Flight-plan geometry and pose model',
    blockedBy: [],
    isBlocked: false,
    missionId: 'm-demo-plan',
  };
  const mLanded1: MissionSummary = {
    id: 'm-demo-plan',
    status: 'complete',
    merged: true,
    goal: tLanded1.title,
    createdAt: iso(-600),
  };

  const tLanded2: TicketSummary = {
    slug: 'board-fleet-fold',
    priority: 1,
    state: 'done',
    title: 'Fold pipeline rows and asks into craft',
    blockedBy: [],
    isBlocked: false,
    missionId: 'm-demo-fold',
  };
  const mLanded2: MissionSummary = {
    id: 'm-demo-fold',
    status: 'complete',
    merged: true,
    goal: tLanded2.title,
    createdAt: iso(-520),
  };

  // Traveller A: cycles reviewable -> queued -> running (grant, permission, gate, revision) -> delivered -> landed
  let tTravellerA: TicketSummary;
  let mTravellerA: MissionSummary;
  let stateTravellerA: MissionState | undefined;

  if (step === 0) {
    tTravellerA = {
      slug: 'board-lens',
      priority: 2,
      state: 'review',
      title: 'Board lens listing everything waiting on you',
      blockedBy: [],
      isBlocked: false,
      missionId: 'm-demo-lens',
    };
    mTravellerA = {
      id: 'm-demo-lens',
      status: 'planning',
      merged: null,
      goal: tTravellerA.title,
      createdAt: iso(-240),
    };
  } else if (step === 1) {
    tTravellerA = {
      slug: 'board-lens',
      priority: 2,
      state: 'queued',
      title: 'Board lens listing everything waiting on you',
      blockedBy: [],
      isBlocked: false,
      missionId: 'm-demo-lens',
    };
    mTravellerA = {
      id: 'm-demo-lens',
      status: 'approved',
      merged: null,
      goal: tTravellerA.title,
      createdAt: iso(-240),
    };
  } else if (step >= 2 && step <= 5) {
    tTravellerA = {
      slug: 'board-lens',
      priority: 2,
      state: 'running',
      title: 'Board lens listing everything waiting on you',
      blockedBy: [],
      isBlocked: false,
      missionId: 'm-demo-lens',
    };
    mTravellerA = {
      id: 'm-demo-lens',
      status: 'running',
      merged: null,
      goal: tTravellerA.title,
      createdAt: iso(-240),
    };
    if (step === 2) {
      stateTravellerA = makeDemoState('m-demo-lens', tTravellerA.title, iso(-240), 1, 3, 'running', {
        pendingGrantRequest: {
          milestoneId: 'ms-1',
          kind: 'egress',
          command: 'registry.npmjs.org:443',
        },
      });
    } else if (step === 3) {
      stateTravellerA = makeDemoState('m-demo-lens', tTravellerA.title, iso(-240), 2, 3, 'running', {
        permissions: { 'perm-demo-1': demoPermission('m-demo-lens', futureDeadline) },
      });
    } else if (step === 4) {
      stateTravellerA = makeDemoState('m-demo-lens', tTravellerA.title, iso(-240), 2, 3, 'running', {
        gateEvaluations: { 'att-demo-1': demoGate('m-demo-lens', futureDeadline) },
      });
    } else {
      stateTravellerA = makeDemoState('m-demo-lens', tTravellerA.title, iso(-240), 2, 3, 'running', {
        pendingRevision: {
          revision: 2,
          instructions: 'Split canvas plot into its own lazy chunk',
          plan: { goal: tTravellerA.title, validationContract: [], milestones: [] },
        },
      });
    }
  } else if (step === 6 || step === 7) {
    tTravellerA = {
      slug: 'board-lens',
      priority: 2,
      state: 'done',
      title: 'Board lens listing everything waiting on you',
      blockedBy: [],
      isBlocked: false,
      missionId: 'm-demo-lens',
    };
    mTravellerA = {
      id: 'm-demo-lens',
      status: 'complete',
      merged: false,
      goal: tTravellerA.title,
      createdAt: iso(-240),
    };
  } else {
    tTravellerA = {
      slug: 'board-lens',
      priority: 2,
      state: 'done',
      title: 'Board lens listing everything waiting on you',
      blockedBy: [],
      isBlocked: false,
      missionId: 'm-demo-lens',
    };
    mTravellerA = {
      id: 'm-demo-lens',
      status: 'complete',
      merged: true,
      goal: tTravellerA.title,
      createdAt: iso(-240),
    };
  }

  // Traveller B (Free-return failure story): running (question, then blocked) -> failed (free return!)
  let tFreeReturn: TicketSummary;
  let mFreeReturn: MissionSummary;
  let stateFreeReturn: MissionState | undefined;

  if (step <= 2) {
    tFreeReturn = {
      slug: 'board-sound',
      priority: 3,
      state: 'running',
      title: 'Launch and touchdown audio cues',
      blockedBy: [],
      isBlocked: false,
      missionId: 'm-demo-sound',
    };
    mFreeReturn = {
      id: 'm-demo-sound',
      status: step === 2 ? 'blocked' : 'running',
      merged: null,
      goal: tFreeReturn.title,
      createdAt: iso(-300),
    };
    if (step <= 1) {
      stateFreeReturn = makeDemoState('m-demo-sound', tFreeReturn.title, iso(-300), 1, 2, 'running', {
        pendingQuestions: [
          {
            questionId: 'q-audio-1',
            role: 'worker',
            text: 'Mute launch audio by default in headless browsers?',
            options: ['Mute by default', 'Require explicit toggle'],
            featureId: 'f-1-1',
            milestoneId: 'ms-1',
          },
        ],
      });
    } else {
      stateFreeReturn = makeDemoState('m-demo-sound', tFreeReturn.title, iso(-300), 1, 2, 'blocked');
    }
  } else {
    tFreeReturn = {
      slug: 'board-sound',
      priority: 3,
      state: 'failed',
      title: 'Launch and touchdown audio cues',
      blockedBy: [],
      isBlocked: false,
      missionId: 'm-demo-sound',
    };
    mFreeReturn = {
      id: 'm-demo-sound',
      status: 'failed',
      merged: null,
      goal: tFreeReturn.title,
      createdAt: iso(-300),
    };
  }

  // Traveller C (Delivered / lunar orbit story): always keeps lunar orbit / delivered represented
  const tLunar: TicketSummary = {
    slug: 'board-2d-plot',
    priority: 2,
    state: step <= 5 ? 'done' : 'running',
    title: '2D figure-8 plot and non-overlapping chips',
    blockedBy: [],
    isBlocked: false,
    missionId: 'm-demo-plot',
  };
  const mLunar: MissionSummary = {
    id: 'm-demo-plot',
    status: step <= 5 ? 'complete' : 'running',
    merged: step <= 5 ? false : null,
    goal: tLunar.title,
    createdAt: iso(-360),
  };
  const stateLunar =
    step > 5
      ? makeDemoState('m-demo-plot', tLunar.title, iso(-360), 2, 4, 'running', {
          pendingGrantRequest: {
            milestoneId: 'ms-1',
            kind: 'command',
            command: 'npm run check-embedded',
          },
        })
      : undefined;

  // Ground pad progression: captured -> drafting -> needs-you -> wrong-plan -> reviewable
  const tGroundCycle: TicketSummary = {
    slug: 'board-act-in-place',
    priority: 2,
    state:
      step === 0
        ? 'new'
        : step === 1
          ? 'drafting'
          : step === 2
            ? 'needs-context'
            : step === 3
              ? 'wrong-plan'
              : 'review',
    title: 'Answer an ask beside the board by reusing its owner',
    blockedBy: [],
    isBlocked: false,
    missionId: step >= 1 ? 'm-demo-act' : null,
  };
  const mGroundCycle: MissionSummary | null =
    step >= 1
      ? {
          id: 'm-demo-act',
          status: 'planning',
          merged: null,
          goal: tGroundCycle.title,
          createdAt: iso(-120),
        }
      : null;

  // Ticketless mission that gets scrubbed (abandoned) mid-loop: running -> abandoned
  const mScrub: MissionSummary = {
    id: 'm-demo-scrub',
    status: step < 4 ? 'running' : 'abandoned',
    merged: null,
    goal: 'Experimental WebGL shader prototype',
    createdAt: iso(-180),
  };
  const stateScrub =
    step < 4 ? makeDemoState('m-demo-scrub', mScrub.goal, iso(-180), 1, 3, 'running') : undefined;

  // Extra captured backlog item so the hangar count is visible
  const tCapturedBacklog: TicketSummary = {
    slug: 'board-surgeon-overlay',
    priority: 3,
    state: 'new',
    title: 'Grant-latency overlay from the flight surgeon',
    blockedBy: [],
    isBlocked: false,
    missionId: null,
  };

  const tickets: TicketSummary[] = [
    tLanded1,
    tLanded2,
    tTravellerA,
    tFreeReturn,
    tLunar,
    tGroundCycle,
    tCapturedBacklog,
  ];

  const missions: MissionSummary[] = [
    mLanded1,
    mLanded2,
    mTravellerA,
    mFreeReturn,
    mLunar,
    ...(mGroundCycle ? [mGroundCycle] : []),
    mScrub,
  ];

  const missionStates = new Map<string, MissionState>();
  if (stateTravellerA) missionStates.set(stateTravellerA.mission.id, stateTravellerA);
  if (stateFreeReturn) missionStates.set(stateFreeReturn.mission.id, stateFreeReturn);
  if (stateLunar) missionStates.set(stateLunar.mission.id, stateLunar);
  if (stateScrub) missionStates.set(stateScrub.mission.id, stateScrub);

  return {
    step,
    nowMs,
    tickets,
    missions,
    missionStates,
  };
}
