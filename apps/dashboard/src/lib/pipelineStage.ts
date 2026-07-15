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
  | 'failed'
  | 'abandoned';

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
  if (mission.status === 'planning') return 'reviewable';
  if (mission.status === 'approved') return 'queued';
  // 'abandoned', plus a defensive 'deleted' (mission summaries cast status
  // via `as`, so that string can arrive even though MissionStatus omits it).
  return 'abandoned';
}

/** Pure derivation: WorkItem -> one of the nine canonical pipeline stages. */
export function pipelineStage(item: WorkItem): PipelineStage {
  if (item.kind === 'mission') {
    return stageFromMission(item.mission);
  }

  // A ticket's own state tracks its whole lifecycle; the joined mission is
  // only consulted to split the terminal 'done' state into delivered/landed.
  // Mission.status must NOT influence a ticket-backed row's head stages.
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
    case 'done': {
      const mission = item.mission;
      // No live mission joined (direct-fixed) or the joined mission is dead
      // (abandoned/deleted) — the ticket's work already landed; there's
      // nothing left to merge.
      if (mission === undefined || mission.status === 'abandoned' || (mission.status as string) === 'deleted') {
        return 'landed';
      }
      return mission.merged === true ? 'landed' : 'delivered';
    }
    case 'failed':
      return 'failed';
    case 'parked':
      // Plan is already committed — surface under reviewable so Queue works.
      return 'reviewable';
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
  abandoned: null,
};

/** The one primary action (plus any secondary offer) documented per stage. */
export function primaryAction(stage: PipelineStage): ActionDescriptor | null {
  return PRIMARY_ACTIONS[stage];
}

export interface PlanEstimate {
  lowUsd: number;
  expectedUsd: number;
  highUsd: number;
}

// Matches the "## Cost estimate" line rendered by
// `orchestrator::render_plan_markdown` — "Estimated **$1.00 – $2.00**
// (expected ~$1.50)." — the estimate is not exposed on any other GET, so the
// pipeline view reads it out of the plan markdown it already fetches.
const ESTIMATE_RE = /Estimated \*\*\$([0-9.]+)\s*[–-]\s*\$([0-9.]+)\*\*\s*\(expected ~\$([0-9.]+)\)/;

/** Pulls the persisted low/expected/high USD estimate out of plan.md. */
export function parseEstimateFromPlanMd(markdown: string): PlanEstimate | null {
  const m = ESTIMATE_RE.exec(markdown);
  if (m === null) return null;
  return { lowUsd: Number(m[1]), highUsd: Number(m[2]), expectedUsd: Number(m[3]) };
}
