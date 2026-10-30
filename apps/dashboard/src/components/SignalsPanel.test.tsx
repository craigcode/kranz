import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { SignalsPanel } from './SignalsPanel';
import { api } from '../lib/api';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      ...actual.api,
      signals: vi.fn(),
    },
  };
});

beforeEach(() => {
  cleanup();
  vi.mocked(api.signals).mockReset();
});

describe('SignalsPanel', () => {
  it('lists signals across triage outcomes with ledger evidence', async () => {
    vi.mocked(api.signals).mockResolvedValueOnce({
      signals: [
        {
          signalId: 'sig-1',
          source: 'sentry',
          fingerprint: 'issue-101',
          project: 'kranz',
          severity: 'error',
          rawSha256: 'abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789',
          status: 'proposed',
          ticketSlug: 'signal-sentry-issue-101',
          dedupeCount: 3,
          candidateMissions: ['m-111111'],
          ledgerEntries: [
            { seq: 1, timestamp: '2026-07-28T00:00:00Z', record: { kind: 'received' } },
            { seq: 2, timestamp: '2026-07-28T00:00:01Z', record: { kind: 'triaged' } },
          ],
        },
        {
          signalId: 'sig-2',
          source: 'sentry',
          fingerprint: 'issue-202',
          project: 'kranz',
          severity: 'info',
          rawSha256: '1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef',
          status: 'parked',
          reason: 'below severity threshold',
          dedupeCount: 0,
          candidateMissions: [],
          ledgerEntries: [
            { seq: 3, timestamp: '2026-07-28T00:01:00Z', record: { kind: 'received' } },
          ],
        },
      ],
      dropped: [
        {
          seq: 4,
          timestamp: '2026-07-28T00:02:00Z',
          source: 'sentry',
          reason: 'invalid signature',
          rawSha256: 'deadbeef0000111122223333444455556666777788889999aaaabbbbccccdddd',
        },
      ],
    });

    render(<SignalsPanel />);

    expect(await screen.findByText('sig-1')).toBeTruthy();
    expect(screen.getByText('proposed')).toBeTruthy();
    expect(screen.getByText('signal-sentry-issue-101')).toBeTruthy();
    expect(screen.getByText(/sha256:abcdef012345/)).toBeTruthy();
    expect(screen.getByText('sig-2')).toBeTruthy();
    expect(screen.getByText('parked')).toBeTruthy();
    expect(screen.getByText(/invalid signature/)).toBeTruthy();
  });
});
