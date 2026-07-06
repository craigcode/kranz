// Shown when the URL has no #/m/<id>: fetches /api/missions and lists them.
// Selecting one sets the hash; App reacts to hashchange and connects.
//
// Management actions are status-aware (mirroring the CLI's abandon/clean):
// planning/paused/blocked → abandon; terminal → delete (Complete needs the
// explicit opt-in — it feeds the cost-calibration corpus). Running/validating
// missions are managed from their detail view, not the list. Destructive
// actions are two-step: first click arms, second click fires, auto-disarm.

import { useEffect, useRef, useState } from 'react';
import { useKranzStore } from '../lib/store';
import { relTime } from '../lib/format';
import { missionCounts } from '../lib/missionCounts';
import type { MissionSummary } from '../lib/types';

// MissionSummary.status is a plain string (the list endpoint stays lenient
// about rows it couldn't fold), so these sets match on strings.
const TERMINAL: ReadonlySet<string> = new Set(['complete', 'failed', 'abandoned']);
const ABANDONABLE: ReadonlySet<string> = new Set(['planning', 'paused', 'blocked']);
const DELETED = 'deleted';

export function MissionPicker() {
  const missions = useKranzStore((s) => s.missions);
  const missionsError = useKranzStore((s) => s.missionsError);
  const loadMissions = useKranzStore((s) => s.loadMissions);
  const abandonMission = useKranzStore((s) => s.abandonMission);
  const deleteMission = useKranzStore((s) => s.deleteMission);

  // The one armed (awaiting-confirm) action, keyed by mission id. Auto-disarms
  // so a stray first click can't lie in wait indefinitely.
  const [armed, setArmed] = useState<string | null>(null);
  const disarmTimer = useRef<number | undefined>(undefined);
  const arm = (id: string) => {
    window.clearTimeout(disarmTimer.current);
    setArmed(id);
    disarmTimer.current = window.setTimeout(() => setArmed(null), 5000);
  };
  useEffect(() => () => window.clearTimeout(disarmTimer.current), []);

  useEffect(() => {
    void loadMissions();
  }, [loadMissions]);

  const active = missions.filter((m) => !TERMINAL.has(m.status) && m.status !== DELETED);
  const closed = missions.filter((m) => TERMINAL.has(m.status) || m.status === DELETED);
  const { finished, running } = missionCounts(missions);

  const row = (m: MissionSummary) => (
    <li key={m.id} className="picker-item">
      <button
        type="button"
        className="picker-row"
        onClick={() => {
          window.location.hash = `#/m/${encodeURIComponent(m.id)}`;
        }}
      >
        <span className={`status-pill pill-${m.status}`}>
          <span className="status-dot" aria-hidden="true" />
          {m.status}
        </span>
        <span className="mono picker-id">{m.id}</span>
        <span className="picker-goal">{m.goal}</span>
        <span className="picker-age dim">{m.createdAt ? relTime(m.createdAt) : '?'}</span>
      </button>
      {m.status !== DELETED && ABANDONABLE.has(m.status) &&
        (armed === m.id ? (
          <button
            type="button"
            className="btn-small picker-action picker-action-danger"
            onClick={() => {
              setArmed(null);
              void abandonMission(m.id);
            }}
          >
            confirm abandon
          </button>
        ) : (
          <button
            type="button"
            className="btn-small picker-action"
            title="retire this mission (recorded in its event log; the directory stays)"
            onClick={() => arm(m.id)}
          >
            abandon
          </button>
        ))}
      {m.status !== DELETED && TERMINAL.has(m.status) &&
        (armed === m.id ? (
          <button
            type="button"
            className="btn-small picker-action picker-action-danger"
            title={
              m.status === 'complete'
                ? 'completed missions feed cost calibration — deleting weakens future estimates'
                : 'removes the mission directory (branches and the missions index are kept)'
            }
            onClick={() => {
              setArmed(null);
              void deleteMission(m.id, m.status === 'complete');
            }}
          >
            {m.status === 'complete' ? 'delete anyway' : 'confirm delete'}
          </button>
        ) : (
          <button
            type="button"
            className="btn-small picker-action"
            title="remove the mission directory (branches and the missions index are kept)"
            onClick={() => arm(m.id)}
          >
            delete
          </button>
        ))}
    </li>
  );

  return (
    <div className="picker">
      <div className="picker-box">
        <div className="picker-title">
          <span className="picker-brand mono">KRANZ</span>
          <span className="section-label">Missions</span>
          <span className="picker-counts dim">
            {finished} finished · {running} running
          </span>
          <button
            type="button"
            className="btn-small new-mission-btn"
            onClick={() => {
              window.location.hash = '#/new';
            }}
          >
            + new mission
          </button>
          <button
            type="button"
            className="btn-small"
            onClick={() => {
              window.location.hash = '#/backlog';
            }}
          >
            backlog
          </button>
        </div>
        {missionsError !== null && (
          <div className="picker-error" role="alert">
            Could not load missions: {missionsError}{' '}
            <button type="button" className="btn-small" onClick={() => void loadMissions()}>
              retry
            </button>
          </div>
        )}
        {missionsError === null && missions.length === 0 && (
          <div className="dim picker-empty" role="status">
            No missions found. Start one with <code>kranz run</code>.
          </div>
        )}
        <ul className="picker-list">{active.map(row)}</ul>
        {closed.length > 0 && (
          <details className="picker-closed">
            <summary className="dim">closed</summary>
            <ul className="picker-list">{closed.map(row)}</ul>
          </details>
        )}
      </div>
    </div>
  );
}
