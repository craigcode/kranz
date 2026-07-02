// Typed fetch wrappers for the Kranz REST protocol (docs/protocol.md).
// Base URL: window.__KRANZ_SERVER__ ?? '' — same-origin by default; the Tauri
// shell injects the variable when the embedded server runs on another port.

import type {
  ControlCommand,
  MissionEvent,
  MissionState,
  MissionSummary,
  Plan,
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

async function getJson<T>(path: string): Promise<T> {
  const res = await fetch(serverBase() + path);
  if (!res.ok) {
    throw new Error(`GET ${path} failed: ${res.status} ${res.statusText}`);
  }
  return (await res.json()) as T;
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
    const res = await fetch(
      `${serverBase()}/api/missions/${encodeURIComponent(id)}/control`,
      {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(command),
      },
    );
    if (!res.ok) {
      throw new Error(`control ${command.kind} failed: ${res.status}`);
    }
  },
};
