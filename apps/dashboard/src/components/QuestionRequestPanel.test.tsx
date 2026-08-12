import { beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { QuestionRequestPanel } from './QuestionRequestPanel';
import { useKranzStore } from '../lib/store';
import type { MissionState, PendingQuestion } from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      answerQuestion: vi.fn(),
    },
  };
});

import { api, ApiError } from '../lib/api';

const INITIAL_STORE_STATE = useKranzStore.getState();

function missionState(pending?: PendingQuestion[]): MissionState {
  return {
    mission: {
      id: 'm-1',
      goal: 'test question UI',
      validationContract: [],
      milestones: [],
      status: 'running',
      createdAt: '2026-07-13T00:00:00Z',
      baseBranch: 'main',
      missionBranch: 'kranz/mission-m-1',
    },
    runs: {},
    totals: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
    totalCostUsd: 0,
    localExecutorMilestones: 0,
    escalatedMilestones: 0,
    pendingUserMessages: [],
    recentDecisions: [],
    // config is not read by this panel; a bare cast keeps the fixture small.
    config: {} as MissionState['config'],
    latestPlanRevision: 0,
    pendingQuestions: pending,
    lastSeq: 1,
  };
}

const OPEN_QUESTION: PendingQuestion = {
  questionId: 'q-1',
  role: 'worker',
  text: 'Which storage engine should the cache use?',
  options: ['sqlite', 'in-memory'],
  runId: 'r-1',
  featureId: 'f-1-1',
  milestoneId: 'ms-1',
};

beforeEach(() => {
  cleanup();
  vi.mocked(api.answerQuestion).mockReset().mockResolvedValue(undefined);
  useKranzStore.setState(
    { ...INITIAL_STORE_STATE, missionId: 'm-1', state: missionState() },
    true,
  );
});

describe('QuestionRequestPanel', () => {
  it('does not render when no question is open', () => {
    render(<QuestionRequestPanel />);
    expect(screen.queryByText('q-1', { exact: false })).toBeNull();
  });

  it('renders the open question and answers an option by index', async () => {
    useKranzStore.setState({ state: missionState([OPEN_QUESTION]) });
    render(<QuestionRequestPanel />);

    expect(screen.getByText('Which storage engine should the cache use?')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'sqlite' }));

    await waitFor(() =>
      expect(api.answerQuestion).toHaveBeenCalledWith('m-1', 'q-1', 'sqlite', 0),
    );
  });

  it('sends a free-text answer with no option index', async () => {
    useKranzStore.setState({ state: missionState([OPEN_QUESTION]) });
    render(<QuestionRequestPanel />);

    fireEvent.change(screen.getByPlaceholderText('Free-text answer...'), {
      target: { value: 'postgres, actually' },
    });
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));

    await waitFor(() =>
      expect(api.answerQuestion).toHaveBeenCalledWith('m-1', 'q-1', 'postgres, actually', undefined),
    );
  });

  it('renders a free-text-only question with no option buttons', () => {
    useKranzStore.setState({
      state: missionState([{ ...OPEN_QUESTION, questionId: 'q-2', options: [] }]),
    });
    render(<QuestionRequestPanel />);

    expect(screen.queryByRole('button', { name: 'sqlite' })).toBeNull();
    expect(screen.getByPlaceholderText('Free-text answer...')).toBeTruthy();
  });

  it('surfaces an error verbatim in a role="alert" block', async () => {
    vi.mocked(api.answerQuestion).mockRejectedValueOnce(
      new ApiError(409, "no open question 'q-1'"),
    );
    useKranzStore.setState({ state: missionState([OPEN_QUESTION]) });
    render(<QuestionRequestPanel />);

    fireEvent.click(screen.getByRole('button', { name: 'sqlite' }));

    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toBe("no open question 'q-1'");
  });
});
