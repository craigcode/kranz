// Flight-surgeon outcomes fold — dashboard mirror of `GET
// /api/missions/outcomes` (crates/engine/src/outcomes.rs). Renders the same
// pure fold three surfaces share: autonomy ratio, grant-latency buckets, and
// the escalation ledger. No local computation — the engine is the only
// source of truth.

import { useEffect, useState } from 'react';
import { getOutcomes } from '../lib/api';
import type { Outcomes } from '../lib/types';

function pct(share: number): string {
  return `${Math.round(share * 100)}%`;
}

export function OutcomesPanel() {
  const [outcomes, setOutcomes] = useState<Outcomes | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getOutcomes()
      .then((o) => !cancelled && setOutcomes(o))
      .catch((err: unknown) => !cancelled && setError(err instanceof Error ? err.message : String(err)));
    return () => {
      cancelled = true;
    };
  }, []);

  if (error !== null) {
    return (
      <section className="panel outcomes-panel">
        <div className="section-label">Outcomes</div>
        <div className="picker-error" role="alert">
          Could not load outcomes: {error}
        </div>
      </section>
    );
  }

  if (outcomes === null) {
    return (
      <section className="panel outcomes-panel">
        <div className="section-label">Outcomes</div>
        <div className="dim panel-empty">—</div>
      </section>
    );
  }

  const { autonomyRatio, grantLatency, escalations } = outcomes;

  return (
    <section className="panel outcomes-panel">
      <div className="section-label">Outcomes</div>

      <div className="outcomes-section outcomes-autonomy">
        <h3 className="outcomes-heading">Autonomy ratio</h3>
        {autonomyRatio.closedMissions === 0 ? (
          <div className="dim panel-empty" role="status">
            No closed missions yet
          </div>
        ) : (
          <ul className="outcomes-stat-list">
            <li>
              <span className="outcomes-stat-label">closed missions</span>
              <span className="outcomes-stat-value mono">{autonomyRatio.closedMissions}</span>
            </li>
            <li>
              <span className="outcomes-stat-label">interventions / closed mission</span>
              <span className="outcomes-stat-value mono">
                {autonomyRatio.interventionsPerClosedMission.toFixed(2)}
              </span>
            </li>
            <li>
              <span className="outcomes-stat-label">zero-intervention share</span>
              <span className="outcomes-stat-value mono">{pct(autonomyRatio.zeroInterventionShare)}</span>
            </li>
          </ul>
        )}
      </div>

      <div className="outcomes-section outcomes-latency">
        <h3 className="outcomes-heading">Grant latency</h3>
        {grantLatency.totalDecided === 0 ? (
          <div className="dim panel-empty" role="status">
            No decided grants yet
          </div>
        ) : (
          <table className="outcomes-latency-table">
            <tbody>
              {grantLatency.buckets.map((bucket) => (
                <tr key={bucket.label}>
                  <td className="outcomes-latency-label">{bucket.label}</td>
                  <td className="outcomes-latency-count mono">{bucket.count}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>

      <div className="outcomes-section outcomes-escalations">
        <h3 className="outcomes-heading">Escalation ledger</h3>
        {escalations.length === 0 ? (
          <div className="dim panel-empty" role="status">
            No escalations recorded
          </div>
        ) : (
          <table className="outcomes-escalation-table">
            <thead>
              <tr>
                <th>ts</th>
                <th>mission</th>
                <th>kind</th>
                <th>summary</th>
                <th>decision</th>
                <th>latency</th>
              </tr>
            </thead>
            <tbody>
              {escalations.map((row, i) => (
                <tr key={`${row.missionId}-${row.ts}-${i}`}>
                  <td className="mono">{row.ts}</td>
                  <td>{row.missionId}</td>
                  <td>{row.kind}</td>
                  <td>{row.summary}</td>
                  <td>{row.decision}</td>
                  <td className="mono">{row.latencyMs !== null ? `${row.latencyMs}ms` : '—'}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
    </section>
  );
}
