import { describe, expect, it } from 'vitest';
import { pipelineStage, primaryAction, type WorkItem, type PipelineStage } from './pipelineStage';

function ticket(state: 'new' | 'drafting' | 'needs-context' | 'review' | 'queued' | 'running' | 'done' | 'failed'): WorkItem {
  return { kind: 'ticket', ticket: { slug: 't-1', state } };
}

function ticketWithMission(
  state: 'running' | 'done' | 'failed',
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

  it('maps ticket state failed directly to failed', () => {
    expect(pipelineStage(ticket('failed'))).toBe('failed');
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

  it('maps a complete ticket-joined mission with merged=false to delivered', () => {
    expect(pipelineStage(ticketWithMission('running', { status: 'complete', merged: false }))).toBe('delivered');
  });

  it('maps a complete ticket-joined mission with merged=true to landed', () => {
    expect(pipelineStage(ticketWithMission('running', { status: 'complete', merged: true }))).toBe('landed');
  });

  it('maps running/paused/blocked/validating mission statuses to running', () => {
    for (const status of ['running', 'paused', 'blocked', 'validating']) {
      expect(pipelineStage(missionItem(status))).toBe('running');
      expect(pipelineStage(ticketWithMission('running', { status, merged: null }))).toBe('running');
    }
  });

  it('maps a failed mission to failed', () => {
    expect(pipelineStage(missionItem('failed'))).toBe('failed');
    expect(pipelineStage(ticketWithMission('failed', { status: 'failed', merged: null }))).toBe('failed');
  });

  it('mission status governs the tail once a ticket has a joined mission, even mid-flow ticket states', () => {
    // ticket state is stale/irrelevant once a mission exists; the mission governs.
    expect(pipelineStage(ticketWithMission('done', { status: 'running', merged: null }))).toBe('running');
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
});
