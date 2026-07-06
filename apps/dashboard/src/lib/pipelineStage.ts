// Stage-derivation model for the pipeline view — docs/scoping/pipeline-view.md
// "The stage model". One row of the pipeline table always reduces to exactly
// one of these nine stages, derived from ticket + (optionally joined) mission
// state. Surfaces render this model, never raw ticket/mission states.

import type { MissionStatus, TicketState } from './types';

export type PipelineStage =
  | 'captured'
  | 'drafting'
  | 'needs-you'
  | 'reviewable'
  | 'queued'
  | 'running'
  | 'delivered'
  | 'landed'
  | 'failed';

/**
 * Minimal mission shape the stage derivation needs. `merged` is the
 * ancestry-probe bit from `GET /api/missions` — true=landed, false/null=delivered.
 */
export interface WorkItemMission {
  status: MissionStatus;
  merged: boolean | null;
}

/** A ticket-backed row, optionally joined to its mission once one exists. */
export interface TicketWorkItem {
  kind: 'ticket';
  ticket: {
    slug: string;
    state: TicketState;
  };
  mission?: WorkItemMission;
}

/** A ticketless mission row — work with no originating ticket. */
export interface MissionWorkItem {
  kind: 'mission';
  mission: WorkItemMission;
}

export type WorkItem = TicketWorkItem | MissionWorkItem;

const MISSION_TAIL_RUNNING: ReadonlySet<MissionStatus> = new Set([
  'running',
  'paused',
  'blocked',
  'validating',
]);

function stageFromMission(mission: WorkItemMission): PipelineStage {
  if (mission.status === 'failed') return 'failed';
  if (mission.status === 'complete') return mission.merged === true ? 'landed' : 'delivered';
  if (MISSION_TAIL_RUNNING.has(mission.status)) return 'running';
  // planning / approved / abandoned: no mission-tail mapping is documented;
  // treat as running since the mission is the governing state at this point.
  return 'running';
}

/** Pure derivation: WorkItem -> one of the nine canonical pipeline stages. */
export function pipelineStage(item: WorkItem): PipelineStage {
  if (item.kind === 'mission') {
    return stageFromMission(item.mission);
  }

  if (item.mission) {
    return stageFromMission(item.mission);
  }

  switch (item.ticket.state) {
    case 'new':
      return 'captured';
    case 'drafting':
      return 'drafting';
    case 'needs-context':
      return 'needs-you';
    case 'review':
      return 'reviewable';
    case 'queued':
      return 'queued';
    case 'running':
      return 'running';
    case 'done':
      return 'delivered';
    case 'failed':
      return 'failed';
    default:
      return item.ticket.state satisfies never;
  }
}

export interface ActionDescriptor {
  label: string;
  secondary?: string;
}

const PRIMARY_ACTIONS: Record<PipelineStage, ActionDescriptor | null> = {
  captured: { label: 'Draft' },
  drafting: null,
  'needs-you': { label: 'Answer + redraft' },
  reviewable: { label: 'Queue', secondary: 'Reshape' },
  queued: null,
  running: null,
  delivered: { label: 'Merge', secondary: 'Iterate' },
  landed: { label: 'Iterate' },
  failed: { label: 'Redraft' },
};

/** The one primary action (plus any secondary offer) documented per stage. */
export function primaryAction(stage: PipelineStage): ActionDescriptor | null {
  return PRIMARY_ACTIONS[stage];
}
