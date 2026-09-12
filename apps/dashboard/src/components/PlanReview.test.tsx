import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, cleanup, fireEvent, within } from '@testing-library/react';
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
  it('keeps legacy assertions free of negative-control UI', () => {
    const reviewed = plan();
    reviewed.validationContract = [{ id: 'a1', check: 'command', statement: 'tests pass', command: 'npm test' }];
    useKranzStore.setState(s => ({
      planning: { ...s.planning, review: { ...s.planning.review!, plan: reviewed } },
    }));
    render(<PlanReview />);
    expect(screen.getByText('npm test')).toBeTruthy();
    expect(screen.queryByText('Negative control')).toBeNull();
    expect(screen.queryByText(/Advisory control configuration/)).toBeNull();
  });

  it.each([null, 'kranz/mission-m1'])('shows reviewable controls without claiming execution state (approved branch: %s)', (approvedBranch) => {
    const reviewed = plan();
    reviewed.validationContract = [{
      id: 'a1', check: 'command', statement: 'reject invalid input', command: 'node check.js',
      negativeControl: {
        checkerFiles: [{ path: 'check.js', content: 'checkFixture("input.json");\n' }],
        validFiles: [{ path: 'input.json', content: '{ "valid": true }\n' }],
        defectiveFiles: [{ path: 'input.json', content: '<script>defective input</script>\n' }],
        expectedFailure: 'invalid_input', timeoutSeconds: 90,
      },
    }];
    useKranzStore.setState(s => ({
      planning: { ...s.planning, approvedBranch, review: { ...s.planning.review!, plan: reviewed } },
    }));
    const { container } = render(<PlanReview />);
    const panel = screen.getByLabelText('Negative control for a1');
    expect(screen.getByText('node check.js')).toBeTruthy();
    expect(within(panel).getByText('Advisory control configuration; results are recorded with gate evidence.')).toBeTruthy();
    expect(within(panel).queryByText(/not yet run/)).toBeNull();
    expect(within(panel).getByText('invalid_input')).toBeTruthy();
    expect(within(panel).getByText('Timeout: 90s per fixture (maximum 180s).')).toBeTruthy();
    for (const label of ['Checker files', 'Valid fixture files', 'Defective fixture files']) {
      expect(within(panel).getByText(label)).toBeTruthy();
    }
    expect(within(panel).getAllByText('input.json')).toHaveLength(2);
    const details = panel.querySelectorAll('details');
    expect(details).toHaveLength(3);
    for (const file of details) {
      expect(file.open).toBe(false);
      fireEvent.click(file.querySelector('summary')!);
      expect(file.open).toBe(true);
    }
    expect(details[0].querySelector('pre')?.textContent).toBe('checkFixture("input.json");\n');
    expect(details[1].querySelector('pre')?.textContent).toBe('{ "valid": true }\n');
    expect(details[2].querySelector('pre')?.textContent).toBe('<script>defective input</script>\n');
    expect(container.querySelector('script')).toBeNull();
    expect(screen.getByRole('button', { name: approvedBranch ? 'Start execution' : 'Approve & commit plan' })).toBeTruthy();
  });

  it('shows the default control timeout and an explicit empty file', () => {
    const reviewed = plan();
    reviewed.validationContract = [{
      id: 'a2', check: 'command', statement: 'detect empty fixture', command: 'node check.js',
      negativeControl: {
        checkerFiles: [{ path: 'check.js', content: 'check();' }],
        validFiles: [{ path: 'value.txt', content: 'present' }],
        defectiveFiles: [{ path: 'value.txt', content: '' }],
        expectedFailure: 'empty_fixture',
      },
    }];
    useKranzStore.setState(s => ({
      planning: { ...s.planning, review: { ...s.planning.review!, plan: reviewed } },
    }));
    render(<PlanReview />);
    const panel = screen.getByLabelText('Negative control for a2');
    expect(within(panel).getByText('Timeout: 60s per fixture (maximum 180s).')).toBeTruthy();
    expect(within(panel).getByText('Empty file')).toBeTruthy();
  });

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
