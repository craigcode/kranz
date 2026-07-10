// #/backlog/<slug> — one ticket in full: goal/context (rendered as markdown),
// scoping answers / acceptance hints / needs-context questions as lists, a
// Draft button with live progress from the drafted mission's WS feed
// (draftTicket already calls connectMission — this pane just renders
// s.events/s.state the same way ProgressLog does), and a blocked-by-aware
// Queue button (courtesy mirror only: the server's 409 stays authoritative
// — see docs/tickets.md "Dependencies (blocked-by)").

import { useEffect, useState } from 'react';
import { useKranzStore } from '../lib/store';
import { api } from '../lib/api';
import { renderMarkdown } from '../lib/markdown';
import { relTime } from '../lib/format';
import type { MissionEvent, Ticket } from '../lib/types';

function describeEvent(e: MissionEvent): string {
  switch (e.type) {
    case 'worker.message':
      return `${e.payload.runId}: ${e.payload.content}`;
    case 'worker.spawned':
      return `worker ${e.payload.runId} spawned`;
    case 'worker.completed':
      return `worker ${e.payload.runId} completed (${e.payload.result})`;
    case 'mission.completed':
      return 'mission completed';
    case 'mission.failed':
      return `mission failed: ${e.payload.reason}`;
    default:
      return e.type;
  }
}

function ListSection({ title, items }: { title: string; items: string[] }) {
  if (items.length === 0) return null;
  return (
    <div className="ticket-section">
      <div className="section-label">{title}</div>
      <ul>
        {items.map((item, i) => (
          <li key={i}>{item}</li>
        ))}
      </ul>
    </div>
  );
}

export function TicketDetail({ slug }: { slug: string }) {
  const [ticket, setTicket] = useState<Ticket | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  const loadTickets = useKranzStore((s) => s.loadTickets);
  const ticketError = useKranzStore((s) => s.ticketError);
  const draftTicket = useKranzStore((s) => s.draftTicket);
  const approveTicket = useKranzStore((s) => s.approveTicket);
  const ticketBusySlug = useKranzStore((s) => s.ticketBusySlug);
  const missionId = useKranzStore((s) => s.missionId);
  const connection = useKranzStore((s) => s.connection);
  const events = useKranzStore((s) => s.events);
  const missionState = useKranzStore((s) => s.state);

  const load = () => {
    setLoadError(null);
    api
      .ticket(slug)
      .then(setTicket)
      .catch((err: unknown) => setLoadError(err instanceof Error ? err.message : String(err)));
  };

  useEffect(() => {
    load();
    void loadTickets();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [slug]);

  if (loadError !== null) {
    return (
      <div className="picker-error" role="alert">
        Could not load ticket: {loadError}{' '}
        <button type="button" className="btn-small" onClick={load}>
          retry
        </button>
      </div>
    );
  }

  if (ticket === null) {
    return (
      <div className="dim picker-empty" role="status">
        Loading ticket…
      </div>
    );
  }

  const showApprove = ticket.state === 'review';
  const ticketBusy = ticketBusySlug !== null;
  const approveDisabled = ticket.isBlocked || ticketBusy;
  const approveTitle = ticket.isBlocked
    ? `blocked by ${ticket.blockedBy.join(', ')}`
    : 'queue for run';
  // Only show draft progress when the live feed belongs to THIS ticket —
  // matching ticket.missionId, or no linked mission yet (fresh draft before
  // the ticket record catches up). Hide when the feed is for a different
  // mission so a previously viewed mission cannot leak here.
  const showDraftProgress =
    missionId !== null &&
    (ticket.missionId === undefined || ticket.missionId === missionId);

  return (
    <div className="picker">
      <div className="picker-box ticket-detail">
        <div className="picker-title">
          <span className="picker-brand mono">KRANZ</span>
          <span className="mono">{ticket.slug}</span>
          <span className={`status-pill pill-${ticket.state}`}>
            <span className="status-dot" aria-hidden="true" />
            {ticket.state}
          </span>
          <button
            type="button"
            className="btn-small new-mission-btn"
            onClick={() => {
              window.location.hash = '#/backlog';
            }}
          >
            backlog
          </button>
        </div>

        <h2>{ticket.title}</h2>

        <div className="ticket-section">
          <div className="section-label">Goal</div>
          {renderMarkdown(ticket.goal)}
        </div>

        <div className="ticket-section">
          <div className="section-label">Context</div>
          {renderMarkdown(ticket.context)}
        </div>

        <ListSection title="Scoping answers" items={ticket.scopingAnswers} />
        <ListSection title="Acceptance hints" items={ticket.acceptanceHints} />
        <ListSection title="Needs context" items={ticket.needsContext} />

        {ticketError !== null && (
          <div className="picker-error" role="alert">
            {ticketError}
          </div>
        )}

        <div className="composer-row">
          <button
            type="button"
            className="btn-small"
            disabled={ticketBusy}
            onClick={() => void draftTicket(slug)}
          >
            Draft
          </button>
          {showApprove && (
            <button
              type="button"
              className="btn-small"
              disabled={approveDisabled}
              title={approveTitle}
              onClick={() => void approveTicket(slug, false)}
            >
              Queue for run
            </button>
          )}
        </div>

        {showDraftProgress && (
          <div className="ticket-section">
            <div className="section-label">Draft progress ({connection})</div>
            {missionState !== null && (
              <div className="dim">mission status: {missionState.mission.status}</div>
            )}
            <ul className="log-list">
              {events.map((e) => (
                <li key={e.seq} className="log-row">
                  <span className="log-text">{describeEvent(e)}</span>
                  <span className="log-time dim mono">{relTime(e.ts)}</span>
                </li>
              ))}
            </ul>
          </div>
        )}
      </div>
    </div>
  );
}
