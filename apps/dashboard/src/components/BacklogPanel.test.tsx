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
    isBlocked: false,
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
      makeSummary({
        slug: 'fix-b',
        title: 'Fix another thing',
        blockedBy: ['fix-a'],
        isBlocked: true,
      }),
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

  it('renders no active chip when isBlocked is false and blockedBy is empty', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeSummary({ slug: 'free-a', blockedBy: [], isBlocked: false }),
    ]);

    render(<BacklogPanel />);

    expect(await screen.findByText('free-a')).toBeTruthy();
    expect(screen.queryByText(/blocked by/)).toBeFalsy();
  });

  it('renders an active red chip when isBlocked is true and state is not done', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeSummary({ slug: 'blocked-a', state: 'review', blockedBy: ['dep-a'], isBlocked: true }),
    ]);

    render(<BacklogPanel />);

    const chip = await screen.findByText('blocked by dep-a');
    expect(chip.className).toContain('ticket-blocker-badge');
    expect(chip.className).not.toContain('ticket-blocker-badge--satisfied');
  });

  it('renders muted provenance when blockedBy is non-empty but isBlocked is false', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeSummary({ slug: 'satisfied-a', blockedBy: ['dep-a'], isBlocked: false }),
    ]);

    render(<BacklogPanel />);

    const chip = await screen.findByText('was blocked by dep-a');
    expect(chip.className).toContain('ticket-blocker-badge--satisfied');
    expect(screen.queryByText('blocked by dep-a')).toBeFalsy();
  });

  it('renders no active chip for a done ticket even when the payload sets isBlocked true', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeSummary({ slug: 'done-a', state: 'done', blockedBy: ['dep-a'], isBlocked: true }),
    ]);

    render(<BacklogPanel />);

    await screen.findByText('done-a');
    expect(screen.queryByText('blocked by dep-a')).toBeFalsy();
    const chip = await screen.findByText('was blocked by dep-a');
    expect(chip.className).toContain('ticket-blocker-badge--satisfied');
  });

  it('renders delivered + UNMERGED for a done ticket with merged:false', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeSummary({ slug: 'done-delivered', state: 'done', merged: false }),
    ]);

    render(<BacklogPanel />);

    const pill = await screen.findByText('delivered');
    expect(pill.className).toContain('pill-delivered');
    expect(screen.getByText('UNMERGED')).toBeTruthy();
    expect(screen.queryByText('done')).toBeFalsy();
  });

  it('renders landed with no UNMERGED badge for a done ticket with merged:true', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeSummary({ slug: 'done-landed', state: 'done', merged: true }),
    ]);

    render(<BacklogPanel />);

    const pill = await screen.findByText('landed');
    expect(pill.className).toContain('pill-landed');
    expect(screen.queryByText('UNMERGED')).toBeFalsy();
    expect(screen.queryByText('done')).toBeFalsy();
  });

  it('renders landed for a done ticket with merged:null', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeSummary({ slug: 'done-null', state: 'done', merged: null }),
    ]);

    render(<BacklogPanel />);

    const pill = await screen.findByText('landed');
    expect(pill.className).toContain('pill-landed');
    expect(screen.queryByText('UNMERGED')).toBeFalsy();
  });

  it('renders the raw state pill for a non-done ticket', async () => {
    vi.mocked(api.tickets).mockResolvedValueOnce([
      makeSummary({ slug: 'in-review', state: 'review' }),
    ]);

    render(<BacklogPanel />);

    const pill = await screen.findByText('review');
    expect(pill.className).toContain('pill-review');
  });
});
