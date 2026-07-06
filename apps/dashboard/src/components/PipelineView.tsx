// #/ (default route) — the pipeline view: one flat list, one row per work
// item, reduced through the nine-stage model (lib/pipelineStage.ts). Replaces
// the old two-list landing (MissionPicker + BacklogPanel) — the picker/backlog
// panels stay reachable at their own hashes for now, this is just no longer
// the default. Every ticket is a row; every mission with no originating
// ticket ("kranz new") is also a row, so nothing in progress is hidden.

import { useEffect } from 'react';
import { useKranzStore } from '../lib/store';
import { RunQueueButton } from './RunQueueButton';
import { pipelineStage, primaryAction } from '../lib/pipelineStage';
import type { WorkItem, WorkItemMission } from '../lib/pipelineStage';
import type { MissionSummary, TicketSummary } from '../lib/types';

interface Row {
  key: string;
  item: WorkItem;
  id: string;
  title: string;
  isBlocked: boolean;
  blockedBy: string[];
  missionId?: string;
  slug?: string;
}

function toWorkItemMission(m: MissionSummary): WorkItemMission {
  return { status: m.status as WorkItemMission['status'], merged: m.merged ?? null };
}

function buildRows(tickets: TicketSummary[], missions: MissionSummary[]): Row[] {
  const missionsById = new Map(missions.map((m) => [m.id, m]));
  const consumedMissionIds = new Set<string>();

  const ticketRows: Row[] = tickets.map((t) => {
    const mission = t.missionId !== undefined ? missionsById.get(t.missionId) : undefined;
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
      missionId: t.missionId,
      slug: t.slug,
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
    }));

  return [...ticketRows, ...missionRows];
}

export function PipelineView() {
  const tickets = useKranzStore((s) => s.tickets);
  const ticketsError = useKranzStore((s) => s.ticketsError);
  const loadTickets = useKranzStore((s) => s.loadTickets);
  const missions = useKranzStore((s) => s.missions);
  const missionsError = useKranzStore((s) => s.missionsError);
  const loadMissions = useKranzStore((s) => s.loadMissions);
  const draftTicket = useKranzStore((s) => s.draftTicket);
  const approveTicket = useKranzStore((s) => s.approveTicket);

  useEffect(() => {
    void loadTickets();
    void loadMissions();
  }, [loadTickets, loadMissions]);

  const rows = buildRows(tickets, missions);

  const renderPrimary = (row: Row, stage: ReturnType<typeof pipelineStage>) => {
    const action = primaryAction(stage);
    if (action === null) return null;

    if (stage === 'captured' && row.slug !== undefined) {
      return (
        <button
          type="button"
          className="btn-small pipeline-primary-action"
          onClick={() => void draftTicket(row.slug!)}
        >
          {action.label}
        </button>
      );
    }

    if (stage === 'reviewable' && row.slug !== undefined) {
      const title = row.isBlocked ? `blocked by ${row.blockedBy.join(', ')}` : 'queue for run';
      return (
        <button
          type="button"
          className="btn-small pipeline-primary-action"
          disabled={row.isBlocked}
          title={title}
          onClick={() => void approveTicket(row.slug!, false)}
        >
          Queue for run
        </button>
      );
    }

    if ((stage === 'delivered' || stage === 'landed') && row.missionId !== undefined) {
      return (
        <a
          className="btn-small pipeline-primary-action"
          href={`#/m/${encodeURIComponent(row.missionId)}`}
        >
          {action.label}
        </a>
      );
    }

    if (stage === 'failed') {
      const href =
        row.slug !== undefined
          ? `#/backlog/${encodeURIComponent(row.slug)}`
          : row.missionId !== undefined
            ? `#/m/${encodeURIComponent(row.missionId)}`
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
          href={`#/backlog/${encodeURIComponent(row.slug)}`}
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
          href={`#/backlog/${encodeURIComponent(row.slug)}`}
        >
          {action.secondary}
        </a>
      );
    }

    if (stage === 'delivered' && row.missionId !== undefined) {
      return (
        <a
          className="btn-small pipeline-secondary-action"
          href={`#/m/${encodeURIComponent(row.missionId)}`}
        >
          {action.secondary}
        </a>
      );
    }

    return null;
  };

  const row = (r: Row) => {
    const stage = pipelineStage(r.item);
    const showBlockedBadge =
      (stage === 'captured' || stage === 'reviewable') && r.blockedBy.length > 0 && r.isBlocked;

    return (
      <li key={r.key} className="picker-item">
        <div className="picker-row">
          <span className={`status-pill pill-${stage}`}>
            <span className="status-dot" aria-hidden="true" />
            {stage}
          </span>
          <span className="mono picker-id">{r.id}</span>
          <span className="picker-goal">{r.title}</span>
          {showBlockedBadge && (
            <span className="ticket-blocker-badge" title={`blocked by ${r.blockedBy.join(', ')}`}>
              blocked by {r.blockedBy.join(', ')}
            </span>
          )}
        </div>
        {renderPrimary(r, stage)}
        {renderSecondary(r, stage)}
      </li>
    );
  };

  return (
    <div className="picker">
      <div className="picker-box">
        <div className="picker-title">
          <span className="picker-brand mono">KRANZ</span>
          <span className="section-label">Pipeline</span>
          <button
            type="button"
            className="btn-small new-mission-btn"
            onClick={() => {
              window.location.hash = '#/new';
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
        <RunQueueButton />
        {ticketsError === null && missionsError === null && rows.length === 0 && (
          <div className="dim picker-empty" role="status">
            No work items found. Create one with <code>kranz ticket new</code>.
          </div>
        )}
        <ul className="picker-list">{rows.map(row)}</ul>
      </div>
    </div>
  );
}
