// Typed fetch wrappers for the Kranz REST protocol (docs/protocol.md).
// Base URL: window.__KRANZ_SERVER__ ?? '' — same-origin by default; the Tauri
// shell injects the variable when the embedded server runs on another port.
//
// Authority (docs/protocol.md "Authority: mutation token"): every POST sends
// the per-serve session token via x-kranz-token. A 401 parks the request on
// the token gate (lib/token.ts) — <TokenPrompt/> collects a pasted token and
// the request retries; cancelling rejects with a clear error.

import { awaitToken, resolveToken } from './token';
import { repoIdFromHash } from './routes';
import type {
  ControlCommand,
  DrainState,
  MissionEvent,
  MissionState,
  MissionSummary,
  Outcomes,
  Plan,
  PlanRequestResponse,
  PrHandoff,
  QueueState,
  ReadinessReport,
  RepoSummary,
  Ticket,
  TicketSummary,
  TranscriptEntry,
  WorkspaceSummary,
} from './types';

declare global {
  interface Window {
    __KRANZ_SERVER__?: string;
  }
}

export function serverBase(): string {
  return window.__KRANZ_SERVER__ ?? '';
}

/** http(s) origin used for both fetch and the ws:// URL derivation. */
export function httpOrigin(): string {
  return serverBase() || window.location.origin;
}

export function scopedApiPath(path: string, repoId = repoIdFromHash()): string {
  if (
    repoId === null ||
    path === '/api/repos' ||
    path.startsWith('/api/repos/') ||
    path === '/api/health' ||
    !path.startsWith('/api/')
  ) {
    return path;
  }
  return `/api/repos/${encodeURIComponent(repoId)}${path.slice('/api'.length)}`;
}

/** Error carrying the HTTP status + the server's `{"error":...}` text. */
export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
  }
}

/** True for 409 "not hosted" — this mission is being planned from a terminal. */
export function isNotHosted(err: unknown): boolean {
  return err instanceof ApiError && err.status === 409 && /not hosted/i.test(err.message);
}

/** True for a 401 surfaced by `{ tokenGate: false }` — a token is required
 *  but the caller opted out of parking on the token gate. */
export function isTokenRequired(err: unknown): boolean {
  return err instanceof ApiError && err.status === 401;
}

async function errorFrom(res: Response, fallback: string): Promise<ApiError> {
  let message = fallback;
  try {
    const body: unknown = await res.json();
    if (typeof body === 'object' && body !== null && 'error' in body) {
      const text = (body as { error: unknown }).error;
      if (typeof text === 'string' && text !== '') message = text;
    }
  } catch {
    /* keep the fallback */
  }
  return new ApiError(res.status, message);
}

const STALE_SERVE_MESSAGE = 'endpoint unavailable — server restart needed?';

/** Parses a successful response body, guarding against a stale `serve`
 *  returning the SPA's HTML fallback (200 text/html) for an /api route —
 *  `res.json()` would otherwise throw a raw JSON-parse SyntaxError. */
async function parseJsonBody<T>(res: Response): Promise<T> {
  const contentType = res.headers.get('content-type') ?? '';
  const text = await res.text();
  if (!contentType.includes('application/json')) {
    throw new ApiError(res.status, STALE_SERVE_MESSAGE);
  }
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new ApiError(res.status, STALE_SERVE_MESSAGE);
  }
}

export interface GetJsonOptions {
  /** false: reject a 401 immediately (see isTokenRequired) instead of parking
   *  on the token gate. For interval-driven background polls only — a poll
   *  tick must never (re)open the TokenPrompt or pile waiters onto the gate.
   *  User-initiated fetches keep the default gate behaviour. */
  tokenGate?: boolean;
}

export async function getJson<T>(path: string, opts?: GetJsonOptions): Promise<T> {
  // Off-loopback serves require the mutation token on GETs too (header or
  // ?token=). Always attach when we have one — loopback ignores it.
  // Capture repository scope once: a token retry after navigation must keep
  // targeting the operation's original repository.
  const requestPath = scopedApiPath(path);
  for (;;) {
    const token = resolveToken();
    const headers: Record<string, string> = {};
    if (token !== null) headers['x-kranz-token'] = token;
    const res = await fetch(serverBase() + requestPath, { headers });
    if (res.status === 401) {
      if (opts?.tokenGate === false) {
        throw await errorFrom(res, `GET ${path} failed: 401 token required`);
      }
      await awaitToken();
      continue;
    }
    if (!res.ok) {
      throw await errorFrom(res, `GET ${path} failed: ${res.status} ${res.statusText}`);
    }
    return parseJsonBody<T>(res);
  }
}

