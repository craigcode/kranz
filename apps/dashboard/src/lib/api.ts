// Typed fetch wrappers for the Kranz REST protocol (docs/protocol.md).
// Base URL: window.__KRANZ_SERVER__ ?? '' — same-origin by default; the Tauri
// shell injects the variable when the embedded server runs on another port.
//
// Authority (docs/protocol.md "Authority: mutation token"): every POST sends
// the per-serve session token via x-kranz-token. A 401 parks the request on
// the token gate (lib/token.ts) — <TokenPrompt/> collects a pasted token and
// the request retries; cancelling rejects with a clear error.

import { awaitToken, resolveToken } from './token';
import type {
  ControlCommand,
  MissionEvent,
  MissionState,
  MissionSummary,
  Plan,
  PlanRequestResponse,
  TranscriptEntry,
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

async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(serverBase() + path);
  if (!res.ok) {
    throw new Error(`GET ${path} failed: ${res.status} ${res.statusText}`);
  }
  return (await res.json()) as T;
}

/**
 * POST with the mutation token. On 401 the request waits on the token gate
 * and retries with the newly pasted token (loops until success, a non-401
 * failure, or the user cancels the prompt).
 */
async function postJson<T>(path: string, body: unknown): Promise<T> {
  for (;;) {
    const token = resolveToken();
    const headers: Record<string, string> = { 'content-type': 'application/json' };
    if (token !== null) headers['x-kranz-token'] = token;
    const res = await fetch(serverBase() + path, {
      method: 'POST',
      headers,
      body: JSON.stringify(body),
    });
    if (res.status === 401) {
      await awaitToken(); // resolves when a token is provided; throws on cancel
      continue;
    }
    if (!res.ok) {
      throw await errorFrom(res, `POST ${path} failed: ${res.status} ${res.statusText}`);
    }
    return (await res.json()) as T;
  }
}

export const api = {
  missions(): Promise<MissionSummary[]> {
    return getJson('/api/missions');
  },

  missionState(id: string): Promise<MissionState> {
    return getJson(`/api/missions/${encodeURIComponent(id)}/state`);
  },

  events(id: string, since?: number): Promise<MissionEvent[]> {
    const q = since !== undefined ? `?since=${since}` : '';
    return getJson(`/api/missions/${encodeURIComponent(id)}/events${q}`);
  },

  plan(id: string): Promise<Plan> {
    return getJson(`/api/missions/${encodeURIComponent(id)}/plan`);
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

  approvePlan(id: string, plan: Plan): Promise<{ branch: string }> {
    return postJson(`/api/missions/${encodeURIComponent(id)}/approve`, { plan });
  },

  startMission(id: string): Promise<{ running: boolean }> {
    return postJson(`/api/missions/${encodeURIComponent(id)}/start`, {});
  },
};
