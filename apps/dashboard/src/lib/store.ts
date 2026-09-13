// Zustand store — the single client-side source of truth for the dashboard.
//
// `state` frames from the server replace MissionState wholesale (the Rust
// reducer is the single source of truth; the UI is a pure view). `event`
// frames append to a capped ring buffer used by the log/conversation views.

import { create } from 'zustand';
import { api, httpOrigin, isNotHosted, isStalePlan } from './api';
import { repoIdFromHash } from './routes';
import { MissionSocket } from './ws';
import type {
  ConnectionStatus,
  ControlCommand,
  CostEstimate,
  MissionEvent,
  MissionState,
  MissionSummary,
  Plan,
  TicketSummary,
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
  review: { plan: Plan; planIdentity: string; estimate: CostEstimate } | null;
  /** True while POST approve-pending is in flight. */
  approving: boolean;
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
  approving: false,
  approvedBranch: null,
  starting: false,
  error: null,
};

let nextLocalId = 1;

interface KranzStore {
  repoId: string | null;
  missions: MissionSummary[];
  missionsError: string | null;
  tickets: TicketSummary[];
  ticketsError: string | null;
  ticketError: string | null;
  /** Slug with an in-flight draftTicket/approveTicket POST, or null. */
  ticketBusySlug: string | null;
  /** Slug of the ticket whose Draft action created the current mission
   *  connection, or null. The ticket record's own missionId only catches up
   *  on re-fetch (the server serializes undrafted tickets as missionId:null),
   *  so TicketDetail uses this to show the fresh draft's live feed right
   *  away — and only on that ticket's page. Cleared whenever the connection
   *  moves to a mission the draft didn't create (connectMission/disconnect). */
  draftingSlug: string | null;
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
  /** True while POST start is in flight for the mission page's Start action
   *  (StatusStrip). Distinct from planning.starting — that's PlanReview's
   *  consent-#2 start; this is starting an already-approved-idle mission. */
  startingRun: boolean;
  /** Host's rejection message for the mission-page Start action (e.g. a 409
   *  double-start conflict), or null. */
  startRunError: string | null;

  selectRepo: (repoId: string | null) => void;
  loadMissions: () => Promise<void>;
  /** Load the backlog list for the dashboard panel (`GET /api/tickets`). */
  loadTickets: () => Promise<void>;
  /** POST draft for a ticket, then connect to the returned mission's live
   *  feed via the existing `connectMission` action. Reloads tickets and
   *  missions so PipelineView stages stay current. */
  draftTicket: (slug: string) => Promise<void>;
  /** POST approve for a ticket; refreshes tickets and missions on success and
   *  surfaces the server's refusal message verbatim on failure. */
  approveTicket: (slug: string, force: boolean) => Promise<void>;
  connectMission: (id: string) => void;
  disconnect: () => void;
  selectRun: (runId: string | null) => void;
  sendControl: (command: ControlCommand) => Promise<void>;
  /** Web twin of `kranz abandon`: terminal-refusing, event-recorded. */
  abandonMission: (id: string) => Promise<void>;
  /** Web twin of `kranz clean` for ONE mission; `all` opts in to deleting a
   *  Complete mission (kept by default — they feed cost calibration). */
  deleteMission: (id: string, all: boolean) => Promise<void>;

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
  /** Mission-page Start action (StatusStrip): POST start for an
   *  approved-idle mission. Surfaces the host's conflict message on a race
   *  (e.g. a 409 double-start) rather than enforcing refusal client-side. */
  startRun: () => Promise<void>;
}

