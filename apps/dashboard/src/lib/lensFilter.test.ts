import { describe, expect, it } from 'vitest';
import { filterLensRows, landedCount, LENSES, type LensRow } from './lensFilter';
import type { WorkItem } from './pipelineStage';

function ticketRow(
  state: 'new' | 'drafting' | 'needs-context' | 'wrong-plan' | 'review' | 'queued' | 'running' | 'done' | 'failed',
  opts?: { mission?: { status: any; merged: boolean | null }; missionCreatedAt?: string },
): LensRow {
  const item: WorkItem = { kind: 'ticket', ticket: { slug: 't-1', state }, mission: opts?.mission };
  return { item, missionCreatedAt: opts?.missionCreatedAt };
}

function missionRow(status: any, merged: boolean | null = null, missionCreatedAt?: string): LensRow {
  const item: WorkItem = { kind: 'mission', mission: { status, merged } };
  return { item, missionCreatedAt };
}

describe('LENSES', () => {
  it('lists lenses in the exact order/labels', () => {
    expect(LENSES).toEqual([
      { id: 'actionable', label: 'Actionable' },
      { id: 'backlog', label: 'Backlog' },
      { id: 'missions', label: 'Missions' },
      { id: 'all', label: 'All' },
    ]);
  });
});

describe('filterLensRows - all', () => {
  it('All lens shows every row', () => {
    const rows = [
      ticketRow('new'),
      ticketRow('done'),
      missionRow('running'),
      missionRow('abandoned'),
    ];
    expect(filterLensRows(rows, 'all')).toEqual(rows);
  });
});

describe('filterLensRows - actionable', () => {
  it('actionable excludes landed and abandoned', () => {
    const captured = ticketRow('new');
    const needsYou = ticketRow('needs-context');
    const wrongPlan = ticketRow('wrong-plan');
    const reviewable = ticketRow('review');
    const delivered = ticketRow('done', { mission: { status: 'complete', merged: false } });
    const failed = ticketRow('failed');
    const landed = ticketRow('done');
    const abandoned = missionRow('abandoned');

    const rows = [captured, needsYou, wrongPlan, reviewable, delivered, failed, landed, abandoned];
    const result = filterLensRows(rows, 'actionable');

    expect(result).toEqual([captured, needsYou, wrongPlan, reviewable, delivered, failed]);
  });
});

describe('filterLensRows - backlog', () => {
  it('Backlog lens shows only ticket-kind rows', () => {
    const ticket1 = ticketRow('new');
    const ticket2 = ticketRow('done');
    const ticketlessMission = missionRow('running');

    const rows = [ticket1, ticketlessMission, ticket2];
    const result = filterLensRows(rows, 'backlog');

    expect(result).toEqual([ticket1, ticket2]);
  });
});

describe('filterLensRows - missions', () => {
  it('Missions lens shows only mission-backed rows', () => {
    const ticketOnly = ticketRow('new');
    const joinedTicket = ticketRow('done', { mission: { status: 'complete', merged: false } });

    const rows = [ticketOnly, joinedTicket, missionRow('running')];
    const result = filterLensRows(rows, 'missions');

    expect(result).not.toContain(ticketOnly);
    expect(result).toContain(joinedTicket);
  });

  it('caps terminal (landed/failed) mission rows to the 10 most recent, always keeps active rows', () => {
    const activeRunning = missionRow('running');
    const landedRows = Array.from({ length: 12 }, (_, i) =>
      missionRow('complete', true, `2026-01-${String(i + 1).padStart(2, '0')}T00:00:00Z`),
    );

    const rows = [activeRunning, ...landedRows];
    const result = filterLensRows(rows, 'missions');

    // active row with no createdAt is always kept
    expect(result).toContain(activeRunning);

    // exactly 10 of the 12 landed rows survive
    const survivingLanded = result.filter((r) => r !== activeRunning);
    expect(survivingLanded).toHaveLength(10);

    // the 10 most recent (days 03..12) survive; days 01,02 are dropped
    const survivingDates = survivingLanded.map((r) => r.missionCreatedAt).sort();
    expect(survivingDates).toEqual([
      '2026-01-03T00:00:00Z',
      '2026-01-04T00:00:00Z',
      '2026-01-05T00:00:00Z',
      '2026-01-06T00:00:00Z',
      '2026-01-07T00:00:00Z',
      '2026-01-08T00:00:00Z',
      '2026-01-09T00:00:00Z',
      '2026-01-10T00:00:00Z',
      '2026-01-11T00:00:00Z',
      '2026-01-12T00:00:00Z',
    ]);

    // preserves original input order
    expect(result).toEqual(rows.filter((r) => result.includes(r)));
  });

  it('excludes abandoned mission rows entirely', () => {
    const rows = [missionRow('abandoned'), missionRow('running')];
    const result = filterLensRows(rows, 'missions');
    expect(result).toEqual([missionRow('running')]);
  });

  it('keeps queued, reviewable, and delivered active mission-backed rows', () => {
    const queued = missionRow('approved');
    const reviewable = missionRow('planning');
    const delivered = missionRow('complete', false);
    const rows = [queued, reviewable, delivered];
    expect(filterLensRows(rows, 'missions')).toEqual(rows);
  });
});

describe('landedCount', () => {
  it('counts only landed-stage rows', () => {
    const rows = [
      ticketRow('done'),
      ticketRow('done', { mission: { status: 'complete', merged: true } }),
      ticketRow('done', { mission: { status: 'complete', merged: false } }),
      ticketRow('new'),
      missionRow('complete', true),
    ];
    expect(landedCount(rows)).toBe(3);
  });
});
