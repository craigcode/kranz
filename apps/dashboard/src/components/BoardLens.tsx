// Trajectory Board lens body — docs/scoping/trajectory-board.md (slices 3, 4, 6).
// Loaded on demand via React.lazy so its code and the fleet/plot modules stay out
// of the main dashboard chunk.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api, isTokenRequired } from '../lib/api';
import type { GetJsonOptions } from '../lib/api';
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

function seedStoreMissionState(id: string, known: MissionState | undefined): void {
  if (known === undefined) return;
  const snap = useKranzStore.getState();
  if (snap.missionId !== id) return;
  const cur = snap.state;
  const curSeq = snap.lastSeq ?? cur?.lastSeq ?? -Infinity;
  if (
    cur === null ||
    cur.mission.id !== id ||
    (known.lastSeq >= cur.lastSeq && known.lastSeq >= curSeq)
  ) {
    useKranzStore.setState({ state: known, lastSeq: known.lastSeq });
  }
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
  const [pollingPaused, setPollingPaused] = useState(false);
  const pollingPausedRef = useRef(false);

  const scopeKey = `${repoId ?? ''}:${demo}`;
  const genRef = useRef<number>(0);
  const lastListFetchRef = useRef<number>(-Infinity);
  const lastStateFetchRef = useRef<Map<string, number>>(new Map());
  const prevModelRef = useRef<{ scopeKey: string; model: FleetModel } | null>(null);
  const missionStatesRef = useRef<ReadonlyMap<string, MissionState>>(missionStates);
  missionStatesRef.current = missionStates;

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
    setMissionStates(new Map());
    setStatePollError(null);
    setSelectedCraftKey(null);
    setSelectedAskId(null);
    setDemoStep(0);
    setNowMs(Date.now());
    pollingPausedRef.current = false;
    setPollingPaused(false);
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

  // A background poll failing authentication must stop polling entirely
  // rather than keep parking fresh waiters on the token gate (one per tick).
  const pauseForTokenRequired = useCallback(() => {
    pollingPausedRef.current = true;
    setPollingPaused(true);
  }, []);

  const fetchRunningStates = useCallback(
    (force: boolean, opts?: GetJsonOptions) => {
      if (demo) return;
      if (pollingPausedRef.current) return;
      if (typeof document !== 'undefined' && document.hidden) return;
      const gen = genRef.current;
      const now = Date.now();
      for (const id of runningIdsRef.current) {
        const last = lastStateFetchRef.current.get(id) ?? -Infinity;
        if (!force && now - last < STATE_POLL_MS) continue;
        lastStateFetchRef.current.set(id, now);
        (opts !== undefined ? api.missionState(id, opts) : api.missionState(id))
          .then((st) => {
            if (genRef.current !== gen || !runningIdsRef.current.has(id)) return;
            setMissionStates((prev) => {
              const next = new Map(prev);
              next.set(id, st);
              return next;
            });
            seedStoreMissionState(id, st);
            setStatePollError(null);
            setNowMs(Date.now());
          })
          .catch((err: unknown) => {
            if (genRef.current !== gen) return;
            if (opts?.tokenGate === false && isTokenRequired(err)) {
              pauseForTokenRequired();
              return;
            }
            setStatePollError(err instanceof Error ? err.message : String(err));
          });
      }
    },
    [demo, pauseForTokenRequired],
  );

  const refreshLists = useCallback(
    (force: boolean, opts?: GetJsonOptions) => {
      if (demo) return;
      if (pollingPausedRef.current) return;
      if (typeof document !== 'undefined' && document.hidden) return;
      const now = Date.now();
      if (!force && now - lastListFetchRef.current < LIST_POLL_MS) return;
      const gen = genRef.current;
      lastListFetchRef.current = now;
      setNowMs(now);
      void Promise.all([loadTickets(opts), loadMissions(opts)])
        .catch((err: unknown) => {
          if (opts?.tokenGate === false && isTokenRequired(err)) {
            pauseForTokenRequired();
          }
        })
        .finally(() => {
          if (genRef.current === gen) {
            setNowMs(Date.now());
          }
        });
    },
    [demo, loadTickets, loadMissions, pauseForTokenRequired],
  );

  // Immediate refresh after an in-place act completes (slice 6)
  const refreshAfterAct = useCallback(() => {
    if (demo) return;
    const gen = genRef.current;
    const now = Date.now();
    lastListFetchRef.current = now;
    setNowMs(now);
    void Promise.all([loadTickets(), loadMissions()]).finally(() => {
      if (genRef.current === gen) {
        setNowMs(Date.now());
      }
    });
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

    // A paused board must issue no further requests until the operator
    // explicitly resumes — skip starting intervals/listener altogether.
    if (pollingPaused) return;

    const backgroundOpts: GetJsonOptions = { tokenGate: false };

    refreshLists(false, backgroundOpts);
    fetchRunningStates(false, backgroundOpts);

    const listTimer = window.setInterval(() => {
      refreshLists(false, backgroundOpts);
    }, LIST_POLL_MS);

    const stateTimer = window.setInterval(() => {
      fetchRunningStates(false, backgroundOpts);
    }, STATE_POLL_MS);

    const onVisibilityChange = () => {
      if (typeof document !== 'undefined' && !document.hidden) {
        refreshLists(true, backgroundOpts);
        fetchRunningStates(true, backgroundOpts);
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
  }, [demo, repoId, pollingPaused, refreshLists, fetchRunningStates]);

  const resumePolling = useCallback(() => {
    pollingPausedRef.current = false;
    setPollingPaused(false);
    setStatePollError(null);
    // User-initiated: keep the default token gate so a missing/rejected
    // token re-opens the TokenPrompt instead of failing silently.
    refreshLists(true);
    fetchRunningStates(true);
  }, [refreshLists, fetchRunningStates]);

  // Build the FleetModel purely; commit prevModelRef inside useEffect so StrictMode never wipes transitions
  const currentHash = typeof window !== 'undefined' ? window.location.hash : undefined;
  const model = useMemo(() => {
    const prev = prevModelRef.current?.scopeKey === scopeKey ? prevModelRef.current.model : null;
    if (demo) {
      const snap = demoSnapshotAt(demoStep, nowMs);
      return buildFleetModel(
        snap.tickets,
        snap.missions,
        snap.missionStates,
        snap.nowMs,
        prev,
        currentHash,
      );
    }
    return buildFleetModel(
      tickets,
      missions,
      missionStates,
      nowMs,
      prev,
      currentHash,
    );
  }, [scopeKey, demo, demoStep, tickets, missions, missionStates, nowMs, currentHash]);

  useEffect(() => {
    prevModelRef.current = { scopeKey, model };
  }, [scopeKey, model]);

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

  const clearSelection = useCallback(() => {
    setSelectedAskId(null);
    setSelectedCraftKey(null);
  }, []);

  const selectedAskMissionId = selectedAsk?.missionId;
  const selectedAskKind = selectedAsk?.kind;

  // Synchronize connected mission whenever selectedAsk resolves to an in-flight mission ask
  useEffect(() => {
    if (
      !demo &&
      selectedAskKind !== undefined &&
      isInFlightAskKind(selectedAskKind) &&
      selectedAskMissionId !== undefined
    ) {
      connectMission(selectedAskMissionId);
      seedStoreMissionState(selectedAskMissionId, missionStatesRef.current.get(selectedAskMissionId));
    }
  }, [demo, selectedAskMissionId, selectedAskKind, connectMission]);

  // Seed store state if polled missionStates arrives while WS snapshot is still in flight
  useEffect(() => {
    if (
      !demo &&
      selectedAsk &&
      isInFlightAskKind(selectedAsk.kind) &&
      selectedAsk.missionId !== undefined &&
      connectedMissionId === selectedAsk.missionId &&
      connectedState === null
    ) {
      seedStoreMissionState(selectedAsk.missionId, missionStates.get(selectedAsk.missionId));
    }
  }, [demo, selectedAsk, connectedMissionId, connectedState, missionStates]);

  // Close drawer / clear selection on Escape
  useEffect(() => {
    if (selectedAsk === null && selectedCraftKey === null) return;
    const onKeyDown = (ev: KeyboardEvent) => {
      if (ev.key === 'Escape') {
        clearSelection();
      }
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [selectedAsk, selectedCraftKey, clearSelection]);

  const selectAsk = useCallback(
    (ask: BoardAsk) => {
      setSelectedAskId(ask.id);
      setSelectedCraftKey(ask.craftKey);
      if (!demo && isInFlightAskKind(ask.kind) && ask.missionId !== undefined) {
        connectMission(ask.missionId);
        seedStoreMissionState(ask.missionId, missionStates.get(ask.missionId));
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
        seedStoreMissionState(ask.missionId, missionStates.get(ask.missionId));
      }
    },
    [model.asks, demo, connectMission, missionStates],
  );

  const showActColumn = !demo && selectedAsk !== null && canActInPlace(selectedAsk);
  const inFlightReady =
    selectedAsk !== null &&
    selectedAsk.missionId !== undefined &&
    connectedMissionId === selectedAsk.missionId &&
    connectedState !== null &&
    connectedState.mission.id === selectedAsk.missionId;

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

      {pollingPaused && (
        <div className="dim trajectory-board-polling-paused" role="status">
          Polling paused — token required.{' '}
          <button type="button" className="btn-small" onClick={resumePolling}>
            Resume polling
          </button>
        </div>
      )}

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
                      {actable || demo ? (
                        <button
                          type="button"
                          className="btn-small pipeline-primary-action trajectory-board-ask-link"
                          aria-expanded={isSelected}
                          onClick={(ev) => {
                            ev.stopPropagation();
                            if (selectedAsk?.id === ask.id) {
                              clearSelection();
                            } else {
                              selectAsk(ask);
                            }
                          }}
                        >
                          {visible(ask.kindLabel)}
                        </button>
                      ) : (
                        <a
                          className="btn-small pipeline-primary-action trajectory-board-ask-link"
                          href={ask.href}
                          onClick={(ev) => {
                            ev.stopPropagation();
                          }}
                        >
                          {visible(ask.kindLabel)}
                        </a>
                      )}
                    </li>
                  );
                })}
              </ul>
            )}
          </section>

          {showActColumn && selectedAsk !== null && (
            <aside
              key={selectedAsk.missionId ?? selectedAsk.id}
              className="trajectory-board-act"
              aria-label="Answer ask"
            >
              <div className="trajectory-board-act-header">
                <span className="mono picker-id">{visible(selectedAsk.workItemId)}</span>
                <div className="trajectory-board-act-header-actions">
                  {!demo && (
                    <a className="btn-small trajectory-board-open-mission" href={selectedAsk.href}>
                      Open mission
                    </a>
                  )}
                  <button
                    type="button"
                    className="btn-small trajectory-board-act-close"
                    aria-label="Close"
                    onClick={clearSelection}
                  >
                    Close
                  </button>
                </div>
              </div>
              {isInFlightAskKind(selectedAsk.kind) && selectedAsk.missionId !== undefined && (
                inFlightReady ? (
                  <div
                    key={selectedAsk.missionId ?? selectedAsk.id}
                    className="trajectory-board-act-panels"
                  >
                    {selectedAsk.kind === 'blocked' && <StatusStrip />}
                    <PermissionRequestPanel onActed={refreshAfterAct} />
                    <GrantRequestPanel onActed={refreshAfterAct} />
                    <GateReviewPanel />
                    {selectedAsk.kind === 'revision' && (
                      <RevisionPanel onActed={refreshAfterAct} />
                    )}
                    <QuestionRequestPanel onActed={refreshAfterAct} />
                  </div>
                ) : (
                  <div className="dim picker-empty" role="status">
                    Connecting to mission…
                  </div>
                )
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
