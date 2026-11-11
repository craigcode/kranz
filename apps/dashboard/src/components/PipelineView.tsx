// #/ (default route) — the pipeline view: one flat list, one row per work
// item, reduced through the nine-stage model (lib/pipelineStage.ts). The
// #/backlog route is an alias for this same surface with the Backlog lens
// selected. Every ticket is a row; every mission with no originating ticket
// ("kranz new") is also a row, so nothing in progress is hidden.

import { lazy, Suspense, useEffect, useRef, useState } from 'react';
import { useKranzStore } from '../lib/store';
import { RunQueueButton } from './RunQueueButton';
import { api } from '../lib/api';
import { renderMarkdown } from '../lib/markdown';
import { visible } from '../lib/presentation';
import { buildRows, type Row } from '../lib/pipelineRows';
import { pipelineStage, primaryAction, parseEstimateFromPlanMd } from '../lib/pipelineStage';
import { LENSES, filterLensRows, landedCount, type Lens } from '../lib/lensFilter';
import { missionHash, repoHash, ticketHash } from '../lib/routes';
import { OutcomesPanel } from './OutcomesPanel';
import { FlightSurgeonPanel } from './FlightSurgeonPanel';
import { SignalsPanel } from './SignalsPanel';

const LazyBoardLens = lazy(() => import('./BoardLens'));

/** Reviewable rows: the persisted plan + cost estimate, read inline off
 *  `GET /api/missions/:id/plan.md` — no navigation away from the pipeline. */
