import { beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { StandardsPanel } from './StandardsPanel';
import { useKranzStore } from '../lib/store';
import type { MissionStandardsView } from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      standards: vi.fn(),
      approveStandardsWaiver: vi.fn(),
    },
  };
});

import { api } from '../lib/api';

const INITIAL_STORE_STATE = useKranzStore.getState();

function view(): MissionStandardsView {
  const rule = {
    id: 'ENG-SEC-007',
    revision: 2,
    rfc: 'RFC-007',
    level: 'must',
    effectiveStatus: 'enforced',
    statement: 'Authentication changes require negative-path proof.',
    domains: ['security'],
    stages: ['validation'],
    whenPaths: ['src/auth/'],
    taskClasses: [],
    checker: 'gate:auth-negative',
    waivable: true,
  };
  return {
    manifest: {
      packName: 'engineering-codex',
      packDir: 'vendor/codex',
      standardsRoot: 'standards',
      digest: 'ab'.repeat(32),
      source: 'repo-tracked',
      rules: [rule],
    },
    coverage: {
      packName: 'engineering-codex',
      packDir: 'vendor/codex',
      standardsRoot: 'standards',
      digest: 'ab'.repeat(32),
      source: 'repo-tracked',
      approvalSeq: 2,
      rules: [
        {
          id: rule.id,
          revision: rule.revision,
          lifecycle: 'enforced',
          level: 'must',
          checker: rule.checker,
          statement: rule.statement,
          disposition: 'failed',
          evidence: [
            {
              seq: 9,
              event: 'validation.finding',
              mechanism: '__engine__',
              bearing: 'fail',
              reference: `flight-rule:${rule.id}`,
            },
          ],
        },
      ],
      drift: [
        {
          seq: 12,
          approvedDigest: 'ab'.repeat(32),
          currentDigest: 'cd'.repeat(32),
          changedRules: ['ENG-SEC-007 checker changed'],
        },
      ],
    },
    waiverCandidates: [
      {
        rule,
        findingSubject: `flight-rule:${rule.id}`,
        findingEvidence: 'negative-path test failed at assertion 4',
        runId: '__engine__',
      },
    ],
  };
}

beforeEach(() => {
  cleanup();
  useKranzStore.setState({ ...INITIAL_STORE_STATE, missionId: 'm-1' }, true);
  vi.mocked(api.standards).mockReset();
  vi.mocked(api.approveStandardsWaiver).mockReset();
  vi.mocked(api.standards).mockResolvedValue(view());
});

describe('StandardsPanel', () => {
  it('flight_rules_dashboard_renders_text_status_drift_and_exact_waiver_evidence', async () => {
    render(<StandardsPanel />);

    expect(await screen.findByText('failed: 1')).toBeTruthy();
    expect(screen.getByText('ENG-SEC-007 r2')).toBeTruthy();
    expect(screen.getByText('Policy drift refused merge.')).toBeTruthy();
    expect(screen.getByText(/there is no bypass/)).toBeTruthy();

    fireEvent.click(screen.getByRole('button', { name: /Review exact waiver/ }));
    expect(screen.getAllByText(/Authentication changes require/)).toHaveLength(2);
    expect(screen.getByText(/negative-path test failed at assertion 4/)).toBeTruthy();
    expect(screen.getByLabelText('Reason')).toBeTruthy();
    expect(screen.getByLabelText('Expires')).toBeTruthy();
  });

  it('renders nothing for an old mission with no standards', async () => {
    vi.mocked(api.standards).mockResolvedValue({});
    const { container } = render(<StandardsPanel />);
    await vi.waitFor(() => expect(api.standards).toHaveBeenCalledWith('m-1'));
    expect(container.textContent).toBe('');
  });
});
