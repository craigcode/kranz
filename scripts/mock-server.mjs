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
// Live tail + control handling
// ---------------------------------------------------------------------------

const sockets = new Set();

function broadcast(frame) {
  const data = wsEncode(JSON.stringify(frame));
  for (const sock of sockets) sock.write(data);
}

/** Append a live event; broadcast it (+ a state re-fold for lifecycle events). */
function appendLive(type, payload) {
  const event = { seq: ++seq, ts: new Date().toISOString(), missionId: MISSION_ID, type, payload };
  events.push(event);
  state.lastSeq = seq;
  broadcast({ type: 'event', seq, event });
  if (type !== 'worker.message') {
    broadcast({ type: 'state', seq, state });
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
  appendLive('worker.message', { runId: 'r-3', tag, content });
}, 6000).unref();

function handleControl(cmd) {
  switch (cmd.kind) {
    case 'msg':
      state.pendingUserMessages.push(cmd.text);
      appendLive('user.message', { text: cmd.text, interrupt: Boolean(cmd.interrupt) });
      break;
    case 'pause':
      if (state.mission.status === 'running') {
        state.mission.status = 'paused';
        appendLive('mission.paused', {});
      }
      break;
    case 'resume':
      if (state.mission.status === 'paused') {
        state.mission.status = 'running';
        appendLive('mission.resumed', {});
      }
      break;
    case 'config-change':
      for (const [role, patch] of Object.entries(cmd.patch ?? {})) {
        if (state.config[role] && typeof patch === 'object' && patch !== null) {
          Object.assign(state.config[role], patch);
        }
      }
      appendLive('config.changed', { patch: cmd.patch ?? {} });
      break;
    default:
      break;
  }
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
    'access-control-allow-headers': 'content-type',
    'access-control-allow-methods': 'GET, POST, OPTIONS',
  });
  res.end(data);
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

  if (path === '/api/health') {
    sendJson(res, 200, { ok: true, version: 'mock-0.1.0' });
    return;
  }
  if (path === '/api/missions') {
    sendJson(res, 200, [
      {
        id: MISSION_ID,
        status: state.mission.status,
        goal: state.mission.goal,
        createdAt: state.mission.createdAt,
      },
    ]);
    return;
  }

  const mission = path.match(/^\/api\/missions\/([^/]+)(\/.*)?$/);
  if (mission) {
    if (decodeURIComponent(mission[1]) !== MISSION_ID) {
      sendJson(res, 404, { error: 'unknown mission' });
      return;
    }
    const rest = mission[2] ?? '';
    if (rest === '/state') {
      sendJson(res, 200, state);
      return;
    }
    if (rest === '/events') {
      const since = url.searchParams.get('since');
      const from = since === null ? 0 : Number(since);
      sendJson(res, 200, events.filter((e) => e.seq > from));
      return;
    }
    if (rest === '/plan') {
      sendJson(res, 200, plan);
      return;
    }
    const run = rest.match(/^\/runs\/([^/]+)\/transcript$/);
    if (run) {
      const t = transcripts[decodeURIComponent(run[1])];
      if (t) sendJson(res, 200, t);
      else sendJson(res, 404, { error: 'no transcript' });
      return;
    }
    if (rest === '/control' && req.method === 'POST') {
      let body = '';
      req.on('data', (chunk) => (body += chunk));
      req.on('end', () => {
        try {
          handleControl(JSON.parse(body));
          sendJson(res, 202, { queued: true });
        } catch {
          sendJson(res, 400, { error: 'bad control command' });
        }
      });
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
  if (!match || decodeURIComponent(match[1]) !== MISSION_ID) {
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
  sockets.add(socket);

  // Protocol: ?since with a small gap → replay events (no snapshot);
  // otherwise a fresh snapshot at head.
  const sinceParam = url.searchParams.get('since');
  const since = sinceParam === null ? NaN : Number(sinceParam);
  if (Number.isFinite(since) && since <= seq && seq - since <= 5000) {
    for (const e of events) {
      if (e.seq > since) socket.write(wsEncode(JSON.stringify({ type: 'event', seq: e.seq, event: e })));
    }
  } else {
    socket.write(wsEncode(JSON.stringify({ type: 'snapshot', seq, state })));
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
  const drop = () => sockets.delete(socket);
  socket.on('close', drop);
  socket.on('error', drop);
});

server.listen(PORT, '127.0.0.1', () => {
  console.log(`kranz mock server: http://127.0.0.1:${PORT} (mission ${MISSION_ID})`);
  console.log(`static dir: ${DIST} ${existsSync(DIST) ? '(serving)' : '(missing — API only)'}`);
});
