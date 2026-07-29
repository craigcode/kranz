import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { EscalationMetrics } from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    getEscalationMetrics: vi.fn(),
  };
});

import { getEscalationMetrics } from '../lib/api';
import { FlightSurgeonPanel } from './FlightSurgeonPanel';

function emptyMetrics(): EscalationMetrics {
  return {
    autonomy: {
      closedMissions: 0,
      zeroInterventionMissions: 0,
      zeroInterventionShare: null,
      completed: { missions: 0, zeroIntervention: 0, zeroInterventionShare: null },
      failed: { missions: 0, zeroIntervention: 0, zeroInterventionShare: null },
    },
    rubberStamp: {
      decidedGrants: 0,
      p50Ms: null,
      p90Ms: null,
      underTenSeconds: 0,
    },
    falseGreens: {
      completedMissions: 0,
      falseGreens: 0,
      falseGreenRate: null,
      withInterventions: { completedMissions: 0, falseGreens: 0, rate: null },
      zeroIntervention: { completedMissions: 0, falseGreens: 0, rate: null },
      tracedDefects: [],
    },
    ledger: [],
  };
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('FlightSurgeonPanel', () => {
  it('renders the three number cards from the fold', async () => {
    const metrics = emptyMetrics();
    metrics.autonomy = {
      closedMissions: 3,
      zeroInterventionMissions: 1,
      zeroInterventionShare: 1 / 3,
      completed: { missions: 2, zeroIntervention: 1, zeroInterventionShare: 0.5 },
      failed: { missions: 1, zeroIntervention: 0, zeroInterventionShare: 0 },
    };
    metrics.rubberStamp = { decidedGrants: 2, p50Ms: 5_000, p90Ms: 900_000, underTenSeconds: 1 };
    metrics.falseGreens = {
      completedMissions: 2,
      falseGreens: 1,
      falseGreenRate: 0.5,
      withInterventions: { completedMissions: 1, falseGreens: 0, rate: 0 },
      zeroIntervention: { completedMissions: 1, falseGreens: 1, rate: 1 },
      tracedDefects: [{ ticket: 'defect-login-regression', missionId: 'm-1' }],
    };
    vi.mocked(getEscalationMetrics).mockResolvedValue(metrics);

    render(<FlightSurgeonPanel />);

    await waitFor(() => {
      expect(screen.getByTestId('autonomy-card').textContent).toContain('33%');
    });
    const autonomy = screen.getByTestId('autonomy-card');
    expect(autonomy.textContent).toContain('1 of 3 closed');
    expect(autonomy.textContent).toContain('completed 50%');
    expect(autonomy.textContent).toContain('failed 0%');

    const stamp = screen.getByTestId('rubber-stamp-card');
    expect(stamp.textContent).toContain('5s');
    expect(stamp.textContent).toContain('p90 15m');
    expect(stamp.textContent).toContain('1 of 2 decided under 10s');

    const greens = screen.getByTestId('false-greens-card');
    expect(greens.textContent).toContain('50%');
    expect(greens.textContent).toContain('1 of 2 completed');
    expect(greens.textContent).toContain('autonomous 100%');
  });

  it('renders ledger rows newest-first as received', async () => {
    const metrics = emptyMetrics();
    metrics.ledger = [
      {
        ts: '2026-07-20T10:00:00Z',
        missionId: 'm-newest',
        kind: 'grant',
        milestoneId: 'ms-1',
        ask: 'command: cargo test',
        decision: 'approved',
        latencyMs: 5000,
      },
      {
        ts: '2026-07-19T10:00:00Z',
        missionId: 'm-oldest',
        kind: 'steer',
        milestoneId: null,
        ask: 'skip the flaky test',
        decision: 'steered',
        latencyMs: null,
      },
    ];
    vi.mocked(getEscalationMetrics).mockResolvedValue(metrics);

    render(<FlightSurgeonPanel />);

    await waitFor(() => {
      expect(screen.getByText('command: cargo test')).toBeTruthy();
    });
    const rows = screen.getAllByRole('row').slice(1); // drop header row
    expect(rows[0].textContent).toContain('m-newest');
    expect(rows[0].textContent).toContain('ms-1');
    expect(rows[0].textContent).toContain('5000ms');
    expect(rows[1].textContent).toContain('m-oldest');
    expect(rows[1].textContent).toContain('steered');
  });

  it('renders empty states for an empty payload', async () => {
    vi.mocked(getEscalationMetrics).mockResolvedValue(emptyMetrics());

    render(<FlightSurgeonPanel />);

    await waitFor(() => {
      expect(screen.getByText('No escalations recorded')).toBeTruthy();
    });
    // Null shares render as dashes, never as vacuous zeros.
    expect(screen.getByTestId('autonomy-card').textContent).toContain('—');
    expect(screen.getByTestId('rubber-stamp-card').textContent).toContain('0 of 0 decided under 10s');
    expect(screen.getByTestId('false-greens-card').textContent).toContain('0 of 0 completed');
  });

  it('renders the error state when the fetch fails', async () => {
    vi.mocked(getEscalationMetrics).mockRejectedValue(new Error('401 unauthorized'));

    render(<FlightSurgeonPanel />);

    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toContain('401 unauthorized');
    });
  });
});
