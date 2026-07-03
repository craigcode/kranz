#!/usr/bin/env node
// mock-server.mjs — Kranz dashboard dev harness (no dependencies).
//
// Serves the REST + WebSocket protocol from docs/protocol.md with a canned
// demo mission so apps/dashboard can be developed and smoke-tested without
// the Rust server. Also serves apps/dashboard/dist at / (SPA fallback) when
// a build exists, mirroring `kranz serve`.
//
// Usage:
//   node scripts/mock-server.mjs            # listens on :4560
//   PORT=5000 node scripts/mock-server.mjs
//
// Demo behavior:
//   - GET  /api/missions, /:id/state, /:id/events?since, /:id/plan,
//          /:id/runs/:runId/transcript, /api/health
//   - POST /:id/control — msg/pause/resume/config-change are applied to the
//          canned state and broadcast, so the UI is fully interactive.
//   - WS   /:id/ws?since — snapshot or replay per the protocol; a ticker
//          emits a worker.message every few seconds to demo live tailing.
//
// Mission lifecycle (M2.5, docs/protocol.md "Mission lifecycle"):
//   - Every POST /api/... requires a NON-EMPTY x-kranz-token header (any
//     value accepted — this fakes the per-serve mutation token); missing or
//     empty → 401 {"error":"missing or invalid token"}.
//   - POST /api/missions {goal, config?}       → 201 {id} — creates a hosted
//     planning mission (config is a partial MissionConfig patch over the
//     canned defaults).
//   - POST /:id/planning/turn {text}           → 200 {reply} with canned
//     replies (cycling); emits user.message + orchestrator worker.message
//     over the WS feed. 409 when not hosted here (the canned demo mission),
//     not in planning, or a turn is already in flight.
//   - POST /:id/planning/request-plan          → first call 200 {ready:false,
//     reply} (NotReady returns to conversation); later calls 200 {ready:true,
//     plan, estimate} with a small plan + CostEstimate.
//   - POST /:id/approve {plan}                 → 200 {branch}; materializes
//     the plan into state milestones and emits plan.approved.
//   - POST /:id/start                          → 202 {running:true}; then
//     flips the mission through running → complete over a few ticks
//     (milestone/feature/worker events streaming over the WS feed).
//     409 while already running.
//   - Canned mission m-term-01 stays in "planning" but is NOT hosted here
//     (as if `kranz plan` runs in a terminal): its planning mutations 409
//     with "not hosted", exercising the dashboard's terminal-planning notice.

