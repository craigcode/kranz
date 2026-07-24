import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Outcomes } from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    getOutcomes: vi.fn(),
  };
});

import { getOutcomes } from '../lib/api';
import { OutcomesPanel } from './OutcomesPanel';

function emptyOutcomes(): Outcomes {
  return {
    autonomyRatio: {
      closedMissions: 0,
      totalInterventions: 0,
      interventionsPerClosedMission: 0,
      zeroInterventionMissions: 0,
      zeroInterventionShare: 0,
    },
    grantLatency: {
      buckets: [
        { label: '<10s', count: 0 },
        { label: '<60s', count: 0 },
        { label: '<10m', count: 0 },
        { label: '>=10m', count: 0 },
      ],
      totalDecided: 0,
    },
    escalations: [],
    costPerChange: {
      totalCostUsd: 0,
      nonMetaCommits: 0,
      usdPerCommit: null,
    },
    cycleTime: {
      closedMissions: 0,
      totalMs: 0,
      meanMs: null,
    },
  };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('OutcomesPanel', () => {
  it('renders the four latency buckets with their counts', async () => {
    const outcomes = emptyOutcomes();
    outcomes.grantLatency = {
      buckets: [
        { label: '<10s', count: 3 },
        { label: '<60s', count: 5 },
        { label: '<10m', count: 1 },
        { label: '>=10m', count: 2 },
      ],
      totalDecided: 11,
    };
    vi.mocked(getOutcomes).mockResolvedValue(outcomes);

    render(<OutcomesPanel />);

    await waitFor(() => {
      expect(screen.getByText('<10s').nextSibling?.textContent).toBe('3');
      expect(screen.getByText('<60s').nextSibling?.textContent).toBe('5');
      expect(screen.getByText('<10m').nextSibling?.textContent).toBe('1');
      expect(screen.getByText('>=10m').nextSibling?.textContent).toBe('2');
    });
  });

  it('renders escalation rows in newest-first order as received', async () => {
    const outcomes = emptyOutcomes();
    outcomes.escalations = [
      {
        ts: '2026-07-20T10:00:00Z',
        missionId: 'm-newest',
        kind: 'grant',
        summary: 'newest row',
        decision: 'approved',
        latencyMs: 500,
      },
      {
        ts: '2026-07-19T10:00:00Z',
        missionId: 'm-oldest',
        kind: 'block',
        summary: 'oldest row',
        decision: 'blocked',
        latencyMs: null,
      },
    ];
    vi.mocked(getOutcomes).mockResolvedValue(outcomes);

    render(<OutcomesPanel />);

    await waitFor(() => {
      expect(screen.getByText('newest row')).toBeTruthy();
    });
    const rows = screen.getAllByRole('row').slice(1); // drop header row
    expect(rows[0].textContent).toContain('m-newest');
    expect(rows[1].textContent).toContain('m-oldest');
  });

  it('renders every section empty state for an empty outcomes payload', async () => {
    vi.mocked(getOutcomes).mockResolvedValue(emptyOutcomes());

    render(<OutcomesPanel />);

    await waitFor(() => {
      expect(screen.getByText('No closed missions yet')).toBeTruthy();
    });
    expect(screen.getByText('No decided grants yet')).toBeTruthy();
    expect(screen.getByText('No escalations recorded')).toBeTruthy();
    expect(screen.getByText('No non-meta commits recorded yet')).toBeTruthy();
  });

  it('renders cost per change and cycle time from the fold', async () => {
    const outcomes = emptyOutcomes();
    outcomes.costPerChange = { totalCostUsd: 42.5, nonMetaCommits: 5, usdPerCommit: 8.5 };
    outcomes.cycleTime = { closedMissions: 3, totalMs: 7_200_000, meanMs: 2_400_000 };
    vi.mocked(getOutcomes).mockResolvedValue(outcomes);

    render(<OutcomesPanel />);

    await waitFor(() => {
      expect(screen.getByText('$8.50')).toBeTruthy();
      expect(screen.getByText('40m')).toBeTruthy();
    });
    expect(screen.getByText('$42.50')).toBeTruthy();
    expect(screen.getByText('5')).toBeTruthy();
  });
});
