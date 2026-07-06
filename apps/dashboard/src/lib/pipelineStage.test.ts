import { describe, expect, it } from 'vitest';
import { pipelineStage, primaryAction, type WorkItem, type PipelineStage } from './pipelineStage';

function ticket(state: 'new' | 'drafting' | 'needs-context' | 'review' | 'queued' | 'running' | 'done' | 'failed'): WorkItem {
  return { kind: 'ticket', ticket: { slug: 't-1', state } };
}

function ticketWithMission(
  state: 'new' | 'drafting' | 'needs-context' | 'review' | 'queued' | 'running' | 'done' | 'failed',
  mission: { status: any; merged: boolean | null },
): WorkItem {
  return { kind: 'ticket', ticket: { slug: 't-1', state }, mission };
}

function missionItem(status: any, merged: boolean | null = null): WorkItem {
  return { kind: 'mission', mission: { status, merged } };
}

describe('pipelineStage', () => {
  it('maps each pre-mission ticket state to its stage', () => {
    expect(pipelineStage(ticket('new'))).toBe('captured');
    expect(pipelineStage(ticket('drafting'))).toBe('drafting');
    expect(pipelineStage(ticket('needs-context'))).toBe('needs-you');
    expect(pipelineStage(ticket('review'))).toBe('reviewable');
    expect(pipelineStage(ticket('queued'))).toBe('queued');
  });

  it('maps ticket state running to running', () => {
    expect(pipelineStage(ticket('running'))).toBe('running');
  });

  it('maps ticket state failed directly to failed', () => {
    expect(pipelineStage(ticket('failed'))).toBe('failed');
  });

  it('maps a done ticket with no joined mission to landed (direct-fixed, terminal)', () => {
    expect(pipelineStage(ticket('done'))).toBe('landed');
  });

  it('maps a complete ticketless mission with merged=false to delivered', () => {
    expect(pipelineStage(missionItem('complete', false))).toBe('delivered');
  });

  it('maps a complete ticketless mission with merged=null to delivered', () => {
    expect(pipelineStage(missionItem('complete', null))).toBe('delivered');
  });

  it('maps a complete ticketless mission with merged=true to landed', () => {
    expect(pipelineStage(missionItem('complete', true))).toBe('landed');
  });

  it('maps a done ticket with joined mission merged=false to delivered', () => {
    expect(pipelineStage(ticketWithMission('done', { status: 'complete', merged: false }))).toBe('delivered');
  });

  it('maps a done ticket with joined mission merged=true to landed', () => {
    expect(pipelineStage(ticketWithMission('done', { status: 'complete', merged: true }))).toBe('landed');
  });

  it('maps running/paused/blocked/validating ticketless mission statuses to running', () => {
    for (const status of ['running', 'paused', 'blocked', 'validating']) {
      expect(pipelineStage(missionItem(status))).toBe('running');
    }
  });

  it('maps a failed ticketless mission to failed', () => {
    expect(pipelineStage(missionItem('failed'))).toBe('failed');
  });

  it('maps ticketless mission status planning to reviewable', () => {
    expect(pipelineStage(missionItem('planning'))).toBe('reviewable');
  });

  it('maps ticketless mission status approved to queued', () => {
    expect(pipelineStage(missionItem('approved'))).toBe('queued');
  });

  it('maps ticketless mission status abandoned to abandoned (inert)', () => {
    expect(pipelineStage(missionItem('abandoned'))).toBe('abandoned');
  });

  it('maps ticketless mission status deleted to abandoned (inert)', () => {
    expect(pipelineStage(missionItem('deleted'))).toBe('abandoned');
  });

  it('maps a done ticket joined to a deleted mission to landed', () => {
    expect(pipelineStage(ticketWithMission('done', { status: 'deleted', merged: null }))).toBe('landed');
  });

  it('maps a done ticket joined to an abandoned mission to landed', () => {
    expect(pipelineStage(ticketWithMission('done', { status: 'abandoned', merged: null }))).toBe('landed');
  });

  // Regression coverage: the ticket's own state governs its head stages.
  // A joined mission (recorded on the ticket as soon as drafting starts)
  // must NOT override the ticket's state for anything before 'done'.
  it('a review-state ticket with a joined approved mission stays reviewable', () => {
    expect(pipelineStage(ticketWithMission('review', { status: 'approved', merged: null }))).toBe('reviewable');
  });

  it('a queued-state ticket with a joined approved mission stays queued', () => {
    expect(pipelineStage(ticketWithMission('queued', { status: 'approved', merged: null }))).toBe('queued');
  });

  it('a drafting-state ticket with a joined planning mission stays drafting', () => {
    expect(pipelineStage(ticketWithMission('drafting', { status: 'planning', merged: null }))).toBe('drafting');
  });

  it('a needs-context ticket with a joined planning mission stays needs-you', () => {
    expect(pipelineStage(ticketWithMission('needs-context', { status: 'planning', merged: null }))).toBe(
      'needs-you',
    );
  });
});

describe('primaryAction', () => {
  const cases: Array<[PipelineStage, string, string | undefined]> = [
    ['captured', 'Draft', undefined],
    ['needs-you', 'Answer + redraft', undefined],
    ['reviewable', 'Queue', 'Reshape'],
    ['delivered', 'Merge', 'Iterate'],
    ['landed', 'Iterate', undefined],
    ['failed', 'Redraft', undefined],
  ];

  it.each(cases)('returns the documented action for %s', (stage, label, secondary) => {
    const action = primaryAction(stage);
    expect(action?.label).toBe(label);
    expect(action?.secondary).toBe(secondary);
  });

  it('has no primary action for drafting, queued, and running (watch / reorder / steer-only stages)', () => {
    expect(primaryAction('drafting')).toBeNull();
    expect(primaryAction('queued')).toBeNull();
    expect(primaryAction('running')).toBeNull();
  });

  it('has no primary action for abandoned (inert, dead row)', () => {
    expect(primaryAction('abandoned')).toBeNull();
  });
});
