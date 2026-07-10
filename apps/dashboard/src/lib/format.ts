// Small formatting helpers shared across panels.

import type { AgentBackend, MissionEvent, ReasoningEffort, Role } from './types';

/** "12s" / "5m" / "3h" / "10d" style relative age. */
export function relTime(iso: string, nowMs: number = Date.now()): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return '?';
  const s = Math.max(0, Math.floor((nowMs - then) / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

/** "117:42:06" — unbounded hours, zero-padded minutes/seconds. */
export function fmtElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${pad(h)}:${pad(m)}:${pad(s)}`;
}

export function fmtCost(usd: number): string {
  return `$${usd.toFixed(2)}`;
}

/** 950 -> "950", 12_340 -> "12.3k", 4_200_000 -> "4.2M". */
export function fmtTokens(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(1).replace(/\.0$/, '')}k`;
  return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
}

const EFFORT_LEVELS: Record<ReasoningEffort, number> = {
  low: 1,
  medium: 2,
  high: 3,
  xhigh: 4,
  max: 5,
};

/** 1..5 filled pips for a reasoning effort; unknown strings render as 0. */
export function effortLevel(effort: string): number {
  return EFFORT_LEVELS[effort as ReasoningEffort] ?? 0;
}

export const EFFORT_OPTIONS: ReasoningEffort[] = ['low', 'medium', 'high', 'xhigh', 'max'];

export const BACKEND_OPTIONS: AgentBackend[] = ['claude', 'codex', 'droid'];

export const MODEL_PLACEHOLDERS: Record<AgentBackend, string> = {
  claude: 'opus · sonnet · haiku',
  codex: 'gpt-5-codex',
  droid: 'fable · accounts/fireworks/models/glm-5p2',
};

export function roleLabel(role: Role): string {
  switch (role) {
    case 'orchestrator':
      return 'Orchestrator';
    case 'worker':
      return 'Worker';
    case 'validator-scrutiny':
      return 'Scrutiny validator';
    case 'validator-functional':
      return 'Functional validator';
  }
}

export function truncate(text: string, max: number): string {
  const oneLine = text.replace(/\s+/g, ' ').trim();
  return oneLine.length > max ? `${oneLine.slice(0, max - 1)}…` : oneLine;
}

/**
 * Total milliseconds the mission has spent paused, derived from
 * mission.paused / mission.resumed pairs; an unmatched pause counts up to now.
 * Feed this the store's uncapped `pauseEvents` list, not the capped event
 * ring — eviction there would silently drop pause spans.
 */
export function pausedMs(events: MissionEvent[], nowMs: number): number {
  let total = 0;
  let pausedAt: number | null = null;
  for (const e of events) {
    if (e.type === 'mission.paused') {
      pausedAt = Date.parse(e.ts);
    } else if (e.type === 'mission.resumed' && pausedAt !== null) {
      total += Math.max(0, Date.parse(e.ts) - pausedAt);
      pausedAt = null;
    }
  }
  if (pausedAt !== null) total += Math.max(0, nowMs - pausedAt);
  return total;
}
