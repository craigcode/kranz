// Left sidebar: sessions (runs) newest first. Click selects a run and the
// centre pane switches to its transcript.

import { useMemo } from 'react';
import { useKranzStore } from '../lib/store';
import { relTime, roleLabel } from '../lib/format';
import { useNow } from '../lib/useNow';
import type { Role, WorkerRun } from '../lib/types';

const ROLE_GLYPH: Record<Role, string> = {
  orchestrator: 'O',
  worker: 'W',
  'validator-scrutiny': 'S',
  'validator-functional': 'F',
};

function runLabel(run: WorkerRun, featureTitles: Map<string, string>): string {
  switch (run.role) {
    case 'orchestrator':
      return 'Orchestrator';
    case 'worker': {
      const title = run.featureId ? featureTitles.get(run.featureId) : undefined;
      return `Worker · ${run.featureId ?? '?'}${title ? ` ${title}` : ''}`;
    }
    case 'validator-scrutiny':
      return `Scrutiny validator · ${run.milestoneId ?? '?'}`;
    case 'validator-functional':
      return `Functional validator · ${run.milestoneId ?? '?'}`;
  }
}

export function Sidebar() {
  const state = useKranzStore((s) => s.state);
  const events = useKranzStore((s) => s.events);
  const selectedRun = useKranzStore((s) => s.selectedRun);
  const selectRun = useKranzStore((s) => s.selectRun);
  const now = useNow(30_000);

  const deniedRuns = useMemo(() => {
    const denied = new Set<string>();
    for (const e of events) {
      if (e.type === 'worker.message' && e.payload.tag === 'denied') {
        denied.add(e.payload.runId);
      }
    }
    return denied;
  }, [events]);

  const featureTitles = useMemo(() => {
    const map = new Map<string, string>();
    if (state) {
      for (const ms of state.mission.milestones) {
        for (const f of ms.features) map.set(f.id, f.title);
      }
    }
    return map;
  }, [state]);

  const runs = useMemo(() => {
    if (!state) return [];
    return Object.values(state.runs).sort(
      (a, b) => Date.parse(b.startedAt) - Date.parse(a.startedAt),
    );
  }, [state]);

  return (
    <nav className="sidebar" aria-label="Sessions">
      <div className="section-label">Sessions</div>
      {runs.length === 0 && <div className="sidebar-empty dim">No runs yet</div>}
      <ul className="session-list">
        {runs.map((run) => {
          const label = runLabel(run, featureTitles);
          return (
            <li key={run.id}>
              <button
                type="button"
                className={`session-row${selectedRun === run.id ? ' selected' : ''}`}
                onClick={() => selectRun(selectedRun === run.id ? null : run.id)}
                title={`${run.id} · ${roleLabel(run.role)} · ${run.model}`}
              >
                <span className={`role-glyph role-${run.role}`} aria-hidden="true">
                  {ROLE_GLYPH[run.role]}
                </span>
                <span className="session-label">{label}</span>
                {deniedRuns.has(run.id) && (
                  <span className="denied-badge" title="run had denied tool calls">
                    ⛔
                  </span>
                )}
                <span className="session-age dim">{relTime(run.startedAt, now)}</span>
                {run.endedAt === undefined ? (
                  <span className="spinner" title="running" aria-label="running" />
                ) : (
                  <span className={`result-chip chip-${run.result ?? 'partial'}`}>
                    {run.result ?? '—'}
                  </span>
                )}
              </button>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
