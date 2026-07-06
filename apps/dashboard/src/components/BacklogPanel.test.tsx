import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/react';
import { BacklogPanel } from './BacklogPanel';
import { useKranzStore } from '../lib/store';
import type { TicketSummary } from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      tickets: vi.fn(),
    },
  };
});

import { api } from '../lib/api';

function makeSummary(overrides: Partial<TicketSummary> = {}): TicketSummary {
  return {
    slug: 'fix-a',
    priority: 2,
    state: 'review',
    title: 'Fix the thing',
    blockedBy: [],
    ...overrides,
  };
}

const INITIAL_STORE_STATE = useKranzStore.getState();

beforeEach(() => {
  cleanup();
  vi.mocked(api.tickets).mockReset();
  useKranzStore.setState({ ...INITIAL_STORE_STATE, tickets: [], ticketsError: null }, true);
});

describe('BacklogPanel', () => {
  it('renders ticket rows from store state', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeSummary({ slug: 'fix-a', title: 'Fix the thing' }),
      makeSummary({ slug: 'fix-b', title: 'Fix another thing', blockedBy: ['fix-a'] }),
    ]);

    render(<BacklogPanel />);

    expect(await screen.findByText('fix-a')).toBeTruthy();
    expect(screen.getByText('fix-b')).toBeTruthy();
    expect(screen.getByText('Fix the thing')).toBeTruthy();
    expect(screen.getByText('blocked by fix-a')).toBeTruthy();
  });

  it('shows ticketsError with a retry action', async () => {
    vi.mocked(api.tickets).mockRejectedValueOnce(new Error('backlog unreachable'));

    render(<BacklogPanel />);

    expect(await screen.findByText(/backlog unreachable/)).toBeTruthy();
  });
});
