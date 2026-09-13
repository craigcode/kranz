// Pure lens-filter model for the pipeline view — docs/scoping/pipeline-view.md
// Filters WorkItem rows into one of four lenses without any React dependency.

import { pipelineStage, type PipelineStage, type WorkItem } from './pipelineStage';

export type Lens = 'actionable' | 'backlog' | 'missions' | 'all';

export const LENSES: ReadonlyArray<{ id: Lens; label: string }> = [
  { id: 'actionable', label: 'Actionable' },
  { id: 'backlog', label: 'Backlog' },
  { id: 'missions', label: 'Missions' },
  { id: 'all', label: 'All' },
];

/** The minimal row shape the filter needs; the real PipelineView Row satisfies this structurally. */
export interface LensRow {
  item: WorkItem;
  missionCreatedAt?: string;
}

const ACTIONABLE_STAGES: ReadonlySet<PipelineStage> = new Set([
  'captured',
  'needs-you',
  'wrong-plan',
  'reviewable',
  'delivered',
  'failed',
]);

const MISSIONS_ACTIVE_STAGES: ReadonlySet<PipelineStage> = new Set([
  'running',
  'queued',
  'reviewable',
  'delivered',
]);

const MISSIONS_TERMINAL_STAGES: ReadonlySet<PipelineStage> = new Set(['landed', 'failed']);

const TERMINAL_CAP = 10;

function isMissionBacked(item: WorkItem): boolean {
  return item.kind === 'mission' || item.mission !== undefined;
}

function filterActionable<T extends LensRow>(rows: T[]): T[] {
  return rows.filter((row) => ACTIONABLE_STAGES.has(pipelineStage(row.item)));
}

function filterBacklog<T extends LensRow>(rows: T[]): T[] {
  return rows.filter((row) => row.item.kind === 'ticket');
}

function filterMissions<T extends LensRow>(rows: T[]): T[] {
  const missionRows = rows
    .map((row, index) => ({ row, index }))
    .filter(({ row }) => isMissionBacked(row.item));

  const kept = new Set<number>();
  const terminal: Array<{ row: T; index: number }> = [];

  for (const entry of missionRows) {
    const stage = pipelineStage(entry.row.item);
    if (MISSIONS_ACTIVE_STAGES.has(stage)) {
      kept.add(entry.index);
    } else if (MISSIONS_TERMINAL_STAGES.has(stage)) {
      terminal.push(entry);
    }
    // 'abandoned' and any other stage: excluded entirely.
  }

  const sortedTerminal = [...terminal].sort((a, b) => {
    const aCreated = a.row.missionCreatedAt;
    const bCreated = b.row.missionCreatedAt;
    if (aCreated === undefined && bCreated === undefined) return 0;
    if (aCreated === undefined) return 1;
    if (bCreated === undefined) return -1;
    if (aCreated === bCreated) return 0;
    return aCreated > bCreated ? -1 : 1;
  });

  for (const entry of sortedTerminal.slice(0, TERMINAL_CAP)) {
    kept.add(entry.index);
  }

  return rows.filter((_, index) => kept.has(index));
}

/** Filters rows for the given lens, preserving input order (except for the missions terminal cap). */
export function filterLensRows<T extends LensRow>(rows: T[], lens: Lens): T[] {
  switch (lens) {
    case 'all':
      return rows;
    case 'actionable':
      return filterActionable(rows);
    case 'backlog':
      return filterBacklog(rows);
    case 'missions':
      return filterMissions(rows);
    default:
      return lens satisfies never;
  }
}

/** Number of rows currently in the 'landed' stage — used for the "Landed (N)" hint. */
export function landedCount(rows: LensRow[]): number {
  return rows.filter((row) => pipelineStage(row.item) === 'landed').length;
}