function ReviewablePlan({ missionId }: { missionId: string }) {
  const [markdown, setMarkdown] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setMarkdown(null);
    setError(null);
    api
      .planMd(missionId)
      .then((res) => {
        if (!cancelled) setMarkdown(res.markdown);
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [missionId]);

  if (error !== null) {
    return (
      <div className="picker-error pipeline-inline-error">
        Could not load plan: {visible(error)}
      </div>
    );
  }
  if (markdown === null) {
    return <div className="dim pipeline-inline-loading">Loading plan…</div>;
  }

  const normalizedMarkdown = visible(markdown.replace(/\r\n/g, '\n'));
  const estimate = parseEstimateFromPlanMd(markdown);

  return (
    <div className="pipeline-inline-panel pipeline-inline-plan">
      {estimate !== null && (
        <div className="pipeline-estimate">
          Estimate: ${estimate.lowUsd.toFixed(2)} – ${estimate.highUsd.toFixed(2)} (expected $
          {estimate.expectedUsd.toFixed(2)})
        </div>
      )}
      {renderMarkdown(normalizedMarkdown)}
    </div>
  );
}

/** Shared reviewable ticket row (plan + persisted cost estimate + Queue for run),
 *  rendered both in the PipelineView list and beside the Trajectory Board (slice 6). */
export function ReviewableTicketRow({
  row: r,
  onQueued,
}: {
  row: Row;
  onQueued?: () => void;
}) {
  const approveTicket = useKranzStore((s) => s.approveTicket);
  const ticketBusySlug = useKranzStore((s) => s.ticketBusySlug);
  const action = primaryAction('reviewable');
  const busy = r.slug !== undefined && ticketBusySlug === r.slug;
  const blockedSummary = visible(r.blockedBy.join(', '));
  const title = r.isBlocked ? `blocked by ${blockedSummary}` : 'queue for run';
  const showBlockedBadge = r.blockedBy.length > 0 && r.isBlocked;
  const missionId =
    r.item.kind === 'mission' || r.item.mission !== undefined ? r.missionId : undefined;

  return (
    <>
      <div className="picker-item-header">
        <div className="picker-row">
          <span className="status-pill pill-reviewable">
            <span className="status-dot" aria-hidden="true" />
            reviewable
          </span>
          {missionId !== undefined ? (
            <a className="mono picker-id" href={missionHash(missionId)}>
              {visible(r.id)}
            </a>
          ) : (
            <span className="mono picker-id">{visible(r.id)}</span>
          )}
          <span className="picker-goal">{visible(r.title)}</span>
          {showBlockedBadge && (
            <span className="ticket-blocker-badge" title={`blocked by ${blockedSummary}`}>
              blocked by {blockedSummary}
            </span>
          )}
        </div>
        {r.slug !== undefined && (
          <button
            type="button"
            className="btn-small pipeline-primary-action"
            disabled={r.isBlocked || ticketBusySlug !== null}
            title={title}
            onClick={() => {
              void approveTicket(r.slug!, false, r.missionId ?? null).then(() => {
                onQueued?.();
              });
            }}
          >
            {busy ? 'Queuing…' : 'Queue for run'}
          </button>
        )}
        {r.slug !== undefined && action?.secondary !== undefined && (
          <a className="btn-small pipeline-secondary-action" href={ticketHash(r.slug)}>
            {action.secondary}
          </a>
        )}
      </div>
      {r.missionId !== undefined && <ReviewablePlan missionId={r.missionId} />}
    </>
  );
}

/** Delivered rows: the mission report + diff summary, read inline off
 *  `GET /api/missions/:id/report.md` and `GET /api/missions/:id/diff-stat`. */
function DeliveredReport({ missionId }: { missionId: string }) {
  const [report, setReport] = useState<string | null>(null);
  const [diffStat, setDiffStat] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setReport(null);
    setDiffStat(null);
    setError(null);
    Promise.all([api.reportMd(missionId), api.diffStat(missionId)])
      .then(([reportRes, diffRes]) => {
        if (cancelled) return;
        setReport(reportRes.markdown);
        setDiffStat(diffRes.diffStat);
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [missionId]);

  if (error !== null) {
    return (
      <div className="picker-error pipeline-inline-error">
        Could not load report: {visible(error)}
      </div>
    );
  }
  if (report === null || diffStat === null) {
    return <div className="dim pipeline-inline-loading">Loading report…</div>;
  }

  return (
    <div className="pipeline-inline-panel pipeline-inline-report">
      <pre className="pipeline-diff-stat">{visible(diffStat.replace(/\r\n/g, '\n'))}</pre>
      {renderMarkdown(visible(report.replace(/\r\n/g, '\n')))}
    </div>
  );
}

/** Abandoned rows are inert for work actions, but still need cleanup. This is
 *  the same two-step affordance as StatusStrip's abandon control, routed
 *  through the existing `kranz clean` web twin. */
function DeleteMissionControl({ missionId }: { missionId: string }) {
  const deleteMission = useKranzStore((s) => s.deleteMission);
  const [armedDelete, setArmedDelete] = useState(false);

  useEffect(() => {
    if (!armedDelete) return;
    const t = window.setTimeout(() => setArmedDelete(false), 5000);
    return () => window.clearTimeout(t);
  }, [armedDelete]);

  if (armedDelete) {
    return (
      <button
        type="button"
        className="btn-small picker-action-danger pipeline-delete-action"
        title="permanently removes this abandoned mission and prunes it from the index"
        onClick={() => {
          setArmedDelete(false);
          void deleteMission(missionId, false);
        }}
      >
        confirm delete
      </button>
    );
  }

  return (
    <button
      type="button"
      className="btn-small pipeline-delete-action"
      onClick={() => setArmedDelete(true)}
    >
      Delete
    </button>
  );
}

/** Iterate (Delivered + Landed): one tap opens an inline one-line-direction
 *  box; submitting creates a follow-up ticket via `POST /api/tickets`,
 *  seeded with the finished mission's report as context, then routes to the
 *  new ticket's row. */
function IterateControl({ missionId, className }: { missionId: string; className: string }) {
  const [open, setOpen] = useState(false);
  const [direction, setDirection] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async () => {
    const trimmed = direction.trim();
    if (trimmed === '') return;
    setSubmitting(true);
    setError(null);
    try {
      const report = await api
        .reportMd(missionId)
        .then((res) => res.markdown)
        .catch(() => '');
      const context =
        report !== ''
          ? `Follow-up on ${missionId}:\n\n${trimmed}\n\n---\n\n${report}`
          : trimmed;
      const created = await api.createTicket({
        slug: `iterate-${missionId}-${Math.random().toString(36).slice(2, 8)}`,
        title: trimmed,
        goal: trimmed,
        context,
      });
      window.location.hash = ticketHash(created.slug);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setSubmitting(false);
    }
  };

  if (!open) {
    return (
      <button type="button" className={`btn-small ${className}`} onClick={() => setOpen(true)}>
        Iterate
      </button>
    );
  }

  return (
    <div className="pipeline-iterate-form">
      <input
        type="text"
        className="pipeline-iterate-input"
        placeholder="One-line direction for the follow-up"
        value={direction}
        onChange={(e) => setDirection(e.target.value)}
      />
      <button
        type="button"
        className="btn-small pipeline-iterate-submit"
        disabled={submitting || direction.trim() === ''}
        onClick={() => void submit()}
      >
        Iterate
      </button>
      {error !== null && <span className="pipeline-inline-error">{error}</span>}
    </div>
  );
}

export function PipelineView({
  initialLens = 'actionable',
  boardDemo = false,
}: {
  initialLens?: Lens;
  boardDemo?: boolean;
}) {
  const repoId = useKranzStore((s) => s.repoId);
  const tickets = useKranzStore((s) => s.tickets);
  const ticketsError = useKranzStore((s) => s.ticketsError);
  const loadTickets = useKranzStore((s) => s.loadTickets);
  const missions = useKranzStore((s) => s.missions);
  const missionsError = useKranzStore((s) => s.missionsError);
  const loadMissions = useKranzStore((s) => s.loadMissions);
  const draftTicket = useKranzStore((s) => s.draftTicket);
  const approveTicket = useKranzStore((s) => s.approveTicket);
  const ticketBusySlug = useKranzStore((s) => s.ticketBusySlug);
  const [lens, setLens] = useState<Lens>(initialLens);
  const pendingExitDemoLensRef = useRef<Lens | null>(null);
  const isBoard = lens === 'board';

  useEffect(() => {
    if (boardDemo || isBoard) return;
    void loadTickets();
    void loadMissions();
  }, [boardDemo, isBoard, loadTickets, loadMissions]);

  useEffect(() => {
    if (!boardDemo && pendingExitDemoLensRef.current !== null) {
      setLens(pendingExitDemoLensRef.current);
      pendingExitDemoLensRef.current = null;
      return;
    }
    setLens(initialLens);
  }, [initialLens, boardDemo]);

  const selectLens = (next: Lens) => {
    if (boardDemo && next !== 'board') {
      pendingExitDemoLensRef.current = next;
      const suffix = next === 'actionable' && repoId !== null ? '' : next;
      window.location.hash = repoHash(suffix);
    }
    setLens(next);
  };

  const rows = buildRows(tickets, missions);
  const visibleRows = filterLensRows(rows, lens);

  const renderPrimary = (row: Row, stage: ReturnType<typeof pipelineStage>) => {
    const action = primaryAction(stage);
    if (action === null) return null;

    if (stage === 'captured' && row.slug !== undefined) {
      const busy = ticketBusySlug === row.slug;
      return (
        <button
          type="button"
          className="btn-small pipeline-primary-action"
          disabled={ticketBusySlug !== null}
          onClick={() =>
            void (row.isUnconsentedSignal
              ? draftTicket(row.slug!, { consentSignal: true })
              : draftTicket(row.slug!))
          }
        >
          {busy ? 'Drafting…' : action.label}
        </button>
      );
    }

    if (stage === 'reviewable' && row.slug !== undefined) {
      const busy = ticketBusySlug === row.slug;
      const title = row.isBlocked ? `blocked by ${row.blockedBy.join(', ')}` : 'queue for run';
      return (
        <button
          type="button"
          className="btn-small pipeline-primary-action"
          disabled={row.isBlocked || ticketBusySlug !== null}
          title={title}
          onClick={() => void approveTicket(row.slug!, false, row.missionId ?? null)}
        >
          {busy ? 'Queuing…' : 'Queue for run'}
        </button>
      );
    }

    if (stage === 'reviewable' && row.slug === undefined && row.missionId !== undefined) {
      return (
        <a
          className="btn-small pipeline-primary-action"
          href={missionHash(row.missionId)}
        >
          Approve plan
        </a>
      );
    }

    if (stage === 'delivered' && row.missionId !== undefined) {
      return (
        <a
          className="btn-small pipeline-primary-action"
          href={missionHash(row.missionId)}
        >
          {action.label}
        </a>
      );
    }

    if (stage === 'landed' && row.missionId !== undefined) {
      return <IterateControl missionId={row.missionId} className="pipeline-primary-action" />;
    }

    if (stage === 'failed') {
      const href =
        row.slug !== undefined
          ? ticketHash(row.slug)
          : row.missionId !== undefined
            ? missionHash(row.missionId)
            : undefined;
      if (href === undefined) return null;
      return (
        <a className="btn-small pipeline-primary-action" href={href}>
          {action.label}
        </a>
      );
    }

    if ((stage === 'needs-you' || stage === 'wrong-plan') && row.slug !== undefined) {
      return (
        <a
          className="btn-small pipeline-primary-action"
          href={ticketHash(row.slug)}
        >
          {action.label}
        </a>
      );
    }

    return null;
  };

  const renderSecondary = (row: Row, stage: ReturnType<typeof pipelineStage>) => {
    const action = primaryAction(stage);
    if (action?.secondary === undefined) return null;

    if (stage === 'reviewable' && row.slug !== undefined) {
      return (
        <a
          className="btn-small pipeline-secondary-action"
          href={ticketHash(row.slug)}
        >
          {action.secondary}
        </a>
      );
    }

    if (stage === 'delivered' && row.missionId !== undefined) {
      return <IterateControl missionId={row.missionId} className="pipeline-secondary-action" />;
    }

    return null;
  };

  const renderCleanup = (row: Row, stage: ReturnType<typeof pipelineStage>) => {
    if (stage === 'abandoned' && row.missionId !== undefined) {
      return <DeleteMissionControl missionId={row.missionId} />;
    }

    return null;
  };

  const linkedMissionId = (row: Row) =>
    row.item.kind === 'mission' || row.item.mission !== undefined ? row.missionId : undefined;

  const renderRowId = (row: Row) => {
    const missionId = linkedMissionId(row);
    if (missionId !== undefined) {
      return (
        <a className="mono picker-id" href={missionHash(missionId)}>
          {visible(row.id)}
        </a>
      );
    }

    return <span className="mono picker-id">{visible(row.id)}</span>;
  };

  const row = (r: Row) => {
    const stage = pipelineStage(r.item);
    if (stage === 'reviewable' && r.slug !== undefined) {
      return (
        <li key={r.key} className="picker-item picker-item--stacked">
          <ReviewableTicketRow row={r} />
        </li>
      );
    }
    const showBlockedBadge =
      (stage === 'captured' || stage === 'reviewable') && r.blockedBy.length > 0 && r.isBlocked;
    const blockedSummary = visible(r.blockedBy.join(', '));

    return (
      <li key={r.key} className="picker-item picker-item--stacked">
        <div className="picker-item-header">
          <div className="picker-row">
            <span className={`status-pill pill-${stage}`}>
              <span className="status-dot" aria-hidden="true" />
              {stage}
            </span>
            {renderRowId(r)}
            <span className="picker-goal">{visible(r.title)}</span>
            {showBlockedBadge && (
              <span className="ticket-blocker-badge" title={`blocked by ${blockedSummary}`}>
                blocked by {blockedSummary}
              </span>
            )}
            {stage === 'delivered' && (
              <span className="unmerged-badge" title="mission complete, not yet merged">
                UNMERGED
              </span>
            )}
          </div>
          {renderPrimary(r, stage)}
          {renderSecondary(r, stage)}
          {renderCleanup(r, stage)}
        </div>
        {stage === 'reviewable' && r.missionId !== undefined && (
          <ReviewablePlan missionId={r.missionId} />
        )}
        {stage === 'delivered' && r.missionId !== undefined && (
          <DeliveredReport missionId={r.missionId} />
        )}
      </li>
    );
  };

  return (
    <div className={'picker' + (isBoard ? ' picker--board' : '')}>
      <div className={'picker-box' + (isBoard ? ' picker-box--board' : '')}>
        <div className="picker-title">
          <span className="picker-brand mono">KRANZ</span>
          {repoId !== null && (
            <button
              type="button"
              className="btn-small"
              onClick={() => {
                window.location.hash = '#/';
              }}
            >
              projects
            </button>
          )}
          {repoId !== null && <span className="mono project-current">{repoId}</span>}
          <span className="section-label">Pipeline</span>
          <button
            type="button"
            className="btn-small new-ticket-btn"
            onClick={() => {
              window.location.hash = repoHash('new-ticket');
            }}
          >
            + new ticket
          </button>
          <button
            type="button"
            className="btn-small new-mission-btn"
            onClick={() => {
              window.location.hash = repoHash('new');
            }}
          >
            + new mission
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
        {missionsError !== null && (
          <div className="picker-error" role="alert">
            Could not load missions: {missionsError}{' '}
            <button type="button" className="btn-small" onClick={() => void loadMissions()}>
              retry
            </button>
          </div>
        )}
        <div className="lens-bar" role="tablist">
          {LENSES.map((l) => (
            <button
              key={l.id}
              type="button"
              className={'lens-tab' + (lens === l.id ? ' lens-tab--active' : '')}
              aria-pressed={lens === l.id}
              onClick={() => selectLens(l.id)}
            >
              {l.label}
            </button>
          ))}
        </div>
        {!boardDemo && <RunQueueButton />}
        {isBoard ? (
          <Suspense fallback={<div className="dim picker-empty">Loading board…</div>}>
            <LazyBoardLens demo={boardDemo && lens === 'board'} />
          </Suspense>
        ) : (
          <>
            {lens === 'actionable' && landedCount(rows) > 0 && (
              <button
                type="button"
                className="lens-landed-hint"
                onClick={() => selectLens('all')}
              >
                Landed ({landedCount(rows)}) — view all
              </button>
            )}
            {ticketsError === null && missionsError === null && rows.length === 0 && (
              <div className="dim picker-empty" role="status">
                No work items found. Create one with <code>kranz ticket new</code>.
              </div>
            )}
            <ul className="picker-list">{visibleRows.map(row)}</ul>
            {!boardDemo && (
              <>
                <SignalsPanel />
                <FlightSurgeonPanel />
                <OutcomesPanel />
              </>
            )}
          </>
        )}
      </div>
    </div>
  );
}
