// #/ (default route) — the pipeline view: one flat list, one row per work
// item, reduced through the nine-stage model (lib/pipelineStage.ts). The
// #/backlog route is an alias for this same surface with the Backlog lens
// selected. Every ticket is a row; every mission with no originating ticket
// ("kranz new") is also a row, so nothing in progress is hidden.

import { useEffect, useState } from 'react';
import { useKranzStore } from '../lib/store';
import { RunQueueButton } from './RunQueueButton';
import { api } from '../lib/api';
import { renderMarkdown } from '../lib/markdown';
import { pipelineStage, primaryAction, parseEstimateFromPlanMd } from '../lib/pipelineStage';
import type { WorkItem, WorkItemMission } from '../lib/pipelineStage';
import type { MissionSummary, TicketSummary } from '../lib/types';
import { LENSES, filterLensRows, landedCount, type Lens } from '../lib/lensFilter';
import { missionHash, repoHash, ticketHash } from '../lib/routes';
import { OutcomesPanel } from './OutcomesPanel';

interface Row {
  key: string;
  item: WorkItem;
  id: string;
  title: string;
  isBlocked: boolean;
  blockedBy: string[];
  missionId?: string;
  slug?: string;
  missionCreatedAt?: string;
}

function toWorkItemMission(m: MissionSummary): WorkItemMission {
  return { status: m.status as WorkItemMission['status'], merged: m.merged ?? null };
}

function buildRows(tickets: TicketSummary[], missions: MissionSummary[]): Row[] {
  const missionsById = new Map(missions.map((m) => [m.id, m]));
  const consumedMissionIds = new Set<string>();

  const ticketRows: Row[] = tickets.map((t) => {
    const mission = t.missionId !== null ? missionsById.get(t.missionId) : undefined;
    if (mission !== undefined) consumedMissionIds.add(mission.id);
    return {
      key: `ticket-${t.slug}`,
      item: {
        kind: 'ticket',
        ticket: { slug: t.slug, state: t.state },
        mission: mission !== undefined ? toWorkItemMission(mission) : undefined,
      },
      id: t.slug,
      title: t.title,
      isBlocked: t.isBlocked,
      blockedBy: t.blockedBy,
      missionId: t.missionId ?? undefined,
      slug: t.slug,
      missionCreatedAt: mission?.createdAt,
    };
  });

  const missionRows: Row[] = missions
    .filter((m) => m.status !== 'deleted' && !consumedMissionIds.has(m.id))
    .map((m) => ({
      key: `mission-${m.id}`,
      item: { kind: 'mission', mission: toWorkItemMission(m) },
      id: m.id,
      title: m.goal,
      isBlocked: false,
      blockedBy: [],
      missionId: m.id,
      missionCreatedAt: m.createdAt,
    }));

  return [...ticketRows, ...missionRows];
}

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
    return <div className="picker-error pipeline-inline-error">Could not load plan: {error}</div>;
  }
  if (markdown === null) {
    return <div className="dim pipeline-inline-loading">Loading plan…</div>;
  }

  const estimate = parseEstimateFromPlanMd(markdown);

  return (
    <div className="pipeline-inline-panel pipeline-inline-plan">
      {estimate !== null && (
        <div className="pipeline-estimate">
          Estimate: ${estimate.lowUsd.toFixed(2)} – ${estimate.highUsd.toFixed(2)} (expected $
          {estimate.expectedUsd.toFixed(2)})
        </div>
      )}
      {renderMarkdown(markdown)}
    </div>
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
      <div className="picker-error pipeline-inline-error">Could not load report: {error}</div>
    );
  }
  if (report === null || diffStat === null) {
    return <div className="dim pipeline-inline-loading">Loading report…</div>;
  }

  return (
    <div className="pipeline-inline-panel pipeline-inline-report">
      <pre className="pipeline-diff-stat">{diffStat}</pre>
      {renderMarkdown(report)}
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

export function PipelineView({ initialLens = 'actionable' }: { initialLens?: Lens }) {
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

  useEffect(() => {
    void loadTickets();
    void loadMissions();
  }, [loadTickets, loadMissions]);

  useEffect(() => {
    setLens(initialLens);
  }, [initialLens]);

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
          onClick={() => void draftTicket(row.slug!)}
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
          onClick={() => void approveTicket(row.slug!, false)}
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

    if (stage === 'needs-you' && row.slug !== undefined) {
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
          {row.id}
        </a>
      );
    }

    return <span className="mono picker-id">{row.id}</span>;
  };

  const row = (r: Row) => {
    const stage = pipelineStage(r.item);
    const showBlockedBadge =
      (stage === 'captured' || stage === 'reviewable') && r.blockedBy.length > 0 && r.isBlocked;

    return (
      <li key={r.key} className="picker-item picker-item--stacked">
        <div className="picker-item-header">
          <div className="picker-row">
            <span className={`status-pill pill-${stage}`}>
              <span className="status-dot" aria-hidden="true" />
              {stage}
            </span>
            {renderRowId(r)}
            <span className="picker-goal">{r.title}</span>
            {showBlockedBadge && (
              <span className="ticket-blocker-badge" title={`blocked by ${r.blockedBy.join(', ')}`}>
                blocked by {r.blockedBy.join(', ')}
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
    <div className="picker">
      <div className="picker-box">
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
              onClick={() => setLens(l.id)}
            >
              {l.label}
            </button>
          ))}
        </div>
        <RunQueueButton />
        {lens === 'actionable' && landedCount(rows) > 0 && (
          <button
            type="button"
            className="lens-landed-hint"
            onClick={() => setLens('all')}
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
        <OutcomesPanel />
      </div>
    </div>
  );
}
