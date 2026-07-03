// Zustand store — the single client-side source of truth for the dashboard.
//
// `state` frames from the server replace MissionState wholesale (the Rust
// reducer is the single source of truth; the UI is a pure view). `event`
// frames append to a capped ring buffer used by the log/conversation views.

import { create } from 'zustand';
import { api, httpOrigin, isNotHosted } from './api';
import { MissionSocket } from './ws';
import type {
  ConnectionStatus,
  ControlCommand,
  CostEstimate,
  MissionEvent,
  MissionState,
  MissionSummary,
  Plan,
  WsFrame,
} from './types';

export const EVENT_BUFFER_CAP = 2000;

// ---------------------------------------------------------------------------
// Planning (server-hosted lifecycle; M2.5)
// ---------------------------------------------------------------------------

/** Chat entries that only this client knows about (optimistic user turns and
 *  POST replies). The WS feed remains authoritative: PlanningView hides a
 *  local item once an identical event arrives. */
export interface PlanningLocalItem {
  id: number;
  kind: 'user' | 'reply' | 'notice';
  ts: string;
  text: string;
}

export type PlanningBusy = 'turn' | 'plan-request' | null;

export interface PlanningSlice {
  localItems: PlanningLocalItem[];
  /** What the engine is busy doing (mirrors the TUI's BusyKind), or null. */
  busy: PlanningBusy;
  /** Epoch ms when the in-flight request started (for the elapsed counter). */
  busySince: number | null;
  /** Client-side queue: typing is safe — sends when the current turn finishes. */
  queued: string[];
  /** 409 "not hosted": this mission is being planned from a terminal. */
  notHosted: boolean;
  /** request-plan came back ready:true — the PlanReview panel takes over. */
  review: { plan: Plan; estimate: CostEstimate } | null;
  /** Branch returned by approve; consent #2 (start) is still pending. */
  approvedBranch: string | null;
  /** True while POST start is in flight. */
  starting: boolean;
  error: string | null;
}

const PLANNING_RESET: PlanningSlice = {
  localItems: [],
  busy: null,
  busySince: null,
  queued: [],
  notHosted: false,
  review: null,
  approvedBranch: null,
  starting: false,
  error: null,
};

let nextLocalId = 1;

interface KranzStore {
  missions: MissionSummary[];
  missionsError: string | null;
  missionId: string | null;
  state: MissionState | null;
  events: MissionEvent[];
  /**
   * Uncapped mission.paused / mission.resumed history, kept apart from the
   * capped `events` ring so elapsed-time pause accounting never loses spans
   * to eviction. Seeded from the REST event fetch, appended on matching WS
   * frames, deduped by seq.
   */
  pauseEvents: MissionEvent[];
  connection: ConnectionStatus;
  selectedRun: string | null;
  /** Last seq seen over the wire (frames or seeded events). */
  lastSeq: number | null;
  planning: PlanningSlice;

  loadMissions: () => Promise<void>;
  connectMission: (id: string) => void;
  disconnect: () => void;
  selectRun: (runId: string | null) => void;
  sendControl: (command: ControlCommand) => Promise<void>;

  /** Enter send in the planning composer: runs a turn, or queues it while one
   *  is in flight (typing is safe — sends when the current turn finishes). */
  sendPlanningMessage: (text: string) => void;
  /** POST planning/request-plan — ready:false flows back into the chat,
   *  ready:true opens the PlanReview panel. */
  requestPlan: () => void;
  /** Consent #1: POST approve with the reviewed plan; stores the branch. */
  approvePlan: () => void;
  /** Consent #2: POST start; the live mission view takes over via WS. */
  startMission: () => void;
  /** Back to conversation from the review panel (either step). */
  planningBack: () => void;
}

let socket: MissionSocket | null = null;

function capEvents(events: MissionEvent[]): MissionEvent[] {
  return events.length > EVENT_BUFFER_CAP
    ? events.slice(events.length - EVENT_BUFFER_CAP)
    : events;
}

/** Merge seq-ordered batches, dropping duplicates (REST seed vs WS replay). */
function mergeEvents(a: MissionEvent[], b: MissionEvent[]): MissionEvent[] {
  const bySeq = new Map<number, MissionEvent>();
  for (const e of a) bySeq.set(e.seq, e);
  for (const e of b) bySeq.set(e.seq, e);
  return capEvents([...bySeq.values()].sort((x, y) => x.seq - y.seq));
}

export function isPauseEvent(e: MissionEvent): boolean {
  return e.type === 'mission.paused' || e.type === 'mission.resumed';
}

/**
 * Fold pause/resume events from `incoming` into the uncapped `existing`
 * list: seq-deduped, seq-ordered. Returns `existing` unchanged (same
 * reference) when `incoming` adds nothing, so subscribers don't re-render.
 */
