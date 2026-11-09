// Trajectory Board lens body — docs/scoping/trajectory-board.md (slices 3, 4, 6).
// Loaded on demand via React.lazy so its code and the fleet/plot modules stay out
// of the main dashboard chunk.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../lib/api';
import { DEMO_STEP_MS, demoSnapshotAt } from '../lib/board/demo';
import { buildFleetModel, type BoardAsk, type FleetModel } from '../lib/board/fleet';
import { buildRows } from '../lib/pipelineRows';
import { pipelineStage } from '../lib/pipelineStage';
import { visible } from '../lib/presentation';
import { useKranzStore } from '../lib/store';
import type { MissionState } from '../lib/types';
import { BoardPlot } from './BoardPlot';
import { DeliveredPanel } from './DeliveredPanel';
import { GateReviewPanel } from './GateReviewPanel';
import { GrantRequestPanel } from './GrantRequestPanel';
import { PermissionRequestPanel } from './PermissionRequestPanel';
import { ReviewableTicketRow } from './PipelineView';
import { QuestionRequestPanel } from './QuestionRequestPanel';
import { RevisionPanel } from './RevisionPanel';
import { StatusStrip } from './StatusStrip';

const LIST_POLL_MS = 10_000;
const STATE_POLL_MS = 5_000;

export interface BoardLensProps {
  demo?: boolean;
}

function isInFlightAskKind(kind: BoardAsk['kind']): boolean {
  return (
    kind === 'permission' ||
    kind === 'grant' ||
    kind === 'blocked' ||
    kind === 'gate' ||
    kind === 'revision' ||
    kind === 'question'
  );
}

function canActInPlace(ask: BoardAsk): boolean {
  if (isInFlightAskKind(ask.kind) && ask.missionId !== undefined) return true;
  if (ask.kind === 'reviewable' && ask.slug !== undefined) return true;
  if (ask.kind === 'delivered' && ask.missionId !== undefined) return true;
  return false;
}

