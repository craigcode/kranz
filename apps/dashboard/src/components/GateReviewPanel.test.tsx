import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { GateReviewPanel } from './GateReviewPanel';
import { useKranzStore } from '../lib/store';
import type { GateEvaluation, MissionState } from '../lib/types';

afterEach(cleanup);
const pending = (): GateEvaluation => ({
  requested: { request: { params: { attemptId: 'attempt-1', gateId: 'reviewer', stage: 'final-gate',
    deadline: '2000-01-01T00:00:00Z', subject: { kind: 'deliverable' }, binding: {} } } },
  requestedSeq: 7,
  finished: { outcome: { status: 'evaluated', result: { rationale: '<script>untrusted finding</script>' } } },
  resolution: { disposition: 'require-human', rationale: 'review needed' },
});
function show(records?: Record<string, GateEvaluation>) {
  useKranzStore.setState({ state: { gateEvaluations: records } as MissionState });
  return render(<GateReviewPanel />);
}
describe('gate review obligations', () => {
  it('shows escaped evidence and requires a fresh attempt after expiry', () => {
    show({ a: pending() });
    expect(screen.getByText('<script>untrusted finding</script>')).toBeTruthy();
    expect(screen.getByRole('status').textContent).toContain('expired');
    expect(document.querySelector('script')).toBeNull();
    expect(screen.queryByRole('button', { name: /approve/i })).toBeNull();
  });
  it('omits closed, consumed and proceeding attempts', () => {
    show({ closed: { ...pending(), closed: 'superseded' }, consumed: { ...pending(), consumed: {} },
      proceeded: { ...pending(), resolution: { disposition: 'proceed', rationale: 'done' } } });
    expect(screen.queryByText('Gate review needed')).toBeNull();
  });
  it('preserves the empty view for legacy logs', () => {
    const { container } = show();
    expect(container.textContent).toBe('');
  });
});
