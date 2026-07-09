import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { PlanReview } from './PlanReview';
import { useKranzStore } from '../lib/store';
import type { CostEstimate, Plan } from '../lib/types';

const INITIAL_STORE_STATE = useKranzStore.getState();

function estimate(): CostEstimate {
  return {
    workerRuns: 1,
    validatorRuns: 1,
    lowUsd: 1,
    expectedUsd: 2,
    highUsd: 3,
  };
}

function plan(): Plan {
  return {
    goal: 'ship the thing',
    validationContract: [],
    consideredAlternatives: {
      chosen: 'small vertical slice',
      rejected: [
        { approach: 'big bang', tradeOff: 'too much review surface' },
        { approach: 'docs only', tradeOff: 'does not deliver behavior' },
      ],
    },
    milestones: [
      {
        title: 'M1',
        features: [
          {
            title: 'F1',
            spec: 'build it',
            validationCriteria: ['works'],
          },
        ],
      },
    ],
  };
}

beforeEach(() => {
  cleanup();
  useKranzStore.setState(
    {
      ...INITIAL_STORE_STATE,
      planning: {
        ...INITIAL_STORE_STATE.planning,
        review: { plan: plan(), estimate: estimate() },
      },
    },
    true,
  );
});

describe('PlanReview', () => {
  it('renders considered alternatives from the reviewed plan', () => {
    render(<PlanReview />);

    expect(screen.getByLabelText('Considered alternatives')).toBeTruthy();
    expect(screen.getByText(/small vertical slice/)).toBeTruthy();
    expect(screen.getByText(/big bang/)).toBeTruthy();
    expect(screen.getByText(/docs only/)).toBeTruthy();
  });

  it('disables the Approve button and shows Approving… while planning.approving', () => {
    useKranzStore.setState((s) => ({
      planning: { ...s.planning, approving: true },
    }));

    render(<PlanReview />);

    const button = screen.getByRole('button', { name: 'Approving…' }) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
  });
});