/**
 * POST with the mutation token. On 401 the request waits on the token gate
 * and retries with the newly pasted token (loops until success, a non-401
 * failure, or the user cancels the prompt).
 */
export async function postJson<T>(path: string, body: unknown): Promise<T> {
  // Same fixed-scope rule as GET: never retarget a parked mutation because
  // the operator selected another repository while entering a token.
  const requestPath = scopedApiPath(path);
  for (;;) {
    const token = resolveToken();
    const headers: Record<string, string> = { 'content-type': 'application/json' };
    if (token !== null) headers['x-kranz-token'] = token;
    const res = await fetch(serverBase() + requestPath, {
      method: 'POST',
      headers,
      body: JSON.stringify(body),
    });
    if (res.status === 401) {
      await awaitToken(); // clears stale token, then waits for a fresh paste
      continue;
    }
    if (!res.ok) {
      throw await errorFrom(res, `POST ${path} failed: ${res.status} ${res.statusText}`);
    }
    return parseJsonBody<T>(res);
  }
}

/** `GET /api/missions/outcomes` — flight-surgeon outcomes fold (autonomy
 *  ratio, grant-latency distribution, escalation ledger). */
export function getOutcomes(): Promise<Outcomes> {
  return getJson('/api/missions/outcomes');
}

export const api = {
  repos(): Promise<RepoSummary[]> {
    return getJson('/api/repos');
  },

  missions(): Promise<MissionSummary[]> {
    return getJson('/api/missions');
  },

  missionState(id: string): Promise<MissionState> {
    return getJson(`/api/missions/${encodeURIComponent(id)}/state`);
  },

  workspace(id: string): Promise<WorkspaceSummary> {
    return getJson(`/api/missions/${encodeURIComponent(id)}/workspace`);
  },

  events(id: string, since?: number): Promise<MissionEvent[]> {
    const q = since !== undefined ? `?since=${since}` : '';
    return getJson(`/api/missions/${encodeURIComponent(id)}/events${q}`);
  },

  plan(id: string): Promise<Plan> {
    return getJson(`/api/missions/${encodeURIComponent(id)}/plan`);
  },

  planMd(id: string): Promise<{ markdown: string }> {
    return getJson(`/api/missions/${encodeURIComponent(id)}/plan.md`);
  },

  reportMd(id: string): Promise<{ markdown: string }> {
    return getJson(`/api/missions/${encodeURIComponent(id)}/report.md`);
  },

  diffStat(id: string): Promise<{ diffStat: string; baseSha: string; tip: string }> {
    return getJson(`/api/missions/${encodeURIComponent(id)}/diff-stat`);
  },

  prHandoff(id: string): Promise<PrHandoff> {
    return getJson(`/api/missions/${encodeURIComponent(id)}/pr-handoff`);
  },

  createPr(id: string): Promise<{ url: string }> {
    return postJson(`/api/missions/${encodeURIComponent(id)}/pr-handoff/create`, {});
  },

  readiness(id: string): Promise<ReadinessReport> {
    return getJson(`/api/missions/${encodeURIComponent(id)}/readiness`);
  },

  revisionDiff(id: string): Promise<{
    revision: number;
    instructions: string;
    markdown: string;
    diff: string;
  }> {
    return getJson(`/api/missions/${encodeURIComponent(id)}/revision-diff`);
  },

  transcript(id: string, runId: string): Promise<TranscriptEntry[]> {
    return getJson(
      `/api/missions/${encodeURIComponent(id)}/runs/${encodeURIComponent(runId)}/transcript`,
    );
  },

  health(): Promise<{ ok: boolean; version: string }> {
    return getJson('/api/health');
  },

  async control(id: string, command: ControlCommand): Promise<void> {
    await postJson<{ queued: boolean }>(
      `/api/missions/${encodeURIComponent(id)}/control`,
      command,
    );
  },

  async requestRevision(id: string, instructions: string): Promise<void> {
    await postJson<{ queued: boolean }>(
      `/api/missions/${encodeURIComponent(id)}/revise`,
      { instructions },
    );
  },

  async approveRevision(id: string, revision: number): Promise<void> {
    await postJson<{ queued: boolean }>(
      `/api/missions/${encodeURIComponent(id)}/revision/approve`,
      { revision },
    );
  },

  async rejectRevision(id: string, revision: number): Promise<void> {
    await postJson<{ queued: boolean }>(
      `/api/missions/${encodeURIComponent(id)}/revision/reject`,
      { revision },
    );
  },

  async approveGrant(id: string, command: string): Promise<void> {
    await postJson<{ queued: boolean }>(
      `/api/missions/${encodeURIComponent(id)}/grant/approve`,
      { command },
    );
  },

  async denyGrant(id: string, command: string, reason?: string): Promise<void> {
    const body: { command: string; reason?: string } = { command };
    if (reason !== undefined) body.reason = reason;
    await postJson<{ queued: boolean }>(
      `/api/missions/${encodeURIComponent(id)}/grant/deny`,
      body,
    );
  },

  // --- mission lifecycle (server-hosted engine; M2.5) ----------------------

  createMission(goal: string, config?: Record<string, unknown>): Promise<{ id: string }> {
    const body: { goal: string; config?: Record<string, unknown> } = { goal };
    if (config !== undefined) body.config = config;
    return postJson('/api/missions', body);
  },

  planningTurn(id: string, text: string): Promise<{ reply: string }> {
    return postJson(`/api/missions/${encodeURIComponent(id)}/planning/turn`, { text });
  },

  requestPlan(id: string): Promise<PlanRequestResponse> {
    return postJson(`/api/missions/${encodeURIComponent(id)}/planning/request-plan`, {});
  },

  // --- mission hygiene (web twins of `kranz abandon` / `kranz clean`) ------

  abandonMission(id: string, reason?: string): Promise<{ abandoned: boolean }> {
    return postJson(
      `/api/missions/${encodeURIComponent(id)}/abandon`,
      reason !== undefined ? { reason } : {},
    );
  },

  /** `all` opts in to deleting a Complete mission (they feed cost calibration
   *  and are kept by default — same contract as `kranz clean --all`). */
  deleteMission(id: string, all: boolean): Promise<{ deleted: boolean }> {
    return postJson(`/api/missions/${encodeURIComponent(id)}/delete`, { all });
  },

  approvePending(id: string): Promise<{ branch: string; started: boolean }> {
    return postJson(`/api/missions/${encodeURIComponent(id)}/approve-pending`, { start: false });
  },

  startMission(id: string): Promise<{ running: boolean }> {
    return postJson(`/api/missions/${encodeURIComponent(id)}/start`, {});
  },

  // --- tickets (Pipeline Backlog lens) ---------------------------------------

  tickets(): Promise<TicketSummary[]> {
    return getJson('/api/tickets');
  },

  ticket(slug: string): Promise<Ticket> {
    return getJson(`/api/tickets/${encodeURIComponent(slug)}`);
  },

  createTicket(fields: {
    slug: string;
    title: string;
    goal?: string;
    context?: string;
  }): Promise<TicketSummary> {
    return postJson('/api/tickets', fields);
  },

  draftTicket(slug: string): Promise<{ missionId: string }> {
    return postJson(`/api/tickets/${encodeURIComponent(slug)}/draft`, {});
  },

  approveTicket(slug: string, force: boolean): Promise<{ approved: boolean; missionId: string }> {
    return postJson(`/api/tickets/${encodeURIComponent(slug)}/approve`, { force });
  },

  // --- queue (run-the-queue affordance) ------------------------------------

  queue(opts?: GetJsonOptions): Promise<QueueState> {
    return getJson('/api/queue', opts);
  },

  drainQueue(): Promise<DrainState> {
    return postJson('/api/queue/drain', {});
  },

  // --- delivered stage (gated Merge action) --------------------------------

  merge(id: string): Promise<{ merged: boolean; commit?: string }> {
    return postJson(`/api/missions/${encodeURIComponent(id)}/merge`, {});
  },
};