import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { readFileSync, existsSync } from 'node:fs';
import { extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const PORT = Number(process.env.PORT ?? 4560);
const DIST = fileURLToPath(new URL('../apps/dashboard/dist', import.meta.url));
const MISSION_ID = 'm-demo-01';

// ---------------------------------------------------------------------------
// Canned mission data
// ---------------------------------------------------------------------------

const NOW = Date.now();
const at = (minAgo) => new Date(NOW - minAgo * 60_000).toISOString();

const defaultRole = (model, effort, turns, budget) => ({
  model,
  reasoningEffort: effort,
  maxTurns: turns,
  maxBudgetUsd: budget,
});

const config = {
  orchestrator: defaultRole('opus', 'high', null, 20.0),
  worker: defaultRole('sonnet', 'medium', 50, 10.0),
  validatorScrutiny: defaultRole('opus', 'high', 40, 10.0),
  validatorFunctional: defaultRole('sonnet', 'medium', 40, 5.0),
  skipScrutiny: false,
  skipFunctional: false,
  maxFixCyclesPerMilestone: 2,
  maxRespawns: 2,
  maxParallelWorkers: 1,
  eventStreamThrottleMs: 250,
  denyPatterns: ['git push'],
  allowValidatorCommands: ['npm test'],
  dangerouslyAllowAll: false,
};

// Strip nulls so shapes match serde's skip_serializing_if = Option::is_none.
for (const rc of Object.values(config)) {
  if (rc && typeof rc === 'object') {
    for (const k of Object.keys(rc)) if (rc[k] === null) delete rc[k];
  }
}

const feature = (id, title, spec, origin, status, runs = [], commits = []) => ({
  id,
  title,
  spec,
  validationCriteria: [`${title} works as specced`],
  origin,
  status,
  workerRuns: runs,
  commits,
  respawns: 0,
});

const plan = {
  goal: 'Ship the Kranz mission-control dashboard MVP',
  validationContract: [
    {
      id: 'a-1',
      statement: 'npx tsc --noEmit passes in apps/dashboard',
      check: 'command',
      command: 'npx tsc --noEmit',
    },
    {
      id: 'a-2',
      statement: 'Dashboard reconnects after server restart without losing events',
      check: 'agent-judgement',
    },
  ],
  milestones: [
    {
      title: 'Protocol data layer',
      features: [
        {
          title: 'Typed REST client',
          spec: 'fetch wrappers for docs/protocol.md endpoints',
          validationCriteria: ['all endpoints typed'],
        },
        {
          title: 'WebSocket reconnect with since cursor',
          spec: 'exponential backoff 0.5s→8s, ?since=<lastSeq>',
          validationCriteria: ['no events lost across restarts'],
        },
      ],
    },
    {
      title: 'Mission Control layout',
      features: [
        {
          title: 'Four-region layout shell',
          spec: 'sidebar / topbar / centre / right column',
          validationCriteria: ['matches reference doc'],
        },
        {
          title: 'Right column panels',
          spec: 'models, features, progress log',
          validationCriteria: ['all three panels render live data'],
        },
      ],
    },
  ],
};

const state = {
  mission: {
    id: MISSION_ID,
    goal: 'Ship the Kranz mission-control dashboard MVP',
    validationContract: plan.validationContract,
    milestones: [
      {
        id: 'ms-1',
        title: 'Protocol data layer',
        status: 'active',
        fixCycles: 1,
        startSha: '3f2c1ab',
        features: [
          feature(
            'f-1-1',
            'Typed REST client',
            'fetch wrappers for docs/protocol.md endpoints',
            'plan',
            'complete',
            ['r-2'],
            ['a1b2c3d'],
          ),
          feature(
            'f-1-2',
            'WebSocket reconnect with since cursor',
            'exponential backoff 0.5s→8s, ?since=<lastSeq>',
            'plan',
            'active',
            ['r-3'],
          ),
          feature(
            'f-1-3',
            'Fix: never retry POST control requests',
            'control commands are not idempotent; only GETs may retry',
            'fix',
            'pending',
          ),
        ],
      },
      {
        id: 'ms-2',
        title: 'Mission Control layout',
        status: 'pending',
        fixCycles: 0,
        features: [
          feature(
            'f-2-1',
            'Four-region layout shell',
            'sidebar / topbar / centre / right column',
            'plan',
            'pending',
          ),
          feature(
            'f-2-2',
            'Right column panels',
            'models, features, progress log',
            'plan',
            'pending',
          ),
        ],
      },
    ],
    status: 'running',
    createdAt: at(150),
    baseBranch: 'main',
    missionBranch: `kranz/mission-${MISSION_ID}`,
  },
  runs: {
    'r-1': {
      id: 'r-1',
      role: 'orchestrator',
      sdkSessionId: '6f1d2c3b-1111-4aaa-bbbb-000000000001',
      model: 'opus',
      startedAt: at(148),
      tokens: { input: 91_200, output: 14_300, cacheRead: 410_000, cacheWrite: 22_000 },
      costUsd: 0.61,
      transcriptPath: 'runs/r-1.jsonl',
      promptHash: 'sha256:9e2f11',
    },
    'r-2': {
      id: 'r-2',
      role: 'worker',
      featureId: 'f-1-1',
      sdkSessionId: '6f1d2c3b-2222-4aaa-bbbb-000000000002',
      model: 'sonnet',
      startedAt: at(145),
      endedAt: at(120),
      tokens: { input: 48_210, output: 9_120, cacheRead: 154_000, cacheWrite: 8_200 },
      costUsd: 0.87,
      transcriptPath: 'runs/r-2.jsonl',
      result: 'pass',
      report: {
        result: 'pass',
        summary: 'Typed REST client + shared protocol types, all endpoints covered.',
        filesTouched: ['apps/dashboard/src/lib/api.ts', 'apps/dashboard/src/lib/types.ts'],
        testsAdded: [],
        testEvidence: 'npx tsc --noEmit clean',
        dependenciesAdded: [],
        knownGaps: ['retries POST /control on failure'],
        commits: ['a1b2c3d'],
      },
      promptHash: 'sha256:77ab19',
    },
    'r-3': {
      id: 'r-3',
      role: 'worker',
      featureId: 'f-1-2',
      sdkSessionId: '6f1d2c3b-3333-4aaa-bbbb-000000000003',
      model: 'sonnet',
      startedAt: at(115),
      tokens: { input: 30_100, output: 5_400, cacheRead: 98_000, cacheWrite: 4_100 },
      costUsd: 0.15,
      transcriptPath: 'runs/r-3.jsonl',
      promptHash: 'sha256:77ab19',
    },
  },
  totals: { input: 169_510, output: 28_820, cacheRead: 662_000, cacheWrite: 34_300 },
  totalCostUsd: 1.63,
  pendingUserMessages: [],
  recentDecisions: [
    'Filed fix feature f-1-3 from review finding; starting f-1-2 (WebSocket client).',
  ],
  config,
  lastSeq: 0, // set after the canned history below
};

// --- canned event history (blocked→unblocked, a denied call, conversation) --
let seq = 0;
const events = [];
const past = (type, payload, minAgo) => {
  events.push({ seq: ++seq, ts: at(minAgo), missionId: MISSION_ID, type, payload });
};

past('mission.created', {
  goal: state.mission.goal,
  baseBranch: 'main',
  missionBranch: state.mission.missionBranch,
  config,
}, 150);
past('plan.approved', { plan }, 148);
past('worker.spawned', {
  runId: 'r-1',
  role: 'orchestrator',
  sdkSessionId: state.runs['r-1'].sdkSessionId,
  model: 'opus',
  promptHash: 'sha256:9e2f11',
  transcriptPath: 'runs/r-1.jsonl',
}, 148);
past('orchestrator.decision', {
  summary: 'Plan approved: 2 milestones / 5 features. Starting ms-1 with f-1-1.',
  detail:
    'Sequencing:\n\n- **ms-1** data layer first — everything else renders from it\n- **ms-2** layout once the store is live\n\nWorker budget `$10` each, deny pattern `git push` active.',
}, 147);
past('milestone.started', { milestoneId: 'ms-1', startSha: '3f2c1ab' }, 146);
past('feature.started', { featureId: 'f-1-1' }, 146);
past('worker.spawned', {
  runId: 'r-2',
  role: 'worker',
  featureId: 'f-1-1',
  sdkSessionId: state.runs['r-2'].sdkSessionId,
  model: 'sonnet',
  promptHash: 'sha256:77ab19',
  transcriptPath: 'runs/r-2.jsonl',
}, 145);
past('worker.message', {
  runId: 'r-2',
  tag: 'text',
  content: 'Reading docs/protocol.md; adding typed fetch wrappers in api.ts.',
}, 144);
past('worker.message', { runId: 'r-2', tag: 'tool-use', content: 'Bash: npx tsc --noEmit' }, 140);
past('worker.message', {
  runId: 'r-2',
  tag: 'denied',
  content: "Bash: git push origin main (matched deny pattern 'git push')",
}, 138);
past('worker.message', {
  runId: 'r-2',
  tag: 'denied',
  content: "Bash: git push --force (matched deny pattern 'git push')",
}, 137);
past('milestone.blocked', {
  milestoneId: 'ms-1',
  reason: 'worker r-2 hit guardrails twice (git push); pausing feature work for guidance',
}, 136);
past('user.message', {
  text: "Don't push — commit locally on the mission branch only. Unblock and continue.",
  interrupt: false,
}, 130);
past('orchestrator.decision', {
  summary: 'User guidance: no pushes. Instructed r-2 to commit locally; resuming ms-1.',
}, 129);
past('milestone.unblocked', { milestoneId: 'ms-1', reason: 'user guidance received' }, 129);
past('worker.message', {
  runId: 'r-2',
  tag: 'text',
  content: 'Committing locally; wrapping up api.ts + types.ts.',
}, 125);
past('worker.completed', {
  runId: 'r-2',
  result: 'pass',
  tokens: state.runs['r-2'].tokens,
  costUsd: 0.87,
  report: state.runs['r-2'].report,
}, 120);
past('feature.completed', { featureId: 'f-1-1', commits: ['a1b2c3d'] }, 120);
past('validation.finding', {
  milestoneId: 'ms-1',
  runId: 'r-1',
  finding: {
    subject: 'f-1-1',
    severity: 'major',
    evidence: 'api.ts retries POST /control on failure — control commands are not idempotent',
    suggestedFix: 'only retry GET requests',
  },
}, 118);
past('fixfeature.created', {
  milestoneId: 'ms-1',
  feature: state.mission.milestones[0].features[2],
}, 118);
past('orchestrator.decision', {
  summary: 'Filed fix feature f-1-3 from review finding; starting f-1-2 (WebSocket client).',
  detail: 'The retry bug is `major` but not blocking — f-1-3 queued behind f-1-2.',
}, 117);
past('feature.started', { featureId: 'f-1-2' }, 116);
past('worker.spawned', {
  runId: 'r-3',
  role: 'worker',
  featureId: 'f-1-2',
  sdkSessionId: state.runs['r-3'].sdkSessionId,
  model: 'sonnet',
  promptHash: 'sha256:77ab19',
  transcriptPath: 'runs/r-3.jsonl',
}, 115);
past('worker.message', {
  runId: 'r-3',
  tag: 'text',
  content: 'Implementing exponential backoff 0.5s→8s with ?since resume.',
}, 90);
past('mission.paused', {}, 60);
past('mission.resumed', {}, 45);
past('user.message', { text: 'Status check — how is the WS client going?', interrupt: false }, 30);
past('orchestrator.decision', {
  summary: 'r-3 mid-flight on f-1-2; reconnect tests passing locally.',
}, 29);
past('worker.message', {
  runId: 'r-3',
  tag: 'tool-use',
  content: 'Bash: node scripts/ws-reconnect-test.mjs',
}, 20);
past('worker.message', {
  runId: 'r-3',
  tag: 'text',
  content: 'Backoff caps at 8s; replay from since=17 verified.',
}, 5);

state.lastSeq = seq;

// --- transcripts (Claude Code stream-json values, one array per run) --------

const usage = (i, o) => ({ input_tokens: i, output_tokens: o, cache_read_input_tokens: 0 });
const assistantText = (model, text, u) => ({
  type: 'assistant',
  message: { model, role: 'assistant', content: [{ type: 'text', text }], usage: u },
});
const assistantTool = (model, id, name, input) => ({
  type: 'assistant',
  message: { model, role: 'assistant', content: [{ type: 'tool_use', id, name, input }] },
});
const toolResult = (id, content, isError = false) => ({
  type: 'user',
  message: {
    role: 'user',
    content: [{ type: 'tool_result', tool_use_id: id, content, is_error: isError }],
  },
});

const transcripts = {
  'r-1': [
    { type: 'system', subtype: 'init', model: 'opus', session_id: state.runs['r-1'].sdkSessionId },
    assistantText(
      'opus',
      'Plan drafted: **2 milestones, 5 features**. ms-1 covers the protocol data layer, ms-2 the layout. Submitting for approval.',
      usage(41_000, 2_100),
    ),
    assistantText(
      'opus',
      'r-2 reported `pass` on f-1-1 but the report lists a known gap:\n\n- retries POST /control on failure\n\nFiling fix feature f-1-3 and moving on to f-1-2.',
      usage(50_200, 4_800),
    ),
  ],
  'r-2': [
    { type: 'system', subtype: 'init', model: 'sonnet', session_id: state.runs['r-2'].sdkSessionId },
    assistantText('sonnet', 'Reading `docs/protocol.md`, then writing `api.ts` and `types.ts`.', usage(9_000, 400)),
    assistantTool('sonnet', 'toolu_01', 'Bash', { command: 'npx tsc --noEmit', description: 'Typecheck' }),
    toolResult('toolu_01', 'exit 0 — no errors'),
    assistantTool('sonnet', 'toolu_02', 'Bash', { command: 'git push origin main' }),
    toolResult('toolu_02', "Permission denied by Kranz guardrail: matched deny pattern 'git push'", true),
    assistantText('sonnet', 'Understood — committing locally instead.\n\n```json\n{"result":"pass","summary":"Typed REST client + shared protocol types","commits":["a1b2c3d"]}\n```', usage(39_210, 8_720)),
    { type: 'result', subtype: 'success', is_error: false, num_turns: 4, total_cost_usd: 0.87, result: 'done', usage: usage(48_210, 9_120) },
  ],
  'r-3': [
    { type: 'system', subtype: 'init', model: 'sonnet', session_id: state.runs['r-3'].sdkSessionId },
    assistantText('sonnet', 'Building the reconnecting WebSocket client with a `since` cursor.', usage(8_000, 300)),
    assistantTool('sonnet', 'toolu_03', 'Bash', { command: 'node scripts/ws-reconnect-test.mjs' }),
    toolResult('toolu_03', 'reconnected in 512ms, replayed 4 events, no gaps'),
  ],
};

// ---------------------------------------------------------------------------
// Mission registry (canned demo + hosted M2.5 missions)
// ---------------------------------------------------------------------------

// Each mission record: { id, state, events, seq, transcripts, plan,
//   hostedHere, planRequests, turns, inFlight }
const demoMission = {
  id: MISSION_ID,
  state,
  events,
  seq,
  transcripts,
  plan,
  hostedHere: false, // planning endpoints 409 — "planned from a terminal"
  planRequests: 0,
  turns: 0,
  inFlight: false,
};

/** id → every non-demo mission (hosted ones from POST /api/missions, plus
 *  the canned terminal-planned mission below). */
const extraMissions = new Map();
let hostedCounter = 0;

function findMission(id) {
  if (id === MISSION_ID) return demoMission;
  return extraMissions.get(id) ?? null;
}

function allMissions() {
  return [demoMission, ...extraMissions.values()];
}

// A planning mission NOT hosted here — as if `kranz plan` runs in a terminal.
// Planning mutations 409 ("not hosted") so the dashboard shows its notice;
// the conversation still streams read-only from the event log.
const termMission = {
  id: 'm-term-01',
  state: {
    mission: {
      id: 'm-term-01',
      goal: 'Migrate the config loader to TOML',
      validationContract: [],
      milestones: [],
      status: 'planning',
      createdAt: at(20),
      baseBranch: 'main',
      missionBranch: 'kranz/mission-m-term-01',
    },
    runs: {
      'r-orch': {
        id: 'r-orch',
        role: 'orchestrator',
        sdkSessionId: 'mock-m-term-01-orch',
        model: 'opus',
        startedAt: at(19),
        tokens: { input: 12_000, output: 900, cacheRead: 0, cacheWrite: 0 },
        transcriptPath: 'runs/r-orch.jsonl',
        promptHash: 'sha256:mock',
      },
    },
    totals: { input: 12_000, output: 900, cacheRead: 0, cacheWrite: 0 },
    totalCostUsd: 0.09,
    pendingUserMessages: [],
    recentDecisions: [],
    config,
    lastSeq: 4,
  },
  events: [
    { seq: 1, ts: at(20), missionId: 'm-term-01', type: 'mission.created', payload: { goal: 'Migrate the config loader to TOML', baseBranch: 'main', missionBranch: 'kranz/mission-m-term-01', config } },
    { seq: 2, ts: at(19), missionId: 'm-term-01', type: 'worker.spawned', payload: { runId: 'r-orch', role: 'orchestrator', sdkSessionId: 'mock-m-term-01-orch', model: 'opus', promptHash: 'sha256:mock', transcriptPath: 'runs/r-orch.jsonl' } },
    { seq: 3, ts: at(18), missionId: 'm-term-01', type: 'user.message', payload: { text: 'Keep backwards compatibility with the JSON config.', interrupt: false } },
    { seq: 4, ts: at(17), missionId: 'm-term-01', type: 'worker.message', payload: { runId: 'r-orch', tag: 'text', content: 'Understood — the loader will read TOML first and fall back to JSON with a deprecation warning.' } },
  ],
  seq: 4,
  transcripts: {},
  plan: null,
  hostedHere: false,
  planRequests: 0,
  turns: 0,
  inFlight: false,
};
extraMissions.set(termMission.id, termMission);

// ---------------------------------------------------------------------------
// Live tail + control handling
// ---------------------------------------------------------------------------

const sockets = new Set(); // { sock, missionId }

function broadcast(missionId, frame) {
  const data = wsEncode(JSON.stringify(frame));
  for (const entry of sockets) {
    if (entry.missionId === missionId) entry.sock.write(data);
  }
}

/** Append a live event; broadcast it (+ a state re-fold for lifecycle events). */
function appendLive(m, type, payload) {
  const seqNo = ++m.seq;
  const event = { seq: seqNo, ts: new Date().toISOString(), missionId: m.id, type, payload };
  m.events.push(event);
  m.state.lastSeq = seqNo;
  broadcast(m.id, { type: 'event', seq: seqNo, event });
  if (type !== 'worker.message') {
    broadcast(m.id, { type: 'state', seq: seqNo, state: m.state });
  }
}

const TICKS = [
  ['text', 'Testing frame handling for snapshot/event/state.'],
  ['tool-use', 'Bash: npx vite build'],
  ['text', 'Ring buffer capped at 2000 events; oldest dropped cleanly.'],
  ['tool-use', 'Read: apps/dashboard/src/lib/ws.ts'],
];
let tick = 0;
setInterval(() => {
  const [tag, content] = TICKS[tick++ % TICKS.length];
  appendLive(demoMission, 'worker.message', { runId: 'r-3', tag, content });
}, 6000).unref();

function handleControl(m, cmd) {
  switch (cmd.kind) {
    case 'msg':
      m.state.pendingUserMessages.push(cmd.text);
      appendLive(m, 'user.message', { text: cmd.text, interrupt: Boolean(cmd.interrupt) });
      break;
    case 'pause':
      if (m.state.mission.status === 'running') {
        m.state.mission.status = 'paused';
        appendLive(m, 'mission.paused', {});
      }
      break;
    case 'resume':
      if (m.state.mission.status === 'paused') {
        m.state.mission.status = 'running';
        appendLive(m, 'mission.resumed', {});
      }
      break;
    case 'config-change':
      for (const [role, patch] of Object.entries(cmd.patch ?? {})) {
        if (m.state.config[role] && typeof patch === 'object' && patch !== null) {
          Object.assign(m.state.config[role], patch);
        }
      }
      appendLive(m, 'config.changed', { patch: cmd.patch ?? {} });
      break;
    default:
      break;
  }
}

// ---------------------------------------------------------------------------
// Mission lifecycle handlers (M2.5)
// ---------------------------------------------------------------------------

/** Deep-ish merge of a partial MissionConfig patch over the canned defaults. */
function mergeConfig(patch) {
  const merged = structuredClone(config);
  if (typeof patch !== 'object' || patch === null) return merged;
  for (const [key, value] of Object.entries(patch)) {
    if (typeof value === 'object' && value !== null && typeof merged[key] === 'object') {
      Object.assign(merged[key], value);
    } else {
      merged[key] = value;
    }
  }
  return merged;
}

function createHostedMission(goal, configPatch) {
  hostedCounter += 1;
  const id = `m-web-${String(hostedCounter).padStart(2, '0')}`;
  const m = {
    id,
    state: {
      mission: {
        id,
        goal,
        validationContract: [],
        milestones: [],
        status: 'planning',
        createdAt: new Date().toISOString(),
        baseBranch: 'main',
        missionBranch: `kranz/mission-${id}`,
      },
      runs: {},
      totals: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
      totalCostUsd: 0,
      pendingUserMessages: [],
      recentDecisions: [],
      config: mergeConfig(configPatch),
      lastSeq: 0,
    },
    events: [],
    seq: 0,
    transcripts: {},
    plan: null, // set on approve
    hostedHere: true,
    planRequests: 0,
    turns: 0,
    inFlight: false,
  };
  appendLive(m, 'mission.created', {
    goal,
    baseBranch: 'main',
    missionBranch: m.state.mission.missionBranch,
    config: m.state.config,
  });
  extraMissions.set(id, m);
  return m;
}

/** 409 body when a planning mutation cannot run; null when it can. */
function planningGate(m) {
  if (!m.hostedHere) return `mission '${m.id}' is not hosted by this server`;
  if (m.state.mission.status !== 'planning') return 'mission is not in planning';
  if (m.inFlight) return 'a planning turn is already in flight';
  return null;
}

/** Lazily create the hosted planning orchestrator run (r-orch). */
function ensureOrchRun(m) {
  if (m.state.runs['r-orch']) return;
  m.state.runs['r-orch'] = {
    id: 'r-orch',
    role: 'orchestrator',
    sdkSessionId: `mock-${m.id}-orch`,
    model: m.state.config.orchestrator.model,
    startedAt: new Date().toISOString(),
    tokens: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    transcriptPath: 'runs/r-orch.jsonl',
    promptHash: 'sha256:mock',
  };
  appendLive(m, 'worker.spawned', {
    runId: 'r-orch',
    role: 'orchestrator',
    sdkSessionId: `mock-${m.id}-orch`,
    model: m.state.config.orchestrator.model,
    promptHash: 'sha256:mock',
    transcriptPath: 'runs/r-orch.jsonl',
  });
}

const PLANNING_REPLIES = [
  'Understood. Before I draft the plan, two questions:\n\n1. Is there an existing test command the validation contract should gate on?\n2. Any part of the goal that is explicitly out of scope?',
  'Good — noted. I would split this into a **foundation** milestone and a **hardening** milestone. Anything you want pulled forward?',
  'Noted. The scope feels settled — request the plan whenever you are ready.',
];

function handlePlanningTurn(m, text, res) {
  ensureOrchRun(m);
  m.inFlight = true;
  appendLive(m, 'user.message', { text, interrupt: false });
  const reply = PLANNING_REPLIES[Math.min(m.turns, PLANNING_REPLIES.length - 1)];
  m.turns += 1;
  // Simulated model latency so the dashboard's busy/queue affordance shows.
  setTimeout(() => {
    appendLive(m, 'worker.message', { runId: 'r-orch', tag: 'text', content: reply });
    m.inFlight = false;
    sendJson(res, 200, { reply });
  }, 900);
}

const NOT_READY_REPLY =
  'Not ready yet — one thing first: should the functional validator run the FULL test suite ' +
  'per milestone, or a smoke subset? Answer and request the plan again.';

function mockPlanFor(goal) {
  return {
    goal,
    validationContract: [
      { id: 'a-1', statement: 'npm test passes', check: 'command', command: 'npm test' },
      {
        id: 'a-2',
        statement: 'The goal works end-to-end as described',
        check: 'agent-judgement',
      },
    ],
    milestones: [
      {
        title: 'Foundation',
        features: [
          {
            title: 'Scaffolding',
            spec: 'module layout, wiring, config plumbing',
            validationCriteria: ['builds clean'],
          },
          {
            title: 'Core behavior',
            spec: 'implement the primary flow of the goal',
            validationCriteria: ['happy path works end-to-end'],
          },
        ],
      },
      {
        title: 'Hardening',
        features: [
          {
            title: 'Edge cases and tests',
            spec: 'cover failure modes with regression tests',
            validationCriteria: ['npm test passes', 'no known gaps left'],
          },
        ],
      },
    ],
  };
}

const MOCK_ESTIMATE = {
  workerRuns: 3.8,
  validatorRuns: 4,
  lowUsd: 2.1,
  expectedUsd: 4.2,
  highUsd: 10.5,
};

function handleRequestPlan(m, res) {
  m.inFlight = true;
  m.planRequests += 1;
  const notReady = m.planRequests === 1;
  setTimeout(() => {
    m.inFlight = false;
    if (notReady) {
      appendLive(m, 'worker.message', { runId: 'r-orch', tag: 'text', content: NOT_READY_REPLY });
      sendJson(res, 200, { ready: false, reply: NOT_READY_REPLY });
    } else {
      sendJson(res, 200, { ready: true, plan: mockPlanFor(m.state.mission.goal), estimate: MOCK_ESTIMATE });
    }
  }, 700);
}

function handleApprove(m, plan, res) {
  m.plan = plan;
  m.state.mission.validationContract = plan.validationContract ?? [];
  m.state.mission.milestones = (plan.milestones ?? []).map((ms, mi) => ({
    id: `ms-${mi + 1}`,
    title: ms.title,
    status: 'pending',
    fixCycles: 0,
    features: (ms.features ?? []).map((f, fi) => ({
      id: `f-${mi + 1}-${fi + 1}`,
      title: f.title,
      spec: f.spec,
      validationCriteria: f.validationCriteria ?? [],
      origin: 'plan',
      status: 'pending',
      workerRuns: [],
      commits: [],
      respawns: 0,
    })),
  }));
  appendLive(m, 'plan.approved', { plan });
  sendJson(res, 200, { branch: m.state.mission.missionBranch });
}

/** start: 202, then walk the mission running → complete over a few ticks. */
function handleStart(m, res) {
  m.state.mission.status = 'running';
  appendLive(m, 'orchestrator.decision', {
    summary: 'Execution started: walking milestones sequentially.',
  });
  sendJson(res, 202, { running: true });

  const steps = [];
  for (const [mi, ms] of m.state.mission.milestones.entries()) {
    steps.push(() => {
      ms.status = 'active';
      appendLive(m, 'milestone.started', { milestoneId: ms.id, startSha: `mock${mi}00` });
    });
    for (const f of ms.features) {
      steps.push(() => {
        f.status = 'active';
        appendLive(m, 'feature.started', { featureId: f.id });
      });
      steps.push(() => {
        appendLive(m, 'worker.message', {
          runId: 'r-orch',
          tag: 'text',
          content: `Working on ${f.id}: ${f.title}…`,
        });
      });
      steps.push(() => {
        f.status = 'complete';
        appendLive(m, 'feature.completed', { featureId: f.id, commits: [`c${f.id}`] });
      });
    }
    steps.push(() => {
      ms.status = 'complete';
      appendLive(m, 'milestone.completed', { milestoneId: ms.id });
    });
  }
  steps.push(() => {
    m.state.mission.status = 'complete';
    appendLive(m, 'mission.completed', {});
  });

  let i = 0;
  const timer = setInterval(() => {
    if (i >= steps.length) {
      clearInterval(timer);
      return;
    }
    steps[i++]();
  }, 1200);
  timer.unref();
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.json': 'application/json',
  '.map': 'application/json',
  '.ico': 'image/x-icon',
};

function sendJson(res, code, body) {
  const data = JSON.stringify(body);
  res.writeHead(code, {
    'content-type': 'application/json',
    'access-control-allow-origin': '*',
    'access-control-allow-headers': 'content-type, x-kranz-token',
    'access-control-allow-methods': 'GET, POST, OPTIONS',
  });
  res.end(data);
}

function readJsonBody(req) {
  return new Promise((resolve, reject) => {
    let body = '';
    req.on('data', (chunk) => (body += chunk));
    req.on('end', () => {
      try {
        resolve(body === '' ? {} : JSON.parse(body));
      } catch (err) {
        reject(err);
      }
    });
    req.on('error', reject);
  });
}

function serveStatic(res, pathname) {
  if (!existsSync(DIST)) {
    res.writeHead(200, { 'content-type': 'text/plain' });
    res.end('kranz mock server: build the dashboard first (npm run build in apps/dashboard)\n');
    return;
  }
  let rel = normalize(pathname).replace(/^([/\\]|\.\.)+/, '');
  if (rel === '' || rel === '.') rel = 'index.html';
  let file = join(DIST, rel);
  if (!existsSync(file)) file = join(DIST, 'index.html'); // SPA fallback
  const type = MIME[extname(file)] ?? 'application/octet-stream';
  res.writeHead(200, { 'content-type': type });
  res.end(readFileSync(file));
}

const server = createServer((req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  const path = url.pathname;

  if (req.method === 'OPTIONS') {
    sendJson(res, 204, {});
    return;
  }

  // Authority (docs/protocol.md): every POST /api/... needs the mutation
  // token. The mock accepts ANY non-empty x-kranz-token value.
  if (req.method === 'POST' && path.startsWith('/api/')) {
    const token = req.headers['x-kranz-token'];
    if (typeof token !== 'string' || token.trim() === '') {
      sendJson(res, 401, { error: 'missing or invalid token' });
      return;
    }
  }

  if (path === '/api/health') {
    sendJson(res, 200, { ok: true, version: 'mock-0.1.0' });
    return;
  }
  if (path === '/api/missions') {
    if (req.method === 'POST') {
      readJsonBody(req)
        .then((body) => {
          if (typeof body.goal !== 'string' || body.goal.trim() === '') {
            sendJson(res, 400, { error: 'goal is required' });
            return;
          }
          const m = createHostedMission(body.goal.trim(), body.config);
          sendJson(res, 201, { id: m.id });
        })
        .catch(() => sendJson(res, 400, { error: 'bad JSON body' }));
      return;
    }
    sendJson(
      res,
      200,
      allMissions().map((m) => ({
        id: m.id,
        status: m.state.mission.status,
        goal: m.state.mission.goal,
        createdAt: m.state.mission.createdAt,
      })),
    );
    return;
  }

  const mission = path.match(/^\/api\/missions\/([^/]+)(\/.*)?$/);
  if (mission) {
    const m = findMission(decodeURIComponent(mission[1]));
    if (m === null) {
      sendJson(res, 404, { error: 'unknown mission' });
      return;
    }
    const rest = mission[2] ?? '';
    if (rest === '/state') {
      sendJson(res, 200, m.state);
      return;
    }
    if (rest === '/events') {
      const since = url.searchParams.get('since');
      const from = since === null ? 0 : Number(since);
      sendJson(res, 200, m.events.filter((e) => e.seq > from));
      return;
    }
    if (rest === '/plan') {
      if (m.plan) sendJson(res, 200, m.plan);
      else sendJson(res, 404, { error: 'no approved plan yet' });
      return;
    }
    const run = rest.match(/^\/runs\/([^/]+)\/transcript$/);
    if (run) {
      const t = m.transcripts[decodeURIComponent(run[1])];
      if (t) sendJson(res, 200, t);
      else sendJson(res, 404, { error: 'no transcript' });
      return;
    }
    if (rest === '/control' && req.method === 'POST') {
      readJsonBody(req)
        .then((cmd) => {
          handleControl(m, cmd);
          sendJson(res, 202, { queued: true });
        })
        .catch(() => sendJson(res, 400, { error: 'bad control command' }));
      return;
    }

    // --- mission lifecycle (M2.5) -----------------------------------------
    if (rest === '/planning/turn' && req.method === 'POST') {
      const gate = planningGate(m);
      if (gate !== null) {
        sendJson(res, 409, { error: gate });
        return;
      }
      readJsonBody(req)
        .then((body) => {
          if (typeof body.text !== 'string' || body.text.trim() === '') {
            sendJson(res, 400, { error: 'text is required' });
            return;
          }
          handlePlanningTurn(m, body.text, res);
        })
        .catch(() => sendJson(res, 400, { error: 'bad JSON body' }));
      return;
    }
    if (rest === '/planning/request-plan' && req.method === 'POST') {
      const gate = planningGate(m);
      if (gate !== null) {
        sendJson(res, 409, { error: gate });
        return;
      }
      handleRequestPlan(m, res);
      return;
    }
    if (rest === '/approve' && req.method === 'POST') {
      const gate = planningGate(m);
      if (gate !== null) {
        sendJson(res, 409, { error: gate });
        return;
      }
      readJsonBody(req)
        .then((body) => {
          if (typeof body.plan !== 'object' || body.plan === null) {
            sendJson(res, 400, { error: 'plan is required' });
            return;
          }
          handleApprove(m, body.plan, res);
        })
        .catch(() => sendJson(res, 400, { error: 'bad JSON body' }));
      return;
    }
    if (rest === '/start' && req.method === 'POST') {
      if (!m.hostedHere) {
        sendJson(res, 409, { error: `mission '${m.id}' is not hosted by this server` });
        return;
      }
      if (m.state.mission.status === 'running') {
        sendJson(res, 409, { error: 'mission is already running' });
        return;
      }
      if (!m.plan) {
        sendJson(res, 409, { error: 'no approved plan — approve before starting' });
        return;
      }
      handleStart(m, res);
      return;
    }

    sendJson(res, 404, { error: 'not found' });
    return;
  }

  serveStatic(res, path);
});

// ---------------------------------------------------------------------------
// WebSocket (hand-rolled, text frames only)
// ---------------------------------------------------------------------------

const WS_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

function wsEncode(text) {
  const payload = Buffer.from(text);
  const len = payload.length;
  let header;
  if (len < 126) {
    header = Buffer.from([0x81, len]);
  } else if (len < 65_536) {
    header = Buffer.alloc(4);
    header[0] = 0x81;
    header[1] = 126;
    header.writeUInt16BE(len, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x81;
    header[1] = 127;
    header.writeBigUInt64BE(BigInt(len), 2);
  }
  return Buffer.concat([header, payload]);
}

/** Decode client→server frames (always masked). Returns [{opcode, text}]. */
function wsDecode(buf) {
  const frames = [];
  let off = 0;
  while (off + 2 <= buf.length) {
    const opcode = buf[off] & 0x0f;
    let len = buf[off + 1] & 0x7f;
    const masked = (buf[off + 1] & 0x80) !== 0;
    let p = off + 2;
    if (len === 126) {
      len = buf.readUInt16BE(p);
      p += 2;
    } else if (len === 127) {
      len = Number(buf.readBigUInt64BE(p));
      p += 8;
    }
    const mask = masked ? buf.subarray(p, p + 4) : null;
    if (masked) p += 4;
    if (p + len > buf.length) break; // partial frame
    const payload = Buffer.from(buf.subarray(p, p + len));
    if (mask) for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i % 4];
    frames.push({ opcode, text: payload.toString('utf8') });
    off = p + len;
  }
  return [frames, off];
}

server.on('upgrade', (req, socket) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  const match = url.pathname.match(/^\/api\/missions\/([^/]+)\/ws$/);
  const m = match ? findMission(decodeURIComponent(match[1])) : null;
  if (m === null) {
    socket.destroy();
    return;
  }
  const accept = createHash('sha1')
    .update(req.headers['sec-websocket-key'] + WS_GUID)
    .digest('base64');
  socket.write(
    'HTTP/1.1 101 Switching Protocols\r\n' +
      'Upgrade: websocket\r\nConnection: Upgrade\r\n' +
      `Sec-WebSocket-Accept: ${accept}\r\n\r\n`,
  );
  const entry = { sock: socket, missionId: m.id };
  sockets.add(entry);

  // Protocol: ?since with a small gap → replay events (no snapshot);
  // otherwise a fresh snapshot at head.
  const sinceParam = url.searchParams.get('since');
  const since = sinceParam === null ? NaN : Number(sinceParam);
  if (Number.isFinite(since) && since <= m.seq && m.seq - since <= 5000) {
    for (const e of m.events) {
      if (e.seq > since) socket.write(wsEncode(JSON.stringify({ type: 'event', seq: e.seq, event: e })));
    }
  } else {
    socket.write(wsEncode(JSON.stringify({ type: 'snapshot', seq: m.seq, state: m.state })));
  }

  let pending = Buffer.alloc(0);
  socket.on('data', (chunk) => {
    pending = Buffer.concat([pending, chunk]);
    const [frames, consumed] = wsDecode(pending);
    pending = pending.subarray(consumed);
    for (const frame of frames) {
      if (frame.opcode === 0x8) {
        socket.end();
      } else if (frame.opcode === 0x9) {
        socket.write(Buffer.from([0x8a, 0x00])); // ws-level ping → pong
      } else if (frame.opcode === 0x1) {
        try {
          if (JSON.parse(frame.text).type === 'ping') {
            socket.write(wsEncode(JSON.stringify({ type: 'pong' })));
          }
        } catch {
          /* ignore */
        }
      }
    }
  });
  const drop = () => sockets.delete(entry);
  socket.on('close', drop);
  socket.on('error', drop);
});

server.listen(PORT, '127.0.0.1', () => {
  console.log(`kranz mock server: http://127.0.0.1:${PORT} (mission ${MISSION_ID})`);
  console.log(`static dir: ${DIST} ${existsSync(DIST) ? '(serving)' : '(missing — API only)'}`);
});
