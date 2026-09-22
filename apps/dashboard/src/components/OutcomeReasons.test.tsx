import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { OutcomeReasons } from './OutcomeReasons';
import type { OutcomeReasonReport } from '../lib/types';

afterEach(cleanup);

describe('OutcomeReasons', () => {
  it('keeps repaired history, overlapping denominators and provenance separate from current state', () => {
    const report: OutcomeReasonReport = {
      mappingVersion: 1, from: '2026-09-21T00:00:00Z', through: '2026-09-22T00:00:00Z',
      selection: 'Categories overlap; completion is not deployment.', unavailableLogs: ['m-unreadable'],
      taskClasses: [{ taskClass: 'bug-fix', missions: 1, mixedMissions: 1, unresolvedMissions: 0,
        counts: [
          { category: 'environment-prerequisite', missions: 1, observations: 2, share: 1 },
          { category: 'reported-defect', missions: 1, observations: 1, share: 1 },
        ] }],
      missions: [{ missionId: 'm-fixed', taskClass: 'bug-fix', currentStatus: 'complete', observations: [{
        seq: 3, ts: '2026-09-21T00:00:00Z', eventType: 'milestone.blocked', category: 'environment-prerequisite',
        detail: '<script>bad()</script>', state: 'resolved', resolutionSeq: 4, milestoneId: 'ms-1',
        featureId: null, runId: 'r-1', attemptId: null, permissionRequestId: null, stage: 'milestone-validation',
        actor: null, blockContext: { owner: 'engine', cause: 'authentication' }, deadline: null,
      }] }],
    };
    const { container } = render(<OutcomeReasons report={report} />);
    expect(screen.getByText(/m-fixed · current state: complete/)).toBeTruthy();
    expect(screen.getByText(/event #3 · milestone.blocked · resolved at event #4/)).toBeTruthy();
    expect(screen.getAllByText('1 / 1')).toHaveLength(2);
    expect(screen.getByText(/Logs unavailable.*m-unreadable/)).toBeTruthy();
    expect(screen.getByText('<script>bad()</script>')).toBeTruthy();
    expect(container.querySelector('script')).toBeNull();
    expect(container.querySelector('button')).toBeNull();
  });

  it('does not replace absent legacy data with zero counts', () => {
    render(<OutcomeReasons />);
    expect(screen.getByText('Reason mapping unavailable in this report.')).toBeTruthy();
    expect(screen.queryByRole('table')).toBeNull();
  });
});
