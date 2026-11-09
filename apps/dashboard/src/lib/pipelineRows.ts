// Shared row builder for PipelineView and the Trajectory Board fleet model.

import type { WorkItem, WorkItemMission } from './pipelineStage';
import type { MissionSummary, TicketSummary } from './types';

export interface Row {
  key: string;
  item: WorkItem;
  id: string;
  title: string;
  isBlocked: boolean;
  blockedBy: string[];
  isUnconsentedSignal?: boolean;
  missionId?: string;
  slug?: string;
  missionCreatedAt?: string;
}

export function toWorkItemMission(m: MissionSummary): WorkItemMission {
  return { status: m.status as WorkItemMission['status'], merged: m.merged ?? null };
}

export function buildRows(tickets: TicketSummary[], missions: MissionSummary[]): Row[] {
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
      isUnconsentedSignal: t.signalProposal === 'unconsented',
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
