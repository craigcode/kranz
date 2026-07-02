// Zustand store — the single client-side source of truth for the dashboard.
//
// `state` frames from the server replace MissionState wholesale (the Rust
// reducer is the single source of truth; the UI is a pure view). `event`
// frames append to a capped ring buffer used by the log/conversation views.

import { create } from 'zustand';
import { api, httpOrigin } from './api';
import { MissionSocket } from './ws';
import type {
  ConnectionStatus,
  ControlCommand,
  MissionEvent,
  MissionState,
  MissionSummary,
  WsFrame,
} from './types';

export const EVENT_BUFFER_CAP = 2000;

interface KranzStore {
  missions: MissionSummary[];
  missionsError: string | null;
  missionId: string | null;
  state: MissionState | null;
  events: MissionEvent[];
  connection: ConnectionStatus;
  selectedRun: string | null;
  /** Last seq seen over the wire (frames or seeded events). */
  lastSeq: number | null;

  loadMissions: () => Promise<void>;
  connectMission: (id: string) => void;
  disconnect: () => void;
  selectRun: (runId: string | null) => void;
  sendControl: (command: ControlCommand) => Promise<void>;
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

export const useKranzStore = create<KranzStore>()((set, get) => {
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
          return { events, lastSeq: Math.max(s.lastSeq ?? 0, frame.seq) };
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
    connection: 'connecting',
    selectedRun: null,
    lastSeq: null,

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
        connection: 'connecting',
        selectedRun: null,
        lastSeq: null,
      });

      // Seed the event log over REST so history predating the WS snapshot
      // is visible; overlaps with replayed frames are deduped by seq.
      api
        .events(id)
        .then((seeded) => {
          if (get().missionId !== id) return;
          set((s) => ({ events: mergeEvents(seeded, s.events) }));
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
      set({ missionId: null, state: null, events: [], selectedRun: null, lastSeq: null });
    },

    selectRun: (runId) => set({ selectedRun: runId }),

    sendControl: async (command) => {
      const id = get().missionId;
      if (!id) throw new Error('no mission selected');
      await api.control(id, command);
    },
  };
});