export function mergePauseEvents(
  existing: MissionEvent[],
  incoming: MissionEvent[],
): MissionEvent[] {
  const bySeq = new Map<number, MissionEvent>();
  for (const e of existing) bySeq.set(e.seq, e);
  let added = false;
  for (const e of incoming) {
    if (isPauseEvent(e) && !bySeq.has(e.seq)) {
      bySeq.set(e.seq, e);
      added = true;
    }
  }
  if (!added) return existing;
  return [...bySeq.values()].sort((x, y) => x.seq - y.seq);
}

/** Web adaptation of the TUI's PLAN_NOT_READY_NOTICE ("/plan" → the button). */
export const PLAN_NOT_READY_NOTICE = 'not ready to emit — answer above, then request the plan again';

export const useKranzStore = create<KranzStore>()((set, get) => {
  function patchPlanning(partial: Partial<PlanningSlice>): void {
    set((s) => ({ planning: { ...s.planning, ...partial } }));
  }

  function pushLocal(kind: PlanningLocalItem['kind'], text: string): void {
    set((s) => ({
      planning: {
        ...s.planning,
        localItems: [
          ...s.planning.localItems,
          { id: nextLocalId++, kind, ts: new Date().toISOString(), text },
        ],
      },
    }));
  }

  /** Common failure path: 409 "not hosted" flips the terminal-planning
   *  notice; anything else surfaces as a plain error line. */
  function failPlanning(err: unknown): void {
    if (isNotHosted(err)) {
      patchPlanning({ notHosted: true, busy: null, busySince: null, queued: [] });
      return;
    }
    patchPlanning({
      error: err instanceof Error ? err.message : String(err),
      busy: null,
      busySince: null,
    });
  }

  /** Send queued messages in order once the current turn finishes. */
  function drainPlanningQueue(): void {
    const p = get().planning;
    if (p.busy !== null || p.notHosted || p.queued.length === 0) return;
    const [next, ...rest] = p.queued;
    patchPlanning({ queued: rest });
    void runPlanningTurn(next);
  }

  async function runPlanningTurn(text: string): Promise<void> {
    const id = get().missionId;
    if (id === null) return;
    patchPlanning({ busy: 'turn', busySince: Date.now(), error: null });
    pushLocal('user', text);
    try {
      const { reply } = await api.planningTurn(id, text);
      if (get().missionId !== id) return;
      if (reply.trim() !== '') pushLocal('reply', reply);
      patchPlanning({ busy: null, busySince: null });
      drainPlanningQueue();
    } catch (err) {
      if (get().missionId !== id) return;
      failPlanning(err);
    }
  }

  function applyFrame(frame: WsFrame): void {
    switch (frame.type) {
      case 'snapshot':
      case 'state':
        set((s) => ({
          state: frame.state,
          lastSeq: Math.max(s.lastSeq ?? 0, frame.seq),
        }));
        break;
      case 'event':
        set((s) => {
          const last = s.events.length > 0 ? s.events[s.events.length - 1].seq : 0;
          const events =
            frame.event.seq > last
              ? capEvents([...s.events, frame.event])
              : mergeEvents(s.events, [frame.event]);
          const pauseEvents = isPauseEvent(frame.event)
            ? mergePauseEvents(s.pauseEvents, [frame.event])
            : s.pauseEvents;
          return { events, pauseEvents, lastSeq: Math.max(s.lastSeq ?? 0, frame.seq) };
        });
        break;
      case 'pong':
        break;
    }
  }

  return {
    missions: [],
    missionsError: null,
    missionId: null,
    state: null,
    events: [],
    pauseEvents: [],
    connection: 'connecting',
    selectedRun: null,
    lastSeq: null,
    planning: PLANNING_RESET,

    loadMissions: async () => {
      set({ missionsError: null });
      try {
        const missions = await api.missions();
        set({ missions });
      } catch (err) {
        set({ missionsError: err instanceof Error ? err.message : String(err) });
      }
    },

    connectMission: (id: string) => {
      if (get().missionId === id && socket) return;
      socket?.close();
      socket = null;
      set({
        missionId: id,
        state: null,
        events: [],
        pauseEvents: [],
        connection: 'connecting',
        selectedRun: null,
        lastSeq: null,
        planning: PLANNING_RESET,
      });

      // Seed the event log over REST so history predating the WS snapshot
      // is visible; overlaps with replayed frames are deduped by seq. Pause
      // events are folded into the uncapped pause history before the ring
      // cap can evict them.
      api
        .events(id)
        .then((seeded) => {
          if (get().missionId !== id) return;
          set((s) => ({
            events: mergeEvents(seeded, s.events),
            pauseEvents: mergePauseEvents(s.pauseEvents, seeded),
          }));
        })
        .catch(() => {
          /* the WS snapshot still gives us live state */
        });

      socket = new MissionSocket({
        origin: httpOrigin(),
        missionId: id,
        // Only ask for a replay once we hold a state to apply events onto;
        // otherwise request a fresh snapshot.
        getSince: () => (get().state !== null ? get().lastSeq : null),
        onFrame: applyFrame,
        onStatus: (connection) => {
          set({ connection });
          // Reconnects with ?since= replay events without a snapshot; re-fetch
          // the fold once so lifecycle changes missed offline aren't stale.
          if (connection === 'live' && get().state !== null) {
            api
              .missionState(id)
              .then((fresh) => {
                if (get().missionId !== id) return;
                set((s) =>
                  s.state === null || fresh.lastSeq >= s.state.lastSeq
                    ? { state: fresh, lastSeq: Math.max(s.lastSeq ?? 0, fresh.lastSeq) }
                    : {},
                );
                // A large-gap reconnect yields a fresh snapshot with no event
                // replay, which would leave a permanent hole in the ring
                // between the old buffer tail and the snapshot seq. Backfill
                // the missing range over REST; overlaps with any replayed
                // frames are deduped by seq and the ring cap still applies.
                const buffered = get().events;
                const bufTail = buffered.length > 0 ? buffered[buffered.length - 1].seq : 0;
                if (fresh.lastSeq > bufTail) {
                  return api.events(id, bufTail).then((missing) => {
                    if (get().missionId !== id) return;
                    set((s) => ({
                      events: mergeEvents(s.events, missing),
                      pauseEvents: mergePauseEvents(s.pauseEvents, missing),
                    }));
                  });
                }
              })
              .catch(() => {});
          }
        },
      });
      socket.connect();
    },

    disconnect: () => {
      socket?.close();
      socket = null;
      set({
        missionId: null,
        state: null,
        events: [],
        pauseEvents: [],
        selectedRun: null,
        lastSeq: null,
        planning: PLANNING_RESET,
      });
    },

    selectRun: (runId) => set({ selectedRun: runId }),

    sendControl: async (command) => {
      const id = get().missionId;
      if (!id) throw new Error('no mission selected');
      await api.control(id, command);
    },

    sendPlanningMessage: (text) => {
      const trimmed = text.trim();
      if (trimmed === '') return;
      const p = get().planning;
      if (p.notHosted) return;
      if (p.busy !== null) {
        // Typing is safe — the message queues and sends when the current
        // turn finishes (mirrors the TUI's busy affordance).
        patchPlanning({ queued: [...p.queued, trimmed] });
        return;
      }
      void runPlanningTurn(trimmed);
    },

    requestPlan: () => {
      const id = get().missionId;
      if (id === null) return;
      const p = get().planning;
      if (p.busy !== null || p.notHosted) return;
      patchPlanning({ busy: 'plan-request', busySince: Date.now(), error: null });
      api
        .requestPlan(id)
        .then((res) => {
          if (get().missionId !== id) return;
          if (res.ready) {
            patchPlanning({
              busy: null,
              busySince: null,
              review: { plan: res.plan, estimate: res.estimate },
            });
            return;
          }
          // NotReady returns to conversation: the orchestrator's prose flows
          // into the chat with a notice (no error styling).
          if (res.reply.trim() !== '') pushLocal('reply', res.reply);
          pushLocal('notice', PLAN_NOT_READY_NOTICE);
          patchPlanning({ busy: null, busySince: null });
          drainPlanningQueue();
        })
        .catch((err: unknown) => {
          if (get().missionId === id) failPlanning(err);
        });
    },

    approvePlan: () => {
      const id = get().missionId;
      const review = get().planning.review;
      if (id === null || review === null) return;
      patchPlanning({ error: null });
      api
        .approvePlan(id, review.plan)
        .then(({ branch }) => {
          if (get().missionId !== id) return;
          patchPlanning({ approvedBranch: branch });
        })
        .catch((err: unknown) => {
          if (get().missionId === id) failPlanning(err);
        });
    },

    startMission: () => {
      const id = get().missionId;
      if (id === null) return;
      patchPlanning({ starting: true, error: null });
      api
        .startMission(id)
        .then(() => {
          if (get().missionId !== id) return;
          // The WS state frame flips mission.status to running; the live
          // mission view takes over from there.
          patchPlanning({ starting: false, review: null, approvedBranch: null });
        })
        .catch((err: unknown) => {
          if (get().missionId !== id) return;
          patchPlanning({ starting: false });
          failPlanning(err);
        });
    },

    planningBack: () => patchPlanning({ review: null, approvedBranch: null, error: null }),
  };
});
