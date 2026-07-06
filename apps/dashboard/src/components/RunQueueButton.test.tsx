import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/react';
import { RunQueueButton } from './RunQueueButton';
import type { DrainState, QueueEntry, QueueState } from '../lib/types';

vi.mock('../lib/api', async () => {
  const actual = await vi.importActual<typeof import('../lib/api')>('../lib/api');
  return {
    ...actual,
    api: {
      queue: vi.fn(),
      drainQueue: vi.fn(),
    },
  };
});

import { api, ApiError } from '../lib/api';

function makeEntry(overrides: Partial<QueueEntry> = {}): QueueEntry {
  return { missionId: 'm-1', priority: 5, seq: 1, ...overrides };
}

function idleDrain(): DrainState {
  return { live: false, currentMissionId: null, ran: [] };
}

function makeQueue(overrides: Partial<QueueState> = {}): QueueState {
  return { entries: [], busyWith: null, drain: idleDrain(), ...overrides };
}

beforeEach(() => {
  cleanup();
  vi.mocked(api.queue).mockReset();
  vi.mocked(api.drainQueue).mockReset();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('RunQueueButton', () => {
  it('disables the button and shows "queue empty" when there are no entries', async () => {
    vi.mocked(api.queue).mockResolvedValueOnce(makeQueue({ entries: [] }));

    render(<RunQueueButton />);

    const button = (await screen.findByRole('button', { name: 'Run queue' })) as HTMLButtonElement;
    await waitFor(() => expect(button.hasAttribute('disabled')).toBe(true));
    expect(screen.getByText('queue empty')).toBeTruthy();
  });

  it('enables the button and shows the queued count when entries exist', async () => {
    vi.mocked(api.queue).mockResolvedValueOnce(
      makeQueue({ entries: [makeEntry({ missionId: 'm-1' }), makeEntry({ missionId: 'm-2', seq: 2 })] }),
    );

    render(<RunQueueButton />);

    const button = (await screen.findByRole('button', { name: 'Run queue' })) as HTMLButtonElement;
    await waitFor(() => expect(button.hasAttribute('disabled')).toBe(false));
    expect(screen.getByText('2 queued')).toBeTruthy();
  });

  it('calls api.drainQueue on click and renders the live draining state', async () => {
    vi.mocked(api.queue).mockResolvedValueOnce(makeQueue({ entries: [makeEntry()] }));
    vi.mocked(api.drainQueue).mockResolvedValueOnce({
      live: true,
      currentMissionId: 'm-1',
      ran: [],
    });

    render(<RunQueueButton />);

    const button = (await screen.findByRole('button', { name: 'Run queue' })) as HTMLButtonElement;
    await waitFor(() => expect(button.hasAttribute('disabled')).toBe(false));

    fireEvent.click(button);

    expect(api.drainQueue).toHaveBeenCalledTimes(1);
    expect(await screen.findByText('Draining… m-1')).toBeTruthy();
    await waitFor(() => expect(button.hasAttribute('disabled')).toBe(true));
  });

  it('surfaces a failed drain POST as an inline error', async () => {
    vi.mocked(api.queue).mockResolvedValueOnce(makeQueue({ entries: [makeEntry()] }));
    vi.mocked(api.drainQueue).mockRejectedValueOnce(new ApiError(401, 'missing token'));

    render(<RunQueueButton />);

    const button = (await screen.findByRole('button', { name: 'Run queue' })) as HTMLButtonElement;
    await waitFor(() => expect(button.hasAttribute('disabled')).toBe(false));

    fireEvent.click(button);

    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toBe('missing token');
  });
});