let socket: MissionSocket | null = null;
let repoGeneration = 0;
let missionGeneration = 0;
let missionsRequest = 0;
let ticketsRequest = 0;

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
  /** A mission id alone cannot distinguish A → B → A. Capture the connection
   *  lifetime so responses from an earlier visit cannot update this one. */
  function missionScope(): () => boolean {
    const repo = repoGeneration;
    const mission = missionGeneration;
    const id = get().missionId;
    return () => repoGeneration === repo && missionGeneration === mission && get().missionId === id;
  }

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
    const isCurrent = missionScope();
    patchPlanning({ busy: 'turn', busySince: Date.now(), error: null });
    pushLocal('user', text);
    try {
      const { reply } = await api.planningTurn(id, text);
      if (!isCurrent()) return;
      if (reply.trim() !== '') pushLocal('reply', reply);
      patchPlanning({ busy: null, busySince: null });
      drainPlanningQueue();
    } catch (err) {
      if (!isCurrent()) return;
      failPlanning(err);
    }
  }

  function applyFrame(frame: WsFrame): void {
    switch (frame.type) {
      case 'snapshot':
      case 'state':
        set((s) => {
          // A reconnect REST refresh can finish before the socket's older
          // initial snapshot arrives. Keep the newest authoritative fold.
          if (s.state !== null && frame.seq < s.state.lastSeq) return {};
          return { state: frame.state, lastSeq: Math.max(s.lastSeq ?? 0, frame.seq) };
        });
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
    repoId: null,
    missions: [],
    missionsError: null,
    tickets: [],
    ticketsError: null,
    ticketError: null,
    ticketBusySlug: null,
    draftingSlug: null,
    missionId: null,
    state: null,
    events: [],
    pauseEvents: [],
    connection: 'connecting',
    selectedRun: null,
    lastSeq: null,
    planning: PLANNING_RESET,
    startingRun: false,
    startRunError: null,

    selectRepo: (repoId: string | null) => {
      if (get().repoId === repoId) return;
      repoGeneration += 1;
      missionGeneration += 1;
      socket?.close();
      socket = null;
      set({
        repoId,
        missions: [],
        missionsError: null,
        tickets: [],
        ticketsError: null,
        ticketError: null,
        ticketBusySlug: null,
        draftingSlug: null,
        missionId: null,
        state: null,
        events: [],
        pauseEvents: [],
        connection: 'connecting',
        selectedRun: null,
        lastSeq: null,
        planning: PLANNING_RESET,
        startingRun: false,
        startRunError: null,
      });
    },

    loadMissions: async () => {
      const generation = repoGeneration;
      const request = ++missionsRequest;
      const connection = missionGeneration;
      set({ missionsError: null });
      // For the vanished-mission check below: only let the fresh list rule
      // on a connection that already existed when the fetch STARTED — a
      // mission connected mid-flight may legitimately postdate the list.
      const connectedAtStart = get().missionId;
      try {
        const missions = await api.missions();
        if (repoGeneration !== generation || missionsRequest !== request) return;
        set({ missions });
        // The connected mission vanishing from a fresh list means it was
        // deleted out-of-band (`kranz clean` in a terminal, another tab, or
        // a delete POST whose response was lost): tear the socket down, or
        // it reconnect-loops against the server's 404 forever.
        const id = get().missionId;
        if (
          missionGeneration === connection && id !== null && id === connectedAtStart
          && !missions.some((m) => m.id === id)
        ) {
          get().disconnect();
        }
      } catch (err) {
        if (repoGeneration !== generation || missionsRequest !== request) return;
        set({ missionsError: err instanceof Error ? err.message : String(err) });
      }
    },

    loadTickets: async () => {
      const generation = repoGeneration;
      const request = ++ticketsRequest;
      set({ ticketsError: null });
      try {
        const tickets = await api.tickets();
        if (repoGeneration !== generation || ticketsRequest !== request) return;
        set({ tickets });
      } catch (err) {
        if (repoGeneration !== generation || ticketsRequest !== request) return;
        set({ ticketsError: err instanceof Error ? err.message : String(err) });
      }
    },

    draftTicket: async (slug: string) => {
      if (get().ticketBusySlug !== null) return;
      set({ ticketError: null, ticketBusySlug: slug });
      const generation = repoGeneration;
      try {
        const { missionId } = await api.draftTicket(slug);
        if (repoGeneration !== generation) return;
        // connectMission resets draftingSlug (any prior draft's claim is
        // stale for a new connection), so record ours only after it runs.
        get().connectMission(missionId);
        set({ draftingSlug: slug });
        await Promise.all([get().loadTickets(), get().loadMissions()]);
      } catch (err) {
        if (repoGeneration !== generation) return;
        set({ ticketError: err instanceof Error ? err.message : String(err) });
      } finally {
        if (repoGeneration === generation && get().ticketBusySlug === slug) {
          set({ ticketBusySlug: null });
        }
      }
    },

    approveTicket: async (slug: string, force: boolean) => {
      if (get().ticketBusySlug !== null) return;
      set({ ticketError: null, ticketBusySlug: slug });
      const generation = repoGeneration;
      try {
        await api.approveTicket(slug, force);
        if (repoGeneration !== generation) return;
        await Promise.all([get().loadTickets(), get().loadMissions()]);
      } catch (err) {
        if (repoGeneration !== generation) return;
        set({ ticketError: err instanceof Error ? err.message : String(err) });
      } finally {
        if (repoGeneration === generation && get().ticketBusySlug === slug) {
          set({ ticketBusySlug: null });
        }
      }
    },

    abandonMission: async (id: string) => {
      const generation = repoGeneration;
      try {
        await api.abandonMission(id);
      } catch (err) {
        if (repoGeneration !== generation) return;
        set({ missionsError: err instanceof Error ? err.message : String(err) });
      }
      if (repoGeneration !== generation) return;
      await get().loadMissions();
    },

    deleteMission: async (id: string, all: boolean) => {
      const generation = repoGeneration;
      try {
        await api.deleteMission(id, all);
        if (repoGeneration !== generation) return;
        // Deleting the connected mission removes its dir server-side; the WS
        // would otherwise reconnect-loop against a 404 forever with stale
        // state (abandon keeps the dir, so it needs no such teardown).
        if (get().missionId === id) get().disconnect();
      } catch (err) {
        if (repoGeneration !== generation) return;
        set({ missionsError: err instanceof Error ? err.message : String(err) });
      }
      if (repoGeneration !== generation) return;
      await get().loadMissions();
    },

    connectMission: (id: string) => {
      if (get().missionId === id && socket) return;
      missionGeneration += 1;
      socket?.close();
      socket = null;
      set({
        draftingSlug: null,
        missionId: id,
        state: null,
        events: [],
        pauseEvents: [],
        connection: 'connecting',
        selectedRun: null,
        lastSeq: null,
        planning: PLANNING_RESET,
        startingRun: false,
        startRunError: null,
      });
      const isCurrent = missionScope();

      // Seed the event log over REST so history predating the WS snapshot
      // is visible; overlaps with replayed frames are deduped by seq. Pause
      // events are folded into the uncapped pause history before the ring
      // cap can evict them.
      api
        .events(id)
        .then((seeded) => {
          if (!isCurrent()) return;
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
        repoId: get().repoId ?? repoIdFromHash(),
        // Only ask for a replay once we hold a state to apply events onto;
        // otherwise request a fresh snapshot.
        getSince: () => (isCurrent() && get().state !== null ? get().lastSeq : null),
        onFrame: (frame) => {
          if (isCurrent()) applyFrame(frame);
        },
        onStatus: (connection) => {
          if (!isCurrent()) return;
          if (connection === 'gone') {
            // The socket proved the mission 404s (deleted out-of-band —
            // `kranz clean`, another tab) and stopped reconnecting for
            // good; drop the dead connection entirely.
            if (get().missionId === id) get().disconnect();
            return;
          }
          set({ connection });
          // Reconnects with ?since= replay events without a snapshot; re-fetch
          // the fold once so lifecycle changes missed offline aren't stale.
          if (connection === 'live' && get().state !== null) {
            api
              .missionState(id)
              .then((fresh) => {
                if (!isCurrent()) return;
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
                    if (!isCurrent()) return;
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
      missionGeneration += 1;
      socket?.close();
      socket = null;
      set({
        draftingSlug: null,
        missionId: null,
        state: null,
        events: [],
        pauseEvents: [],
        selectedRun: null,
        lastSeq: null,
        planning: PLANNING_RESET,
        startingRun: false,
        startRunError: null,
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
      const isCurrent = missionScope();
      patchPlanning({ busy: 'plan-request', busySince: Date.now(), error: null });
      api
        .requestPlan(id)
        .then((res) => {
          if (!isCurrent()) return;
          if (res.ready) {
            patchPlanning({
              busy: null,
              busySince: null,
              review: { plan: res.plan, planIdentity: res.planIdentity, estimate: res.estimate },
              approvedBranch: null,
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
          if (isCurrent()) failPlanning(err);
        });
    },

    approvePlan: () => {
      const id = get().missionId;
      const review = get().planning.review;
      if (id === null || review === null || get().planning.approving) return;
      if (!review.planIdentity) {
        patchPlanning({
          review: null,
          approvedBranch: null,
          error: 'Plan identity is missing. Request the plan again and review it before approving.',
        });
        return;
      }
      const isCurrent = missionScope();
      patchPlanning({ error: null, approving: true });
      api
        .approvePending(id, review.planIdentity)
        .then(({ branch }) => {
          if (!isCurrent()) return;
          patchPlanning({ approving: false, approvedBranch: branch });
        })
        .catch((err: unknown) => {
          if (!isCurrent()) return;
          if (isStalePlan(err)) {
            patchPlanning({ review: null, approvedBranch: null });
          }
          patchPlanning({ approving: false });
          failPlanning(err);
        });
    },

    startMission: () => {
      const id = get().missionId;
      if (id === null) return;
      const isCurrent = missionScope();
      patchPlanning({ starting: true, error: null });
      api
        .startMission(id)
        .then(() => {
          if (!isCurrent()) return;
          // The WS state frame flips mission.status to running; the live
          // mission view takes over from there.
          patchPlanning({ starting: false, review: null, approvedBranch: null });
        })
        .catch((err: unknown) => {
          if (!isCurrent()) return;
          patchPlanning({ starting: false });
          failPlanning(err);
        });
    },

    planningBack: () => patchPlanning({ review: null, approvedBranch: null, error: null }),

    startRun: async () => {
      const id = get().missionId;
      if (id === null) return;
      const isCurrent = missionScope();
      set({ startingRun: true, startRunError: null });
      try {
        await api.startMission(id);
        if (isCurrent()) {
          set({ startingRun: false });
        }
      } catch (err) {
        if (isCurrent()) {
          set({ startingRun: false, startRunError: err instanceof Error ? err.message : String(err) });
        }
      }
    },
  };
});
