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
        review: { planIdentity: 'reviewed-plan-a', plan: plan(), estimate: estimate() },
      },
    },
    true,
  );
});

describe('PlanReview', () => {
  it('displays the reviewer requirements pinned by the preview', () => {
    const reviewed = plan();
    reviewed.reviewerIndependence = { scrutiny: true, functional: false };
    useKranzStore.setState((s) => ({
      planning: { ...s.planning, review: { ...s.planning.review!, plan: reviewed } },
    }));
    render(<PlanReview />);
    expect(screen.getByLabelText('Reviewer independence')).toBeTruthy();
    expect(screen.getByText('Scrutiny: required · Functional: not required')).toBeTruthy();
    expect(screen.getByText(/different from every worker attempt/)).toBeTruthy();
  });

  it('flight_rules_dashboard_groups_exact_consent_by_rfc_with_digest_and_waiver_posture', () => {
    const governed = plan();
    governed.standardsManifest = {
      packName: 'engineering-codex',
      packDir: 'vendor/codex',
      standardsRoot: 'standards',
      digest: 'ab'.repeat(32),
      source: 'repo-tracked',
      rules: [
        {
          id: 'ENG-RUST-014',
          revision: 3,
          rfc: 'RFC-014',
          level: 'must',
          effectiveStatus: 'enforced',
          statement: 'Rust changes must pass the workspace gate.',
          domains: ['rust'],
          stages: ['validation'],
          whenPaths: ['crates/'],
          taskClasses: [],
          checker: 'gate:workspace',
          waivable: false,
        },
      ],
    };
    useKranzStore.setState((state) => ({
      planning: { ...state.planning, review: { planIdentity: 'reviewed-plan-a', plan: governed, estimate: estimate() } },
    }));

    render(<PlanReview />);

    expect(screen.getByLabelText('Flight Rules consent')).toBeTruthy();
    expect(screen.getByText('RFC-014')).toBeTruthy();
    expect(screen.getByText('ENG-RUST-014 r3')).toBeTruthy();
    expect(screen.getByText(/enforced · MUST/)).toBeTruthy();
    expect(screen.getByText(/waiver: prohibited/)).toBeTruthy();
    expect(screen.getByText(`sha256:${'ab'.repeat(32)}`)).toBeTruthy();
  });

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
