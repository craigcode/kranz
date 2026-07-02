// Shown when the URL has no #/m/<id>: fetches /api/missions and lists them.
// Selecting one sets the hash; App reacts to hashchange and connects.

import { useEffect } from 'react';
import { useKranzStore } from '../lib/store';
import { relTime } from '../lib/format';

export function MissionPicker() {
  const missions = useKranzStore((s) => s.missions);
  const missionsError = useKranzStore((s) => s.missionsError);
  const loadMissions = useKranzStore((s) => s.loadMissions);

  useEffect(() => {
    void loadMissions();
  }, [loadMissions]);

  return (
    <div className="picker">
      <div className="picker-box">
        <div className="picker-title">
          <span className="picker-brand mono">KRANZ</span>
          <span className="section-label">Missions</span>
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
        <ul className="picker-list">
          {missions.map((m) => (
            <li key={m.id}>
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
                <span className="picker-age dim">{relTime(m.createdAt)}</span>
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
