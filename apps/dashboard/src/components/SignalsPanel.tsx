// Read-only signal intake ledger view (`GET /api/signals`, ticket
// `signal-intake-10-surfaces`). Lists signals across all triage outcomes
// (received, parked, not-a-code-change, proposed, consented, closed) with
// their ledger evidence.

import { useEffect, useState } from 'react';
import { api } from '../lib/api';
import { ticketHash } from '../lib/routes';
import type { SignalsView } from '../lib/types';

export function SignalsPanel() {
  const [view, setView] = useState<SignalsView | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (typeof api.signals !== 'function') return;
    let cancelled = false;
    api
      .signals({ tokenGate: false })
      .then((res) => {
        if (!cancelled) setView(res);
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (error !== null) {
    return (
      <section className="panel signals-panel">
        <div className="section-label">Signals</div>
        <div className="picker-error" role="alert">
          Could not load signals: {error}
        </div>
      </section>
    );
  }

  if (view === null) {
    return (
      <section className="panel signals-panel">
        <div className="section-label">Signals</div>
        <div className="dim panel-empty">—</div>
      </section>
    );
  }

  const { signals, dropped } = view;

  return (
    <section className="panel signals-panel" data-testid="signals-panel">
      <div className="section-label">Signals</div>
      {signals.length === 0 && dropped.length === 0 ? (
        <div className="dim panel-empty" role="status">
          No signals recorded
        </div>
      ) : (
        <>
          {signals.length > 0 && (
            <table className="outcomes-escalation-table signals-table">
              <thead>
                <tr>
                  <th>signal</th>
                  <th>source</th>
                  <th>status</th>
                  <th>severity</th>
                  <th>fingerprint</th>
                  <th>ticket</th>
                  <th>evidence</th>
                </tr>
              </thead>
              <tbody>
                {signals.map((sig) => (
                  <tr key={sig.signalId} data-testid={`signal-row-${sig.signalId}`}>
                    <td className="mono">{sig.signalId}</td>
                    <td>{sig.source}</td>
                    <td>
                      <span className={`status-pill pill-${sig.status}`}>{sig.status}</span>
                    </td>
                    <td>{sig.severity}</td>
                    <td className="mono">{sig.fingerprint}</td>
                    <td>
                      {sig.ticketSlug != null ? (
                        <a className="mono" href={ticketHash(sig.ticketSlug)}>
                          {sig.ticketSlug}
                        </a>
                      ) : (
                        '—'
                      )}
                    </td>
                    <td className="mono">
                      sha256:{sig.rawSha256.slice(0, 12)}
                      {sig.dedupeCount > 1 ? ` (×${sig.dedupeCount})` : ''}
                      {sig.reason != null ? ` — ${sig.reason}` : ''}
                      {sig.confirmedTracedMission != null
                        ? ` [traced: ${sig.confirmedTracedMission}]`
                        : sig.candidateMissions.length > 0
                          ? ` [candidates: ${sig.candidateMissions.join(', ')}]`
                          : ''}
                      {sig.ledgerEntries.length > 0
                        ? ` (${sig.ledgerEntries.length} ledger events)`
                        : ''}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {dropped.length > 0 && (
            <div className="signals-dropped" data-testid="signals-dropped">
              <h3 className="outcomes-heading">Dropped signals</h3>
              <ul>
                {dropped.map((d) => (
                  <li key={d.seq} className="mono dim">
                    #{d.seq} {d.source}: {d.reason} (sha256:{d.rawSha256.slice(0, 12)})
                  </li>
                ))}
              </ul>
            </div>
          )}
        </>
      )}
    </section>
  );
}
