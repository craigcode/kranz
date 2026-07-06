// #/backlog — the dashboard's browsable ticket backlog (docs/tickets.md
// "The backlog over REST"). Mirrors MissionPicker's list styling: rows use
// .picker-item / .picker-row, clicking one navigates to #/backlog/<slug>.

import { useEffect } from 'react';
import { useKranzStore } from '../lib/store';
import type { TicketSummary } from '../lib/types';

export function BacklogPanel() {
  const tickets = useKranzStore((s) => s.tickets);
  const ticketsError = useKranzStore((s) => s.ticketsError);
  const loadTickets = useKranzStore((s) => s.loadTickets);

  useEffect(() => {
    void loadTickets();
  }, [loadTickets]);

  const row = (t: TicketSummary) => (
    <li key={t.slug} className="picker-item">
      <button
        type="button"
        className="picker-row"
        onClick={() => {
          window.location.hash = `#/backlog/${encodeURIComponent(t.slug)}`;
        }}
      >
        <span className={`status-pill pill-${t.state}`}>
          <span className="status-dot" aria-hidden="true" />
          {t.state}
        </span>
        <span className="mono picker-id">{t.slug}</span>
        <span className="dim">p{t.priority}</span>
        <span className="picker-goal">{t.title}</span>
        {t.blockedBy.length > 0 &&
          (t.isBlocked && t.state !== 'done' ? (
            t.blockedBy.map((b) => (
              <span key={b} className="ticket-blocker-badge" title={`blocked by ${b}`}>
                blocked by {b}
              </span>
            ))
          ) : (
            <span
              className="ticket-blocker-badge ticket-blocker-badge--satisfied dim"
              title={`was blocked by ${t.blockedBy.join(', ')}`}
            >
              was blocked by {t.blockedBy.join(', ')}
            </span>
          ))}
      </button>
    </li>
  );

  return (
    <div className="picker">
      <div className="picker-box">
        <div className="picker-title">
          <span className="picker-brand mono">KRANZ</span>
          <span className="section-label">Backlog</span>
          <button
            type="button"
            className="btn-small new-mission-btn"
            onClick={() => {
              window.location.hash = '';
            }}
          >
            missions
          </button>
        </div>
        {ticketsError !== null && (
          <div className="picker-error" role="alert">
            Could not load tickets: {ticketsError}{' '}
            <button type="button" className="btn-small" onClick={() => void loadTickets()}>
              retry
            </button>
          </div>
        )}
        {ticketsError === null && tickets.length === 0 && (
          <div className="dim picker-empty" role="status">
            No tickets found. Create one with <code>kranz ticket new</code>.
          </div>
        )}
        <ul className="picker-list">{tickets.map(row)}</ul>
      </div>
    </div>
  );
}