export function BoardLens({ demo = false }: BoardLensProps) {
  const repoId = useKranzStore((s) => s.repoId);
  const tickets = useKranzStore((s) => s.tickets);
  const missions = useKranzStore((s) => s.missions);
  const loadTickets = useKranzStore((s) => s.loadTickets);
  const loadMissions = useKranzStore((s) => s.loadMissions);
  const connectMission = useKranzStore((s) => s.connectMission);
  const connectedMissionId = useKranzStore((s) => s.missionId);
  const connectedState = useKranzStore((s) => s.state);

  const [missionStates, setMissionStates] = useState<ReadonlyMap<string, MissionState>>(
    () => new Map(),
  );
  const [statePollError, setStatePollError] = useState<string | null>(null);
  const [nowMs, setNowMs] = useState<number>(() => Date.now());
  const [demoStep, setDemoStep] = useState<number>(0);
  const [selectedCraftKey, setSelectedCraftKey] = useState<string | null>(null);
  const [selectedAskId, setSelectedAskId] = useState<string | null>(null);

  const genRef = useRef<number>(0);
  const lastListFetchRef = useRef<number>(-Infinity);
  const lastStateFetchRef = useRef<Map<string, number>>(new Map());
  const prevModelRef = useRef<FleetModel | null>(null);

  // Compute current running mission IDs from the store's rows
  const runningMissionIds = useMemo(() => {
    if (demo) return [];
    const rows = buildRows(tickets, missions);
    const ids: string[] = [];
    for (const r of rows) {
      if (pipelineStage(r.item) === 'running' && r.missionId !== undefined) {
        ids.push(r.missionId);
      }
    }
    return ids;
  }, [demo, tickets, missions]);

  const runningIdsRef = useRef<ReadonlySet<string>>(new Set());
  runningIdsRef.current = new Set(runningMissionIds);

  // Reset all local state when repository changes or demo mode toggles
  useEffect(() => {
    genRef.current += 1;
    lastListFetchRef.current = -Infinity;
    lastStateFetchRef.current.clear();
    prevModelRef.current = null;
    setMissionStates(new Map());
    setStatePollError(null);
    setSelectedCraftKey(null);
    setSelectedAskId(null);
    setDemoStep(0);
    setNowMs(Date.now());
  }, [repoId, demo]);

  // Discard state for any mission that is no longer in the `running` stage
  useEffect(() => {
    if (demo) return;
    const activeSet = new Set(runningMissionIds);
    for (const id of lastStateFetchRef.current.keys()) {
      if (!activeSet.has(id)) {
        lastStateFetchRef.current.delete(id);
      }
    }
    setMissionStates((prev) => {
      let changed = false;
      for (const id of prev.keys()) {
        if (!activeSet.has(id)) {
          changed = true;
          break;
        }
      }
      if (!changed) return prev;
      const next = new Map<string, MissionState>();
      for (const [id, st] of prev) {
        if (activeSet.has(id)) next.set(id, st);
      }
      return next;
    });
  }, [demo, runningMissionIds]);

  // Mirror live WebSocket state updates for a connected running mission
  useEffect(() => {
    if (demo || connectedMissionId === null || connectedState === null) return;
    if (connectedState.mission.id !== connectedMissionId) return;
    if (!runningIdsRef.current.has(connectedMissionId)) return;
    setMissionStates((prev) => {
      const existing = prev.get(connectedMissionId);
      if (existing && existing.lastSeq >= connectedState.lastSeq) return prev;
      const next = new Map(prev);
      next.set(connectedMissionId, connectedState);
      return next;
    });
    setNowMs(Date.now());
  }, [demo, connectedMissionId, connectedState]);

  const fetchRunningStates = useCallback(
    (force: boolean) => {
      if (demo) return;
      if (typeof document !== 'undefined' && document.hidden) return;
      const gen = genRef.current;
      const now = Date.now();
      for (const id of runningIdsRef.current) {
        const last = lastStateFetchRef.current.get(id) ?? -Infinity;
        if (!force && now - last < STATE_POLL_MS) continue;
        lastStateFetchRef.current.set(id, now);
        api
          .missionState(id)
          .then((st) => {
            if (genRef.current !== gen || !runningIdsRef.current.has(id)) return;
            setMissionStates((prev) => {
              const next = new Map(prev);
              next.set(id, st);
              return next;
            });
            if (useKranzStore.getState().missionId === id) {
              const cur = useKranzStore.getState().state;
              if (cur === null || st.lastSeq >= cur.lastSeq) {
                useKranzStore.setState({ state: st, lastSeq: st.lastSeq });
              }
            }
            setStatePollError(null);
            setNowMs(Date.now());
          })
          .catch((err: unknown) => {
            if (genRef.current !== gen) return;
            setStatePollError(err instanceof Error ? err.message : String(err));
          });
      }
    },
    [demo],
  );

  const refreshLists = useCallback(
    (force: boolean) => {
      if (demo) return;
      if (typeof document !== 'undefined' && document.hidden) return;
      const now = Date.now();
      if (!force && now - lastListFetchRef.current < LIST_POLL_MS) return;
      lastListFetchRef.current = now;
      void loadTickets();
      void loadMissions();
    },
    [demo, loadTickets, loadMissions],
  );

  // Immediate refresh after an in-place act completes (slice 6)
  const refreshAfterAct = useCallback(() => {
    if (demo) return;
    lastListFetchRef.current = Date.now();
    void loadTickets();
    void loadMissions();
    fetchRunningStates(true);
  }, [demo, loadTickets, loadMissions, fetchRunningStates]);

  // Whenever new running missions appear in the store, fetch their state if due
  useEffect(() => {
    if (demo) return;
    fetchRunningStates(false);
  }, [demo, runningMissionIds, fetchRunningStates]);

  // Polling timers + visibilitychange handler (slice 3 D-B)
  useEffect(() => {
    if (demo) {
      const id = window.setInterval(() => {
        if (typeof document !== 'undefined' && document.hidden) return;
        setDemoStep((s) => s + 1);
        setNowMs(Date.now());
      }, DEMO_STEP_MS);
      return () => window.clearInterval(id);
    }

    refreshLists(false);
    fetchRunningStates(false);

    const listTimer = window.setInterval(() => {
      refreshLists(false);
    }, LIST_POLL_MS);

    const stateTimer = window.setInterval(() => {
      fetchRunningStates(false);
    }, STATE_POLL_MS);

    const onVisibilityChange = () => {
      if (typeof document !== 'undefined' && !document.hidden) {
        refreshLists(true);
        fetchRunningStates(true);
      }
    };

    if (typeof document !== 'undefined') {
      document.addEventListener('visibilitychange', onVisibilityChange);
    }

    return () => {
      window.clearInterval(listTimer);
      window.clearInterval(stateTimer);
      if (typeof document !== 'undefined') {
        document.removeEventListener('visibilitychange', onVisibilityChange);
      }
    };
  }, [demo, repoId, refreshLists, fetchRunningStates]);

  // Build the FleetModel (preserving single-step transitions across updates)
  const currentHash = typeof window !== 'undefined' ? window.location.hash : undefined;
  const model = useMemo(() => {
    if (demo) {
      const snap = demoSnapshotAt(demoStep, nowMs);
      const next = buildFleetModel(
        snap.tickets,
        snap.missions,
        snap.missionStates,
        snap.nowMs,
        prevModelRef.current,
        currentHash,
      );
      prevModelRef.current = next;
      return next;
    }
    const next = buildFleetModel(
      tickets,
      missions,
      missionStates,
      nowMs,
      prevModelRef.current,
      currentHash,
    );
    prevModelRef.current = next;
    return next;
  }, [demo, demoStep, tickets, missions, missionStates, nowMs, currentHash]);

  // Resolve the currently selected ask
  const selectedAsk: BoardAsk | null = useMemo(() => {
    if (selectedAskId !== null) {
      const byId = model.asks.find((a) => a.id === selectedAskId);
      if (byId) return byId;
    }
    if (selectedCraftKey !== null) {
      const byCraft = model.asks.find((a) => a.craftKey === selectedCraftKey);
      if (byCraft) return byCraft;
    }
    return null;
  }, [model.asks, selectedAskId, selectedCraftKey]);

  const activeCraftKey = selectedAsk?.craftKey ?? selectedCraftKey;

  const selectAsk = useCallback(
    (ask: BoardAsk) => {
      setSelectedAskId(ask.id);
      setSelectedCraftKey(ask.craftKey);
      if (!demo && isInFlightAskKind(ask.kind) && ask.missionId !== undefined) {
        connectMission(ask.missionId);
        const known = missionStates.get(ask.missionId);
        if (known !== undefined) {
          useKranzStore.setState({ state: known, lastSeq: known.lastSeq });
        }
      }
    },
    [demo, connectMission, missionStates],
  );

  const handleSelectCraft = useCallback(
    (craftKey: string | null) => {
      setSelectedCraftKey(craftKey);
      if (craftKey === null) {
        setSelectedAskId(null);
        return;
      }
      const ask = model.asks.find((a) => a.craftKey === craftKey) ?? null;
      setSelectedAskId(ask ? ask.id : null);
      if (ask && !demo && isInFlightAskKind(ask.kind) && ask.missionId !== undefined) {
        connectMission(ask.missionId);
        const known = missionStates.get(ask.missionId);
        if (known !== undefined) {
          useKranzStore.setState({ state: known, lastSeq: known.lastSeq });
        }
      }
    },
    [model.asks, demo, connectMission, missionStates],
  );

  const showActColumn = !demo && selectedAsk !== null && canActInPlace(selectedAsk);

  return (
    <div className="trajectory-board-lens">
      <header className="trajectory-board-header">
        {demo && (
          <span className="trajectory-board-demo-badge mono" role="status">
            DEMO
          </span>
        )}
        <dl className="trajectory-board-counts">
          <div className={model.counts.waiting > 0 ? 'trajectory-board-count--alert' : ''}>
            <dt>Waiting on you</dt>
            <dd className="mono" data-testid="board-count-waiting">
              {model.counts.waiting}
            </dd>
          </div>
          <div>
            <dt>In flight</dt>
            <dd className="mono" data-testid="board-count-inflight">
              {model.counts.inFlight}
            </dd>
          </div>
          <div>
            <dt>Landed</dt>
            <dd className="mono" data-testid="board-count-landed">
              {model.counts.landed}
            </dd>
          </div>
          {model.hangar.total > 0 && (
            <div>
              <dt>Hangar</dt>
              <dd className="mono" data-testid="board-count-hangar">
                {model.hangar.total}
              </dd>
            </div>
          )}
        </dl>
      </header>

      {statePollError !== null && (
        <div className="dim trajectory-board-quiet-error" role="status">
          Using last known mission state ({visible(statePollError)})
        </div>
      )}

      <div className="trajectory-board-body">
        <BoardPlot
          model={model}
          selectedKey={activeCraftKey}
          onSelectCraft={handleSelectCraft}
        />

        <div className="trajectory-board-columns">
          <section className="trajectory-board-asks" aria-label="Waiting on you">
            <div className="section-label">
              Waiting on you
              <span className="section-count mono">{model.asks.length}</span>
            </div>
            {model.asks.length === 0 ? (
              <p className="dim picker-empty trajectory-board-empty" role="status">
                Nothing is waiting on you.
              </p>
            ) : (
              <ul className="picker-list trajectory-board-ask-list">
                {model.asks.map((ask) => {
                  const isSelected =
                    selectedAsk?.id === ask.id || activeCraftKey === ask.craftKey;
                  const actable = !demo && canActInPlace(ask);
                  return (
                    <li
                      key={ask.id}
                      className={
                        'picker-item trajectory-board-ask-item' +
                        (isSelected ? ' trajectory-board-ask-item--selected' : '')
                      }
                      data-ask-kind={ask.kind}
                      data-craft-key={ask.craftKey}
                      onClick={() => selectAsk(ask)}
                    >
                      <div className="picker-row trajectory-board-ask-row">
                        <span className="status-pill pill-blocked trajectory-board-ask-kind">
                          {visible(ask.kindLabel)}
                        </span>
                        <span className="mono picker-id">{visible(ask.workItemId)}</span>
                        <span className="picker-goal">{visible(ask.title)}</span>
                        <span className="dim mono trajectory-board-ask-detail">
                          {visible(ask.detail)}
                        </span>
                      </div>
                      <a
                        className="btn-small pipeline-primary-action trajectory-board-ask-link"
                        href={ask.href}
                        onClick={(ev) => {
                          if (actable) {
                            ev.preventDefault();
                            selectAsk(ask);
                          }
                        }}
                      >
                        {visible(ask.kindLabel)}
                      </a>
                    </li>
                  );
                })}
              </ul>
            )}
          </section>

          {showActColumn && selectedAsk !== null && (
            <aside className="trajectory-board-act" aria-label="Answer ask">
              {isInFlightAskKind(selectedAsk.kind) && selectedAsk.missionId !== undefined && (
                <div className="trajectory-board-act-panels">
                  {selectedAsk.kind === 'blocked' && <StatusStrip />}
                  <PermissionRequestPanel onActed={refreshAfterAct} />
                  <GrantRequestPanel onActed={refreshAfterAct} />
                  <GateReviewPanel />
                  <RevisionPanel onActed={refreshAfterAct} />
                  <QuestionRequestPanel onActed={refreshAfterAct} />
                </div>
              )}
              {selectedAsk.kind === 'reviewable' && selectedAsk.slug !== undefined && (
                <div className="picker-item picker-item--stacked">
                  <ReviewableTicketRow row={selectedAsk.row} onQueued={refreshAfterAct} />
                </div>
              )}
              {selectedAsk.kind === 'delivered' && selectedAsk.missionId !== undefined && (
                <DeliveredPanel
                  missionId={selectedAsk.missionId}
                  status="complete"
                  onMerged={refreshAfterAct}
                />
              )}
            </aside>
          )}
        </div>
      </div>
    </div>
  );
}

export default BoardLens;
