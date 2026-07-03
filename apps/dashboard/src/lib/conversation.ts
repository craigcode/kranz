// Conversation projection of the event stream, shared by the live
// OrchestratorView and the planning chat: user.message bubbles,
// orchestrator.decision cards, and orchestrator-run worker.message text.

import type { MissionEvent } from './types';

export type ConvoItem =
  | { kind: 'user'; seq: number; ts: string; text: string; interrupt: boolean }
  | { kind: 'decision'; seq: number; ts: string; summary: string; detail?: string }
  | { kind: 'orch-text'; seq: number; ts: string; text: string };

export function buildConversation(events: MissionEvent[], orchRuns: Set<string>): ConvoItem[] {
  const items: ConvoItem[] = [];
  for (const e of events) {
    if (e.type === 'user.message') {
      items.push({
        kind: 'user',
        seq: e.seq,
        ts: e.ts,
        text: e.payload.text,
        interrupt: e.payload.interrupt,
      });
    } else if (e.type === 'orchestrator.decision') {
      items.push({
        kind: 'decision',
        seq: e.seq,
        ts: e.ts,
        summary: e.payload.summary,
        detail: e.payload.detail,
      });
    } else if (
      e.type === 'worker.message' &&
      e.payload.tag === 'text' &&
      orchRuns.has(e.payload.runId)
    ) {
      items.push({ kind: 'orch-text', seq: e.seq, ts: e.ts, text: e.payload.content });
    }
  }
  return items;
}
