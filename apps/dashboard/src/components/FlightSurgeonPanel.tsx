// Flight-surgeon console — dashboard mirror of `GET /api/escalation-metrics`
// (crates/engine/src/escalation_metrics.rs). Three number cards (autonomy
// ratio split by outcome, the rubber-stamp signal, false greens) plus the
// escalation ledger table. No local computation — the engine fold is the only
// source of truth; this panel renders it.

import { useEffect, useState } from 'react';
import { getEscalationMetrics } from '../lib/api';
import type { EscalationMetrics } from '../lib/types';

function pct(share: number | null): string {
  return share === null ? '—' : `${Math.round(share * 100)}%`;
}

/** Milliseconds as a compact duration ("12s", "47m", "2.3h", "3.1d"). */
function formatDurationMs(ms: number | null): string {
  if (ms === null) return '—';
  const S = 1000;
  const M = 60 * S;
  const H = 60 * M;
  const D = 24 * H;
  if (ms >= D) return `${(ms / D).toFixed(1)}d`;
  if (ms >= H) return `${(ms / H).toFixed(1)}h`;
  if (ms >= M) return `${Math.floor(ms / M)}m`;
  return `${Math.floor(ms / S)}s`;
}

export function FlightSurgeonPanel() {
  const [metrics, setMetrics] = useState<EscalationMetrics | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getEscalationMetrics()
      .then((m) => !cancelled && setMetrics(m))
      .catch((err: unknown) => !cancelled && setError(err instanceof Error ? err.message : String(err)));
    return () => {
      cancelled = true;
    };
  }, []);

  if (error !== null) {
    return (
      <section className="panel flight-surgeon-panel">
        <div className="section-label">Flight Surgeon</div>
        <div className="picker-error" role="alert">
          Could not load escalation metrics: {error}
        </div>
      </section>
    );
  }

  if (metrics === null) {
    return (
      <section className="panel flight-surgeon-panel">
        <div className="section-label">Flight Surgeon</div>
        <div className="dim panel-empty">—</div>
      </section>
    );
  }

  const { autonomy, rubberStamp, falseGreens, ledger } = metrics;

  return (
    <section className="panel flight-surgeon-panel">
      <div className="section-label">Flight Surgeon</div>

      <div className="flight-surgeon-cards">
        <div className="flight-surgeon-card" data-testid="autonomy-card">
          <div className="flight-surgeon-card-value mono">{pct(autonomy.zeroInterventionShare)}</div>
          <div className="flight-surgeon-card-label">autonomous (zero-intervention)</div>
          <div className="flight-surgeon-card-detail dim">
            {autonomy.zeroInterventionMissions} of {autonomy.closedMissions} closed — completed{' '}
            {pct(autonomy.completed.zeroInterventionShare)}, failed{' '}
            {pct(autonomy.failed.zeroInterventionShare)}
          </div>
        </div>

        <div className="flight-surgeon-card" data-testid="rubber-stamp-card">
          <div className="flight-surgeon-card-value mono">{formatDurationMs(rubberStamp.p50Ms)}</div>
          <div className="flight-surgeon-card-label">park→grant p50</div>
          <div className="flight-surgeon-card-detail dim">
            p90 {formatDurationMs(rubberStamp.p90Ms)} — {rubberStamp.underTenSeconds} of{' '}
            {rubberStamp.decidedGrants} decided under 10s
          </div>
        </div>

        <div className="flight-surgeon-card" data-testid="false-greens-card">
          <div className="flight-surgeon-card-value mono">{pct(falseGreens.falseGreenRate)}</div>
          <div className="flight-surgeon-card-label">false greens</div>
          <div className="flight-surgeon-card-detail dim">
            {falseGreens.falseGreens} of {falseGreens.completedMissions} completed — steered{' '}
            {pct(falseGreens.withInterventions.rate)}, autonomous{' '}
            {pct(falseGreens.zeroIntervention.rate)}
          </div>
        </div>
      </div>

      <div className="outcomes-section outcomes-escalations">
        <h3 className="outcomes-heading">Escalation ledger</h3>
        {ledger.length === 0 ? (
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
                <th>milestone</th>
                <th>ask</th>
                <th>decision</th>
                <th>latency</th>
              </tr>
            </thead>
            <tbody>
              {ledger.map((row, i) => (
                <tr key={`${row.missionId}-${row.ts}-${i}`}>
                  <td className="mono">{row.ts}</td>
                  <td>{row.missionId}</td>
                  <td>{row.kind}</td>
                  <td>{row.milestoneId ?? '—'}</td>
                  <td>{row.ask}</td>
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
