import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { MissionPicker } from './MissionPicker';
import { useKranzStore } from '../lib/store';
import { missionCounts } from '../lib/missionCounts';
import type { MissionSummary } from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      ...actual.api,
      missions: vi.fn().mockResolvedValue([]),
    },
  };
});

function makeSummary(id: string, status: string, goal = `goal-${id}`, createdAt?: string): MissionSummary {
  return { id, status, goal, createdAt: createdAt ?? '2026-01-01T00:00:00Z' };
}

const FIXTURE: MissionSummary[] = [
  makeSummary('m-1', 'complete'),
  makeSummary('m-2', 'failed'),
  makeSummary('m-3', 'abandoned'),
  makeSummary('m-4', 'approved'),
  makeSummary('m-5', 'running'),
  makeSummary('m-6', 'validating'),
  makeSummary('m-7', 'planning'),
  { id: 'm-8', status: 'deleted', goal: 'deleted mission (no data recorded)', createdAt: undefined as unknown as string },
];

const INITIAL_STORE_STATE = useKranzStore.getState();

beforeEach(() => {
  cleanup();
  useKranzStore.setState({ ...INITIAL_STORE_STATE, missions: [], missionsError: null }, true);
});

describe('missionCounts', () => {
  it('counts finished and running, excluding planning and deleted from both', () => {
    const { finished, running } = missionCounts(FIXTURE);
    expect(finished).toBe(3);
    expect(running).toBe(3);
  });
});

describe('MissionPicker header and placeholder row', () => {
  it('renders the "N finished · M running" header', () => {
    useKranzStore.setState({ missions: FIXTURE });
    render(<MissionPicker />);
    expect(screen.getByText('3 finished · 3 running')).toBeTruthy();
  });

  it('does not render the old "N closed mission(s)" count', () => {
    useKranzStore.setState({ missions: FIXTURE });
    render(<MissionPicker />);
    expect(screen.queryByText(/\d+ closed missions?/i)).toBeNull();
  });

  it('renders a deleted row as a placeholder with no abandon/delete action', () => {
    useKranzStore.setState({ missions: FIXTURE });
    render(<MissionPicker />);
    const idNode = screen.getByText('m-8');
    const row = idNode.closest('li');
    expect(row).toBeTruthy();
    expect(screen.getByText('deleted mission (no data recorded)')).toBeTruthy();
    expect(row?.querySelector('.picker-action')).toBeFalsy();
  });
});
